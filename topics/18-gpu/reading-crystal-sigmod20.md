# GPU vs CPU for analytics: two regimes, two verdicts

Shanbhag, Madden and Yu's Crystal paper ended a decade of "GPU databases: hype?"
papers by building the fairest comparison anyone had: a tile-based GPU query
library against a CPU baseline good enough to beat Hyper, on the Star Schema
Benchmark, with the transfer question made explicit instead of buried in a
footnote. Its two-regime framing is the go/no-go lens for every operator M18
might offload.

It is also the paper this topic's own measurement most directly contradicts —
or rather, agrees with, once you read the caveat. Every headline number in
sections 4 and 5 is measured with the data **already resident in the device's
memory**. This topic measures what happens when it is not: 7197 µs of upload
against a 2723 µs CPU total (`FINDINGS.md:36`). Both are true. Keeping them
straight is the entire skill this guide is teaching.

Citations below are to
[arXiv:2003.01178](https://arxiv.org/abs/2003.01178), the extended version of
the SIGMOD 2020 paper; section, figure and table numbers are the paper's own.
None of the CUDA in this chapter can be run here — this machine has no NVIDIA
device — so every number is read, not reproduced, and is labelled with where it
was read from.

## The problem in one sentence

On the paper's own hardware a V100 reads its own memory at **880 GBps** while
the CPU reads at **53 GBps** (Table 2) — a ratio of **16.2** (§4) — but the
measured PCIe link between them moved only **12.8 GBps** (§5), so the same query
is either a 25× win or a 1.4× loss against a good CPU engine depending on one
question: which side of the bus does the data already live on?

## The concepts, step by step

### Step 1 — SIMT: the GPU hides latency with thread count, not caches

> **In:** a memory access that misses cache.
> **Out:** on a CPU, a stall; on a GPU, a warp swap — provided enough warps are
> resident. Everything else in the paper follows from that difference.

A GPU runs tens of thousands of threads at once, grouped into **warps** (32
threads that issue the same instruction in the same cycle — SIMT, "single
instruction, multiple threads"). Warps live on **SMs** (streaming
multiprocessors); a **thread block**, or CTA, is a group of threads scheduled
onto one SM together, able to share scratch memory and to synchronise with a
barrier.

The paper's own description of what that buys, from §5.3 (its explanation of a
model that under-predicted CPU time by 2.7×):

> "On the GPU, a single streaming multiprocessor (SM) usually has 64 cores that
> can execute 2 warps (64 threads) at any point. However, the SM can keep > 2
> warps active at a time. On Nvidia V100, each SM can hold 64 warps in total
> with 2 executing at any point in time. Any time a warp makes a memory request,
> the warp is swapped out from execution into the active pool and another warp
> that is ready to execute ends up executing."

The measurement that forced that paragraph is worth carrying around. For SSB
q2.1 the authors' bandwidth model predicted **47 ms on the CPU and 3.7 ms on the
GPU**; the actual runtimes were **125 ms and 3.86 ms** (§5.3). The GPU landed
within 4 % of a model that assumes no stalls at all. The CPU missed by 166 %,
because a join probe is an irregular access and prefetchers do not help with
those. Latency hiding by oversubscription is not a small effect; it is the
difference between a model that works and one that does not.

**Occupancy** is the fraction of the maximum resident warps you actually
achieve, and it is bounded by how many registers and how much shared memory each
thread uses — which is why every later step counts bytes per thread. Low
occupancy means too few warps to swap to, and the GPU stalls exactly like a CPU.
The other SIMT rule is **branch divergence**: when lanes of a warp disagree on an
`if`, the warp executes both sides with lanes masked. That is topic 17's
predication, done by hardware whether you asked for it or not — and §4.2's
measurement of it is in Step 4, where it is not what you would guess.

### Step 2 — coalescing and shared memory: the two rules that decide layout

> **In:** 32 lanes issuing 32 loads in one instruction.
> **Out:** one memory transaction if the addresses are adjacent, up to 32 if
> they are not — before any code of yours has run.

**Coalescing**: adjacent lanes touching adjacent addresses collapse into one
transaction. Lane *i* reading `col[i]` (a dense column) fetches one contiguous
block; lane *i* reading `rows[i].field` issues up to 32 transactions and discards
most of each. This is topic 12's columnar argument with a 32× multiplier
attached, and it is why Crystal's write path (Step 6) goes to such trouble to
make the *output* contiguous too.

**Shared memory**: a software-managed scratchpad per SM, shared by a thread
block. The paper gives its size the way that matters for algorithm design rather
than in bytes — per thread, at full occupancy:

> "On the Nvidia V100, each GPU thread can only store roughly 24 4-byte entries
> in shared memory at full occupancy, with 5000 threads running in parallel."
> (§3.2)

Table 2 gives the other side of the same coin: 16 KB of L1 per SM, 6 MB of L2 in
total, and no L3 at all, against the CPU's 32 KB L1 and 256 KB L2 per core plus
a 20 MB shared L3. A GPU has less cache per thread by orders of magnitude; what
it has instead is Step 1. The consequence the paper draws — that a *thread* is
too small a unit to plan around but a *thread block* is not — is Step 5.

### Step 3 — the bus decides: why the coprocessor model fails, in arithmetic

> **In:** SSB Q1.1 over a lineorder table of L rows, four 4-byte columns.
> **Out:** two bounds, one on each processor, whose order does not depend on how
> good your kernel is.

§3.1 states the model in two lines. Let *B_c* be CPU memory bandwidth and *B_p*
PCIe bandwidth. A CPU can answer Q1.1 in one pass over four 4-byte columns, so
its optimal runtime *R_C* is **upper** bounded by `16L / B_c`. In the coprocessor
model those same four columns must cross the bus, so the GPU runtime *R_G* is
**lower** bounded by `16L / B_p`, a bound reached only with perfect
transfer/compute overlap. Since `B_c > B_p` on every real machine, `R_C < R_G`
— the direction of the comparison is fixed before either implementation exists.

Put SSB scale factor 20 through it, which is what §5.1 says the paper ran:
lineorder has **120 million tuples**.

```
  L                = 120e6 rows
  bytes per row    = 4 columns x 4 B = 16 B
  16L              = 1.92e9 B  (1.92 GB)

  CPU upper bound  = 1.92e9 / 53e9  =  36.2 ms     (B_c, Table 2 read BW)
  GPU lower bound  = 1.92e9 / 12.8e9 = 150.0 ms    (B_p, measured, §5)
                                       -------
  the best case for the GPU is 4.1x the worst case for the CPU
```

And that is what the measurement showed (§3.1, Figure 3): the GPU coprocessor
was **1.5× faster than MonetDB but 1.4× slower than Hyper**, and *"for all
queries, the query runtime in GPU coprocessor is bound by the PCIe transfer
time."* The paper's diagnosis of the prior literature — reported coprocessor
speedups from 2× to 100× — is that those papers were beating MonetDB, not
beating a good CPU engine.

Two numbers people quote loosely here, with their actual sources: §2.2 says
*"the PCIe bandwidth of a modern machine is up to 16 GBps"*, an upper figure for
the era; the machine the paper actually measured on delivered **12.8 GBps**
bidirectional (§5). Use 12.8 when you are reproducing their arithmetic, 16 only
when you are quoting their prose.

Now the local translation. This Mac has no PCIe hop at all — CPU and GPU share
one LPDDR pool — and yet:

```
  upload, 2^24 f32 = 67,108,864 B in 7384.7 us  =  9.1 GB/s   (notes.md:16)
  CPU sum, same bytes = 67,108,864 B in 2257.7 us = 29.7 GB/s (notes.md:16)
                                                    --------
  effective transfer path is 3.3x SLOWER than the CPU's own read
```

Structurally identical to `B_c / B_p` = 53 / 12.8 = 4.1, for a completely
different reason: wgpu stages the upload through a private buffer, so "unified"
memory still costs a copy. Crystal's regime A is not a PCIe fact. It is a
boundary-crossing fact, and the boundary is still there.

### Step 4 — the caveat that governs every number in §4 and §5

> **In:** any speedup you are about to quote from this paper.
> **Out:** the sentence you must quote next to it, or the number is misleading.

§4 opens its operator comparisons with the setup line, and then this:

> "For the micro-benchmarks, we use a setup where GPU memory bandwidth is
> 880GBps and CPU memory bandwidth is 54GBps, resulting in a bandwidth ratio of
> 16.2 (see Section 5 for system details). **In all cases, we assume that the
> data is already in the respective device's memory.**"

§5 repeats it for the full-workload numbers: *"In our evaluation, we ensure that
data is already loaded into the respective device's memory before experiments
start."* So every ratio below is a regime-B number. None of them survives
contact with Step 3's bus.

With that said out loud, here is what they measured, operator by operator:

| operator | ratio CPU:GPU | where | note |
|---|---|---|---|
| projection, linear combination (Q1) | **16.56** | §4.1 | ≈ the 16.2 bandwidth ratio |
| projection with UDF (Q2) | **17.95** | §4.1 | |
| selection, averaged over selectivity 0→1 | **15.8** | §4.2 | input 2²⁹ entries |
| hash join probe, HT 32–128 KB | **≈5.5** | §4.3, Fig 13 | HT in L2 on both; CPU is DRAM-bound, GPU L2-bound |
| hash join probe, HT 1–4 MB | **14.5** | §4.3 | GPU L2 vs CPU L3 bandwidth ratio |
| hash join probe, HT > 128 MB | **10.5** | §4.3 | model says 8.1 — see below |
| radix sort, 2²⁸ entries | **17.13** | §4.4 | 464 ms CPU vs 27.08 ms GPU |
| full SSB, SF20, 13 queries | **25** | §5.2 | *exceeds* the bandwidth ratio — Step 7 |

The join row is the one that is usually flattened into a single number and
should not be. It is regime-dependent by an order of magnitude across the table,
and the largest-table case has a hardware reason worth memorising: *"The
granularity of reads from global memory is 128B on GPU while on CPU it is 64B.
Hence, random accesses into the hash table read twice the data on GPU compared
to CPU"* — 16.2 / 2 = 8.1 expected, 10.5 observed, the surplus being Step 1's
latency hiding again.

Two more things §4.2 measured that contradict the folklore. First, on the GPU
there is **no difference between the branching and the predicated selection**
(`GPU If` vs `GPU Pred`) — a single mispredicted branch does not cost a GPU
anything measurable; the branch-versus-predication curve everyone half-remembers
is the *CPU* result in the same figure (`CPU If` vs `CPU Pred` vs
`CPU SIMDPred`). Second, both `CPU SIMDPred` and the GPU variants track their
bandwidth models closely, which is the paper's real claim: a *competently
written* CPU selection also saturates memory bandwidth, so the gap between them
is the hardware ratio and nothing else.

### Step 5 — tiles: the thread block is the unit, not the thread

> **In:** `SELECT y FROM R WHERE y > v`, run on 5000 threads that share no
> cursor.
> **Out:** one kernel instead of three, one pass over the input instead of two,
> and coalesced writes instead of random ones.

Step 2 said a GPU thread can hold ~24 4-byte entries in shared memory at full
occupancy. That is too small to be an execution unit. A thread *block* holding
those 24 entries per thread collectively is not — the paper calls that unit a
**tile**, of size `items per thread × threads per block`, and it is the
GPU-shaped answer to topic 11's ~1000-element vector (Figure 5 draws exactly
that correspondence). §5.2 says their SSB runs used a block size of 256 and a
tile of 2048 (8 items per thread).

What the tile replaces is worth spelling out, because it is what GPU databases
did before (§3.2, Figure 4a):

```
  (a) what existing GPU databases did — three kernels
      K1  each thread reads a strided slice, evaluates the predicate,
          writes count[t]                          ← pass 1 over the input
      K2  prefix sum over count -> pf              ← e.g. a Thrust call
      K3  each thread reads its slice AGAIN and writes matches at
          pf[t] + local_counter                    ← pass 2, random writes

      costs: input read twice, count and pf materialised in global
             memory, every thread writing to a different place

  (b) tile-based, one kernel  (Figure 4b, Figure 6)
      load tile into shared memory                 ← coalesced
      evaluate predicate -> bitmap
      per-thread histogram of matches
      block-wide prefix sum over the histogram     ← offsets within the tile
      ONE atomic add on the global counter, by the block, of the tile total
      block-wide shuffle -> contiguous run in shared memory
      write the run to global memory               ← coalesced
```

Everything after the load reads shared memory, so the input is touched once.
Crystal is a library of these block-wide steps (`BlockLoad`, `BlockPred`,
`BlockScan`, `BlockShuffle`, `BlockStore`…) that compose into one fused kernel
per query. This is topic 11's operator fusion — optional on a CPU, structural
here, because every kernel boundary is a round trip through global memory and
the whole point of being on the GPU was the bandwidth.

### Step 6 — compaction: a prefix scan is the substitute for a cursor

> **In:** a bitmap of survivors, spread across 5000 threads with no ordering
> between them.
> **Out:** a distinct output slot per survivor, computed rather than claimed.

On a CPU, filter output is a cursor: `out[k] = x; k += mask`. §3.2 explains why
that works there and not here — a CPU thread updates the shared counter *once
per ~1000-entry vector* and there are only ~32 threads, so the counter is not the
bottleneck. With 5000 threads each wanting a slot, it is.

The replacement is an **exclusive prefix scan**: each element's output offset is
the count of survivors before it, which the block computes cooperatively in
shared memory. The block then does exactly one atomic add on the global counter
to claim a contiguous range. The paper's own summary: *"By treating the thread
block as an execution unit, we reduce the number of atomic updates of the global
counter by a factor of size of tile T."*

```rust
// ILLUSTRATION — not Crystal source (Crystal is CUDA C++ and is not pinned in
// this repo). This is Figure 6's kernel written as Rust-ish pseudocode; the
// code you are meant to write from it is the filter_count stub at
// experiments/src/gpu.rs:154, whose doc comment asks for exactly this shape.
par_for tile in input.tiles(ITEMS_PER_THREAD * THREADS_PER_BLOCK) {
    let items  = block_load(tile);                   // coalesced, into shared
    let flags  = items.map(|x| pred(x) as u32);      // bitmap
    let hist   = per_thread_count(flags);            // matches per thread
    let (off, total) = block_exclusive_scan(hist);   // offsets within the tile
    let base   = atomic_add(&global_cursor, total);  // ONCE per block
    let packed = block_shuffle(items, flags, off);   // contiguous in shared
    block_store(&mut out[base..], packed);           // coalesced write
}
```

The same rule, three ways, in three files you can actually read: one `atomicAdd`
per workgroup in the WGSL exercise (`experiments/src/gpu.rs:150-153`); one
device-scope `fetch_add` per *warp* in libcudf's conditional join
(`cpp/src/join/conditional_join_kernels.cuh:74-77`); and Crystal's one per
*block*. Nobody who has measured it does one atomic per element.

### Step 7 — the roofline, and why the full query beat the ratio

> **In:** an operator's bytes moved and FLOPs performed.
> **Out:** a predicted winner — plus the one effect that makes the prediction
> too conservative.

For a scan-shaped operator, time is bounded by whichever resource saturates
first:

```
  time = max( bytes / memory_bandwidth , flops / peak_flops )
```

`flops / byte` is the operator's **arithmetic intensity**. §4's models are this
formula with the constants filled in: selection, for instance, is modelled as
`4N/B_r + 4σN/B_w` (§4.2), read every entry, write the σ fraction that survives.
When an implementation tracks that model, it is bandwidth-saturated, and the
CPU:GPU ratio can only be the bandwidth ratio — which is exactly what §4.1-4.4
found, everywhere except the join, where the 128 B vs 64 B access granularity
halves it.

So an operator-level roofline predicts *at most* 16.2× in regime B and a loss in
regime A. Then §5.2 measured **25×** on the full 13-query benchmark. The paper's
explanation is that the ceiling applies to operators in isolation, not to
chains: on the CPU, vectorising a chain of operators leaves gaps the model does
not capture (the q2.1 miss in Step 1 — 47 ms modelled, 125 ms actual), while the
GPU's latency hiding keeps it near its own model even through irregular join
probes. The lesson for M18's go/no-go is uncomfortable and worth stating
plainly: the roofline is a reliable *lower* bound on the GPU's advantage in
regime B and gives no protection at all in regime A, where Step 3's bus decides
everything before the first FLOP.

## How to read the paper (with the concepts in hand)

Read it in this order, not front to back:

- **§3.1 first** (two pages). The coprocessor bound, the SSB SF20 measurement,
  Figure 3. If you read nothing else, read this — it is Step 3, and it is the
  section that makes this topic's own no-crossover result unsurprising.
- **§3.2 and Figure 4/5/6.** The three-kernel selection and its tile-based
  replacement — Steps 5 and 6 in the authors' words. Map each block-wide
  primitive onto its topic 11 / topic 17 CPU ancestor as you go.
- **§4, starting with the last sentence of its opening paragraph.** That is the
  residency assumption (Step 4). Then the operator subsections; in §4.3 read the
  three hash-table size regimes rather than taking an average.
- **§5.1-5.2** for the platform table and the 25×, then **§5.3** for why the
  25× exceeds 16.2 (Step 1's latency hiding, quantified).
- **The CPU-baseline discussion in §5.2**, even if you never touch a GPU: their
  CPU implementation beats Hyper by 1.17× and MonetDB by 2.5×, which is what
  earns them the right to publish a 25×. Topic 0's fair-benchmarking rules,
  applied by someone with something to lose. Any speedup you publish for M18
  gets held to the same standard.

## Questions for notes.md

1. SSB is denormalised-star scans. Which topic 22 benchmark shape would flip the
   verdict back to the CPU even in regime B (hint: point lookups, topic 3 — what
   is the arithmetic intensity of a single B-tree descent, and how many warps
   does it keep busy)?
2. Crystal predates Apple unified memory. Rewrite Step 3's regime table for
   M-series: what replaces PCIe, what replaces HBM, and why does the GPU still
   lose our sum bench? Use the two measured numbers, 9.1 GB/s and 29.7 GB/s.
3. Their group-by uses atomics into a hash table when groups are few. At what
   group cardinality does that collapse, and what is the fallback? Compare with
   libcudf's actual answer — a shared-memory set of 366 slots per block and a
   whole-input re-run in global memory past a cardinality of 128
   (`reading-libcudf.md`, Step 5).
4. Fusing a whole query into one kernel kills operator-at-a-time profiling. What
   replaces topic 0's flamegraph on a GPU? (Name the two counters that matter:
   achieved occupancy and achieved bandwidth per kernel.)
5. For M18 our hot paths are graph expand (random access), filter (streaming),
   distance scoring (dense). Apply Step 7's roofline to each and write the
   one-line go/no-go — then check it against the fact that on this machine the
   bandwidth ratio is ~1.

## Done when

Answer each before unfolding it.

- [ ] You can state the two regimes, name the number that separates them, and connect them to this topic's measured result.

  <details><summary>Answer</summary>

  Regime A (coprocessor): data ships per query, so GPU time ≥ `16L/B_p`. With
  the paper's measured PCIe of 12.8 GBps against a CPU read bandwidth of
  53 GBps (Table 2), the CPU's *upper* bound beats the GPU's *lower* bound —
  36.2 ms vs 150 ms on SSB SF20's 120 M rows. Measured: GPU coprocessor 1.4×
  slower than Hyper, PCIe-bound on every query (§3.1, Fig 3).

  Regime B (primary store): data resident, ratio 880/53 = 16.2 (§4), and 25× on
  the full benchmark (§5.2).

  This topic measured regime A on hardware with no PCIe at all: 7197 µs of
  upload against a 2723 µs CPU total (`FINDINGS.md:36`), no crossover to 2²⁴.
  The boundary crossing survives the removal of the bus.

  </details>

- [ ] You can quote the caveat that governs every speedup in §4 and §5, and say which of this topic's numbers it invalidates.

  <details><summary>Answer</summary>

  §4: *"In all cases, we assume that the data is already in the respective
  device's memory."* §5 repeats it for the workload runs. So 16.56×, 15.8×,
  17.13× and 25× are all regime-B numbers.

  None of them is comparable to `notes.md`'s table, which measures a per-call
  upload. The comparable Crystal number is §3.1's coprocessor result, and that
  one agrees with ours.

  </details>

- [ ] You can explain the two memory rules and what violating each costs.

  <details><summary>Answer</summary>

  Coalescing: 32 lanes on adjacent addresses = 1 transaction; scattered = up to
  32, most of each discarded. Shared memory: a per-SM scratchpad shared by a
  block, but tiny per thread — ~24 4-byte entries at full occupancy on a V100
  (§3.2), against 16 KB of L1 per SM (Table 2). Using more of it per thread
  costs occupancy, and occupancy is the only latency-hiding mechanism the GPU
  has (Step 1).

  The join's largest-table regime is the concrete price of violating the first:
  128 B GPU read granularity vs 64 B on the CPU halves the expected 16.2× to
  8.1× for random hash-table probes (§4.3).

  </details>

- [ ] You can explain why filter output needs a prefix scan rather than a cursor, and what the tile-based version saves over the three-kernel version.

  <details><summary>Answer</summary>

  With ~32 CPU threads updating a counter once per 1000-entry vector, a shared
  cursor is not a bottleneck; with 5000 GPU threads it is (§3.2). A block-wide
  exclusive prefix sum computes each element's offset instead of claiming it,
  and one atomic per block claims the range — atomics reduced *"by a factor of
  size of tile T"*.

  Against the older three-kernel approach (Fig 4a) the fused version saves: the
  second pass over the input column, the materialisation of `count` and `pf` in
  global memory, and the random output writes, which the shuffle step turns
  contiguous (Fig 4b, Fig 6).

  </details>

- [ ] You can say what §4.2 actually found about branch divergence in selection — and what it did not.

  <details><summary>Answer</summary>

  It found **no difference** between `GPU If` and `GPU Pred`: *"A single branch
  misprediction does not impact performance on the GPU."* The predication story
  in that figure belongs to the CPU curves (`CPU If` < `CPU Pred` <
  `CPU SIMDPred`).

  What it did *not* measure is scan-and-compact versus branch-per-thread on the
  GPU; the tile-based design is argued from first principles in §3.2, not
  benchmarked against the three-kernel alternative. If you want that number you
  have to produce it yourself — which is what this topic's `filter_count` stub
  is for.

  </details>

- [ ] You can use the roofline to predict a winner, and say why the full-query result beat what it predicts.

  <details><summary>Answer</summary>

  `time = max(bytes / bandwidth, flops / peak_flops)`; the operator's arithmetic
  intensity picks the term. Bandwidth-bound operators can win by at most the
  bandwidth ratio (16.2), and §4.1-4.4 land at 15.8-17.95 for everything except
  the join.

  The full SSB gave 25× (§5.2) because the ceiling is per operator, not per
  chained query: the CPU loses time between vectorised operators that the model
  does not predict (q2.1: 47 ms modelled, 125 ms actual) while the GPU stays
  near its model even through irregular probes, thanks to 64 resident warps per
  SM (§5.3).

  </details>

- [ ] You wrote answers to all five questions in `notes.md`, including the unified-memory rewrite of the regime analysis.

  <details><summary>Answer</summary>

  The slots are `notes.md:55-61`. Question 2's rewrite needs the two measured
  numbers from `notes.md:16` — 9.1 GB/s effective upload, 29.7 GB/s CPU read —
  not the paper's.

  </details>

## References

**Papers**

- Anil Shanbhag, Samuel Madden, Xiangyao Yu — *"A Study of the Fundamental
  Performance Characteristics of GPUs and CPUs for Database Analytics"*, SIGMOD
  2020. Extended version: [arXiv:2003.01178](https://arxiv.org/abs/2003.01178),
  which is what every citation here points at.
  Route: §3.1 (the coprocessor bound and Figure 3) → §3.2 with Figures 4, 5, 6
  (the tile model) → §4 opening paragraph (the residency caveat) → §4.1-4.4
  (operator ratios; read §4.3's three regimes separately) → §5.1-5.2 (platform
  Table 2, the 25×) → §5.3 (why 25 > 16.2).

**Measurements in this repo**

- `FINDINGS.md:36` — this topic's headline: no crossover to 2²⁴; 7197 µs upload
  against a 2723 µs CPU total at 16 M.
- `topics/18-gpu/notes.md:9-16` — the phase-split table Step 3's local
  arithmetic uses.

**Related guides**

- `reading-wgpu-compute.md` — the same boundary-crossing cost, on hardware you
  can actually run.
- `reading-libcudf.md` — Steps 5 and 6 as shipped production CUDA, with the
  atomic-amortisation rule at warp granularity instead of block.
