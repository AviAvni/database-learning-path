# Topic 50 notes — LLM-era query processing

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin semop_bench`. A **simulation**: no LLM is called,
nothing touches the network. Every count, selectivity and F1 below follows
from seed 50 (lane 2: seeds 1001–1100; lane 3: seed 77) and reproduces
exactly on any machine; dollars and seconds are counts × the MODEL constants
in `src/model.rs` (oracle $0.001 / $0.002 and 0.8 / 1.6 s per call; proxy
$0.00005 and 0.046 s; parallelism 20; oracle accuracy 0.97).

### Lane 1a — `WHERE P1 AND P2`, 10,000 rows, oracle only

Truth: P1 selectivity 0.2987, P2 0.1023; the oracle measures 0.3090 and
0.1235. Truth set 297 rows, gold (oracle on every row) 364, overlap 276.

| plan | oracle calls | $ MODEL | s MODEL | F1 vs truth | F1 vs gold |
|---|---|---|---|---|---|
| (a) every predicate, every row | 20,000 | 30.00 | 1,200 | 0.835 | 1.000 |
| short-circuit, written order P2,P1 | 11,235 | 21.23 | 850 | 0.835 | 1.000 |
| selectivity order P2,P1 (+200-row sample) | 11,635 | 21.84 | 874 | 0.835 | 1.000 |
| (b) rank order P1,P2 (+200-row sample) | 13,490 | 16.78 | 672 | 0.835 | 1.000 |

- **Most calls, least money.** Rank order makes 2,255 more oracle calls than
  the written order and costs 21% less, because its extra calls are the cheap
  predicate's. Sample estimates: P1 0.315, P2 0.130; ranks −685 and −435.
- **The oracle's own F1 vs truth is 0.835** (closed form 0.822; mean of 30
  other seeds 0.825). Its 3% errors inflate P2's selectivity from 0.102 to
  0.124 — a rare predicate's false-positive rate is a large fraction of its
  true rate.
- Sanity, all passed: short-circuit calls = 10,000 + 1,235 exactly; every
  ordering returns the identical gold set.

### Lane 1c — cascade band sweep (rank order, both predicates, band [0.5 − h, 0.5 + h])

Proxy right at a 0.5 cut: P1 0.887, P2 0.821 (μ = 1.2 and 0.9).

| h | oracle | proxy | $ MODEL | s MODEL | P gold | R gold | F1 gold | F1 truth |
|---|---|---|---|---|---|---|---|---|
| 0.00 | 0 | 13,419 | 0.67 | 31 | 0.260 | 0.615 | 0.366 | 0.385 |
| 0.10 | 2,259 | 13,198 | 3.64 | 151 | 0.463 | 0.764 | 0.577 | 0.584 |
| 0.20 | 4,746 | 13,112 | 6.82 | 279 | 0.692 | 0.846 | 0.761 | 0.741 |
| 0.30 | 7,702 | 13,073 | 10.42 | 422 | 0.895 | 0.909 | 0.902 | 0.837 |
| 0.40 | 11,200 | 13,082 | 14.66 | 592 | 0.983 | 0.973 | 0.978 | 0.846 |
| 0.45 | 12,642 | 13,084 | 16.32 | 658 | 1.000 | 1.000 | 1.000 | 0.835 |
| 0.49 | 13,087 | 13,090 | 16.83 | 678 | 1.000 | 1.000 | 1.000 | 0.835 |
| 0.50 | 13,090 | 13,090 | 16.83 | 678 | 1.000 | 1.000 | 1.000 | 0.835 |

- **Negative result: with this proxy the cascade saves 9% at F1 0.978.** At
  h = 0.5 it costs $0.65 more than no cascade (the proxy pass).
- At h = 0.30–0.40 F1 vs truth (0.837, 0.846) is slightly *above* the
  oracle's 0.835: rows the proxy is very sure of escape the oracle's
  independent errors. One seed; do not quote it as a general effect.

### Lane 1c′ — proxy quality μ swept (same oracle, same seed)

Baseline: plan (b)'s main pass, 13,090 calls, $16.18, F1 vs truth 0.835.

| μ | proxy acc P1 | proxy-alone F1 truth / gold | cheapest h for F1 gold ≥ 0.95: oracle, $ vs (b) | cheapest h for F1 truth ≥ 0.835: oracle, $ vs (b) |
|---|---|---|---|---|
| 0.5 | 0.698 | 0.170 / 0.175 | 0.40: 12,496, −0% | 0.45: 13,009, +3% |
| 1.0 | 0.844 | 0.392 / 0.365 | 0.40: 11,582, −7% | 0.30: 8,325, −32% |
| 1.5 | 0.933 | 0.655 / 0.585 | 0.40: 9,886, −20% | 0.15: 2,193, −79% |
| 2.0 | 0.976 | 0.861 / 0.740 | 0.45: 10,855, −13% | 0.00: 0, −96% |
| 2.5 | 0.993 | 0.964 / 0.812 | 0.50: 13,090, +4% | 0.00: 0, −96% |
| 3.0 | 0.999 | 0.993 / 0.833 | 0.50: 13,090, +4% | 0.00: 0, −96% |

Against gold the saving peaks at a middling proxy and disappears once the
proxy beats the oracle; against truth it rises monotonically. The cause is
the simulation's independence of proxy and oracle errors (README Step 3).

### Lane 1d — ten agents, overlapping 2,000-row probes

| | oracle calls | cache hits | $ MODEL | s MODEL |
|---|---|---|---|---|
| no cache | 26,150 | 0 | 32.30 | 1,299 |
| shared (doc, predicate) cache | 11,615 | 14,535 | 14.35 | 589 |

20,000 rows requested, 8,878 distinct (expected 8,926); cached calls =
8,878 + 2,737 distinct P1-yes rows = 11,615 exactly; identical answers.

### Lane 1e — `WHERE P2 LIMIT 10`

Evaluate all then LIMIT: 10,000 calls, $20.00, 800 s. Stream in waves of 20
and stop: 120 calls, $0.24, 10 s. **83×**; expected ≈ 10 / 0.1235 = 81,
rounded up to whole waves.

### Lane 2 — provided row

Naive learner (thresholds that exactly meet the targets on the sample),
targets 0.9 / 0.9 vs gold, δ = 0.2, 300-row sample, 100 workloads of 10,000:
**missed a target in 68%** of trials; mean band calls 2,204; mean recall
0.903, precision 0.901.

### Lane 3 — provided row

Nested loop, 300 × 200: 60,000 oracle calls, $60.00, 2,400 s. Real matches
398; gold 417 = 390 correct + 27 invented on non-matching pairs.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 2: miss rate of your corrected learner over 100 trials | | |
| lane 2: mean band calls — how many more than the naive 2,204? | | |
| lane 2: does a Hoeffding (distribution-free) bound pass the "< 6,000 calls" test? | | |
| lane 3: oracle calls for recall ≥ 0.95 vs gold's correct matches | | |
| lane 3: recall vs *all* of gold at that point | | |
| exercise 3: lane 1c′'s F1-vs-gold savings with a distilled proxy — higher or lower? | | |
| exercise 4: does rank order still win with correlated P1, P2? | | |

<details><summary>Author's predictions — what the first reference attempts assumed, set against what they measured</summary>

- Lane 2: a corrected learner would miss "around 10%" — measured 6%
  (normal approximation, union bound) and 10% without the union bound. I
  expected a Hoeffding learner to pass the call budget; it did not (9,122
  band calls), because at ~93 sampled positives Hoeffding's radius (0.111)
  exceeds the whole 0.10 recall slack.
- Lane 3: I expected top-6 per paper to reach recall 0.95 at 1,800 calls. It
  reached 0.873 vs gold and 0.933 vs gold's correct matches; I had not
  anticipated the 27 invented pairs capping recall against all of gold, which
  is why the contract measures against gold ∩ truth.
- Lane 1c′: I expected cascade savings to grow monotonically with proxy
  quality against both references. Against gold they do not.

</details>

## Reference numbers for the stubs

From reference implementations written for this topic, run, recorded here,
and then removed (the stub tests were run green against them first).

### Lane 2 — three learners, same 100 trials

| learner | missed | mean band calls | mean recall | mean precision |
|---|---|---|---|---|
| naive (provided) | 68% | 2,204 | 0.903 | 0.901 |
| normal approximation σ̂·√(2 ln(1/δ′)/n), δ/2 per threshold, union bound over precision candidates | **6%** | **4,504** | 0.945 | 0.967 |
| same, union bound over 10 candidates only | 7% | 4,281 | 0.945 | 0.958 |
| same, no union bound | 10% | 4,076 | 0.945 | 0.946 |
| Hoeffding √(ln(1/δ′)/2n), same structure | 0% | 9,122 | 0.991 | 1.000 |

The recall correction requires the sample miss-rate of positives q̂ to
satisfy q̂ + σ̂_q·√(2 ln(2/δ)/n_pos) ≤ 1 − γ_R; the precision correction
bounds the precision of the whole cascade output (accepted + oracle-band
positives) from below. The guarantee costs about 2,300 extra oracle calls
over the naive learner — the price of δ = 0.2.

### Lane 3 — per-left top-k blocking (seed 77)

| k | oracle calls | P gold | R gold | R gold∩true | contract |
|---|---|---|---|---|---|
| 6 | 1,800 | 1.000 | 0.873 | 0.933 | fails |
| 8 | 2,400 | 1.000 | 0.894 | 0.954 | passes on 77, fails the three-seed test |
| **10** | **3,000** | **1.000** | **0.916** | **0.977** | passes on 77, 78, 79 |
| 15 | 4,500 | 1.000 | 0.930 | — | |

Global similarity thresholds instead of top-k: 0.50 → 1,776 calls, R gold
0.868; 0.47 → 3,044, 0.916; 0.45 → 4,338, 0.923; 0.42 → 7,323, 0.940. All
500 embedding calls are charged on top; total at k = 10: $3.02, 121 s
simulated, against the nested loop's $60.00 and 2,400 s.

## Paper numbers worth keeping

- **Abacus (Russo et al., PVLDB 19(5) 2026).** Table 2: Abacus 0.261 / 0.662 /
  0.304 quality on BioDEX / CUAD / MMQA at $0.89 / $0.69 / $13.1 and 450 /
  450 / 1,149 s; next best LOTUS 0.216 ($18.9, 2,348 s), DocETL 0.475 ($7.05,
  1,820 s), LOTUS 0.284 ($14.3, 1,208 s). Mean cost ratio 10.8×, time 3.4×
  (the abstract's figures); §4.3's prose (20.3/18.7/39.2%, 12.6×, 2.7×) does
  not match Table 2. Table 3: MinCost reduction 2.50× / 4.25× / **0.81×**
  (MMQA, costlier). ~2,800 operators per map (§2.3), ~3,000 total (§4.1);
  k = 6, j = 4, budget 50 × operators (§4.3). Priors: up to 1.60× / 1.43×
  unconstrained, 3.02× / 2.01× constrained (§4.4). LOTUS joins "> 100,000 LLM
  calls" worst case (§4.3).
- **LOTUS (Patel et al., arXiv 2407.11418v3).** FEVER: 91.2% / 329.1 s
  unoptimized → 91.0% / 190.0 s optimized (1.73×, 99.8%); FacTool 80.9%,
  5,396.11 s (Table 2). BioDEX join plans: 27,687 vs 5,290 calls; gold
  6,092,500 (1,152×) (Table 5). Top-k: quadratic 4,950 = C(100,2) and
  19,900 = C(200,2) comparisons (Table 7). Defaults in evaluation γ = 0.9,
  δ = 0.2 (§5); code defaults γ = 0.8, δ = 0.2, 10% sample (`types.py:164-167`).
  Proxy 17.4× faster than oracle, ~39% less accurate, for sem_group_by (§5.4).
- **SemBench (Lao et al., PVLDB 19(8) 2026).** 5 scenarios, 55 queries;
  gemini-2.5-flash; parallelism 20 (§5.1). LIMIT queries: >100× cost spread
  (§5.3). MMQA Q7: 40,000 calls at scale 200, 160,000 at 400 (§5.3–5.4).
  Table 5 joins: Palimpzest 0.703 / $3.29, LOTUS 0.614 / $2.74, ThalamusDB
  0.487 / $0.14, BigQuery 0.601 / $1.02. Memory up to ~250 GB (§5.4).
  Scalability runs $9,935.80, ~18 days (§5.1).
- **Text-to-SQL benchmarks are broken (Jin et al., CIDR'26).** 52.8% of BIRD
  Mini-Dev (263/498), 66.1% of Spider 2.0-Snow (80/121); CHESS 62 → 81%,
  4th → 1st; OpenSearch-SQL 71 → 69%, 1st → 3rd. Table 4's flips net +22 and
  −4, against EX changes of +19 and −2.
- **Agent-first (Liu et al., CIDR'26).** Hints cut an o3 agent's SQL queries
  12.67 → 10.38 (−18.1%, Table 1); Neon agents create 20× more branches and
  50× more rollbacks than humans (§6.2).
- **LLM as cardinality estimator (Liu, Wang, Wu et al., arXiv 2603.28080).**
  LLM on ~25–35% of sub-queries (cost-flagged) cuts inference time ~66% and
  beats every baseline end to end (§10.1.3); 85.03% of queries need no
  self-correction, 9.24% need up to five iterations (§10.1.3).

## Cross-topic threads

- Topic 10's expensive-predicate ordering is lane 1a; Abacus's Pareto-Cascades
  is topic 10's Cascades with a frontier per group.
- Topic 38's GraphRAG-SDK extraction (NER, then LLM verify) is a cascade with
  no stated guarantee — a candidate for lane 2's machinery.
- Topic 11's pipelining is why LIMIT pushdown is free in Volcano and 83× here.

## Open questions

- LOTUS's recall correction divides per-side means (`cascade_utils.py:120`).
  Does SUPG (the procedure it cites) define them over the whole sample? Read
  Kang et al., VLDB'20, and check.
- Is LOTUS's sampling range of 200 (`cascade_utils.py:20`) intended for
  filters, or only for joins (it sits under "Join cascade args")?
- How correlated are a real small-LLM proxy's errors with a large oracle's?
  Lane 1c′'s gold-vs-truth split depends entirely on it.

## Implementation log

- [ ] `threshold::learn_thresholds` — three stub tests green
- [ ] `join::blocked_join` — two stub tests green; lane 3 row matches the reference table
- [ ] prediction table reconciled
- [ ] exercises 3–5
