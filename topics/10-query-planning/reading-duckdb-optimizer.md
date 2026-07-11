# Reading guide — DuckDB `src/optimizer/`: the readable optimizer (~2 h)

Local clone: [`~/repos/duckdb`](https://github.com/duckdb/duckdb). Start at `optimizer.cpp`, then
`filter_pushdown.cpp`, then the `join_order/` subdirectory — the payoff.

## 1. The pass pipeline (optimizer.cpp)

`Optimizer::Optimize` runs ~25 sequential passes; every one is wrapped in
`RunOptimizer` (:119) which profiles it and `Verify`s (:134–139) column
bindings afterward — rewrites are checked for well-formedness after every
pass, in production. The order tells a story (read :197–367 top to
bottom):

```
 expression rewriter → cte inlining → FILTER PULLUP → FILTER PUSHDOWN →
 in-clause → deliminator (decorrelation cleanup) → …
 → JOIN_ORDER (:285) → … → unused columns → common subexpressions →
 build/probe side (:334) → limit pushdown → TOP_N (:367)
```

- Pullup BEFORE pushdown (:212 then :218) looks backwards — it hoists
  filters through outer-join simplifications so pushdown can then sink
  them FURTHER. Order-dependent heuristics, not a fixpoint engine
  (contrast DataFusion's max_passes loop, Cascades' memo).
- Join order runs mid-pipeline, on a plan already scrubbed of noise.

## 2. Filter pushdown (filter_pushdown.cpp)

`Rewrite` (:106) dispatches on operator type → per-operator pushdown
(`PushdownFilter` :112); non-pushable operators get a fresh child
`FilterPushdown` (:130–137) — filters accumulate in a bag and sink until
something blocks them. Look at `pushdown/` for the per-operator rules
(pushdown_left_join etc. — outer joins are where correctness bites:
a filter on the NULL-padded side cannot sink).

## 3. Join ordering (join_order/ — the core read)

- `query_graph_manager.cpp` / `relation_manager.cpp` — extract relations
  + edges (predicates) from the plan: the QUERY GRAPH.
- `plan_enumerator.cpp`:
  - `SolveJoinOrderExactly` :375 — DPccp-style dynamic programming:
    enumerate connected subgraphs, `EnumerateCmpRecursive` :295,
    `TryEmitPair` :227 / `EmitPair` :185 keep the best plan per
    relation-SET (the memo).
  - The escape hatch at :234: "when the amount of pairs gets too large we
    exit the dynamic programming and resort to a greedy algorithm" —
    `SolveJoinOrderApproximately` :398 (greedily join the cheapest pair;
    smallest-intermediate-result-first). `SolveJoinOrder` :532 picks.
- `cardinality_estimator.cpp`: `EstimateCardinalityWithSet` :897 —
  cardinality = product of base cardinalities × per-predicate
  selectivities, with **total denominators** from matching equivalence
  sets; unknown predicates get `DEFAULT_SELECTIVITY` (:917 — DuckDB's
  0.005 moment). No histograms here: distinct-count-based, plus base
  stats from `relation_statistics_helper.cpp`.
- `cost_model.cpp`: `ComputeCost` :40 — cost = estimated cardinality of
  the join output + children costs. That's it. Cardinality IS the cost
  model (which is why VLDB'15's result stings).

## Questions for notes.md

1. Why does pullup-then-pushdown beat pushdown alone? Find one operator
   in `pullup/` where hoisting first enables a deeper sink.
2. The DP keeps one best plan per relation set. What plan property does
   that discard that Selinger kept (hint: interesting orders) — and why
   does DuckDB get away with it (what physical op dominates)?
3. Exact→greedy threshold: what workload shape triggers it — star schema
   (one fact, k dims) or chain? Count connected subgraphs for both at
   n=10.
4. Cost = output cardinality only: no distinction between hash-join
   build sides at this stage (that's the later BUILD_SIDE_PROBE_SIDE
   pass :334). What does splitting order-choice from side-choice lose?
5. M10: a Cypher chain `(a)-[:R]->(b)-[:S]->(c)` is a chain query graph
   over edge relations. Which DuckDB piece maps to anchor-node selection
   — the enumerator or the cardinality estimator?

## Done when

You can list the pass order from memory (coarse buckets), and explain
DPccp + the greedy fallback + the cardinality formula in three sentences.
