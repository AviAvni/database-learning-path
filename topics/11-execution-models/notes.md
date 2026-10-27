# Topic 11 notes — execution models

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-27, rustc 1.99.0)

`cargo run --release --bin exec_bench` — 50 M rows,
`SELECT k, SUM(v) WHERE f < t GROUP BY k`, best of 3 per invocation; the table
is the median of three invocations on an idle machine (the three runs gave
5%: 511.2 / 502.8 / 543.6, 50%: 170.7 / 183.5 / 186.2, 95%: 294.9 / 328.2 /
295.0 M rows/s).

| selectivity | volcano | ns per scanned row (1e3 / rate) | vectorized | kernels |
|---|---|---|---|---|
| 5% | 0.098 s / 511.2 M rows/s | 1.96 | stub | stub |
| 50% | 0.272 s / 183.5 M rows/s | 5.45 | stub | stub |
| 95% | 0.169 s / 295.0 M rows/s | 3.39 | stub | stub |

**The cost is not monotone in selectivity: 50% is the slowest point, not 95%.**
The 50% case takes 5.45 ns/row against 1.96 ns at 5% and 3.39 ns at 95%. Two
effects stack, and they can be separated:

- **A data-dependent branch.** `FilterOp::next` compiles to a real conditional
  branch per row (`cmp w8, w22; b.hs` back to the `blr` of the child call —
  `objdump -d` of the release binary). On uniform random `f`, a row passes with
  probability t/100, so the predictor is right almost always at 5% and 95% and
  wrong about half the time at 50%. `cargo run --release --bin sorted_filter`
  runs the same Volcano pipeline on the random column and again after sorting
  `f` (identical work per row, predictable branch). Median of three runs,
  ns/row, random -> sorted: 5%: 1.94 -> 1.62; **50%: 5.05 -> 2.23**; 95%:
  2.95 -> 2.84. The peak disappears when the branch becomes predictable, so it
  is the misprediction, not the data volume. Dividing the 2.82 ns/row
  difference by 0.5 mispredictions per row (the most a row can average on
  random data, so this is a lower bound on the per-miss cost, not a counter
  reading) gives at least 5.64 ns, about 23 cycles at an assumed 4 GHz, per
  misprediction — consistent with topic 0's `branch_misprediction` bound of at
  most 7.62 ns, derived in
  [reading-postgres-executor.md](reading-postgres-executor.md).
- **A cost per surviving row.** 5% and 95% mispredict about equally often (the
  predictor is wrong on the rarer outcome, 5% of rows each), so their
  difference is survivor cost: (3.39 - 1.96) / (0.95 - 0.05) = 1.59 ns per
  additional surviving row, about 6 cycles at 4 GHz. The 95% point is the
  noisiest of the three (295-340 M rows/s across nine invocations of
  `exec_bench`, including the three toolchains below), and at 340 M rows/s
  (2.94 ns) the same division gives 1.09 ns; read it as roughly 1.1-1.6 ns,
  the second `dyn` call's return through memory plus the `sums[k]`
  read-modify-write.

Not determined: the exact misprediction count (macOS exposes no counters here).
The shape is not a compiler artifact: the same source built with rustc 1.85.0,
1.91.1 and 1.97.1 prints it too (50%: 190.4 / 190.8 / 188.6; 5%: 512.1 /
532.3 / 450.2; 95%: 340.0 / 298.6 / 337.3 M rows/s).

Predict before implementing: at 50% selectivity, how much of the gap between
183.5 M rows/s and memory bandwidth (topic 12 measures ~50 GB/s = 6.2 G u64/s on
this box) does batching close, and what does the remainder consist of?

## Reference numbers for the exercise lanes

Read these after filling the worksheet. A scratch reference solution
(vectorized: 1024-row batches, selection vector, flat 64-slot sums; kernel: one
branch-free pass `sums[k] += v * (f < t) as i64`; both pass the oracle tests)
printed, in one back-to-back run of `exec_bench` (Apple M3 Pro, 50 M rows, best
of 3, 2026-10-26; the machine was shared with other jobs, so absolute rates
differ from the Baseline block above, which is a separate idle-machine run — compare ratios
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
Baseline block above: **0.272 s = 183.5 M rows/s** — already fast, 5.45 ns/row
including two virtual calls per tuple and a coin-flip filter branch. Modern branch predictors eat stable
indirect calls; the Volcano tax on an M-series core is NOT mostly call overhead.
Where will the vectorized win actually come from? (SIMD, ILP, no
per-row branch.) Predict accordingly:

| engine | predicted M rows/s | predicted ratio vs volcano | actual | actual ratio |
|---|---|---|---|---|
| volcano (sel 50) | — | 1× | 183.5 | 1× |
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
