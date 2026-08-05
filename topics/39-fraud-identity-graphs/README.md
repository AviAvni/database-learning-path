# Topic 39 — Fraud Rings & Identity Graphs

Second of six graph use-case deep dives: the two graph workloads every
fraud team runs — **find the ring** and **resolve the identity**.
**FRAUDAR** (KDD'16): fraud rings are dense bipartite blocks (accounts ×
targets), naive suspicion scores are *row* properties the fraudster
controls, and camouflage (reviewing popular products too) defeats them;
column-weighted density with greedy peeling is provably
camouflage-resistant with a ½-approximation guarantee. **Fellegi–Sunter
record linkage** (Winkler 2006, splink in production): "is this the
same person?" is a likelihood-ratio test over field-agreement patterns,
with blocking to avoid n² and EM to learn the weights from unlabeled
data. **FlowScope** (AAAI'20): money laundering is a dense *multi-step
flow* — fraud detection on a k-partite transfer graph where the middle
accounts must both receive and send.

## The problem, measured (bench lane 1, provided — runs today)

```
   camouflage vs naive rankers — precision@|fraud users|
   (5000x5000 Zipf background, 50k edges; 25x100 block at 1.0 density)
   camo/fraud edge   degree-rank   obscurity-rank
    0.0               0.00           0.52
    0.5               0.28           0.00
    1.0               0.60           0.00
    2.0               0.76           0.00
```

Two row heuristics, failing in opposite regimes. Degree ranking ("flag
the most active accounts") misses economical fraud — honest power users
out-review a 100-edge bot — but lights up once camouflage inflates the
fraudster's row. Obscurity ranking ("active account, only unpopular
products") is the mirror image: decent at camo 0, dead the moment the
fraudster buys popularity-biased camouflage edges (Zipf-1.5 draws into
the honest columns). Both scores are functions of the fraudster's own
row, and he controls his row: at camo ≈ 0.5 he slips between them.
Lane 2's column-weighted peeling catches every regime at F = 1.00.

## FRAUDAR: density the fraudster cannot fake down

```
        objects (columns)
        obscure ... popular
   u1 [ ████████ | . c . . ]     block edges: weight 1/log(d+5), d small
   u2 [ ████████ | c . . c ]     camouflage c: lands on popular columns,
   u3 [ ████████ | . c c . ]        weight ~0 — and the BLOCK's columns
        fraud block           never receive camouflage (Theorem 3)
```

The metric family: g(S) = f(S)/|S| where f sums edge weights inside S.
Unweighted, that is average degree — and camouflage glues the block to
the power-users × hit-products community, so the densest unweighted set
swallows both (measured lane 2: F drops 1.00 → 0.65 at camo 2). The
fix: an edge into object j is worth 1/log(d_j + 5) — column weights.
Camouflage lands on *honest* (popular, hence cheap) columns; the fraud
block's own columns never change. The algorithm is greedy peeling:
repeatedly delete the node whose removal costs the least weighted
degree, track g over the shrinking set, return the best set seen —
O(|E| log |V|) with a priority structure, and Theorem 2 guarantees the
returned g is at least half the optimum. Paper numbers: F above 0.95
injecting 200×200 blocks into real review graphs under all camouflage
attacks; on Twitter's 41.7M-user / 1.47B-edge follower graph it found a
4031 × 4313 block at 68% density where 57% of sampled users were
hand-labeled fraudulent (vs 12–25% in controls).

## Fellegi–Sunter: identity as a likelihood-ratio test

```
   record pair -> agreement pattern gamma = [first, last, dob, city, phone]
   R = P(gamma|M) / P(gamma|U)     log2 R = sum over fields:
       agree field i:    +log2(m_i / u_i)      (m=P(agree|match))
       disagree field i: +log2((1-m_i)/(1-u_i)) (u=P(agree|nonmatch))
   R > T_mu: match   T_lambda..T_mu: clerical review   R < T_lambda: no
```

Nobody scores all n² pairs and nobody labels m and u. **Blocking**:
only compare pairs agreeing on a key, multiple passes (last name OR
dob) so one typo cannot hide a duplicate — measured lane 3: 15,000
records, 112,492,500 naive pairs → 271,012 blocked (415× fewer), and
Winkler's census version of the same idea takes 10¹⁷ pairs to 10¹² while
keeping 99.5% of matches. **Estimation, splink's split**: u from random
record pairs (in n² space almost every pair is a nonmatch — measured
u = [0.0052 0.0021 0.0003 0.0051 0.0006] ≈ 1/pool-size), then one EM
session per blocking pass fits p and m with u fixed — measured
m = [0.80 0.86 0.94 0.78 0.90] vs the (1−typo)² analytic truth
[0.81 0.87 0.94 0.77 0.90], p = 0.184. The trap this crate makes you
hit: every blocked candidate agrees on its blocking key *by
construction*, so an EM that includes that field degenerates (fitted
p → 1.0); each session must exclude the field it blocked on — exactly
why splink's `estimate_parameters_using_expectation_maximisation`
takes a blocking rule and skips those comparisons. Linking at 12 bits
with union-find: pair precision 0.989, recall 0.992, 48 ms end-to-end.

## FlowScope: laundering is dense flow, not a dense block

```
   sources X ──▶ middle accounts W ──▶ destinations Y
   f_i = min(in, out)  q_i = max(in, out)   per middle account
   g = (1/|S|) * sum_i [ (1+λ) f_i − λ q_i ]      λ = 4
```

AML's shape: fraud money fans out of source accounts, hops through
mule ("middle") accounts that *retain almost nothing*, and converges.
FRAUDAR on the transfer graph misses it — no single bipartite block is
dense. FlowScope scores a k-partite subgraph by balanced throughput:
a middle account contributes min(inflow, outflow) and is penalized
λ × max(...) for imbalance, so parking money or camouflage transfers
*hurt* the score. Same near-greedy peeling, same style of guarantee.
On CBank's real 6.13M-account / 43.98M-transfer data with a labeled
ring (4 sources, 12 mules, 2 destinations; the central mule v5 alone
passes ≈452.1M yuan, in ≈ out) it scores
FAUC 0.761/0.843 vs FRAUDAR's 0.529/0.704, and holds F1 ≥ 0.9 down to
injected volumes of $76M vs FRAUDAR's $180M. Covered as a reading guide
plus exercise 5 — the peeling machinery is the same as fraudar.rs.

## Production shape: splink (cloned under ~/repos/splink @ 04189f5)

| anchor (`splink/internals/`) | what to see |
|---|---|
| `linker.py:66` | `Linker` — the API façade; settings = comparisons + blocking rules |
| `linker_components/training.py:163` | `estimate_u_using_random_sampling` — u without labels |
| `linker_components/training.py:231` | `estimate_parameters_using_expectation_maximisation(blocking_rule)` — one session per pass |
| `expectation_maximisation.py:225` | the EM core; E-step `predict_from_comparison_vectors_sqls` at `:268`, M-step `compute_new_parameters_sql` `:45`/`:278` inside `maximisation_step:193` |
| `comparison_level.py:148` | `ComparisonLevel` — m at `:190`, u at `:191`; match weight log2(m/u) at `:426` |
| `comparison_level.py:667` | `_tf_adjustment_sql` — term-frequency: "Smith" agreement is worth less |
| `comparison_level_library.py:406/:458/:493` | Levenshtein / Jaro-Winkler / Jaro levels — agreement is graded, not boolean |
| `predict.py:203` | prior + match weights → probability 1/(1+2^(−mw)); pairwise scoring SQL at `:42` |
| `blocking.py:747` | `block_using_rules_sqls` — blocking passes as SQL self-joins |
| `linker_components/clustering.py:43` | threshold → `connected_components.py:121` — clusters, exactly lane 3's union-find |
| `dialects.py:24` | one model, four engines: DuckDB `:270`, Spark `:402`, SQLite `:532`, PostgreSQL `:573` |

## Reading guides

1. [reading-fraudar.md](reading-fraudar.md) — FRAUDAR: the axioms, column weights, greedy peeling, Theorems 2–3, the Twitter catch.
2. [reading-fellegi-sunter.md](reading-fellegi-sunter.md) — Winkler's survey: the FS decision rule, string comparators, EM estimation, blocking, BigMatch.
3. [reading-splink.md](reading-splink.md) — code read: u by random sampling, EM training sessions, match weights, clustering.
4. [reading-flowscope.md](reading-flowscope.md) — FlowScope: k-partite flow, the balance metric, why FRAUDAR misses laundering.

## Experiments

```
cd experiments
cargo test              # 3 provided tests pass; 6 fix the contract for your stubs
cargo run --release --bin fraud_bench
```

- `review_graph.rs` (PROVIDED) — synthetic review graph: Zipf(0.7) ×
  Zipf(0.8) background, planted 20×80 fraud block, popularity-biased
  camouflage; degree-rank and obscurity-rank baselines.
- `fraudar.rs` (stub) — greedy peeling with a lazy min-heap over
  weighted degrees, unweighted vs 1/log(d+5) column weights.
- `er.rs` (stub) — `estimate_u_random` (u from random pairs), `em_m`
  (EM with u fixed and the blocked field masked), `match_weight`
  (log2 likelihood ratio). Generator, blocking, union-find provided.

Bench lanes: 1 = the camouflage table (provided, above). 2 = unweighted
vs log-weighted F at camo {0, 0.5, 1, 2} (reference: 1.00/0.95/0.69/0.65
vs 1.00 across the board), plus peeling a 100k × 50k node, ~1.02M-edge
graph in ~0.2 s at F = 1.00. 3 = the 415× blocking table, the EM fit,
and precision 0.989 / recall 0.992 at 12 bits in ~48 ms.

## Exercises

1. Implement the stubs until all 9 tests pass and lanes 2–3 print.
2. Work Theorem 3 by hand on lane 2's numbers: at camo 2 the block's
   columns have degree 25 (weight 1/log 30) — show that camouflage
   changed neither f(block) nor any block column's weight, so g(block)
   is identical at camo 0 and camo 2.
3. Re-run `em_m` *without* masking the blocking field (include all 5
   fields in one EM over the unioned candidates) and record the fitted
   p — you should reproduce the p → 1 degeneracy, then explain it in
   two sentences.
4. Threshold sweep: link at {6, 8, 10, 12, 14, 16} bits and plot
   precision/recall. Identify which coincidence pattern (dob+city,
   dob+first, last+phone) each precision cliff corresponds to.
5. FlowScope on paper: take lane 2's bipartite peeling and write the
   pseudocode delta for k=3 partites with f_i = min(in, out) and the
   λ-imbalance penalty — which per-node quantity does the heap key
   become?
6. Zipf-distributed *names*: replace field pools with Zipf(1.0) draws
   and re-measure lane 3. Where does precision fail first, and what
   does splink's `_tf_adjustment_sql` do about exactly this?

## Cross-topic threads

- **Topic 18 (GPU graph analytics)**: greedy peeling is the same
  degree-ordered vertex elimination as k-core decomposition; the lazy
  heap is a CPU stand-in for topic 18's bucketed frontier.
- **Topic 36 (sharding)**: blocking keys are hash-partition keys — the
  two blocking passes are two shuffles, and splink runs them as SQL
  self-joins on Spark for exactly that reason.
- **Topic 35 (overload)**: FRAUDAR's O(|E| log |V|) peel of a 1B-edge
  graph is an offline batch job; the identity-graph lookup (lane 3's
  cluster id per record) is the online path that must stay in
  single-digit ms — same split as topic 32's HTAP.
- **Topic 38 (GraphRAG)**: entity resolution IS GraphRAG-SDK's
  resolution strategy stack — exact → fuzzy → embedding → LLM is a
  learned match_weight with fancier comparators.

## Capstone M39 — fraud primitives on the Rust graph engine

- Dense-block scan as a graph procedure over M31's storage: weighted
  degrees from the CSR (topic 18), lazy-heap peeling, returns
  (users, objects, g) like `fraudar.rs`.
- Identity resolution at write time: blocking keys as indexed
  properties, FS match weights in the property layer, union-find
  cluster ids maintained incrementally on insert.
- Deliverable numbers: peel throughput (edges/s) on a 10M-edge
  synthetic vs `fraud_bench` lane 2; per-insert resolution latency at
  1M records with two blocking indexes; precision/recall vs lane 3.
