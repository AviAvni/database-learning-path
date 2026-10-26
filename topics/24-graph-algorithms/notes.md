# Topic 24 notes — advanced graph algorithms & analytics

## Baseline (provided code, Apple M3 Pro, measured 2026-07-10)

Graphs: RMAT scale 16 (n=65,536, m=1,819,338 directed after
symmetrize+dedup, max deg 9,751) vs uniform (same n, m=2,096,564,
max deg 59). Build 258 ms. The generators are seeded, so every count below (edges, degrees, triangles, iterations) reproduces exactly; the millisecond figures are this run's and move by tens of percent between runs on the same machine — cite one run and say which (`./verify.sh 24` gives your machine's).

| lane | rmat | uniform |
|---|---|---|
| PageRank (pull, ε=1e-4) | 8 iters, 10.8 ms, 1.35 GTEPS-ish | 6 iters, 7.9 ms, 1.59 |
| Triangle count (degree-ordered) | 15,645,988 in 375.8 ms | 5,428 in 158.0 ms |
| Dijkstra ×3 sources | 33.7 ms, 342,909 pops | |
| CC union-find | 18,844 components, 4.2 ms, all m inspected | |

- TC: same n, comparable m, **2,882× more triangles** on RMAT — hub
  neighborhoods intersect; uniform graphs have nothing to count.
  Per-triangle cost is what the skew hides: rmat does 24 ns/triangle
  only because intersections are fat; uniform pays 29 µs/triangle.
- Dijkstra pops = 1.74×n per source — lazy deletion's stale-entry
  tax on a skewed graph.
- PR converges FASTER on uniform (6 vs 8 iters): hubs concentrate
  rank and slow the L1 error's decay.
- 18,844 components at avg_deg 16: RMAT's leaf quadrant (d=0.05)
  strands vertices; real twitter-shaped data does the same — CC
  benchmarks that assume one component are lying.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| delta_stepping relaxations at Δ=16 vs Dijkstra's 343K pops (per source ~114K) | | |
| Δ=2^40 (pure Bellman-Ford): relaxations ×? over Δ=128 | | |
| best-wall-clock Δ for weights 1..=255 on this RMAT | | |
| afforest edges_inspected as % of m (test bound: <50%) | | |
| brandes 8 sources on scale-13 RMAT — ms (8 BFS + 8 backprops over 460K edges) | | |
| brandes full-source n=128 vs bc_brute O(n³) — which is faster and ×? | | |

## Implementation log

- [ ] sssp.rs delta_stepping — matches Dijkstra 3 configs, extremes test
- [ ] bc.rs brandes — matches O(n³) brute on n=128, sampled lane runs
- [ ] cc.rs afforest — partition matches union-find, <50% edges inspected
- [ ] prediction table reconciled
- [ ] stretch: Δ sweep plot (relaxations + buckets vs Δ), find the knee
- [ ] stretch: label-propagation CC (Ligra Components.C style) as a
      third lane — compare edges touched vs afforest on 1-component
      vs 18K-component graphs
- [ ] stretch: Louvain phase-1 local moves with modularity trace;
      property test from reading-louvain-leiden.md Q5 (community
      connectivity check)

Surprises / dead ends:

- RMAT top-1% edge share at scale 12 is 19.1% — under the 20% I
  first asserted in the skew test (share grows with scale: 36.6% at
  16). Skew assertions need scale-aware bounds; loosened to 14% +
  hub-degree check.
- 18,844 components surprised me at avg_deg 16 (uniform G(n,m) at
  that degree would be 1 giant + few strays; RMAT's 0.05 quadrant
  starves the low-id... actually high-id leaves). Afforest's
  "skip the giant component" trick still applies — 71% of vertices
  are in it.

## C FalkorDB on the same graphs

The same two graphs (RMAT scale 16 and uniform, seeds 42/43, n = 65 536, m =
1 819 338 and 2 096 564 directed edges after symmetrise+dedup) dumped from
`graph.rs` and loaded into C FalkorDB with `falkordb-bulk-insert` 1.0.6
(`:N` nodes with an `id` property, `:E` edges; the bulk-loaded edge counts
match). Engine: `~/repos/FalkorDB/bin/macos-arm64v8-release/falkordb.so`,
built 2026-07-24, `MODULE LIST` `ver 999999` (a dev build that cannot be
traced to a commit; the repo's `main` was `25babfe4f` at run time),
redis-server 8.6.2, port 6391, Python client `FalkorDB` 1.8.1, through the
bench lock, measured 2026-10-26 on the Apple M3 Pro. The Rust column is the
provided `algo_bench` run in the same session (it differs a little from the
2026-07-10 baseline above).

| kernel | Rust (provided lane) | C FalkorDB, server-side | what FalkorDB ran |
|---|---|---|---|
| PageRank, rmat | 11.1 ms (8 iters) | 16.7 ms median of 5 | `CALL algo.pageRank('N','E') YIELD node, score RETURN count(*)` (65 536 rows) |
| PageRank, uniform | 8.1 ms (6 iters) | 15.3 ms | same |
| components, rmat | 4.2 ms (union-find) | 18.5 ms | `CALL algo.WCC() YIELD node, componentId RETURN count(DISTINCT componentId)` → **18 844**, equal to the Rust count |
| components, uniform | — | 16.7 ms | → 1 |
| triangles, rmat | 383.8 ms | **31 097 ms** (1 run) | `MATCH (a)-[:E]->(b)-[:E]->(c)-[:E]->(a) WHERE id(a)<id(b) AND id(b)<id(c) RETURN count(*)` → **15 645 988**, equal |
| triangles, uniform | 142.1 ms | **3 015 ms** median of 3 | same → **5 428**, equal |

Reading it:

- Triangle counts agree exactly with the Rust oracle on both graphs, so the
  comparison is of the same computation. The Cypher query is a three-hop
  pattern with a closing edge and an id-ordering filter, not a masked SpGEMM:
  FalkorDB has no triangle-count procedure to call here, so this measures what
  a user would write, **81×** (rmat) and **21×** (uniform) slower than the
  degree-ordered intersection. It does reproduce the skew story in a second
  engine: 2 882× more triangles but only **10.3×** more time (31 097 / 3 015
  ms) in FalkorDB, against **2.7×** (383.8 / 142.1) in Rust.
- PageRank and WCC are one procedure call each, 1.5× (rmat PageRank) to 4.4×
  (WCC) over the scalar Rust code. Each FalkorDB call also builds the result
  rows (65 536), the iteration count and convergence tolerance of
  `algo.pageRank` were not inspected, and the Rust PageRank counts 8 iterations
  at ε = 1e-4 — so these are procedure latencies, not a controlled kernel
  comparison. Do not quote them as an engine-versus-kernel gap.
- Single-process, closed loop, one run for the rmat triangle query (31 s).

## Questions from the reading guides

### GAP (reading-gap.md)

1. Road-vs-twitter kernel ranking flips; diameter vs degree variance:
2. When redundant-relaxation (sssp.cc:44) loses:
3. BC source sampling bias on 18K-component RMAT; stratification:
4. pr.cc vs pr_spmv.cc on kron — gather cost:
5. Why no community-detection kernel in GAP:

### Delta-stepping (reading-delta-stepping.md)

1. Relaxations-vs-Δ curve prediction (table above):
2. Δ=1 integer weights = Dial's algorithm; why O(1) beats heap:
3. Benign races + min = idempotent monoid; the GraphBLAS name:
4. vxm count vs max_dist/Δ; where algebra pays:
5. CALL algo.sssp over M20: semiring, bucket vector, Δ in API:

### Brandes (reading-brandes.md)

1. Recurrence derivation; where the +1 comes from:
2. Brute's O(n²) memory vs oracle-fitness:
3. succ bitmap vs depth recheck — memory touches per edge:
4. Batch size ns limits in LAGr_Betweenness; sweet spot at n=65K:
5. BC under unflushed deltas — flush vs stale-main:

### Ligra (reading-ligra.md)

1. Frontier where m/20 threshold picks wrong:
2. edgeMapDenseForward vs edgeMapDense (early exit value):
3. BC.C constructs ↔ LAGr_Betweenness ops; who batches:
4. Label-prop vs afforest edges touched, 1 vs 18K components:
5. Callback API vs fixed menu for M24; safe-embedding costs:

### Louvain→Leiden (reading-louvain-leiden.md)

1. 5-vertex disconnection example:
2. Resolution limit on fraud rings; γ vs CPM:
3. Greedy-deterministic refinement — what breaks:
4. Leiden iteration on M20 core: SpGEMM vs SPA steps:
5. Connectivity property test for algo.community:

### LAGraph algos (reading-lagraph-algos.md)

1. min_2nd semiring rationale; MIN_TIMES failure on weights:
2. FastSV rounds vs Afforest rounds; why Afforest wins wall-clock:
3. Sandia_LUT urand exception ↔ dot3-vs-saxpy3:
4. Dangling-vertex error of our pull PR on 18K components:
5. algo.wcc under pending deltas — three options, semantics:

## Cross-topic threads

- Direction switching (Ligra m/20, Beamer α/β, SuiteSparse dot-vs-
  saxpy) = one decision, three communities — topic 20's BFS stub
  already implements it; Ligra shows it generalizes past BFS.
- Afforest/FastSV sampling = "do less work than reading the input" —
  same instinct as block-max WAND (topic 23): metadata/bounds prove
  most of the input irrelevant.
- Brandes' restructured sum = IVM thinking (topic 27 preview):
  δ_s(v) is an incrementally-maintainable aggregate over the DAG.
- Louvain's irreversible-aggregation bug = topic 21's rule-ordering
  trap: greedy + destructive = stuck; Leiden's refinement = egg's
  keep-both-forms.
- Modularity ΔQ accumulator = topic 20's SPA; aggregation = S·A·Sᵀ
  SpGEMM; TC's six formulations = semiring/mask algebra as a query
  planner (pick the formulation like topic 10 picks join orders).
- GAP's 5-graph matrix = topic 22's "change any one ⇒ different
  number" — graph SHAPE is the workload axis benchmarks forget.
- proc_pagerank.c's flush-then-run = topic 20's delta-matrix wait:
  analytics force synchronization; M24 must decide the semantics.

## M24 log (capstone)

- [ ] algo crate over M20 core: PR (pull SpMV), CC (FastSV +
      Afforest, race them), BC (batched-matrix Brandes), SSSP
      (MIN_PLUS delta-stepping), TC (masked SpGEMM, method picker)
- [ ] procedure surface `CALL algo.*` copying FalkorDB's
      proc_pagerank.c arg/yield shape
- [ ] snapshot semantics: procedures run post-wait (documented), or
      masked-over-deltas (measured first)
- [ ] GAP lanes into M22's standing suite

## Done when

- Three stubs green with lanes filled; prediction table reconciled;
  guide questions answered; the frontier-vs-algebra choice per
  algorithm written down with our own numbers backing it.
