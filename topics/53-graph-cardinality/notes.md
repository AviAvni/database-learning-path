# Topic 53 notes — cardinality estimation for graph patterns

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin card_bench`. Seeded generator (`gen.rs`,
`Params::lane1(seed, pop)`, seeds 1–5): every number below is a count or a
ratio of counts and reproduces exactly on any machine; **nothing in this
topic is timed**, so there is no load-average table to record. Two runs were
compared byte for byte and are identical.

### Lane 1 — q-error by hop, pooled over seeds 1–5

Every label-consistent walk of 2, 3 and 4 edges (30, 35 and 45 patterns per
mode, summed over five seeds' graphs). q-error as in G-CARE §5.1; `under` is
the share of patterns whose estimate (floored at 1) is below the truth.

| estimator | hop | shared popularity: median / p90 / max q (under) | independent popularity: median / p90 / max q (under) |
|---|---|---|---|
| independence (Kùzu, Neo4j) | 2 | 9.6 / 13.8 / 15.0 (100%) | 1.1 / 1.2 / 1.8 (43%) |
| | 3 | 57.5 / 65.1 / 67.0 (100%) | 1.1 / 1.7 / 2.0 (49%) |
| | 4 | 324.0 / 372.6 / 394.1 (100%) | 1.1 / 1.7 / 1.9 (58%) |
| Memgraph constant `\|L\|·3^k` | 2 | 62.2 / 108.2 / 111.4 (100%) | 6.4 / 11.3 / 20.8 (100%) |
| | 3 | 822.1 / 1561.2 / 1578.9 (100%) | 18.6 / 32.6 / 57.1 (100%) |
| | 4 | 12425.7 / 24405.6 / 25494.1 (100%) | 40.1 / 83.5 / 121.6 (100%) |
| max-degree bound | 2 | 4.4 / 4.9 / 5.6 (0%) | 40.8 / 69.8 / 74.6 (0%) |
| | 3 | 40.8 / 48.7 / 60.0 (0%) | 1741.3 / 3637.1 / 3970.9 (0%) |
| | 4 | 294.4 / 342.9 / 370.0 (0%) | 83367.0 / 119220.1 / 151702.1 (0%) |
| degree-sequence bound | 2 | 1.0 / 1.0 / 1.0 (0%) | 9.7 / 14.8 / 15.3 (0%) |
| ℓ2-norm bound | 2 | 1.3 / 1.4 / 1.4 (0%) | 12.4 / 19.5 / 21.2 (0%) |

- Per hop, the independence median grows ×6.0 (9.6 → 57.5) then ×5.6
  (57.5 → 324.0) on the shared graph, and does not move (1.1, 1.1, 1.1) on
  the independent one. The graphs have the same degree distributions
  (same Zipf exponents and edge counts); the only difference is whether a
  node's rank is shared across types.
- **Prediction that checked out by hand** (README Step 2): the six-node hub
  graph, true 10, independence 4, degree-sequence bound 10, and 6 vs 10
  when the out-degrees are reversed. These are asserted in
  `est.rs::the_worked_example_in_the_pessimistic_guide`.

### Lane 1 — detail, seed 1, shared popularity

`edges per type = [34374, 53343, 80401, 51749, 28132]`, label sizes
`[3000, 6000, 12000, 6000]`.

| pattern | true | independence | q (− under) |
|---|---|---|---|
| r0>r1 | 3,009,074 | 305,602.0 | −9.8 |
| r1>r2 | 4,607,815 | 357,402.5 | −12.9 |
| r1>r4 | 1,779,714 | 125,053.8 | −14.2 |
| r2>r3 | 6,685,881 | 693,445.2 | −9.6 |
| r3>r0 | 4,198,038 | 592,940.0 | −7.1 |
| r4>r0 | 2,659,294 | 322,336.5 | −8.3 |
| r0>r1>r2 | 124,840,475 | 2,047,559.2 | −61.0 |
| r0>r1>r4 | 46,517,831 | 716,433.1 | −64.9 |
| r1>r2>r3 | 176,622,003 | 3,082,537.4 | −57.3 |
| r1>r4>r0 | 90,956,395 | 1,432,866.1 | −63.5 |
| r2>r3>r0 | 266,373,261 | 7,945,495.4 | −33.5 |
| r3>r0>r1 | 187,879,637 | 5,271,533.4 | −35.6 |
| r4>r0>r1 | 112,840,507 | 2,865,732.3 | −39.4 |
| r0>r1>r2>r3 | 6,150,191,151 | 17,659,856.7 | −348.3 |
| r0>r1>r4>r0 | 2,994,139,302 | 8,208,890.1 | −364.7 |
| r1>r2>r3>r0 | 9,091,035,772 | 35,319,713.4 | −257.4 |
| r1>r4>r0>r1 | 4,826,933,048 | 12,738,896.3 | −378.9 |
| r2>r3>r0>r1 | 15,545,357,979 | 70,639,426.7 | −220.1 |
| r3>r0>r1>r2 | 9,916,834,923 | 35,319,713.4 | −280.8 |
| r3>r0>r1>r4 | 3,725,863,926 | 12,358,231.6 | −301.5 |
| r4>r0>r1>r2 | 6,074,678,607 | 19,200,645.0 | −316.4 |
| r4>r0>r1>r4 | 2,284,231,673 | 6,718,231.7 | −340.0 |

### Lane 1 — triangles `r0>r1>r4`

| popularity | seed | true | independence (q) | AGM bound (q) | ℓ2 bound (q) |
|---|---|---|---|---|---|
| shared | 1 | 54,738 | 238.81 (229.2) | 7,182,143 (131.2) | 712,488 (13.0) |
| shared | 2 | 56,124 | 238.86 (235.0) | 7,182,913 (128.0) | 712,012 (12.7) |
| shared | 3 | 54,948 | 238.62 (230.3) | 7,179,254 (130.7) | 702,015 (12.8) |
| independent | 1 | 234 | 238.59 (1.0) | 7,178,751 (30,678.4) | 706,536 (3,019.4) |
| independent | 2 | 234 | 238.19 (1.0) | 7,172,854 (30,653.2) | 708,498 (3,027.8) |
| independent | 3 | 404 | 237.52 (1.7) | 7,162,662 (17,729.4) | 696,263 (1,723.4) |

The independence estimate is ≈ 238 on every graph: it depends only on edge
counts and label sizes. The truth moves by a factor of 234 between modes.

### Lane 1 — plan choice

| popularity | hops | patterns | wrong picks | geomean regret | worst regret | answer share of the best order's cost |
|---|---|---|---|---|---|---|
| shared | 3 | 35 | 5 | 1.00 | 1.00 | 97.7% |
| shared | 4 | 45 | 5 | 1.00 | 1.00 | 98.3% |
| independent | 3 | 35 | 4 | 1.00 | 1.06 | 89.5% |
| independent | 4 | 45 | 4 | 1.00 | 1.03 | 91.0% |

- **Negative result: a 348× underestimate does not change the plan's cost.**
  "Wrong picks" counts patterns where the chosen order is not a truly
  cheapest one; "1.00" is a regret under 1.005. The answer is ≥ 89.5% of
  every pattern's best cost on average, because a path's counts grow with
  length here (the average out-degree exceeds 6.7 for every type), so the
  intermediates are small next to the answer that every order pays. This is
  not a general claim about plans: it is the premise lane 1 does not make the
  estimates fight. Exercise 7 builds the case that does (a selective
  predicate at the far end shrinks the answer below its intermediates).

### Sanity lines (all printed by the bench)

- 2-hop frontier DP equals `Σ_v indeg(v)·outdeg(v)` for every 2-walk: agree.
- 4-hop forward DP and backward DP give the same total for every 4-walk: agree.
- A single edge is estimated exactly (q = 1): holds.
- The max-degree bound is never below the truth: holds.
- Unit tests compare the oracle to explicit enumeration on a 120-node graph
  (paths of 2 and 3 edges forward and backward, triangles).

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 2: median q-error of characteristic sets at 2 hops, shared popularity (independence: 9.6) | | |
| lane 2: number of characteristic sets on seed 1 | | |
| lane 2: does it over- or underestimate at 4 hops? | | |
| lane 3: median q-error of the two-edge catalog at 2 hops | | |
| lane 3: median q-error at 3 and 4 hops | | |
| lane 3: does it over- or underestimate? | | |
| lane 3: number of stored patterns | | |
| lane 3: seed 1, `r0>r1>r2` estimate (true 124,840,475) | | |
| lanes 2–3: wrong plan picks at 4 hops, shared popularity | | |
| exercise 4: colors used by a (label, log₂ in, log₂ out) coloring on seed 1 | | |
| exercise 7: the selectivity `s` at which the independence pick's regret first exceeds 2 | | |

## Reference numbers for the exercise lanes

Read these after filling the worksheet. From reference implementations of
`cset::build` / `estimate_path` and `catalog::build` / `estimate_path`
written to the contracts in the stub doc comments, run against lane 1's
graphs (all 28 tests green, then removed; the solution code is not
committed). Counts and ratios are exact; your implementation must reproduce
them to the printed digit. Apple M3 Pro, measured 2026-10-26.

### Lane 2 — characteristic sets, shared popularity, seeds 1–5

| hop | n | median q | p90 q | max q | under |
|---|---|---|---|---|---|
| 2 | 30 | 8.7 | 10.3 | 10.8 | 100% |
| 3 | 35 | 41.6 | 43.9 | 44.7 | 100% |
| 4 | 45 | 204.3 | 231.6 | 236.9 | 100% |

- Seed 1 has **21 characteristic sets** (label, in-types, out-types).
- Against independence's 9.6 / 57.5 / 324.0 the improvement is 1.1× / 1.4× /
  1.6× — **a small gain**. **Negative result:** a characteristic set records
  *which* edge types a node has, not *how many*, and on this graph the
  skew is in the counts. G-CARE reports the same family underestimating "due
  to the independence assumption" (§6.1.1, §6.2.1); this is the same failure
  reproduced on a graph where the independence that survives conditioning on
  the set is between a node's in- and out-degree. Exercise 8 builds the case
  where the set carries the hub signal.
- Plan choice with this estimator (shared popularity): 3 hops 0 wrong picks of
  35, geomean regret 1.00, worst 1.00; 4 hops 5 wrong of 45, 1.00 and 1.00.

### Lane 3 — two-edge catalog, shared popularity, seeds 1–5

| hop | n | median q | p90 q | max q | under |
|---|---|---|---|---|---|
| 2 | 30 | 1.0 | 1.0 | 1.0 | 0% |
| 3 | 35 | 2.0 | 2.2 | 2.2 | 0% |
| 4 | 45 | 3.2 | 3.5 | 3.6 | 0% |

- Catalog size on seed 1: **11** stored patterns (5 edge counts + 6 two-hop
  counts: `F2[0][1]=3,009,074`, `[1][2]=4,607,815`, `[1][4]=1,779,714`,
  `[2][3]=6,685,881`, `[3][0]=4,198,038`, `[4][0]=2,659,294`).
- Two-edge paths are exact by construction (q = 1.0). The first estimate is at
  three edges and **overestimates**: 0% under at every hop.
- Plan choice with this estimator: 3 hops 0 wrong of 35, 4 hops 0 wrong of
  45; both regrets 1.00.

Seed 1, shared popularity — true count against the three estimators:

| pattern | true | independence | characteristic sets | catalog |
|---|---|---|---|---|
| r0>r1 | 3,009,074 | 305,602.0 | 346,840.3 | 3,009,074.0 |
| r0>r1>r2 | 124,840,475 | 2,047,559.2 | 2,971,857.9 | 259,926,444.2 |
| r0>r1>r2>r3 | 6,150,191,151 | 17,659,856.7 | 26,552,817.3 | 21,614,622,638.0 |
| r1>r4>r0 | 90,956,395 | 1,432,866.1 | 2,112,560.2 | 168,234,848.6 |
| r4>r0>r1>r4 | 2,284,231,673 | 6,718,231.7 | 11,241,692.4 | 7,766,796,555.8 |

- **Why the catalog overestimates (exercise 6, reference).** The truth is
  `Σ_{(b,c)} in(b)·out(c)` over the middle edges and the order-2 estimate is
  `(Σ in)(Σ out)/|E|`. The covariance of `in(b)` and `out(c)` over the
  middle edges, on seed 1, is negative for all seven three-edge patterns
  (between −1,010 and −5,334), and the sign of (estimate − truth) is the
  sign of −covariance for all seven, as Chebyshev's sum inequality says it
  must be. The *reason* it is negative on this generator is not isolated;
  the suspect is the de-duplication of repeated hub–hub edges.

## Paper numbers worth keeping (each checked against the PDF)

- **G-CARE (SIGMOD 2020).** q-error `max(max(1,c)/max(1,ĉ), max(1,ĉ)/max(1,c))`
  (§5.1). Parameters (Table 1): query sizes 3, 6, 9, 12; eight topologies;
  six result-size buckets; five datasets. WanderJoin best on accuracy across
  datasets, topologies, sizes (§6.1–6.2). Successful-sampling rates for
  queries above ten edges: star 27.7%, cycle 16.8%, chain 0.16%, tree 8.6%,
  petal 0.04% (§6.1.3). Impr processes only 3–5-node queries (§6.1.3, §6.2.3).
  §6.5: RDF-3X plans robust for star-shaped Q4 (LUBM) and P1, P2 (DBpedia);
  characteristic sets with good q-error and "very narrow" validity ranges
  give "a significantly worse plan" on P6. The characteristic-sets paper
  (ICDE 2011) was **not obtained**; its estimator is quoted through
  G-CARE §3.2.
- **Pessimistic Cardinality Estimation (arXiv 2412.00642).** Eq. 3
  (density), Eq. 4 (`min(|R|·b, a·|S|)`), Eq. 7–8 (AGM, triangle), Eq. 12
  (chain bound), Eq. 16 (ℓ2 for one join), Eq. 17–20 (ℓp), Eq. 21 (entropic
  constraint), Eq. 22 (DSB `Σ a_i b_i`), Figure 1 (`deg_R(YZ|X) = (3,2,2,1)`),
  Figure 2 (CastInfo: 36·10⁶ rows, 4·10⁶ actors, max degree 10⁴), §10 (ℓp up
  to ℓ30 for JOB "with diminishing returns"; ∧ → min, ∨ → +). DSB is for
  Berge-acyclic queries (§9). PolyB's LP has 2ⁿ variables (§8).
- **LpBound (arXiv 2502.05912).** Abstract: "Postgres derives query plans at
  least as good as those derived using the true cardinalities". §6.2: on
  the cyclic SM queries (11–28 edge relations, DBLP) Postgres and DbX
  estimate 1 ("underestimation by 7–8 orders of magnitude"), DuckDB
  overestimates "by over 12 orders of magnitude"; JOB, STATS, SM (Table 1:
  33–84 relations per SM query). §6.4: statistics 1.6× smaller than
  SafeBound, 1.2–93× smaller than ML-based estimators. HiGHS 1.7.2 (§6.1).
  The LpBound error metric is estimate ÷ truth, not q-error.
- **GOpt (arXiv 2401.17786).** §4: GLogue = frequencies of motifs up to `k ≥ 3`
  vertices. §6.3.1: Eq. 1 `F(P_t) = F(P_s1)·F(P_s2)/F(P_s1∩P_s2)`; Eq. 2
  expand ratio; Example 6.2: `70·1.0·0.2 = 14`. Filters use predefined
  selectivities, e.g. 0.1 (Remark 7.1). §8.2: high-order beats low-order on 3
  of 8 CBO queries, 13.1× average over those, QC4a 31.9×; type inference 51×
  average, QT2 209×; CBO "averaging 14.4× faster than GOpt-Neo-Plan".
- **Color (PVLDB 2024, arXiv 2405.06767v2).** Def. 4 (traditional), Def. 6
  (lifted graph), Def. 8 (estimator), Theorem 1 (stable ⇒ exact),
  Corollary 1 (q-error ≤ ε^{|V_Q|−1}), Def. 9 (γ), §8 claims (median error
  < 10; 80–800× less space; 10–100× faster construction; 3× worse median
  after half the graph is updated). 32 colors typical (§1). **Discrepancies:**
  Example 4's text says the average degree is "≈ 4" and the traditional
  estimate `10100·4^k`; the stated graphs give `29,900/10,100 = 2.96`; the
  guide uses 2.96. §1 says the average degree range drops "1–2 orders of
  magnitude" and §3 says "over 2 orders" for the same figure.
- **Engines (code, pinned).** Kùzu `PlannerKnobs` 0.01 / 0.1
  (`constants.h:159-160`); Memgraph `kExpand = 3.0`, `kFilter = 0.25`
  (`cost_constants.hpp:47,49`); Neo4j `estimateCountsForRelationships` per
  label triple (`TransactionBoundGraphStatistics.scala:149`); FalkorDB: no
  estimate, node counts per label only.

## Cross-topic threads

- Topic 10's Step 3 (q-error, compounding) is the relational version of the
  by-hop table; its Step 2 independence assumption is the one the
  popularity knob breaks.
- Topic 13's open question (notes line 122; reading-ldbc-snb.md line 421)
  is answered by the by-hop table: each hop, by the degree correlation's
  factor, not at a particular hop.
- Topic 47's semi-join reduction is the robust alternative: it makes the join
  order matter less; this topic's plan table says when it mattered at all.
- Topic 13's WCOJ guide: the AGM bound row for the triangle.

## Open questions

- Does the catalog's overestimate shrink or grow when popularity is partly
  shared (a mixture of the two modes)? Lane 3 was run on shared popularity
  only.
- Where does the answer stop dominating plan cost? Exercise 7 asks for the
  predicate selectivity; a derivation from the degree sequences would say
  it in advance.
- The Rust rewrite's `select_scan_node.rs` tie-breaks by label node count
  only. Which of lane 1's orders would it pick on a graph with hubs, and what
  would regret be?
- GLogue's frequencies for union types (GOpt Step 3) were not implemented.

## Implementation log

- [ ] `cset::build`, `estimate_two_path`, `estimate_path`, `num_sets` — 6 tests green; lane 2 matches the table
- [ ] `catalog::build`, `estimate_path`, `size` — 6 tests green; lane 3 matches the table
- [ ] prediction table reconciled
- [ ] exercises 2–9
