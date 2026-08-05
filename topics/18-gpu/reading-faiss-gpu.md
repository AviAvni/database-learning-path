# Faiss GPU: k-select that never leaves registers

Johnson, Douze and Jégou's 2017 paper made billion-scale GPU ANN real, and it
did it with one algorithmic idea and one systems discipline. The idea:
*k*-selection whose entire state lives in registers, so the step everyone else
staged through memory becomes free. The discipline: the index is resident on the
device and only queries cross the bus — Crystal's regime B, practiced three
years before Crystal named it.

This chapter builds the vocabulary the paper assumes (IVF, PQ, ADC), then the
two ideas, then the limits the implementation actually enforces — which are not
the ones the wiki lists.

You cannot run any of it here: the GPU half of Faiss is CUDA, and this machine
has no CUDA device. Every number below is either quoted from the paper with the
2017 hardware it was measured on, or read out of source. The only measurements
this repo owns are on the wgpu/Metal lane (`notes.md:9-16`), and they measure a
sum, not a search.

Code anchors are [facebookresearch/faiss](https://github.com/facebookresearch/faiss)
at tag **v1.15.0** — note that faiss is *not* in this repo's pin table
(`resources/codebases.md`), so the tag is the pin; reproduce any anchor with
`python3 tools/pinned-source.py --ref v1.15.0 show facebookresearch/faiss <path>
-r A:B`. Paper citations are to
[arXiv:1702.08734](https://arxiv.org/abs/1702.08734) (IEEE Trans. Big Data
2019).

## The problem in one sentence

An IVF scan produces millions of candidate distances per query and you want the
best 100; the CPU's answer is a heap, which is serial and branchy and therefore
the worst possible warp code — so on a GPU the *selection*, not the distance
arithmetic, was the bottleneck, and the paper's fix is to keep the selector's
whole state in registers and never write a distance to memory at all.

## The concepts, step by step

### Step 1 — IVF, PQ, ADC: what has to happen before a distance is computed

> **In:** a billion d-dimensional vectors that do not fit anywhere fast.
> **Out:** an 8-byte code per vector and a query plan that touches ~1 % of them.

Three compressions, each named:

- **IVF** (inverted file) — cluster all vectors with a **coarse quantizer**
  (k-means centroids, |C₁| of them); at query time compute the query's distance
  to every centroid and scan only the `nprobe` nearest lists. The coarse step is
  itself a small brute-force search, which is why Step 6's fused kernel matters
  twice.
- **PQ** (product quantization) — split each vector into *m* sub-vectors,
  quantize each against its own 2^nbits-entry codebook. At m = 8, nbits = 8, a
  128-dim float vector (512 B) becomes 8 bytes, and a billion of them fit in
  8 GB.
- **ADC** (asymmetric distance computation) — the query is *not* quantized.
  Per query, build a table of distances from the query's sub-vectors to every
  codebook entry; then each candidate's distance is *m* table lookups and *m*
  adds. §5.2 develops the expansion; the practical shape is 8 lookups per 8
  bytes of code read.

That last line is the whole reason ANN is a GPU workload: it converts "distance
to a million candidates" into a bandwidth problem with a tiny table, and
bandwidth is what a discrete GPU sells.

### Step 2 — the residency rule, with the only transfer rate this repo measured

> **In:** a memory hierarchy split by a bus.
> **Out:** a placement rule per data class — and an order-of-magnitude reason to
> obey it.

```
  data class          lives                  why
  PQ codes (1B x 8B)  device memory          scanned every query; needs bandwidth
  coarse centroids    device memory          small, touched by every query
  full-precision      host RAM / disk        only for optional rescore
  queries             cross the bus, batched  kilobytes, the only per-query traffic
```

The paper simply assumes this and reports numbers against it; `reading-crystal-
sigmod20.md` is where the assumption is made explicit and quantified (Crystal's
Table 2: 880 GBps device memory against 53 GBps for the CPU, with PCIe rated at
"up to 16 GBps" in §2.2 and measured at 12.8 GBps in §5). Do the placement
arithmetic with the only numbers this repo has measured itself — the wgpu/Metal
lane at 16 M elements, `notes.md:16`:

```
  measured upload rate (16M elems, 67,108,864 B in 7384.7 us)  = 9.09 GB/s
  measured CPU sum rate over the same bytes (2257.7 us)        = 29.7 GB/s

  index, 1e9 vectors x 8 B PQ code                             = 8.0 GB
      one-time upload at 9.09 GB/s                             = 0.88 s
  query batch, 1e4 queries x 128 dims x 4 B                    = 5.12 MB
      per-batch upload at 9.09 GB/s                            = 0.56 ms
                                                                 ------
  ratio, index bytes : query bytes                             = 1562 : 1
```

Resident, you pay 0.88 s once. Non-resident, you pay it per batch — 1562× the
traffic of the thing you actually needed to send. This topic's own headline is
the same lesson without the index: at 16 M elements upload alone costs 7197 µs
against a 2723 µs CPU total (`FINDINGS.md:36`).

### Step 3 — why a heap is the wrong data structure on a warp

> **In:** 32 lanes issuing one instruction.
> **Out:** an argument for fixed-schedule compare-exchange networks over
> anything with a data-dependent control path.

A binary heap's sift-down branches on comparisons, so 32 lanes inserting 32
different values want 32 different instruction sequences. The warp executes both
sides of every divergent branch with the wrong lanes masked off; a k-selection
built from heaps runs at a fraction of issue rate no matter how good the memory
system is.

A **sorting network** has a schedule fixed before the data exists: the same
compare-exchange pairs in the same order, "maybe swap" compiled to branch-free
min/max. Zero divergence by construction — the same property that made
branchless filtering win in topic 17, and the property CAGRA's bitonic top-M
relies on (`reading-cagra.md`, Step 5).

Faiss's networks are *odd-size*, and the paper is precise about why that
mattered: Batcher's classic formulation *"would require that 32t = k and is a
power-of-2; thus if k = 1024, t must be 32. We found that the optimal t is way
smaller"* (§4.3). So the paper builds `merge-odd` and `sort-odd` (Algorithms 1
and 2), which merge arrays of unequal, non-power-of-two lengths in
`⌈log₂(max(ℓL, ℓR))⌉ + 1` parallel steps. Calling them "odd-even merge networks"
— as an earlier version of this guide did — names the wrong thing: odd-*even* is
Batcher's, and avoiding its size constraint is the contribution.

### Step 4 — WarpSelect: two queues, both in registers

> **In:** a stream of ℓ distances per query.
> **Out:** the k smallest, in one pass, with no shared memory and no cross-warp
> synchronisation.

The paper's own summary: *"Our k-selection implementation, WarpSelect, maintains
state entirely in registers, requires only a single pass over data and avoids
cross-warp synchronization… Since the register file provides much more storage
than shared memory, it supports k ≤ 1024"* (§4.2).

Two levels. Each lane owns a **thread queue** of *t* elements in registers, and
the warp collectively owns a **warp queue** of k elements held as a *lane-stride
register array* — element i lives in lane `i % 32`, so a "shared" array costs no
memory at all. Lane j reads elements a_j, a_{32+j}, … so the reads are
*"contiguous and coalesced into a minimal number of memory transactions"*
(§4.2).

The whole update rule is 45 lines of C++:

```cpp
// faiss/gpu/utils/Select.cuh:439-447 and 469-482 — construction and the fast
// path. kLane is where the current kth-best lives in the lane-stride array.
   439  struct WarpSelect {
   440      static constexpr int kNumWarpQRegisters = NumWarpQ / kWarpSize;
   442      __device__ inline WarpSelect(K initKVal, V initVVal, int k)
   443              : initK(initKVal),
   444                initV(initVVal),
   445                numVals(0),
   446                warpKTop(initKVal),
   447                kLane((k - 1) % kWarpSize) {
...
   469      __device__ inline void addThreadQ(K k, V v) {
   470          if (Dir ? Comp::gt(k, warpKTop) : Comp::lt(k, warpKTop)) {
   471              // Rotate right
   472  #pragma unroll
   473              for (int i = NumThreadQ - 1; i > 0; --i) {
   474                  threadK[i] = threadK[i - 1];
   475                  threadV[i] = threadV[i - 1];
   476              }
   478              threadK[0] = k;
   479              threadV[0] = v;
   480              ++numVals;
   481          }
   482      }
```

Note what `addThreadQ` is not: there is no memory access, no `__syncthreads`,
and the loop is `#pragma unroll` over a compile-time constant so `threadK[]`
stays in registers. The expensive path runs only when some lane is full, and the
warp finds out with a single ballot:

```cpp
// faiss/gpu/utils/Select.cuh:484-512 — the slow path, and how the threshold is
// republished afterwards.
   484      __device__ inline void checkThreadQ() {
   485          bool needSort = (numVals == NumThreadQ);
   490          needSort = __any_sync(0xffffffff, needSort);
   493          if (!needSort) {
   494              // no lanes have triggered a sort
   495              return;
   496          }
   498          mergeWarpQ();
   500          // Any top-k elements have been merged into the warp queue; we're
   501          // free to reset the thread queues
   502          numVals = 0;
   510          // We have to beat at least this element
   511          warpKTop = shfl(warpK[kNumWarpQRegisters - 1], kLane);
```

Line 511 is the design in one statement: the new rejection threshold is the
current kth-best, fetched from another lane by a **shuffle** — a register-to-
register instruction. Nothing about this algorithm ever addresses memory.

The tuning parameter *t* is chosen per k. §4.3: *"For k ≤ 32, we use t = 2,
k ≤ 128 uses t = 3, k ≤ 256 uses t = 4, and k ≤ 1024 uses t = 8, all
irrespective of ℓ."* The shipped instantiations agree, one file per k, with the
thread-queue length as the last macro argument:

```
  faiss/gpu/utils/warpselect/WarpSelectFloat1.cu:13     WARP_SELECT_IMPL(float, true, 1, 1)
  faiss/gpu/utils/warpselect/WarpSelectFloat32.cu:13    WARP_SELECT_IMPL(float, true, 32, 2)
  faiss/gpu/utils/warpselect/WarpSelectFloat64.cu:13    WARP_SELECT_IMPL(float, true, 64, 3)
  faiss/gpu/utils/warpselect/WarpSelectFloat128.cu:13   WARP_SELECT_IMPL(float, true, 128, 3)
  faiss/gpu/utils/warpselect/WarpSelectFloat256.cu:13   WARP_SELECT_IMPL(float, true, 256, 4)
  faiss/gpu/utils/warpselect/WarpSelectFloatF512.cu:13  WARP_SELECT_IMPL(float, false, 512, 8)
  faiss/gpu/utils/warpselect/WarpSelectFloatF1024.cu:13 WARP_SELECT_IMPL(float, false, 1024, 8)
  faiss/gpu/utils/warpselect/WarpSelectFloatF2048.cu:15 WARP_SELECT_IMPL(float, false, 2048, 8)
```

The last line is a correction to make: the paper's ceiling was 1024, and the
shipped ceiling is **2048**, conditional on the compiler:

```cpp
// faiss/gpu/utils/DeviceDefs.cuh:61-68 — the selection ceiling is a register
// allocation question, and the comment says so.
    61  #if CUDA_VERSION > 9000
    62  // Based on the CUDA version (we assume what version of nvcc/ptxas we were
    63  // compiled with), the register allocation algorithm is much better, so only
    64  // enable the 2048 selection code if we are above 9.0 (9.2 seems to be ok)
    65  #define GPU_MAX_SELECTION_K 2048
    66  #else
    67  #define GPU_MAX_SELECTION_K 1024
    68  #endif
```

### Step 5 — what the ceiling costs: registers, counted

> **In:** `kNumWarpQRegisters = NumWarpQ / kWarpSize` (`Select.cuh:440`).
> **Out:** the per-lane register bill, and the reason there is a ceiling at all.

The warp queue is spread across the warp, so k elements cost k/32 registers per
lane — for keys, and again for values. The thread queue costs t of each. Count
it for the two extremes actually shipped:

```
  k = 128, t = 3 (WarpSelectFloat128.cu)
      warp queue    = 128 / 32       =  4 key regs +  4 value regs =   8
      thread queue  = 3              =  3 key regs +  3 value regs =   6
                                                                     ----
      per lane                                                     =  14 regs
      per warp      = 14 x 32 lanes                                = 448 regs

  k = 2048, t = 8 (WarpSelectFloatF2048.cu)
      warp queue    = 2048 / 32      = 64 key regs + 64 value regs = 128
      thread queue  = 8              =  8 key regs +  8 value regs =  16
                                                                     ----
      per lane                                                     = 144 regs
      per warp      = 144 x 32 lanes                               = 4608 regs
```

144 registers per lane for queue state alone, before the kernel's own working
set — which is exactly why `DeviceDefs.cuh:62-64` makes 2048 conditional on a
compiler with a better allocator, and why the paper stopped at 1024 in 2017.
Registers are not free storage; they are the scarcest storage, and spilling them
would defeat the entire design.

The contrast is in the same file. `BlockSelect` — the whole-block variant used
when one warp per query is not enough parallelism — puts the warp queues in
shared memory instead:

```cpp
// faiss/gpu/utils/Select.cuh:177-187 — BlockSelect's queues are slices of a
// shared-memory array, one per warp, not registers.
   177          int laneId = getLaneId();
   178          int warpId = threadIdx.x / kWarpSize;
   179          warpK = sharedK + warpId * kTotalWarpSortSize;
   180          warpV = sharedV + warpId * kTotalWarpSortSize;
   182          // Fill warp queue (only the actual queue space is fine, not where
   183          // we write the per-thread queues for merging)
   184          for (int i = laneId; i < NumWarpQ; i += kWarpSize) {
   185              warpK[i] = initK;
   186              warpV[i] = initV;
   187          }
```

and the caller pays for it in shared memory that scales with warps × k
(`L2Select.cu:146-149`): at 128 threads (4 warps) and k = 1024 that is
4 × 1024 × (4 + 4) = 32 KB per block. Same algorithm, different storage class,
completely different occupancy.

### Step 6 — fusion: the pass that never happens

> **In:** a GEMM that produces an nq × ℓ partial distance matrix.
> **Out:** two passes over it instead of three — and a measured 25 % for the
> pass you skipped.

Exact search is a matrix multiply: cuBLAS computes the −2⟨x_j, y_i⟩ term into a
partial matrix D′, and then the ‖y_i‖² term has to be added and the top-k taken.
The naive pipeline writes D′, reads it to add norms, writes it again, reads it
to select. Faiss adds and selects in the same kernel:

> *"To complete the distance calculation, we use a fused k-selection kernel that
> adds the ‖y_i‖² term to each entry of the distance matrix and immediately
> submits the value to k-selection in registers… Kernel fusion thus allows for
> only 2 passes (GEMM write, k-select read) over D′, compared to other
> implementations that may require 3 or more."* (§5.1)

The kernel is 50 lines and does exactly what the sentence says:

```cpp
// faiss/gpu/impl/L2Select.cu:161-179 — the fused add-and-select loop. There is
// no intermediate distance array: `v` is computed and consumed in registers.
   161      IndexT row = blockIdx.x;
   163      // Whole warps must participate in the selection
   164      IndexT limit = utils::roundDown(productDistances.getSize(1), kWarpSize);
   165      IndexT i = threadIdx.x;
   167      for (; i < limit; i += blockDim.x) {
   168          T v = Math<T>::add(centroidDistances[i], productDistances[row][i]);
   169          heap.add(v, IndexT(i));
   170      }
   172      // Handle the remainder if any separately (warp is divergent)
   173      if (i < productDistances.getSize(1)) {
   174          T v = Math<T>::add(centroidDistances[i], productDistances[row][i]);
   175          heap.addThreadQ(v, IndexT(i));
   176      }
   178      // Merge all final results
   179      heap.reduce();
```

Measured, on SIFT1M (ℓ = 10⁶, d = 128, nq = 10⁴) on one Maxwell Titan X: the
whole pipeline reaches *"85 % of the peak possible performance, assuming GEMM
usage and our tiling"*, and *"Our same exact algorithm without fusion (requiring
an additional pass through D′) is at least 25 % slower"* (§6.3). The paper also
notes the limit of the idea: *"Row-wise k-selection is likely not fusable with a
well-tuned GEMM kernel"* (§5.1) — you can fuse the cheap pass into the selector,
not the selector into the GEMM.

Why the two halves of an IVF-PQ query behave differently is arithmetic. Take the
coarse quantizer and the PQ scan with the same batch:

```
  coarse quantizer: GEMM, nq x d by d x |C1|, with nq = 1024, d = 128, |C1| = 8192
      flops   = 2 x nq x |C1| x d           = 2.15e9
      bytes   = 4 x (nq x d + |C1| x d + nq x |C1|)
              = 4 x (131,072 + 1,048,576 + 8,388,608)          = 38.3 MB
      intensity = 2.15e9 / 38.3e6                              = 56 flop/byte
                                                                 -> compute bound

  PQ scan (ADC), m = 8 bytes per vector
      per candidate: 8 table lookups + 8 adds                  = ~16 ops
      per candidate: 8 bytes of code read
      intensity = 16 / 8                                       = 2 ops/byte
                                                                 -> bandwidth bound
```

Two kernels, one query, an intensity gap of ~28×. That is why the paper's
performance story is a GEMM story on one side and a memory story on the other —
and why the selection step had to stop touching memory before either could
matter.

### Step 7 — the limits the code actually enforces

> **In:** the IVF-PQ parameters a user picks (m, nbits, d).
> **Out:** the exact rejection rules — several of which are shared-memory
> arithmetic in disguise.

The ADC table is per query and lives in shared memory, so its size is a hard
constraint checked at index construction:

```cpp
// faiss/gpu/GpuIndexIVFPQ.cu:594-608 — the ADC lookup table has to fit in
// shared memory, and the comment states the consequence.
   594          // We must have enough shared memory on the current device to store
   595          // our lookup distances
   596          int lookupTableSize = sizeof(float);
   597          if (ivfpqConfig_.useFloat16LookupTables) {
   598              lookupTableSize = sizeof(half);
   599          }
   601          // 64 bytes per code is only supported with usage of float16, at 2^8
   602          // codes per subquantizer
   603          size_t requiredSmemSize =
   604                  lookupTableSize * subQuantizers_ * utils::pow2(bitsPerCode_);
   605          size_t smemPerBlock = getMaxSharedMemPerBlock(config_.device);
   607          FAISS_THROW_IF_NOT_FMT(
   608                  requiredSmemSize <= getMaxSharedMemPerBlock(config_.device),
```

Evaluate the formula and the comment explains itself:

```
  requiredSmemSize = lookupTableSize x m x 2^nbits

  nbits = 8 (2^8 = 256 entries per sub-quantizer)
      m =  8, float32 tables:  4 x  8 x 256  =   8,192 B     fine
      m = 32, float32 tables:  4 x 32 x 256  =  32,768 B     large
      m = 64, float32 tables:  4 x 64 x 256  =  65,536 B     rejected on most devices
      m = 64, float16 tables:  2 x 64 x 256  =  32,768 B     the comment's case
```

The rest of `verifyPQSettings_` is a list of restrictions worth reading before
you believe any tutorial:

- with cuVS enabled: `4 ≤ nbits ≤ 8` **and** `nbits × m` a multiple of 8
  (`:556-565`);
- classic path with `interleavedLayout`: nbits ∈ {4, 5, 6, 8} (`:568-572`);
  without it, nbits **must be 8** (`:574-577`);
- `d % m == 0` (`:587-592`);
- without `interleavedLayout`, m must be in an explicit list —
  {1, 2, 3, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 96}, where 56, 64 and
  96 are *"only supported with float16"* (`faiss/gpu/impl/IVFPQ.cu:80-102`).

Finally, multi-GPU. The menu is two booleans in a struct, and the distinction is
topic 15's read-scaling question wearing different clothes:

```cpp
// faiss/gpu/GpuClonerOptions.h:56-67 — shard versus replicate, and the
// IVF-specific middle option.
    56  struct GpuMultipleClonerOptions : public GpuClonerOptions {
    57      /// Whether to shard the index across GPUs, versus replication
    58      /// across GPUs
    59      bool shard = false;
    62      int shard_type = 1;
    64      /// set to true if an IndexIVF is to be dispatched to multiple GPUs with a
    65      /// single common IVF quantizer, ie. only the inverted lists are sharded on
    66      /// the sub-indexes (uses an IndexShardsIVF)
    67      bool common_ivf_quantizer = false;
```

Replicate: every GPU holds the whole index, queries are divided, results are
independent — the paper measures *"3.16× for 4 GPUs with 4096 centroids"* on
k-means (§6.2), i.e. near-linear read scaling. Shard: each GPU holds part of the
index, every query goes to every GPU, and the partial top-k lists must be merged
— which is what DEEP1B needed, *"4 GPUs with S = 2, R = 2"* because 20 GB does
not fit one 2017 device (§6.4).

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| `faiss/gpu/utils/Select.cuh:439-447` | `WarpSelect` state: `kNumWarpQRegisters`, `kLane` | 4-5 |
| `faiss/gpu/utils/Select.cuh:469-482` | `addThreadQ` — the branch-predicated fast path | 4 |
| `faiss/gpu/utils/Select.cuh:484-512` | `checkThreadQ` — ballot, merge, republish the threshold by shuffle | 4 |
| `faiss/gpu/utils/Select.cuh:517-532` | `mergeWarpQ` — the odd-size sort/merge networks | 3-4 |
| `faiss/gpu/utils/Select.cuh:147-190` | `BlockSelect`: same algorithm, queues in shared memory | 5 |
| `faiss/gpu/utils/DeviceDefs.cuh:61-68` | `GPU_MAX_SELECTION_K` — 2048 above CUDA 9.0, else 1024 | 4-5 |
| `faiss/gpu/utils/warpselect/WarpSelectFloat*.cu:13` | one instantiation per k, last macro argument is *t* | 4 |
| `faiss/gpu/impl/L2Select.cu:137-186` | `l2SelectMinK` — the fused add-and-select kernel | 6 |
| `faiss/gpu/impl/L2Select.cu:24-70` | `l2SelectMin1` — the k = 1 special case (a block reduction, not WarpSelect) | 6 |
| `faiss/gpu/GpuIndexIVFPQ.cu:544-619` | `verifyPQSettings_` — every restriction, in one function | 7 |
| `faiss/gpu/impl/IVFPQ.cu:80-102` | `isSupportedPQCodeLength` — the legal m values | 7 |
| `faiss/gpu/GpuClonerOptions.h:56-67` | shard vs replicate vs common-quantizer sharding | 7 |

Reading order: `Select.cuh` from line 426 (WarpSelect) — it is the paper's §4
and it is short; then scroll *up* to `BlockSelect` at line 147 to see the same
algorithm with a different storage class. Then `L2Select.cu` for the fusion, and
`GpuIndexIVFPQ.cu:544` last, which reads like a changelog of everything the
kernels cannot do. In the paper: §4 is the contribution (§4.2 the algorithm,
§4.3 the choice of *t*), §5 is the system, §6.1/§6.3/§6.4 the measurements.

## Questions for notes.md

1. The paper caps WarpSelect at k ≤ 1024 (§4.2) and the shipped code at 2048
   (`DeviceDefs.cuh:61-68`). Redo Step 5's register count for the k you would
   actually use, find *t* for it in `faiss/gpu/utils/warpselect/`, and say what
   the thread queue is *for* — what does raising *t* buy and cost (§4.3's N₂C₂
   against N₃C₃)?
2. Fused versus staged: the paper says 2 passes over D′ instead of 3 or more
   (§5.1) and measures ≥ 25 % (§6.3). For nq = 1, ℓ = 10⁶ float distances,
   compute both traffic figures in bytes and the ratio. Why is the measured
   penalty smaller than the traffic ratio suggests?
3. Redo Step 6's two intensity calculations with your own nq, d, |C₁| and m.
   At what nq does the coarse GEMM stop being compute-bound? (Hint: the nq × |C₁|
   output term is the one that grows.)
4. Shard sends every query to every GPU and merges partial top-k lists;
   replicate divides the queries and merges nothing. Map both onto topic 15's
   read-scaling vocabulary, and say which one recall@k is indifferent to and why
   (§6.2's 3.16×/4 GPUs is the replicate data point).
5. For M18: `l2_batch` (1 query × 100 K targets, dim 128) is the paper's exact
   search at batch 1. Predict from Step 6's intensity arithmetic whether Metal
   wins end-to-end *before* implementing it, write the prediction in the table at
   `notes.md:34-41`, then measure. This topic's baseline says no crossover to
   2²⁴ (`FINDINGS.md:36`) — does your prediction agree, and if it does, what
   would have to change for it not to?

## Done when

Answer each before unfolding it.

- [ ] You can state the residency rule and put a number on what breaking it costs.

  <details><summary>Answer</summary>

  PQ codes and centroids on the device, full-precision vectors on the host,
  queries the only per-query traffic. At m = 8 a billion-vector index is 8 GB
  and a 10 K × 128-dim float query batch is 5.12 MB — 1562:1. On this repo's
  measured upload rate (9.09 GB/s, `notes.md:16`) that is 0.88 s once versus
  0.56 ms per batch; re-uploading per batch multiplies query traffic by 1562.

  </details>

- [ ] You can explain why heaps fail on warps, and name the network family Faiss uses instead — precisely.

  <details><summary>Answer</summary>

  Heap inserts branch on data, so 32 lanes want 32 instruction sequences and the
  warp serialises the divergent paths. Sorting networks fix the schedule at
  compile time; "maybe swap" becomes branch-free min/max.

  Faiss uses **odd-size** networks (`merge-odd`, `sort-odd`, Algorithms 1-2), not
  Batcher's odd-even merge — because Batcher requires `32t = k` with k a power of
  two, forcing t = 32 at k = 1024, and the measured optimum for t is far smaller
  (§4.3).

  </details>

- [ ] You can describe WarpSelect's two queues and say where each lives.

  <details><summary>Answer</summary>

  Per-lane **thread queue** of t elements in that lane's registers, a
  first-level filter: reject anything worse than the warp's current kth-best
  (`Select.cuh:469-470`). Warp-wide **warp queue** of k elements held as a
  lane-stride register array — element i in lane `i % 32`, `kNumWarpQRegisters =
  k / 32` registers per lane (`:440`).

  When any lane fills up, `__any_sync` detects it (`:490`), the queues are sorted
  and merged, and the new threshold is broadcast with a shuffle
  (`:511`). No shared memory, no `__syncthreads`, one pass.

  </details>

- [ ] You can say what sets the k ceiling, with the register arithmetic.

  <details><summary>Answer</summary>

  Registers. At k = 2048, t = 8: 2048/32 = 64 key + 64 value registers for the
  warp queue plus 8 + 8 for the thread queue = **144 per lane** before the
  kernel's own working set. The paper stopped at 1024 (§4.2);
  `DeviceDefs.cuh:61-68` raises it to 2048 only when compiled above CUDA 9.0,
  attributing the change to a better register allocator.

  `BlockSelect` avoids the register bill by putting the queues in shared memory
  (`Select.cuh:179-180`) — and then pays warps × k × 8 bytes of it.

  </details>

- [ ] You can explain what "fused" removes, and quote the measured cost of not fusing.

  <details><summary>Answer</summary>

  Adding ‖y‖² and taking the top-k in the same kernel means the partial distance
  matrix D′ is written once by the GEMM and read once by the selector — *"only 2
  passes… compared to other implementations that may require 3 or more"*
  (§5.1). `L2Select.cu:167-170` shows the value computed and consumed in a
  register.

  Unfused is *"at least 25 % slower"* on SIFT1M on one Titan X, and the fused
  pipeline reaches 85 % of peak possible (§6.3).

  </details>

- [ ] You can name at least three IVF-PQ GPU restrictions from the source, not the wiki.

  <details><summary>Answer</summary>

  From `GpuIndexIVFPQ.cu:544-619`: nbits must be exactly 8 without
  `interleavedLayout` (`:574-577`) and is limited to {4,5,6,8} with it
  (`:568-572`); `d % m == 0` (`:587-592`); the ADC table
  `lookupTableSize × m × 2^nbits` must fit in shared memory (`:603-608`), which
  is why m = 64 at nbits = 8 needs float16 tables — 65,536 B versus 32,768 B.
  Plus `isSupportedPQCodeLength`'s explicit m list (`IVFPQ.cu:80-102`), and the
  cuVS path's own rule that `nbits × m` be a multiple of 8 (`:556-565`).

  </details>

- [ ] You wrote answers to all five questions in `notes.md`, including the prediction in question 5 *before* measuring.

  <details><summary>Answer</summary>

  The question slots are `notes.md:95-101`; the `l2_batch` prediction rows are at
  `notes.md:34-41` and are meant to be filled in before the stub is implemented.
  A prediction written afterwards teaches nothing.

  </details>

## References

**Papers**

- Jeff Johnson, Matthijs Douze, Hervé Jégou — *"Billion-scale similarity search
  with GPUs"*, [arXiv:1702.08734](https://arxiv.org/abs/1702.08734); IEEE
  Transactions on Big Data, 2019. §4.1 the odd-size networks, §4.2 WarpSelect,
  §4.3 the choice of *t*, §5.1 exact search and fusion, §5.2 the PQ lookup
  tables, §6 the experiments — all on 2×2.8 GHz Xeon E5-2680v2 with 4 Maxwell
  Titan X GPUs on CUDA 8.0 (§6). Read the numbers as ratios on 2017 hardware:
  55 % of peak at k = 100 but 16 % at k = 1000 (§6.1); 1.62× and 2.01× over fgknn
  at ℓ = 128000 (§6.1); 8.5× over Wieschollek et al. on SIFT1B at equal memory
  (§6.4).

**Code**

- [faiss](https://github.com/facebookresearch/faiss) @ **v1.15.0** — not in this
  repo's pin table; the tag is the pin. CUDA only, so this guide reads it rather
  than running it. Route: `faiss/gpu/utils/Select.cuh` →
  `faiss/gpu/impl/L2Select.cu` → `faiss/gpu/GpuIndexIVFPQ.cu`.

**Measurements in this repo**

- `topics/18-gpu/notes.md:9-16` — the 9.09 GB/s upload and 29.7 GB/s CPU rates
  Step 2's residency arithmetic uses. A sum kernel on Apple unified memory, not
  a search, and not PCIe.
- `FINDINGS.md:36` — no crossover to 2²⁴; 7197 µs upload against a 2723 µs CPU
  total at 16 M.

**Related guides**

- `reading-crystal-sigmod20.md` — the residency argument, quantified, with the
  bandwidth numbers Step 2 leans on.
- `reading-cagra.md` — the other GPU k-select design: bitonic, in shared memory,
  inside the search loop rather than after it.
- `reading-libcudf.md` — the same two-pass-versus-one-pass argument in a
  relational engine.
