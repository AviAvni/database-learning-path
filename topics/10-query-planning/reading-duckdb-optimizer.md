# The readable optimizer: DuckDB's pass pipeline and join-order DP

DuckDB's `src/optimizer/` is the clearest production optimizer you can
read: ~25 ordered rewrite passes, each verified after it runs, feeding a
DPccp join enumerator with a greedy escape hatch and a cost model that is
just cardinality. Before you open the code, this chapter builds the seven
concepts an optimizer is made of — plan trees, rewrites, pushdown, the
query graph, the join-order DP, cardinality estimation, and the cost
model — one at a time, then hands you the file and line anchors to watch
each one run.

## The problem in one sentence

For a query joining n tables there are Catalan-many tree shapes times n!
orderings — a 20-way join has ~10¹⁸ possible plans — and the best and
worst of them differ by 100×–1000× in runtime, so the optimizer must find
a near-best one in single-digit milliseconds.

## The concepts, step by step

### Step 1 — the plan tree: logical says WHAT, physical says HOW

A query is compiled into a **plan** — a tree of operators where data
flows from the leaves (table scans) up to the root (the result). The
distinction everything hangs on:

- a **logical plan** describes *what* to compute — pure algebra:
  `Join(A, B, a.x = b.y)` names no algorithm;
- a **physical plan** describes *how* — `HashJoin(build=B, probe=A)`
  picks an algorithm and a side.

```
        logical (WHAT)                    physical (HOW)
        ──────────────                    ──────────────
          Project                           Project
             │                                 │
           Join(a.x=b.y)        ──►         HashJoin(build=B, probe=A)
           /    \                            /       \
       Scan(A)  Scan(B)                 SeqScan(A)  SeqScan(B)
```

One logical plan maps to *many* physical plans, and they are not close in
cost. Optimization is therefore a search problem in two phases: first
transform the logical plan with rewrites that are always safe, then pick
among alternatives with a cost model. Everything below is one of those
two phases.

### Step 2 — rewrite passes: transformations that never need a cost model

A **rewrite pass** is a whole-plan transformation that is always at least
as good as the input — no cost estimate needed, so you just run them in
sequence. The classic menu: **predicate pushdown** (move a filter as
close to the scan as possible — a 1%-selective filter applied *before* a
join shrinks every downstream operator's input 100×), unused-column
elimination, constant folding (`1+1` becomes `2` at plan time), and
turning cross products plus filters into real joins.

DuckDB runs ~25 such passes in one fixed, hand-tuned order, and — in
production — re-verifies the plan's well-formedness after every single
pass. The order itself tells a story:

```
 expression rewriter → cte inlining → FILTER PULLUP → FILTER PUSHDOWN →
 in-clause → deliminator (decorrelation cleanup) → …
 → JOIN_ORDER (:285) → … → unused columns → common subexpressions →
 build/probe side (:334) → limit pushdown → TOP_N (:367)
```

Two things to notice. Pullup runs BEFORE pushdown — it looks backwards,
but hoisting filters through outer-join simplifications first lets
pushdown then sink them *further* than either pass alone could. And join
ordering runs mid-pipeline, on a plan already scrubbed of noise. This is
an order-dependent heuristic pipeline, not a fixpoint engine — contrast
DataFusion's run-until-nothing-changes loop and Cascades' memo (both in
this topic's other guides).

### Step 3 — filter pushdown mechanics: a bag of filters sinking until blocked

Pushdown is not "move one filter one step". DuckDB's implementation
carries a *bag* of accumulated filter expressions down the tree: at each
operator it asks "can these filters pass through you?", pushes the ones
that can, and deposits the rest as a Filter node right above the blocker.
Per-operator rules decide passability, and **outer joins** are where
correctness bites: a filter on the NULL-padded side of a left join cannot
sink below the join, because rows it would remove are *created* by the
join (as NULL padding), not present in the input.

Cost of getting this wrong: a filter evaluated one operator too late
means every intermediate row between the two positions was materialized,
hashed, or copied for nothing — this is the single highest-leverage
rewrite in the pipeline.

### Step 4 — the query graph: joins become a graph problem

Before ordering joins, DuckDB extracts from the plan a **query graph**:
one node per base relation (table being joined), one edge per join
predicate connecting two of them.

```
   chain:  A ─ B ─ C ─ D          star:   B   C
   (a.x=b.x, b.y=c.y, ...)                 \ /
                                        A ─ F ─ D     F = fact table,
                                           /|          every dim joins F
                                          E ...
```

The graph's *shape* controls how hard ordering is: join orders worth
considering correspond to **connected subgraphs** (subsets of relations
linked by predicates — joining unconnected sets means a cross product,
almost always a disaster). A chain of 10 relations has few connected
subgraphs; a star or clique has exponentially many. Hold that — it
decides when Step 5's exact algorithm gives up.

### Step 5 — join ordering by dynamic programming, with a greedy escape hatch

The search uses **dynamic programming** (DP: solve big problems by
combining stored solutions of subproblems) over relation *sets*: the best
plan for a set S of relations must be built from the best plans of two
connected subsets that partition S. So enumerate connected subgraphs
small-to-large, and for each set keep exactly one entry — the cheapest
plan found — in a **memo** (a table keyed by relation set). This is the
DPccp algorithm ("DP over connected complement pairs"), and unlike
Selinger's original left-deep-only search it considers **bushy** trees
(joins whose both inputs are themselves joins) — which graph-pattern
queries especially want.

DP is exact but its work is proportional to the number of connected
subgraph pairs — fine for chains, explosive for cliques. DuckDB's own
comment at `plan_enumerator.cpp:234`: "when the amount of pairs gets too
large we exit the dynamic programming and resort to a greedy algorithm" —
repeatedly join the pair with the smallest estimated intermediate result.
Greedy is O(n² log n)-ish and can be badly wrong, but a mediocre plan
beats an optimizer that runs longer than the query.

### Step 6 — cardinality estimation: the numbers the whole search runs on

A **cardinality estimate** is the planner's guess at how many rows an
operator will produce — every cost the DP compares is built from these
guesses. DuckDB's estimator is deliberately simple: start from base-table
row counts, and for each join predicate divide by the **distinct count**
(NDV — the number of different values in the column), i.e. assume every
value is equally frequent (**uniformity**). Columns linked by equality
predicates are grouped into equivalence sets so the same denominator
isn't applied twice. No histograms in the join-order path. And when a
predicate is something the estimator can't reason about, it falls back to
a constant `DEFAULT_SELECTIVITY` guess — DuckDB's version of postgres's
famous 0.005 (a **selectivity** is the fraction of input rows a predicate
keeps).

Why it matters: these estimates are the *only* signal ranking 10¹⁸
candidate plans, and on correlated real data they can be off by 10⁴–10⁶
(see reading-how-good-optimizers.md — errors multiply up the tree).

### Step 7 — the cost model is just cardinality (Cout)

With estimates in hand, the **cost model** ranks plans. DuckDB's is one
line: the cost of a join is its estimated *output* cardinality plus its
children's costs — i.e. the sum of all intermediate result sizes, known
in the literature as **Cout**. No CPU weights, no IO constants. Steps 6
and 7 in one function:

```rust
fn cost(plan: &Node) -> f64 {
    match plan {
        Scan(t) => t.estimated_rows,
        Join(l, r, preds) => {
            let mut card = rows(l) * rows(r);
            for p in preds {
                card /= distinct_count(p) as f64;  // total denominators from
            }                                      // matching equivalence sets
            card + cost(l) + cost(r)  // output size + children:
        }                             // cardinality IS the cost model
    }
}
```

This is defensible — VLDB'15 showed that with *true* cardinalities even a
trivial Cout model picks plans within ~2× of optimal — and damning:
cardinality IS the cost model, so cardinality error is plan error,
one-for-one. Note also what it ignores: it ranks join *orders* only;
hash-join build-vs-probe side is chosen by a separate later pass
(Step 2's pipeline, the `build/probe side` entry).

## Where each step lives in the code

Read in this order: `optimizer.cpp`, then `filter_pushdown.cpp`, then
the `join_order/` subdirectory — the payoff.

- **Step 2 — the pipeline** (`optimizer.cpp`): `Optimizer::Optimize`
  runs the ~25 passes; every one is wrapped in `RunOptimizer` (:119)
  which profiles it and `Verify`s (:134–139) column bindings afterward.
  Read :197–367 top to bottom for the order — pullup at :212, pushdown
  at :218, JOIN_ORDER at :285, build/probe side at :334, TOP_N at :367.
- **Step 3 — pushdown** (`filter_pushdown.cpp`): `Rewrite` (:106)
  dispatches on operator type → per-operator pushdown (`PushdownFilter`
  :112); non-pushable operators get a fresh child `FilterPushdown`
  (:130–137) — the bag of filters sinking until something blocks.
  Look at `pushdown/` for the per-operator rules (pushdown_left_join
  etc. — the outer-join correctness cases).
- **Step 4 — the query graph** (`join_order/`):
  `query_graph_manager.cpp` / `relation_manager.cpp` extract relations
  + edges (predicates) from the plan.
- **Step 5 — the DP** (`join_order/plan_enumerator.cpp`):
  `SolveJoinOrderExactly` :375 — DPccp-style dynamic programming:
  enumerate connected subgraphs, `EnumerateCmpRecursive` :295,
  `TryEmitPair` :227 / `EmitPair` :185 keep the best plan per
  relation-SET (the memo). The escape hatch comment at :234;
  `SolveJoinOrderApproximately` :398 is the greedy
  (smallest-intermediate-result-first); `SolveJoinOrder` :532 picks
  between them.
- **Step 6 — estimation** (`join_order/cardinality_estimator.cpp`):
  `EstimateCardinalityWithSet` :897 — product of base cardinalities ×
  per-predicate selectivities, with **total denominators** from matching
  equivalence sets; unknown predicates get `DEFAULT_SELECTIVITY` (:917 —
  DuckDB's 0.005 moment). Base stats from
  `relation_statistics_helper.cpp`.
- **Step 7 — the cost model** (`join_order/cost_model.cpp`):
  `ComputeCost` :40 — cost = estimated cardinality of the join output +
  children costs. That's it.

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

## References

**Code**
- [duckdb](https://github.com/duckdb/duckdb) — `src/optimizer/`:
  `optimizer.cpp` (the pass pipeline, read :197–367 top to bottom),
  `filter_pushdown.cpp` + `pushdown/`, and `join_order/`
  (`plan_enumerator.cpp`, `cardinality_estimator.cpp`, `cost_model.cpp`);
  ~2 h
