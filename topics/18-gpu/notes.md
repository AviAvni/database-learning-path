# Topic 18 notes — GPU acceleration

## Baseline (provided sum kernel, wgpu/Metal, Apple M3 Pro, measured 2026-07-10)

CPU = 8-accumulator autovec sum; GPU = workgroup tree reduction
(sum.wgsl), END-TO-END including buffer creation, upload, dispatch,
readback. 5-rep averages.

| n | CPU µs | GPU µs | upload | kernel+submit | readback | winner |
|---|---|---|---|---|---|---|
| 16K | 2.3 | 1618.9 | 48.5 | 1567.1 | 3.2 | CPU |
| 64K | 9.2 | 1633.5 | 69.6 | 1560.8 | 3.1 | CPU |
| 256K | 36.8 | 1701.5 | 151.8 | 1547.1 | 2.6 | CPU |
| 1M | 154.4 | 1985.5 | 437.3 | 1544.2 | 4.0 | CPU |
| 4M | 588.6 | 4554.8 | 1654.8 | 2887.2 | 12.8 | CPU |
| 16M | 2257.7 | 14332.9 | 7384.7 | 6929.1 | 19.1 | CPU |

**No crossover, ever, for a memory-bound reduction on unified
memory.** Two reasons, cleanly separated by the phase columns:

1. ~1.5 ms FIXED encode/submit/poll cost per dispatch (flat from
   16K to 1M — pure overhead, not work).
2. Even amortized (16M: ~7 ms kernel for 64 MB ≈ 9 GB/s effective),
   the GPU reads the SAME memory the CPU reads at ~30 GB/s — there
   is no bandwidth ratio to win (Crystal's regime B advantage
   doesn't exist on unified memory for streaming ops).

Upload cost is real despite "unified" memory: wgpu stages through a
private buffer (~9 GB/s effective at 16M).

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| filter_count GPU vs CPU branchless (~12.7 GB/s) — crossover anywhere? | | |
| l2_batch dim=128 × 100K targets (51 MB, ~26 MFLOP... intensity ~0.5 FLOP/B on targets): GPU wins end-to-end? | | |
| l2_batch with targets PRE-UPLOADED (regime B): GPU µs at 100K targets? | | |
| row-major vs column-major targets in l2_batch — coalescing gap (×?) | | |
| hoisting upload out of sum (regime B): does GPU beat 2258 µs CPU at 16M? | | |
| one atomicAdd per ELEMENT instead of per workgroup in filter_count — slowdown ×? | | |

## Implementation log

- [ ] filter_count.wgsl + pipeline — test green
- [ ] l2_batch.wgsl + pipeline (row-major) — test green
- [ ] l2_batch column-major variant — coalescing gap recorded
- [ ] regime-B variants (pre-uploaded buffers) — crossover table redone
- [ ] stretch: BFS via dense SpMV in WGSL vs CPU
- [ ] prediction table reconciled

Surprises / dead ends:

## Questions from the reading guides

### Crystal SIGMOD '20 (reading-crystal-sigmod20.md)

1. Which topic 22 benchmark shape flips regime B back to CPU:
2. Regime table rewritten for Apple unified memory:
3. Group-by atomics collapse cardinality + fallback:
4. GPU profiling replacement for flamegraphs:
5. Roofline go/no-go for expand/filter/distance:

### wgpu compute (reading-wgpu-compute.md)

1. Regime-B sum at 16M measured vs prediction:
2. Why workgroup_size is a pipeline-time constant:
3. Why upload ≫ readback on unified memory:
4. subgroupAdd rewrite — barriers removed:
5. M18 API: per-call upload vs GpuVec handle:

### libcudf (reading-libcudf.md)

1. Kernel launches per inner_join × 1.5 ms — min batch to amortize:
2. When size/retrieve recompute is free (roofline):
3. Why conditional_join is NL + device AST:
4. cudf JIT ↔ WGSL pipeline specialization (topic 19):
5. filter compact pass-2 via BlockScan sketch:

### Gunrock (reading-gunrock.md)

1. Advance's unknown output size — Gunrock's two-phase:
2. Why lost CAS races are benign in BFS:
3. Direction-optimizing needs CSC — memory doubling worth it when:
4. Hub degree 10⁶: thread_mapped vs merge_path arithmetic:
5. M24: advance strategy per LDBC frontier shape:

### CAGRA (reading-cagra.md)

1. Where the levels' log-factor went at fixed degree 32:
2. Why bitonic topk beats a heap on a warp:
3. multi_cta partial-itopk merge (no device barrier):
4. ADC tables vs visited hashmap — shared memory budget fight:
5. M14+M18: which rescore half goes GPU first + batch size:

### Faiss GPU (reading-faiss-gpu.md)

1. WarpSelect k limit + thread-queue threshold t:
2. Fused vs staged distance→k-select HBM traffic ratio:
3. Coarse matmul near-peak vs PQ scan bandwidth-bound — intensities:
4. Shard vs replicate ↔ topic 15 read-scaling:
5. l2_batch brute-force prediction vs measurement:

## Cross-topic threads

- SIMT = topic 17's predication done by hardware; branch divergence
  = the branchy filter's mispredict wall, warp edition.
- Coalescing = topic 12's columnar argument with a 32× multiplier;
  shared memory = topic 13's cache blocking, made explicit.
- cudf size/retrieve = simdjson's over-write problem with the
  opposite answer (recompute vs over-allocate) — forced by 10⁵
  threads sharing one output.
- CAGRA deletes Gunrock's load-balancing problem by fixing the
  degree — regularity is bought at BUILD time, spent at SEARCH time.
- The 1.5 ms dispatch floor is topic 7's syscall-batching argument:
  amortize the boundary crossing or die by it.

## M18 log (capstone)

- [ ] GPU feature flag: batch vector-distance scoring (M14 rescore)
      behind `--features gpu`, GpuVec-handle API (regime B)
- [ ] crossover bench per batch size committed as the go/no-go doc
- [ ] verdict paragraph: which engine ops clear the
      arithmetic-intensity bar on unified memory (expect ~only
      dense distance batches; traversal and filter stay CPU — SAY
      WHY with the roofline numbers)

## Done when

- Both stub tests green; crossover + coalescing-gap numbers in the
  tables above; prediction table reconciled; reading-guide questions
  answered; M18 verdict written.
