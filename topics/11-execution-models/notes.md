# Topic 11 notes — execution models

## Predictions (fill BEFORE implementing vectorized.rs / kernels.rs)

Measured baseline (provided volcano, release, 50M rows, sel 50%):
**0.277 s = 180.7 M rows/s** — already fast! ~5.5 ns/row including two
virtual calls per tuple. Modern branch predictors eat stable indirect
calls; the Volcano tax on an M-series core is NOT mostly call overhead.
Where will the vectorized win actually come from? (SIMD, ILP, no
per-row branch.) Predict accordingly:

| engine | predicted M rows/s | predicted ratio vs volcano | actual | actual ratio |
|---|---|---|---|---|
| volcano (sel 50) | — | 1× | 180.7 | 1× |
| vectorized (sel 50) | | | | |
| kernel (sel 50) | | | | |

| question | prediction | actual |
|---|---|---|
| does selectivity 5 vs 95 change volcano most or vectorized most? | | |
| X100 U-curve: vectorized at BATCH_SIZE 64 / 1024 / 65536 | | |
| does the kernel's branchless mask beat a branchy `if` at sel 50? (topic 0 says: yes, hugely) | | |
| kernels.rs: does autovec emit NEON for the fused loop? | | |
| multi-accumulator ILP trick: helps or not with random 64-slot destination? | | |

## Implementation log

- [ ] vectorized.rs: batches + selection vector + flat group array; tests green
- [ ] kernels.rs: fused branchless pass; negative-values test green (mask sign extension!)
- [ ] exec_bench full run recorded above
- [ ] BATCH_SIZE sweep recorded (64 / 1024 / 65536)
- [ ] flamegraph of volcano run — where does time actually go? (dispatch vs branch miss vs agg)
- [ ] look at kernels.rs asm — NEON? record instruction mix

Surprises / dead ends:

- (provided-baseline surprise, already found) LLVM DEVIRTUALIZED the
  `Box<dyn>` operator chain when the tree was statically known —
  202 M rows/s before black_box, 180 after. A compiler will happily turn
  your Volcano engine into a compiled engine if you let it. Real engines
  can't (trees built from plans at runtime).

## Questions from the reading guides

### DuckDB execution (reading-duckdb-execution.md)

1. Why 2048 not 64K (chunk bytes vs L2):
2. CONSTANT vector trace for `2 * price`:
3. Which operators need HAVE_MORE_OUTPUT and why:
4. Salt trick: fraction of non-matches still chasing pointers, for k salt bits:
5. M11 Expand as HAVE_MORE_OUTPUT — state between calls:

### Postgres executor (reading-postgres-executor.md)

1. Indirect branches per tuple for `SELECT sum(x) WHERE y > 10` vs per 2048 in DuckDB:
2. Why computed goto helps (predictor entry per opcode site):
3. Other "first call replaces itself" patterns:
4. eval.rs: linear steps or closure tree; 3 Cypher shapes worth peepholing:

### Rust execution stack (reading-rust-execution-stack.md)

1. Async operators vs hand-rolled state machines for M11 (one-threadpool decision from topic 7):
2. Which Cypher results are order-sensitive (MorselSeq equivalent needed?):
3. Select-in-lanes vs compact-then-compute — selectivity threshold guess:
4. HT probes saved by intern-then-index with 4 aggregates:
5. Which operator contract fits SpMV-produces-a-matrix best:

### X100 (reading-x100.md)

1. Vector length that keeps 3 ops × 8 cols in M-series L1:
2. Full-column materialization: GB moved for Q1, seconds at ~100 GB/s:
3. Rust monomorphization of primitives — compile time/binary size cost:
4. What autovec gives free vs what needs std::simd (answer from kernels.rs asm):

### Compiled vs vectorized (reading-compiled-vs-vectorized.md)

1. Why vectorized probing overlaps misses and fused loops don't (MLP):
2. Why prefetching is easy vectorized, contorted compiled:
3. Wide-pipeline load/store count per row, both models:
4. Prediction for kernels.rs vs vectorized.rs on THIS workload:
5. Graph workloads' scorecard column → JIT priority for M19:

### Morsel-driven parallelism (reading-morsel-parallelism.md)

1. What bounds morsel size below and above:
2. Thread-local HTs + merge vs shared lock-free HT: 64 groups / 64M groups:
3. Restoring order after morsel pulling — cost:
4. P/E cores: NUMA-shaped or skew-shaped problem:
5. **M11 parallelism paragraph**: morselize the frontier? natural morsel for SpMV:

## Cross-topic threads

- Vector size is a CACHE parameter — topic 0's ladder decides it.
- Selection vectors = filter-without-copying = the same
  don't-materialize discipline as late materialization (topic 12 next).
- Salt-in-pointer in the join HT = bit-smuggling ledger entry #6.
- Vectorized probes win by MLP = lookup_shootout's HashMap flatline.
- Morsel pulling vs static partitions = topic 9's scaling.rs skew story.

## M11 log (vectorized runtime: batch.rs / vectorized.rs / eval.rs)

- [ ] batch size chosen by measurement (sweep on M-series, not DuckDB's 2048 by faith)
- [ ] Batch type: node ids + property columns + selection vector
- [ ] operator contract: `next(&mut self, out: &mut Batch)` + a
      HAVE_MORE_OUTPUT-style result enum (Expand explodes)
- [ ] eval.rs: linear-step interpreter over batches, peephole the 3 hot shapes
- [ ] boundary decision: GraphBLAS matrix ops ↔ row-ish batches — where
      does Expand hand off?
- [ ] bench vs M10's naive interpreter; flamegraph both

## Done when

- All three engines agree + full bench table filled + U-curve recorded.
- The compiled-vs-vectorized scorecard argued for M11 with a decision.
- M11 parallelism paragraph written (morsel design for graph queries).
