# The readable optimizer: DuckDB's pass pipeline and join-order DP

DuckDB's `src/optimizer/` is the clearest production optimizer you can
read: a fixed, hand-ordered list of rewrite passes, each re-verified after
it runs, feeding a connected-subgraph join enumerator with two escape
hatches, and a cost model that is essentially cardinality. Before you open
the code, this chapter builds the seven concepts an optimizer is made of —
plan trees, rewrites, pushdown, the query graph, the join-order DP,
cardinality estimation, and the cost model — one at a time, works the
combinatorics on real numbers, then hands you the file and line anchors to
watch each one run.

Every anchor below is DuckDB at the commit this repo pins, **`6c0c1a68`**
(`tools/pinned-source.py ref duckdb`), quoted with the line numbers the code
occupies in that revision. This topic has no measured lane — its only binary
runs *your* planner — so every number here comes from the pinned source, from
a paper section named on the spot, or from arithmetic performed in the guide
on stated assumptions.

## The problem in one sentence

For a query joining n tables the number of candidate plans grows faster than
exponentially — 3.6 million left-deep orders at n = 10, 1.3 *trillion* at
n = 15 (Step 5 does the factorials) — and the gap between the best and worst
of them is not academic: on the Join Order Benchmark the *average* ratio
between the worst and the best plan of a query was **101× with no indexes,
115× with primary-key indexes, and 48,120× with foreign-key indexes**
(Leis et al., VLDB 2015, §6.1) — so the optimizer must find a near-best plan
in single-digit milliseconds.

## The concepts, step by step

### Step 1 — the plan tree: logical says WHAT, physical says HOW

> **In:** nothing yet — this step fixes the two words every later step is
> phrased in.
> **Out:** the logical/physical split, and the statement that optimization is
> a search problem in two phases — which Steps 2-3 and Steps 4-7 then fill in
> separately.

A query is compiled into a **plan** — a tree of operators where data flows
from the leaves (table scans) up to the root (the result). **Relational
algebra** is the small set of operators that tree is built from: scan
(σ-free base access), *selection* σ (keep rows matching a predicate),
*projection* π (keep columns), *join* ⋈ (pair rows from two inputs matching
a predicate), aggregate, sort. Every SQL query is a tree of these, and every
rewrite in this chapter is an algebraic identity — a rule saying two
different trees compute the same relation.

The distinction everything hangs on:

- a **logical plan** describes *what* to compute — pure algebra:
  `Join(A, B, a.x = b.y)` names no algorithm, only the relation it denotes;
- a **physical plan** describes *how* — `HashJoin(build=B, probe=A)` picks an
  algorithm, an order of inputs, and therefore a cost.

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
cost — the 48,120× of the problem statement is exactly this spread.
Optimization is therefore a search problem in two phases: first transform the
logical plan with rewrites that are always safe (Steps 2-3), then pick among
alternatives with a cost model (Steps 4-7). Everything below is one of those
two phases.

Why it matters: the two phases have completely different failure modes. A
rewrite that is wrong is a *correctness* bug; a cost model that is wrong is a
*performance* bug that ships silently. This topic's whole point is that the
second kind is the one that costs 100×.

### Step 2 — the pass pipeline: transformations that never need a cost model

> **In:** the logical plan from the binder, in the shape Step 1 described.
> **Out:** the same plan, algebraically simplified and with filters as low as
> they will go — the input Step 4 turns into a query graph. The pipeline's
> *order* is the output that matters, because Step 3 depends on where in it
> pushdown sits.

A **rewrite pass** is a whole-plan transformation believed to be always at
least as good as its input, so no cost estimate is needed and you can just run
it. The classic menu: **predicate pushdown** (move a filter as close to the
scan as possible), unused-column elimination, constant folding (`1+1` becomes
`2` at plan time), and turning a cross product plus a filter that mentions both
sides into a real join.

DuckDB runs these in one fixed, hand-tuned order and — in production —
re-verifies the plan's column bindings after every single one. The wrapper is
seven lines and is where the discipline lives:

```cpp
// src/optimizer/optimizer.cpp — RunOptimizer, 119-140
   119  void Optimizer::RunOptimizer(OptimizerType type, const std::function<void()> &callback) {
   // ... 120-127: bail on interrupt; skip if this optimizer is disabled ...
   128  	auto &profiler = QueryProfiler::Get(context);
   129  	{
   130  		auto optimizer_timer = profiler.StartTimerInternal("optimizer." + StringUtil::Lower(EnumUtil::ToString(type)));
   131  		callback();
   132  	}
   133  	if (plan) {
   134  		Verify(*plan);
   135  	}
   136  }
   137
   138  void Optimizer::Verify(LogicalOperator &op) {
   139  	ColumnBindingResolver::Verify(context, op);
   140  }
```

Line 134 is the one to look at: *every* pass is followed by a full
`ColumnBindingResolver::Verify` of the resulting plan. Line 130 is the second
one — every pass is separately timed, which is why `EXPLAIN ANALYZE` can tell
you which optimizer cost you the millisecond.

The list itself lives in `Optimizer::RunBuiltInOptimizers` (:178), reached from
`Optimizer::Optimize` (:441) at :458. **Count them before you trust any summary
of this file: there are 39 `RunOptimizer(OptimizerType::…)` calls between :197
and :435, covering 37 distinct optimizer types** — `CTE_INLINING` and
`COLUMN_LIFETIME` each run twice, at different points. (An older version of this
chapter said "~25 passes"; the pinned tree says 39.) The order tells a story:

```
 :197 expression rewriter → :200 cte inlining → :212 FILTER PULLUP →
 :218 FILTER PUSHDOWN → :236 in-clause → :242 deliminator (decorrelation) →
 :272 projection pullup → :278 outer-join simplification →
 :285 JOIN_ORDER → … → :309 unused columns → :321 common subexpressions →
 :334 build/probe side → :350 limit pushdown → :367 TOP_N → …
 → :411 reorder filter → :423 join filter pushdown → :435 type pushdown
```

Three things to notice. **Pullup runs before pushdown** (:212 before :218) — it
looks backwards, but hoisting a filter out of a subtree first lets pushdown then
sink it into *both* branches of a join it could not previously cross. **Join
ordering runs mid-pipeline** (:285), on a plan already scrubbed of noise; the
comment above it (:283-284) notes that the join-order pass "also rewrites cross
products + filters into joins and performs filter pushdowns", so the pipeline's
two halves are not perfectly separated. And **build/probe side selection is a
separate, later pass** (:334) — Step 7 comes back to what that costs.

This is an order-dependent heuristic pipeline, not a fixpoint engine — contrast
DataFusion's run-until-nothing-changes loop and Cascades' memo (this topic's
other guides).

Why it matters: a fixed order is a fixed set of bugs. If pass B only fires on
output pass A produces, and B runs first, the rewrite is simply never found —
and nothing in the system reports that.

### Step 3 — filter pushdown mechanics: a bag of filters sinking until blocked

> **In:** the plan as it stands at :218, the FILTER_PUSHDOWN slot in Step 2's
> list.
> **Out:** the same plan with every filter as deep as it can legally go — and,
> where a filter proves a NULL-padded row impossible, with an outer join
> rewritten to an inner one. That plan is what Step 4 reads relations out of.

Pushdown is not "move one filter one step". DuckDB's implementation carries a
*bag* of accumulated filter expressions down the tree: at each operator it asks
"can these pass through you?", pushes the ones that can, and deposits the rest
as a `Filter` node right above the blocker. `FilterPushdown::Rewrite` (:106) is
a bare `switch` on operator type, one case per rule, and the default case is the
honest one:

```cpp
// src/optimizer/filter_pushdown.cpp — the default arm of Rewrite, 148-151,
// and the function it calls, 339-347
   148  	default:
   149  		return FinishPushdown(std::move(op));
   150  	}
   151  }
   // ... 153-338: PushdownJoin, PushdownProjection, PushdownGet, … one per operator ...
   339  unique_ptr<LogicalOperator> FilterPushdown::FinishPushdown(unique_ptr<LogicalOperator> op) {
   340  	// unhandled type, first perform filter pushdown in its children
   341  	for (auto &child : op->children) {
   342  		FilterPushdown pushdown(optimizer, convert_mark_joins);
   343  		child = pushdown.Rewrite(std::move(child));
   344  	}
   345  	// now push any existing filters
   346  	return PushFinalFilters(std::move(op));
   347  }
```

Line 342 is the argument: an operator this pass does not understand gets a
*fresh, empty* `FilterPushdown` for each child, so the current bag of filters
cannot cross it, and :346 deposits that bag above the blocker. Unknown operator
⇒ nothing sinks through. That is the safe default, and it is why adding an
operator to DuckDB cannot silently break pushdown correctness.

**Outer joins** are where the interesting case lives. A **left outer join**
emits every left row, padding the right columns with NULL when nothing matched
— so a filter on the right-hand columns cannot simply sink below the join: the
rows it would remove are *created by* the join, not present in its input. But
`PushdownLeftJoin` (`src/optimizer/pushdown/pushdown_left_join.cpp:107`) does
something cleverer than refuse:

- :132 classifies each filter by which side its bindings come from;
- :141 pushes LEFT-side filters into the left child, unconditionally;
- :152 asks `FilterRemovesNull(...)` of a right-side filter — *would this
  predicate reject a NULL-padded row?* If yes, no padded row can survive the
  filter anyway, so the outer join is downgraded to an inner join at :154 and
  the whole thing is re-run as `PushdownInnerJoin` (:164), where everything
  sinks;
- :167 keeps the filters that do *not* remove NULLs, and they stay above the
  join.

So the rule is not "filters never cross an outer join"; it is "a
NULL-rejecting filter converts the join, and then crosses it".

Why it matters: a filter evaluated one operator too late means every
intermediate row between the two positions was materialised, hashed or copied
for nothing. With a 1%-selective filter and a 1M-row input, that is 990,000 rows
of pure waste per join above it. This is the highest-leverage rewrite in the
pipeline, and the only one in this chapter that needs no estimate to be worth
doing.

### Step 4 — the fork: the plan becomes a graph *and* a bag of statistics

> **In:** the rewritten plan from Step 3, at the JOIN_ORDER slot (:285).
> **Out:** *two* datasets, and they go to different places. (a) A **query
> graph** — relations and predicate edges — consumed by the enumerator in
> Step 5. (b) A per-relation `RelationStats` — row counts and distinct counts —
> consumed by the estimator in Step 6. Everything after this point sees one or
> the other, never the plan tree.

`QueryGraphManager::Build` (`query_graph_manager.cpp:129`) is the fork, and it
is short enough to read as a table of contents:

- :132 `relation_manager.ExtractJoinRelations(...)` walks the plan and pulls out
  the **base relations** — the leaves that will be re-ordered — plus a
  `can_reorder` flag;
- :134-137 bails out entirely if there are fewer than two relations or something
  said "don't reorder me";
- :139 `relation_manager.ExtractEdges(...)` turns the collected filter operators
  into **edges**: one per join predicate, tagged with the relations on each side;
- :141-145 binds each predicate's endpoints and materialises the hypergraph
  (`CreateHyperGraphEdges` :280, which adds each edge in *both* directions at
  :284-285).

The result is a **query graph**: one node per base relation, one edge per join
predicate connecting two of them.

```
   chain:  A ─ B ─ C ─ D          star:   B   C
   (a.x=b.x, b.y=c.y, ...)                 \ /
                                        A ─ F ─ D     F = fact table,
                                           /|          every dim joins F
                                          E ...
```

The graph's *shape* is not decoration — it is the input size of Step 5. Join
orders worth considering correspond to **connected subgraphs**: subsets of
relations linked by predicates. Joining an unconnected set means a cross
product, which multiplies cardinalities instead of dividing them, and is almost
always a disaster. A chain of 10 relations has 55 connected subgraphs; a
10-clique has 1,023 (Step 5 counts them). Hold that.

Why it matters: this fork is why the join-order optimizer cannot see anything
the plan tree knew and the graph does not carry. Correlation between two
columns of different relations, for instance, has no representation in either
output — which is exactly the "join-crossing correlation" the JOB paper (§4.4)
names as the open frontier.

### Step 5 — join ordering by dynamic programming, with two escape hatches

> **In:** the query graph from Step 4a — relations and predicate edges only.
> **Out:** one chosen join *order* (a tree over the relations), written into
> the `plans` memo and read back by `QueryGraphManager::Reconstruct` (:302).
> No algorithm choice is made here; that is Step 2's :334 pass.

**Dynamic programming** (DP) means solving a big problem by combining stored
solutions to smaller subproblems, each solved once. Here the subproblems are
*sets of relations*: the best plan for a set S must be a join of the best plans
for two disjoint subsets whose union is S. A **memo** is the table that stores
them — keyed by relation set, holding the best plan found for it.

DuckDB's memo is `plans`, and `EmitPair` is the whole memo discipline in one
function:

```cpp
// src/optimizer/join_order/plan_enumerator.cpp — inside EmitPair, 193-207
   193  	auto &new_set = query_graph_manager.set_manager.Union(left, right);
   194  	// create the join tree based on combining the two plans
   195  	auto new_plan = CreateJoinTree(new_set, info, *left_plan->second, *right_plan->second);
   196  	// check if this plan is the optimal plan we found for this set of relations
   197  	auto entry = plans.find(new_set);
   198  	auto new_cost = new_plan->cost;
   199  	double old_cost = NumericLimits<double>::Maximum();
   200  	if (entry != plans.end()) {
   201  		old_cost = entry->second->cost;
   202  	}
   203  	if (entry == plans.end() || new_cost < old_cost) {
   204  		// the new plan costs less than the old plan. Update our DP table.
   205  		plans[new_set] = std::move(new_plan);
   206  		return *plans[new_set];
   207  	}
   // ... 208-222: a tiebreaker for equal-cost plans, to keep LEFT joins LEFT ...
```

The line that defines the whole architecture is 205: the memo is keyed by
`new_set` and holds **exactly one plan per relation set**, the cheapest. That is
the design decision postgres does *not* make (see `reading-postgres-optimizer.md`
— it keeps one plan per interesting order too), and Step 7 explains why DuckDB
can afford it.

Unlike Selinger's original left-deep-only search, this enumerator considers
**bushy** trees — joins whose *both* inputs are themselves joins, as opposed to
**left-deep** trees where the right input of every join is a base relation.
Graph-pattern queries especially want bushy plans. The file names its own
source: the comment at :529-531 says the enumeration is "a straight
implementation of the paper *Dynamic Programming Strikes Back* by Guido
Moerkotte and Thomas Neumann" — i.e. DPhyp, the hypergraph member of the
DPccp family, whose function names (`EmitCSG` :243, `EnumerateCSGRecursive`,
`EnumerateCmpRecursive` :295, `TryEmitPair` :227, `EmitPair` :185) you will
recognise from that literature. A **csg-cmp-pair** is one unit of that search:
a connected subgraph and a connected *complement* subgraph, disjoint, with at
least one edge between them — one candidate join.

**Now the arithmetic, because this is where exhaustive search dies.** Let n be
the number of base relations.

```
left-deep orderings (permutations)   = n!
bushy trees over n labelled leaves   = (2n-2)! / (n-1)!
DP joins considered, left-deep       = Σ(k=2..n) C(n,k)·k = n·2^(n-1) - n
DP memo entries (all subsets)        = 2^n - 1

 n     n!                     bushy                     DP considered   memo
 5             120                       1,680                     75      31
10       3,628,800              17,643,225,600                  5,110   1,023
15   1,307,674,368,000   3,497,296,636,753,920,000            245,745  32,767
```

Read the two right-hand columns against the two left-hand ones. At n = 10, DP
looks at 5,110 combinations instead of 3,628,800 orderings — **710× less work**
for a provably optimal left-deep plan. At n = 15 it is 245,745 against
1.3 × 10¹², **5.3 million× less**. That is the whole reason Selinger's 1979
idea is still in every engine. But look at the memo column: it doubles every
time you add a relation, and *that* is what dies. (Note also that the framing
"Catalan-many shapes × n! orderings ≈ 10¹⁸ for a 20-way join" mixes two
counts: 20! = 2.4 × 10¹⁸ is the *left-deep* count; the bushy count at n = 20 is
4.3 × 10²⁷.)

DPhyp does better than the table's `2^n` by enumerating only *connected*
subgraph pairs, so the real work depends on the graph's shape. These counts are
enumerated exhaustively rather than estimated:

```
csg-cmp-pairs the enumerator must emit, by query-graph shape:

   n     chain      star      clique       DuckDB's budget is 10,000 pairs
   5        20        32          90       (plan_enumerator.cpp:233)
   9       120     1,024       9,330       ← clique just fits
  10       165     2,304      28,501       ← clique blows it by 2.85×
  12       286    11,264     261,625
  15       560         —           —
```

A chain never troubles it. A 9-relation clique emits 9,330 pairs and completes.
A 10-relation clique emits 28,501 and does not. That is escape hatch one:

```cpp
// src/optimizer/join_order/plan_enumerator.cpp — TryEmitPair, 227-241
   227  bool PlanEnumerator::TryEmitPair(JoinRelationSet &left, JoinRelationSet &right,
   228                                   const vector<reference<NeighborInfo>> &info) {
   229  	pairs++;
   // ... 230-232: comment on keeping emission going until a final plan is produced ...
   233  	if (pairs >= 10000) {
   234  		// when the amount of pairs gets too large we exit the dynamic programming and resort to a greedy algorithm
   235  		// FIXME: simple heuristic currently
   236  		// at 10K pairs stop searching exactly and switch to heuristic
   237  		return false;
   238  	}
   239  	EmitPair(left, right, info);
   240  	return true;
   241  }
```

Line 233 is the budget and line 229 is the counter it spends. `false` propagates
up through `EnumerateCmpRecursive` (:315) and `SolveJoinOrderExactly` (:375) as a
plain "I gave up".

Escape hatch two is cruder and fires *first*, on relation count alone:

```cpp
// src/optimizer/join_order/plan_enumerator.cpp — SolveJoinOrder, 532-543
   532  void PlanEnumerator::SolveJoinOrder() {
   533  	bool force_no_cross_product = Settings::Get<DebugForceNoCrossProductSetting>(query_graph_manager.context);
   534  	auto swap_to_approximate_threshold =
   535  	    Settings::Get<ApproximateJoinOrderThresholdSetting>(query_graph_manager.context);
   536
   537  	// first try to solve the join order exactly
   538  	if (query_graph_manager.relation_manager.NumRelations() >= swap_to_approximate_threshold) {
   539  		SolveJoinOrderApproximately();
   540  	} else if (!SolveJoinOrderExactly()) {
   541  		// otherwise, if that times out we resort to a greedy algorithm
   542  		SolveJoinOrderApproximately();
   543  	}
```

Line 538 is the gate, and `approximate_join_order_threshold` defaults to **12**
(`src/include/duckdb/main/settings.hpp:261-267`) — the *same* number as
postgres's `geqo_threshold`. So the two hatches divide the work: the relation
gate catches every query with 12 or more tables regardless of shape, and the
10,000-pair budget catches the dense graphs below that. A 15-relation *chain*
emits only 560 pairs and would be trivially solvable exactly — but at n = 15
the :538 gate has already sent it to the heuristic.

The heuristic is `SolveJoinOrderApproximately` (:398), and the comment at
:400-401 names it: **Greedy Operator Ordering** — start with every base relation
as its own tree, then repeatedly combine the pair with the lowest cost. The
complexity is stated in the code at :407-409, not guessed here: "This is O(r^2)
per step, and every step will reduce the total amount of relations to-be-joined
by 1, so the total cost is O(r^3)". Greedy can be badly wrong, but a mediocre
plan beats an optimizer that runs longer than the query.

Why it matters: every engine in this topic has this same shape — exact search
with a cliff, and a heuristic behind the cliff. Knowing *where your* cliff is
(12 relations, or 10,000 pairs) is the difference between a planner you can
reason about and one that surprises you in production.

### Step 6 — cardinality estimation: the numbers the whole search runs on

> **In:** Step 4b's per-relation `RelationStats`, plus the edges from Step 4a.
> **Out:** one `double` per relation set — the estimated row count — memoised in
> `relation_set_2_cardinality`. Step 7 turns these, and nothing else, into cost.

**Cardinality** is the number of rows a relation or subplan contains; a
**cardinality estimate** is the planner's guess at it before running anything.
**Selectivity** is the fraction of its input a predicate keeps, so
`cardinality_out = selectivity × cardinality_in`. A **histogram** is a
per-column summary — value ranges and the row count in each — that lets an
engine estimate a range predicate's selectivity without scanning; DuckDB's
join-order path does **not** use one.

What it uses instead is a numerator over a denominator:

```cpp
// src/optimizer/join_order/cardinality_estimator.cpp — the whole estimate,
// 889-911 (the leading comment names the method's source)
   889  // Cardinality is calculated using logic based on
   890  // https://blobs.duckdb.org/papers/tom-ebergen-msc-thesis-join-order-optimization-with-almost-no-statistics.pdf
   // ... 891-895: INNER equality predicates use transitive equality classes; composite
   //              same-pair equalities can apply an FK/PK cap; disconnected predicate
   //              subgraphs are merged by cross product; LEFT/SEMI/ANTI adjust the numerator ...
   896  template <>
   897  double CardinalityEstimator::EstimateCardinalityWithSet(JoinRelationSet &new_set) {
   898  	double result;
   899  	auto it = state->relation_set_2_cardinality.find(new_set);
   900  	if (it != state->relation_set_2_cardinality.end()) {
   901  		result = it->second.cardinality_before_filters;
   902  	} else {
   // ... 903-906: comments on zero cardinalities and semi/anti numerators ...
   904  		auto denom = GetDenominator(new_set);
   907  		auto numerator = GetNumerator(denom.numerator_relations);
   908  		result = numerator / denom.denominator;
   909  		state->relation_set_2_cardinality[new_set] = CardinalityHelper(result);
   910  	}
   911  	return ApplyOrFilterSelectivities(new_set, result);
   912  }
```

Line 908 is the entire model. `GetNumerator` (:337-350) is a plain product of
the base relations' row counts — line 347, `numerator *= cardinality_before_filters`.
`GetDenominator` (:880) walks the edges and multiplies in one **total domain**
per equality-equivalence class: the estimated distinct-value count of the
columns that equality predicates have linked together. Grouping into classes is
what stops `a.x = b.x AND b.x = c.x` from dividing by the same domain twice.

So, symbol by symbol, for a set S of relations joined by equality predicates:

```
    card(S)  =  Π  |R|          ÷   Π   tdom(E)
              R∈S                  E∈classes(S)

  |R|       rows in base relation R, after its own local filters
  tdom(E)   the total domain of equality class E — the number of distinct
            values the columns in E are estimated to hold
```

**Worked example**, on this topic's own three-table schema (the one
`experiments/src/explain.rs` plans and `notes.md` asks you to predict):
`users` 10,000 rows, `orders` 50,000 rows, `items` 200,000 rows;
`users.id` has 10,000 distinct values, `orders.id` has 50,000.

```
{users, orders}   joined on users.id = orders.user_id
                  numerator   = 10,000 × 50,000 = 500,000,000
                  tdom        = 10,000  (distinct users.id)
                  card        = 500,000,000 / 10,000       =    50,000

{orders, items}   joined on orders.id = items.order_id
                  numerator   = 50,000 × 200,000 = 10,000,000,000
                  tdom        = 50,000  (distinct orders.id)
                  card        = 10,000,000,000 / 50,000    =   200,000
```

Both are right — each foreign key matches exactly one parent, so the join
preserves the child's row count. Now add the two filters `users.city = 7 AND
users.age = 30`, with 100 distinct cities and 50 distinct ages. **Independence**
— the assumption that predicates on different columns are unrelated, so their
selectivities multiply — gives:

```
sel(city = 7)             = 1/100                        = 0.01
sel(age  = 30)            = 1/50                          = 0.02
sel(both), independent    = 0.01 × 0.02                   = 0.0002
|users| after filters     = 10,000 × 0.0002               = 2 rows

{users, orders} now       = (2 × 50,000) / 10,000         = 10 rows
{orders, items} unchanged                                 = 200,000 rows
```

Ten against two hundred thousand: the filters flip which pair the enumerator
joins first, by four orders of magnitude. That is the prediction `notes.md`
asks you to commit to before running `explain`.

Now break independence, which is what real data does. Suppose city 7 is a
university town where half the users are 30. Then `sel(age=30 | city=7) = 0.5`,
not 0.02, and:

```
true sel(both)            = 0.01 × 0.5                    = 0.005
true |users| after filters= 10,000 × 0.005                = 50 rows
estimate                  = 2 rows            truth = 50 rows
error factor              = 50 / 2                        = 25×
```

A 25× underestimate from *one* correlated pair of columns, in a schema with
three tables. The JOB paper measures what this does at six joins
(`reading-how-good-optimizers.md`); the short version is that the factors
multiply.

One constant deserves correcting, because it is the most-repeated wrong claim
about this file. DuckDB does have a `DEFAULT_SELECTIVITY`, but it is
**0.2** — `src/include/duckdb/optimizer/join_order/relation_statistics_helper.hpp:55`,
`static constexpr double DEFAULT_SELECTIVITY = 0.2;` — **not** postgres's 0.005.
It is 40× larger, and it is not a general "unknown predicate" fallback either.
It is applied in exactly two places: `relation_statistics_helper.cpp:259-262`,
where a base table has non-equality filters and no equality filter to estimate
from, and `cardinality_estimator.cpp:915-918`, where a relation set is covered
by an OR filter. Everything else goes through the numerator/denominator above.

Why it matters: these estimates are the *only* signal ranking the plans Step 5
enumerates, and the guess above is arithmetic over two statistics per column.
There is no histogram anywhere in this path.

### Step 7 — the cost model is (almost) just cardinality

> **In:** Step 6's estimate for the combined set, plus the already-computed
> costs of the two child plans from the Step 5 memo.
> **Out:** one `double` — the number `EmitPair` compares at :198-203 to decide
> what stays in the memo. This closes the loop: Steps 5, 6 and 7 run
> interleaved, once per emitted pair.

The **cost model** is the function that turns estimated cardinalities into one
comparable number per plan. **Cout** is the classic minimal one: the cost of a
plan is the sum of the sizes of all its intermediate results, and nothing else —
no CPU weights, no IO constants. DuckDB's is Cout plus one correction, and the
file is 50 lines long:

```cpp
// src/optimizer/join_order/cost_model.cpp — ComputeCost, the whole model, 37-48
    37  // Currently cost of a join mostly factors in the cardinalities.
    38  // LEFT joins need an explicit RHS input component because their output cardinality preserves the LHS,
    39  // which otherwise makes early LEFT joins over large RHS inputs look almost free.
    40  double CostModel::ComputeCost(DPJoinNode &left, DPJoinNode &right, JoinRelationSet &combination,
    41                                const vector<reference<NeighborInfo>> &possible_connections) {
    42  	auto join_card = cardinality_estimator.EstimateCardinalityWithSet<double>(combination);
    43  	auto join_cost = join_card;
    44  	if (query_graph_manager.GetPredicateModel().HasLeftJoinPredicates()) {
    45  		join_cost += GetLeftJoinInputCost(cardinality_estimator, possible_connections);
    46  	}
    47  	return join_cost + left.cost + right.cost;
    48  }
```

The line that carries the argument is 47: *this join's output cardinality, plus
the two children's costs*. The recursion bottoms out at zero — a leaf
`DPJoinNode` is constructed with `cost(0)` (`join_node.cpp:10`) and
`InitLeafPlans` sets `join_node->cost = 0` explicitly at
`plan_enumerator.cpp:521` — so base-table scans are *free* in this model and a
plan's cost is precisely the sum of its intermediate join outputs. Cout,
exactly. Lines 44-46 are the one exception, and the
comment at :38-39 explains it honestly: a LEFT join's output cardinality equals
its left input's, so without a term for the right input, joining a huge RHS
early looks free. (A summary that says "cost is cardinality, that's it" is
right about :43 and wrong about :45; the pinned tree has both.)

Run it on the worked example above, on the filtered `users`:

```
plan A:  (users ⋈ orders) ⋈ items
         intermediate {users,orders} = 10          ← Step 6
         final        {u,o,i}        = 10 × 200,000 / 50,000 = 40
         Cout = 10 + 40                                       = 50

plan B:  (orders ⋈ items) ⋈ users
         intermediate {orders,items} = 200,000     ← Step 6
         final        {u,o,i}                                 = 40
         Cout = 200,000 + 40                                  = 200,040
```

200,040 / 50 = **4,001× more expensive**, and the only thing separating the two
plans is which intermediate the model was told about. That is the sense in which
cardinality *is* the cost model here: change the estimate, change the plan, and
nothing else in the file gets a vote.

This is defensible — the JOB paper found that in a main-memory setting a
deliberately trivial cost function, given *true* cardinalities, produced query
runtimes 34% faster than PostgreSQL's 4,000-line model in geometric mean
(Leis et al. §5.4) — and damning: cardinality error is plan error, one for one.

Note also what this model does *not* rank. It compares join *orders*. Whether a
hash join builds on the left or the right input is decided later, by the
`BUILD_SIDE_PROBE_SIDE` pass at `optimizer.cpp:334`, using column-lifetime
information the join-order pass did not have.

Why it matters: every knob you might want to tune in this optimizer — a CPU
weight, an IO constant, a sort penalty — does not exist. The only thing you can
improve is Step 6.

## Where each step lives in the code

Read in this order: `optimizer.cpp`, then `filter_pushdown.cpp`, then the
`join_order/` subdirectory — the payoff. All line numbers are `6c0c1a68`.

| Step | File | Lines | What |
|---|---|---|---|
| 2 | `src/optimizer/optimizer.cpp` | 119-140 | `RunOptimizer` — profiles each pass and `Verify`s the plan after it |
| 2 | `src/optimizer/optimizer.cpp` | 178 | `RunBuiltInOptimizers` — the pass list's own function |
| 2 | `src/optimizer/optimizer.cpp` | 197-435 | the 39 `RunOptimizer` calls, in order: pullup :212, pushdown :218, JOIN_ORDER :285, build/probe :334, TOP_N :367 |
| 2 | `src/optimizer/optimizer.cpp` | 441-472 | `Optimizer::Optimize` — extensions, then `RunBuiltInOptimizers` at :458 |
| 3 | `src/optimizer/filter_pushdown.cpp` | 106-151 | `Rewrite` — the dispatch table, one arm per operator |
| 3 | `src/optimizer/filter_pushdown.cpp` | 339-347 | `FinishPushdown` — the safe default: fresh child pushdown, filters deposited |
| 3 | `src/optimizer/pushdown/pushdown_left_join.cpp` | 107-171 | side classification :132, LEFT push :141, `FilterRemovesNull` → INNER :152-164 |
| 4 | `src/optimizer/join_order/query_graph_manager.cpp` | 129-147 | `Build` — relations :132, edges :139, hypergraph :145 |
| 4 | `src/optimizer/join_order/query_graph_manager.cpp` | 280-287 | `CreateHyperGraphEdges` — each predicate added in both directions |
| 4 | `src/optimizer/join_order/relation_manager.cpp` | 267, 674 | `ExtractJoinRelations`, `ExtractEdges` — where the fork's two datasets are built |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 185-225 | `EmitPair` — the memo; :205 keeps one plan per relation set |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 227-241 | `TryEmitPair` — the 10,000-pair budget at :233 |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 243, 295, 375 | `EmitCSG`, `EnumerateCmpRecursive`, `SolveJoinOrderExactly` — the DPhyp enumeration |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 398-527 | `SolveJoinOrderApproximately` — Greedy Operator Ordering, O(r³) per its own comment at :407-409 |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 529-543 | the DPhyp citation, and `SolveJoinOrder`'s two escape hatches |
| 5 | `src/include/duckdb/main/settings.hpp` | 261-267 | `approximate_join_order_threshold`, default `"12"` |
| 6 | `src/optimizer/join_order/cardinality_estimator.cpp` | 889-912 | `EstimateCardinalityWithSet` — `numerator / denominator` at :908 |
| 6 | `src/optimizer/join_order/cardinality_estimator.cpp` | 337-350, 880-887 | `GetNumerator` (product of base rows), `GetDenominator` (total domains) |
| 6 | `src/include/duckdb/optimizer/join_order/relation_statistics_helper.hpp` | 55 | `DEFAULT_SELECTIVITY = 0.2` |
| 7 | `src/optimizer/join_order/cost_model.cpp` | 37-48 | `ComputeCost` — Cout at :43/:47, the LEFT-join correction at :44-46 |
| 7 | `src/optimizer/join_order/join_node.cpp` | 9-11 | a leaf `DPJoinNode` is built with `cost(0)` — base scans are free |

Suggested route: `optimizer.cpp:441` → `:178` and read the list top to bottom
→ `filter_pushdown.cpp:106` → `pushdown_left_join.cpp:107` → then
`join_order/`: `query_graph_manager.cpp:129` → `plan_enumerator.cpp:532` (start
at the *end*, where the two hatches are) → `:375` → `:185` →
`cardinality_estimator.cpp:897` → `cost_model.cpp:40`.

## Questions for notes.md

1. Why does pullup-then-pushdown beat pushdown alone? Find one operator in
   `src/optimizer/pullup/` where hoisting first enables a deeper sink, and name
   the file.
2. The memo keeps one best plan per relation set (`plan_enumerator.cpp:205`).
   What plan property does that discard that postgres keeps (hint: interesting
   orders) — and why does DuckDB get away with it? Which physical operator's
   dominance is the answer?
3. Step 5's table says a 9-clique emits 9,330 csg-cmp-pairs and a 10-clique
   28,501. At what n does a *star* schema cross the 10,000-pair budget, and
   does the :538 relation gate fire before or after that? Work it from the
   table.
4. Cost is output cardinality only (`cost_model.cpp:43`), so build-vs-probe
   side is chosen later, at `optimizer.cpp:334`. What does splitting
   order-choice from side-choice lose? Construct a case where the best order
   under Cout has the worse build side.
5. M10: a Cypher chain `(a)-[:R]->(b)-[:S]->(c)` is a chain query graph over
   edge relations. Which DuckDB piece maps to anchor-node selection — the
   enumerator (`plan_enumerator.cpp`) or the cardinality estimator? Justify
   with the fork in Step 4.

## Takeaway

The pipeline is 39 verified passes in a fixed order; the join search is DPhyp
with a 12-relation gate and a 10,000-pair budget behind it; the cost model is
Cout plus one LEFT-join correction; and the estimator is a product of row
counts over a product of distinct counts, with no histogram in sight. For M10:
copy the fork (Step 4) and the budget (Step 5), and expect every plan bug you
have to be an estimate bug.

## Done when

Answer each before unfolding it.

- [ ] You can name the pass order in coarse buckets, and say why filter pullup runs before filter pushdown.

  <details><summary>Answer</summary>

  Coarse buckets, in the order `RunBuiltInOptimizers` (`optimizer.cpp:178`)
  runs them: expression-level simplification (:197), CTE handling (:200),
  filter movement (pullup :212, pushdown :218), subquery decorrelation
  (deliminator :242), structural simplification (projection pullup :272, outer
  join simplification :278), **join ordering** (:285), column pruning (:309)
  and common-subexpression work (:321), physical-ish choices (build/probe side
  :334), then limit/top-n (:350, :367) and a final cleanup tail out to :435.
  Thirty-nine `RunOptimizer` calls, 37 distinct types — `CTE_INLINING` and
  `COLUMN_LIFETIME` each appear twice.

  Pullup runs first because it *un*-blocks pushdown. A filter stranded inside
  one branch of a subtree can often be hoisted to a point where it applies to
  both branches; once hoisted, the pushdown pass at :218 can sink it into each
  of them, deeper than it started. Running pushdown alone would leave it where
  it was. The pipeline's order is expert knowledge encoded as a list, which is
  also its weakness: a rewrite that only becomes possible after a later pass is
  simply never found, and nothing reports that.

  </details>

- [ ] You can explain DPhyp, the greedy fallback, and both escape hatches — with the actual thresholds.

  <details><summary>Answer</summary>

  The enumerator walks *connected subgraph / connected complement* pairs of the
  query graph, smallest first, and for each pair calls `EmitPair`
  (`plan_enumerator.cpp:185`), which builds the join and keeps it in the `plans`
  memo at :205 only if it beats what is already stored for that relation set.
  One plan per set, cheapest wins. The file cites *Dynamic Programming Strikes
  Back* (Moerkotte and Neumann) at :529-531 as its source.

  Two hatches, in the order they fire. `SolveJoinOrder` (:532) first compares the
  relation count against `approximate_join_order_threshold`, default **12**
  (`settings.hpp:261-267`), at :538 — twelve or more tables goes straight to
  greedy, whatever the shape. Otherwise it tries exact, and `TryEmitPair`
  (:227) aborts the moment its `pairs` counter reaches **10,000** (:233),
  returning `false` all the way up so :540 falls through to greedy at :542.

  The greedy is `SolveJoinOrderApproximately` (:398): Greedy Operator Ordering,
  which starts with every relation as its own tree and repeatedly merges the
  cheapest pair. Its own comment at :407-409 states the complexity — O(r²) per
  step, O(r³) overall. Concretely: a 9-relation clique emits 9,330 csg-cmp-pairs
  and is solved exactly; a 10-relation clique emits 28,501 and is not; a
  15-relation chain emits only 560 but is sent to greedy anyway, by the :538
  gate.

  </details>

- [ ] You can write DuckDB's cardinality formula from memory and run it on three tables, with and without the independence assumption.

  <details><summary>Answer</summary>

  `card(S) = (Π over R in S of |R|) / (Π over equality classes E of tdom(E))` —
  `cardinality_estimator.cpp:908`, with `GetNumerator` (:337-350) supplying the
  product of base row counts at :347 and `GetDenominator` (:880) supplying one
  total domain per equality-equivalence class. `|R|` is R's row count after its
  own local filters; `tdom(E)` is the estimated number of distinct values the
  columns linked by equality in class E hold.

  On this topic's schema — `users` 10,000, `orders` 50,000, `items` 200,000,
  10,000 distinct `users.id`, 50,000 distinct `orders.id`:
  `{users, orders}` = 10,000 × 50,000 / 10,000 = 50,000, and
  `{orders, items}` = 50,000 × 200,000 / 50,000 = 200,000. Add
  `users.city = 7 AND users.age = 30` with 100 cities and 50 ages: independence
  multiplies the selectivities, 0.01 × 0.02 = 0.0002, so `users` becomes 2 rows
  and `{users, orders}` becomes 2 × 50,000 / 10,000 = 10 — four orders of
  magnitude below `{orders, items}`, which is why the filters flip the join
  order.

  Break independence and the same arithmetic lies. If city 7 is a town where
  half the users are 30, the true conditional selectivity is 0.5, not 0.02: the
  true filtered `users` is 10,000 × 0.01 × 0.5 = 50 rows against an estimate of
  2, a factor of 25 from a single correlated column pair. Under Cout the two
  candidate orders here differ by 200,040 / 50 ≈ 4,001×, so an error of that
  size is entirely capable of picking the wrong one.

  </details>

- [ ] You can state what DuckDB's `DEFAULT_SELECTIVITY` actually is, and where it is used — without repeating the postgres number.

  <details><summary>Answer</summary>

  It is **0.2**, declared at
  `src/include/duckdb/optimizer/join_order/relation_statistics_helper.hpp:55`.
  Postgres's `DEFAULT_EQ_SEL` is 0.005; the two are not the same constant and
  DuckDB's is 40× larger. Anything that calls DuckDB's "its version of
  postgres's famous 0.005" is repeating a claim the source does not support.

  It is also not a general fallback for predicates the estimator cannot reason
  about. It appears in exactly two places. At
  `relation_statistics_helper.cpp:259-262`, a base relation that has
  non-optional filters but no equality filter to estimate from has its
  cardinality set to `max(base_cardinality × 0.2, 1)`. At
  `cardinality_estimator.cpp:915-918`, `ApplyOrFilterSelectivities` multiplies a
  relation set's estimate by 0.2 once per OR filter covering it. Everything
  else in the join-order path goes through `numerator / denominator` at :908.

  </details>

- [ ] You can say why the join-order search cannot fix a bad estimate, and where in the code the two are wired together.

  <details><summary>Answer</summary>

  Because search and estimation are not two independent quality knobs — the
  search *consumes* the estimate as its only ranking signal. `ComputeCost`
  (`cost_model.cpp:40`) calls `EstimateCardinalityWithSet` at :42, adds the
  children's costs at :47, and returns; `EmitPair` (`plan_enumerator.cpp:185`)
  compares that number at :198-203 and keeps the smaller. There is no other
  input. A perfectly exhaustive search over wrong numbers returns the plan that
  is optimal *for the wrong numbers*.

  The JOB paper measured exactly this separation. With true cardinalities
  injected, exhaustive dynamic programming produced the optimal plan at the
  median, the 95th percentile and the maximum — 1.00 / 1.00 / 1.00 in their
  Table 3. The same exhaustive DP driven by PostgreSQL's estimates, on the same
  queries with foreign-key indexes, scored 1.66 median and **186,367× at the
  maximum**. Nothing about the search changed. Meanwhile the *heuristics* given
  true cardinalities cost only 1.02-1.20 at the median. Search quality is worth
  tens of percent; estimate quality is worth five orders of magnitude.

  </details>

## References

**Code**
- [duckdb](https://github.com/duckdb/duckdb) at `6c0c1a68` — `src/optimizer/`:
  `optimizer.cpp` (the pass pipeline, read :178-438 top to bottom),
  `filter_pushdown.cpp` + `pushdown/`, and `join_order/`
  (`plan_enumerator.cpp`, `cardinality_estimator.cpp`, `cost_model.cpp`); ~2 h.
- Tom Ebergen, *Join Order Optimization with (Almost) No Statistics* (MSc
  thesis) — the method `cardinality_estimator.cpp:889-890` cites for its
  numerator/denominator model.
- Moerkotte and Neumann, *Dynamic Programming Strikes Back* (SIGMOD 2008) — the
  enumeration algorithm `plan_enumerator.cpp:529-531` says it implements.

**Papers**
- Leis, Gubichev, Mirchev, Boncz, Kemper, Neumann — *How Good Are Query
  Optimizers, Really?* (VLDB 2015). Used here for three figures, each from a
  named section: §6.1 (average worst-to-best plan ratio 101× / 115× / 48,120×
  by index configuration), §5.4 (a trivial main-memory cost model with true
  cardinalities beats PostgreSQL's own by 34% in geometric mean), and Table 3
  (exhaustive DP scores 1.00/1.00/1.00 with true cardinalities and
  1.66/169/186,367 with PostgreSQL's). Read in full via
  `reading-how-good-optimizers.md`.
