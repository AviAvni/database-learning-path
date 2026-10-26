# Topic 11 notes — execution models

## Baseline (provided lane, Apple M3 Pro, measured 2026-07-28)

`cargo run --release --bin exec_bench` — 50 M rows,
`SELECT k, SUM(v) WHERE f < t GROUP BY k`, best of 3.

| selectivity | volcano | vectorized | kernels |
|---|---|---|---|
| 5% | 0.386 s / 129.4 M rows/s | stub | stub |
| 50% | 0.484 s / 103.3 M rows/s | stub | stub |
| 95% | 0.669 s / 74.7 M rows/s | stub | stub |

**103 M rows/s at 50% selectivity is the tuple-at-a-time ceiling on this
machine, and note which direction it moves.** Cost rises monotonically with
selectivity (74.7 M rows/s at 95%) because in a Volcano pipeline every surviving
row pays the full per-tuple interpretation cost — a virtual call per operator
per row — on its way to the aggregate. The filter is not the expensive part;
*passing the filter* is.

Predict before implementing: at 50% selectivity, how much of the gap between
103 M rows/s and memory bandwidth (topic 12 measures ~50 GB/s = 6.2 G u64/s on
this box) does batching close, and what does the remainder consist of?

## Reference numbers for the exercise lanes

Read these after filling the worksheet. A scratch reference solution
(vectorized: 1024-row batches, selection vector, flat 64-slot sums; kernel: one
branch-free pass `sums[k] += v * (f < t) as i64`; both pass the oracle tests)
printed, in one back-to-back run of `exec_bench` (Apple M3 Pro, 50 M rows, best
of 3, 2026-10-26; the machine was shared with other jobs, so absolute rates
differ from the Baseline block above, which is a different run — compare ratios
only within a row):

| selectivity | volcano | vectorized | kernel | vec / volcano | kernel / volcano |
|---|---|---|---|---|---|
| 5% | 449.6 M rows/s | 1408.5 M rows/s | 1663.8 M rows/s | 3.1x | 3.7x |
| 50% | 175.0 M rows/s | 299.5 M rows/s | 1618.1 M rows/s | 1.7x | 9.2x |
| 95% | 294.0 M rows/s | 821.5 M rows/s | 1594.1 M rows/s | 2.8x | 5.4x |

- **The Volcano/vectorized gap is 1.7-3.1x here, not "one to two orders of
  magnitude".** Across three further runs on a heavily loaded machine the 50% ratio
  ranged 1.2x-5.9x (vectorized) and 3.5x-24x (kernel). The textbook figure
  is not reproduced by a modern branch predictor and a
  `Box<dyn>` chain whose indirect calls are perfectly predictable; the
  fused kernel is the only lane that approaches one order of magnitude.
- A second full run reproduced the table's ratios (50%: volcano 178.7, vectorized 300.1, kernel 1660.6 M rows/s = 1.7x / 9.3x; 5%: 2.9x / 3.3x; 95%: 2.8x / 5.5x).
- The kernel is flat across selectivity (1594-1664 M rows/s: it is branch-free,
  so the data it touches does not depend on the filter); Volcano and the
  vectorized lane are not, because both branch per row. At 50% the vectorized
  lane's selection-vector build is a data-dependent branch on random data.
- **BATCH_SIZE sweep** (vectorized, M rows/s at selectivity 5/50/95%): 64 -
  1299/298/808; 1024 - 1409/300/822; 65536 - 903/238/542. No U on the small
  side here: 64 rows already fits L1 and the per-batch overhead is a few
  instructions, so 64 and 1024 are within 10% (each sweep point is its own run); only 65536 (768 KB of three columns,
  past L1) loses, by 20-36%. X100's left arm of the U needs interpretation
  overhead per batch that this engine does not have.
- Not recorded: a NEON instruction mix for the kernel, a flamegraph, the
  multi-accumulator variant.

## Predictions (fill BEFORE implementing vectorized.rs / kernels.rs)

Measured baseline (provided volcano, release, 50M rows, sel 50%), from the
Baseline block above: **0.484 s = 103.3 M rows/s** — already fast, ~9.7 ns/row
including two virtual calls per tuple. Modern branch predictors eat stable
indirect calls; the Volcano tax on an M-series core is NOT mostly call overhead.
Where will the vectorized win actually come from? (SIMD, ILP, no
per-row branch.) Predict accordingly:

| engine | predicted M rows/s | predicted ratio vs volcano | actual | actual ratio |
|---|---|---|---|---|
| volcano (sel 50) | — | 1× | 103.3 | 1× |
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

- Why `volcano.rs` wraps every operator in `black_box`: when the operator
  tree is statically known, LLVM devirtualizes the `Box<dyn>` chain, and a
  compiler will happily turn your Volcano engine into a compiled engine if
  you let it. Real engines can't — their trees are built from plans at run
  time — so the baseline hides the tree from the optimizer to measure the
  dispatch a real engine pays.

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
