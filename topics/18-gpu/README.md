# Topic 18 — GPU Acceleration for Databases

When is the transfer tax worth paying? Topic 17 bought lanes; a GPU
buys thousands of them — behind a bus. On this machine (Apple
Silicon) the "bus" is unified memory, which changes the answer in
ways the experiments measure directly.

## 1. GPU architecture for DB people

```
 CPU core:  wide OoO, ~5 GHz, caches hide latency PER THREAD
 GPU SM:    32-lane warps (SIMT), latency hidden by OVERSUBSCRIPTION
            — thousands of resident threads; when a warp stalls on
            memory, another issues. Occupancy = how many can be
            resident (limited by registers + shared memory per SM).

 branch divergence: both sides of an if execute, lanes masked
   → SIMT is topic 17's predication done by hardware, per warp
 memory coalescing: a warp's 32 loads become ONE transaction iff
   adjacent lanes touch adjacent addresses
   → the GPU word for topic 12's columnar layout argument
 shared memory: ~100 KB/SM software-managed scratchpad
   → the GPU word for topic 13's cache blocking
```

Every GPU-DB trick is one of: coalesce (layout), stay resident
(occupancy), amortize atomics (reduce first), or avoid the bus.

## 2. The bus decides the architecture

```
 discrete GPU:  device HBM  400-3000 GB/s   ← kernels feast
                PCIe 4/5     32-64  GB/s    ← queries starve
                NVLink       ~900   GB/s    ← the expensive fix
 Apple Silicon: unified LPDDR, one pool, ~150-400 GB/s shared
                no copy needed IN PRINCIPLE — but wgpu still
                stages through private buffers (our measured
                "upload" cost is real)
```

Crystal (SIGMOD '20)'s rule of thumb: a discrete GPU beats the CPU
on a scan-heavy query only if the working set LIVES on the device
(the "data resident" assumption); shipping data per query loses to
the CPU at PCIe speeds. Corollary the paper works out: GPU as
accelerator-of-operators fails; GPU as primary-store-with-CPU-fallback
works. Our gpu_bench reproduces the miniature version.

## 3. Measured on this machine (Apple M3 Pro, wgpu/Metal)

```
 sum of n f32 — CPU 8-acc autovec vs GPU workgroup reduction:
 n=16K    CPU     2 µs   GPU 1619 µs    ← ~1.5 ms FIXED dispatch cost
 n=4M     CPU   589 µs   GPU 4555 µs
 n=16M    CPU  2258 µs   GPU 14333 µs   ← no crossover, ever
```

A memory-bound reduction never wins on the GPU here: both processors
see the SAME memory, so the GPU's only edge is FLOPs it doesn't need,
and it pays ~1.5 ms of encode/submit/poll overhead per dispatch.
The lesson is NOT "GPU slow" — it's that arithmetic intensity
(FLOPs/byte) decides: sum is 0.25 FLOP/byte; l2_batch at dim=128 is
~64 FLOP/byte-of-query. The stubs exist to find where the flip
happens.

## 4. GPU joins & aggregation (libcudf)

- Hash join: build with cuco (RAPIDS' cuckoo/open-addressing GPU
  hash tables), probe with COOPERATIVE GROUPS — a warp probes
  together (cudf join/hash_join/, distinct_hash_join.cu) — then a
  two-phase size/retrieve pattern (inner_join_size.cu before
  inner_join_retrieve.cu): count matches first, allocate exactly,
  fill second. GPU code can't `Vec::push` — every output needs its
  size known or an atomic cursor.
- Group-by: shared-memory aggregation per block when cardinality
  fits (groupby/hash/compute_shared_memory_aggs.cu), spilling to
  global-memory atomics when it doesn't — topic 11's two-phase
  partial aggregation, forced by the memory hierarchy.

## 5. GPU graph processing (Gunrock) & ANN (CAGRA)

- Gunrock: BFS = frontier ADVANCE (expand neighbors) + FILTER
  (dedupe/validate) operators; the whole research area is load
  balancing ragged adjacency lists across warps (thread_mapped /
  block_mapped / merge_path in operators/advance/).
- CAGRA (cuVS): HNSW rebuilt for SIMT — a flat degree-regular graph
  (no levels), searched by MANY parallel greedy walks with a shared
  visited hashmap in shared memory; one CTA per query
  (search_single_cta_kernel.cuh). Graph traversal made regular
  enough for warps.

## 6. Programming models

| model | reach | why it matters here |
|---|---|---|
| CUDA | NVIDIA only | where all the DB literature lives |
| Metal | Apple only | what actually runs on this Mac |
| wgpu/WebGPU | everywhere | our experiments: WGSL → Metal/Vulkan/DX12 |
| Mojo/MLIR | CPU SIMD + GPU | topic 17's parametric width, extended to the device axis |

## Experiments (`experiments/`)

wgpu compute on Metal. PROVIDED: `GpuCtx` + working sum-reduction
kernel (`shaders/sum.wgsl` — coalesced strided loads, shared-memory
tree reduction) with per-phase timings, `gpu_bench` crossover sweep
(runs now, prints the table in §3). YOURS:

1. `filter_count` — WGSL skeleton provided: fold per-thread, reduce
   per-workgroup, ONE `atomicAdd` per group. Test: exact vs CPU.
2. `l2_batch` — one invocation per target; row-major first, then
   transpose and measure the coalescing gap. Test: 1e-3 relative.
3. Fill the crossover table in notes.md — l2_batch at dim 128 ×
   100K targets is where the GPU should finally win. Verify.
4. Stretch (M18/M20 bridge): BFS as SpMV over a bitmap frontier in
   WGSL vs SuiteSparse CPU.

## Reading guides

| guide | what it walks |
|---|---|
| [reading-crystal-sigmod20.md](reading-crystal-sigmod20.md) | the tile-based GPU query model + when PCIe kills it |
| [reading-wgpu-compute.md](reading-wgpu-compute.md) | wgpu examples: hello_compute, repeated_compute, workgroups |
| [reading-libcudf.md](reading-libcudf.md) | cuco hash join size/retrieve, shared-mem groupby |
| [reading-gunrock.md](reading-gunrock.md) | advance/filter operators, load-balancing menu, BFS enactor |
| [reading-cagra.md](reading-cagra.md) | ICDE '24 + single-CTA search kernel, hashmap visited set |
| [reading-faiss-gpu.md](reading-faiss-gpu.md) | billion-scale IVF on GPU: k-select in registers, memory tiers |

## Capstone M18

- [ ] experimental GPU backend for ONE hot path (vector distance
      scoring is the honest candidate — M14's rescore loop) behind
      a feature flag
- [ ] CPU-vs-GPU crossover bench INCLUDING transfer, per batch size
      — the go/no-go artifact
- [ ] document the verdict: on unified memory, which engine ops
      clear the arithmetic-intensity bar? (expect: almost none —
      record WHY, that's the deliverable)
