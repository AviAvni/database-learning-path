# Topic 55 notes — Datalog engines: recursion as joins and as sparse algebra

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin datalog_bench`. Seeded generators (`gen.rs`: power-law
`power_law(1500, 4500, 55)` and `power_law(600, 1200, 56)`, grid 24×24, chain 500,
6,000 chains × 10): every count below reproduces exactly on any machine, and
two consecutive runs gave identical counts. Timings are best of 3 within one
run, on a machine that other benchmarks were using (load average recorded
with each run), so quote counts, not milliseconds.

| run | load average (1/5/15) at start | tc linear naive / semi-naive, power-law (ms) | chain (ms) | 6000 chains (ms) |
|---|---|---|---|---|
| quoted in README | 13.51 16.13 21.33 | 107.05 / 13.01 | 1550.63 / 16.84 | 12.85 / 6.42 |
| second run, same session | 21.49 17.99 21.81 | 127.96 / 14.68 | 1463.70 / 16.95 | 10.90 / 5.92 |

Ordering is stable (naive ≫ semi-naive; the nonlinear forms ≫ both); absolute
values move ~15% with load.

### Lane 1 — naive vs semi-naive, exact counts

| program | graph | rounds | naive derivations | semi-naive | naive ÷ semi | new | duplicates left |
|---|---|---|---|---|---|---|---|
| tc linear | power-law n=1500 (3,489 arcs) | 21 | 2,109,908 | 137,482 | 15.3× | 65,289 | 72,193 |
| tc linear | grid 24×24 (1,104) | 46 | 5,248,416 | 165,600 | 31.7× | 89,424 | 76,176 |
| tc linear | chain 500 (499) | 499 | 41,417,499 | 124,750 | 332× | 124,750 | 0 |
| tc linear | 6000 chains × 10 (54,000) | 9 | 1,494,000 | 270,000 | 5.5× | 270,000 | 0 |
| sg | power-law n=600 (960) | 6 | 278,931 | 64,787 | 4.3× | 25,408 | 39,379 |
| sg | grid 24×24 | 23 | 619,986 | 35,468 | 17.5× | 9,177 | 26,291 |

Nonlinear TC (`tc(x,z), tc(z,y)`), derivations (naive / textbook delta /
disjoint "old-delta" delta) and rounds:

| graph | rounds | naive | textbook | old-delta | old-delta saves | vs linear semi-naive |
|---|---|---|---|---|---|---|
| power-law | 6 | 5,882,220 | 2,749,550 | 2,143,923 | 22.0% | 15.6× |
| grid | 7 | 17,567,264 | 8,114,570 | 6,581,680 | 18.9% | 39.7× |
| chain | 10 | 45,138,521 | 24,420,281 | 20,708,999 | 15.2% | 166× |

Goal-directed `?- tc(0,y)` (a magic-set rewrite of linear TC: a unary
recursion seeded at vertex 0): derivations 1,808 / 1,104 / 499 / 9 against
the full closure's 137,482 / 165,600 / 124,750 / 270,000 — **76× / 150× /
250× / 30,000×** fewer — for 661 / 575 / 499 / 9 result tuples.

- **Hand predictions that check out.** The chain's naive count is almost
  all re-derivation: round r re-derives every earlier tuple, 332 derivations
  per closure tuple. The semi-naive count must be exactly |arc| + Σ outdeg(z)
  over the closure — a sanity line, not an estimate (137,482 on the
  power-law graph, exactly). d/new = 1.00 on the chain because each pair
  has exactly one path.
- **Negative result / honest reading.** Semi-naive removes redundancy
  *across rounds*, not *within* them: 72,193 of 137,482 derivations on the
  power-law graph are duplicates; it is the head filter, not the delta, that
  keeps them out of the relation. And fewer rounds is not less work: nonlinear
  TC finishes in 6–10 rounds against 21–499 but derives 15.6×–166× more.
- **Timing surprise.** Nonlinear old-delta on the chain takes 1,638.68 ms
  against 16.84 ms for linear semi-naive, 97× — the round count (10 against
  499) would predict the opposite.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 1: semi-naive duplicates on the 24×24 grid (derivations − 89,424) | | |
| lane 1: nonlinear TC rounds on the 500-chain | | |
| lane 1: `?- tc(0,y)` derivations on the 24×24 grid | | |
| lane 2: derivations of `Left`, `Right`, `Adaptive` on the power-law graph | | |
| lane 2: which graph shapes tie `Left` and `Right`, and why | | |
| lane 2: sorted vs hash time on the 500-chain — who wins, and why | | |
| lane 3: flops of `tc_algebra` on the power-law graph (hint: lane 1's semi-naive derivations minus |arc|) | | |
| lane 3: algebra vs lane-1 time on `6000 chains × 10` — which is faster | | |
| lane 3: `sg_algebra` flops vs lane 1's 64,787 on the power-law graph | | |
| exercise 4: per-source bit-matrix TC vs the per-round matrix loop on the power-law graph | | |

## Reference numbers for the exercise lanes

Read these after filling the worksheet. What a reference solution printed
(written in a scratch copy of the crate and not committed; measured
2026-10-26 on the same Apple M3 Pro, load average 5.81 at the start, best of
3; counts exact):

**Lane 2 — sorted relations.**

| graph | form | rounds | derivations | probes | sorted ms | lane-1 hash semi-naive ms |
|---|---|---|---|---|---|---|
| power-law | `Left` | 21 | 137,482 | 169,244 | 4.59 | 13.21 |
| power-law | `Right` | 21 | 124,357 | 161,822 | 4.38 | |
| power-law | `Adaptive` | 21 | **119,159** | 160,296 | 4.39 | |
| grid | all three tie | 46 | 165,600 | 177,744 | 4.43–6.00 | 15.09 |
| chain | all three tie | 499 | 124,750 | 249,001 | 38.0–38.7 | 15.91 |
| 6000 chains | all three tie | 9 | 270,000 | 486,000 | 4.44–4.79 | 5.98 |

- `Right` derives 9.5% fewer than `Left` on the power-law graph (124,357 vs
  137,482); `Adaptive`, choosing per round from the delta's own degrees,
  derives 13.3% fewer than `Left` and 4.2% fewer than `Right`, and is exactly
  the oracle `|arc| + Σ_r min(L_r, R_r)`. The 13% in derivations is ~4% in
  time (4.59 → 4.39 ms) — the sort and merge, not the probes, dominate.
- The three forms tie on the grid and the chains because their in- and
  out-degrees are symmetric along every path.
- **Sorted beats hash 3.0× and 3.4× on the power-law and grid graphs and
  1.3× on the 10-chains, and loses 2.4× on the 500-chain** (38 ms vs 15.9).
  The reference rewrites the whole sorted closure every round — O(|T|) per
  round × 499 rounds — where the hash set inserts only the delta. SRDatalog's
  head-buffer/body split ([reading-wcoj-gpu-distributed.md](reading-wcoj-gpu-distributed.md)
  Step 3) and Soufflé's B-tree both avoid that rewrite.
- **Negative result — a static form choice cannot be made from degree or
  walk statistics.** `Σ_z out(z)·W↓_k(z)` (walks of length ≤ k ending at z)
  equals `Σ_z in(z)·W↑_k(z)` (walks starting at z) for every k: both count
  the same walks. An estimator built on either always ties and picks `Left`,
  right on 21 of 40 seeded power-law graphs (`power_law(300, 900, seed)`,
  seeds 0–39), worst ratio 1.535 to the cheaper form. An estimator that
  sampled BFS reachability was right on 16 / 18 / 26 / 24 / 28 of 40 at 8 /
  16 / 32 / 64 / 128 sources (worst ratio 1.466 at 64), and on 40 of 40 with
  all 300 sources — i.e. only the closure itself decides. Closure-dependent
  costs need run-time information, which is why the lane's answer is
  `Adaptive`: it reads the delta, whose cost it can sum exactly before
  running the round.

**Lane 3 — masked sparse product.**

| program | graph | rounds | flops | new entries | algebra ms | lane-1 hash semi-naive ms | ratio |
|---|---|---|---|---|---|---|---|
| tc | power-law | 21 | 133,993 | 61,800 | 0.80 | 13.21 | **16.5× faster** |
| tc | grid | 46 | 164,496 | 88,320 | 0.46 | 15.09 | **32.8×** |
| tc | chain | 499 | 124,251 | 124,251 | 1.45 | 15.91 | **11.0×** |
| tc | 6000 chains × 10 | 9 | 216,000 | 216,000 | 7.57 | 5.98 | **1.27× slower** |
| sg | power-law | 6 | 85,124 | 25,408 | 0.41 | 2.82 | 6.9× |
| sg | grid | 23 | 54,328 | 9,177 | 0.27 | 0.38 | 1.4× |

- flops + |arc| = lane 1's semi-naive derivations, exactly: 133,993 + 3,489 =
  137,482; 164,496 + 1,104 = 165,600; 124,251 + 499 = 124,750; 216,000 +
  54,000 = 270,000. The algebra did not reduce the derivation count by one.
- `sg` attempts **more** multiply-accumulates than lane 1 (85,124 vs 64,787,
  1.31×): two products per round with a materialized intermediate `Aᵀ·D`,
  against a three-atom nested loop with none.
- **Where the algebra wins and loses, honestly.** It wins 11–33× on the
  three graphs whose closure fills a good part of an n² bit matrix that fits
  in cache (n² / 8 = 281 KB, 41 KB, 31 KB), and loses 1.27× on
  `6000 chains × 10`, where n = 60,000 makes the mask 450 MB of mostly empty
  address space for a 270,000-tuple closure. Most of the win is the
  per-derivation cost, not the formulation (same derivation counts): a bit
  test against a hash probe and CSR rows against hash-indexed vectors. A
  hash-free per-source version (RecStep PBME, exercise 4) shows how much:
  **1.83 / 1.48 / 2.87 / 24.45 ms** on power-law / grid / chain / 10-chains
  against lane 1's 13.21 / 15.09 / 15.91 / 5.98 — 7.2×, 10.2×, 5.5× faster
  on the first three and **4.1× slower on the last** — and slower than the
  per-round matrix loop on all four (2.3×, 3.2×, 2.0×, 3.2×; the reference
  allocates a vector per source per level, which a tuned version would not).
  So the sparse formulation's advantage over a hash-based Datalog loop is
  mainly the bit-mask membership structure, the n²-bit price is real, and the
  per-round form beat the per-source form here.

## Paper numbers worth keeping

- **SRDatalog (arXiv 2604.20073v2), Table 2, recomputed.** Geometric mean of
  the 17 raw time ratios: vs Ascent **17.6×** (text §7.1: 21×), vs FlowLog
  **13.4×** (14×), vs Soufflé **26.4×** (26×); abstract: "21× to 47×". The
  Ascent figure and the abstract's 47× do not reproduce from the table.
  Clean GPU-vs-GPU: 2.3×–7.1× over VFLog on same-generation (Table 3),
  2.1×–4.0× over cuMatch on LSQB (Figure 4).
- **Distributed (arXiv 2610.12692v1).** Table I replication (64 ranks):
  edges 66 / 17 / 12 × N_E for 1D / 2D / 3D; Influenced 1 / 8 / 16 × N_I.
  Peer Influence (Orkut), Polaris, 512 ranks: 10.087 s (2D) vs 39.556 s (3D);
  2D out of memory at 32 ranks. Table V: loses to FlowLog on Andersen Large
  (17.519 vs 9.408 s) and to Soufflé and FlowLog on SG (78.235 vs 57.347 and
  31.905 s). Table VI (TC, USRoad): 8.9×–11.2× over Slog — a two-atom join.
- **RecStep (arXiv 1812.03975, PVLDB 12(6)).** OOF on CSPA/httpd: 63% (stale
  plan), 41% (full statistics), 24% (lightweight) of the no-optimization
  runtime (Figure 2). DSD worked: |R| = 1,000,000, |Rδ| = 10,000, |r| = 2,000,
  α = 2 → OPSD 2,010,000 vs TPSD 1,034,000 (units of C_p).
- **Soufflé index selection (PVLDB 12(2)).** Four searches {x}, {x,y}, {x,z},
  {x,y,z} need 2 indexes (x≺y≺z, x≺z), not 4 (Example 3); O(|S|^2.5 +
  |S|²·m); brute force at 4 attributes tests ≈ 1.8 × 10¹⁹ subsets.
- **Sato (arXiv 1608.00139), Table 2.** N = 1,000: Matrix ≈ 0.1 s at every
  p_e; B-Prolog 0.004 s at p_e = 0.001, 188.3 s at p_e = 1.0.

## Guide-question checklist

- [ ] reading-souffle.md Q1–Q4
- [ ] reading-recstep.md Q1–Q4
- [ ] reading-ascent.md Q1–Q4
- [ ] reading-cozo.md Q1–Q4
- [ ] reading-wcoj-gpu-distributed.md Q1–Q4
- [ ] reading-datalog-as-linear-algebra.md Q1–Q4

## Cross-topic threads (worked)

- Topic 27 ↔ 55: lane 1's semi-naive rows are the batch version of
  `SemiNaiveReach`; 332× fewer derivations on the chain is the same
  "each edge relaxed O(1) times ever" as topic 27's table, here for the
  full closure.
- Topic 46 ↔ 55: `WITH RECURSIVE … UNION` is the `tc linear semi-naive` row
  evaluated by a relational engine; topic 46's 40,832,336 is a closure count.
- Topic 47 ↔ 55: the goal-directed row (1,808 vs 137,482 derivations) is a
  semi-join reduction toward the query's bound argument.
- Topic 20 ↔ 55: lane 3's complement mask is `GrB_DESC_RSC`; its flops
  identity says GraphBLAS cannot beat semi-naive on *work*, only on the cost
  per unit of it.

## Open questions

- Lane 2's per-round rewrite of the closure loses 2.4× on a long chain. Does
  an LSM-style head/body split recover the hash set's win without losing the
  sorted layout's 3× on shallow graphs?
- Does lane 3's per-round matrix loop keep beating per-source PBME once the
  per-source reference stops allocating per level?
- SRDatalog's Table 2 compares a GPU to 32 CPU cores; a CPU WCOJ engine on
  the same rules would separate the algorithm from the hardware.
