# The readable optimizer: DuckDB's pass pipeline and join-order DP

DuckDB's `src/optimizer/` is the clearest production optimizer you can
read: a fixed, hand-ordered list of rewrite passes, each timed and followed
by a plan-verifier hook, feeding a connected-subgraph join enumerator with
two escape hatches, and a cost model that is essentially cardinality. Before you open
the code, this chapter builds the seven concepts an optimizer is made of —
plan trees, rewrites, pushdown, the query graph, the join-order DP,
cardinality estimation, and the cost model — one at a time, works the
combinatorics on real numbers, then hands you the file and line anchors to
watch each one run.

Every anchor below is DuckDB **v2.0.0**, commit **`d54cbbe3df`**
(`tools/pinned-source.py ref duckdb`), quoted with the line numbers the code
occupies in that revision. This topic has no measured lane — its only binary
runs *your* planner — so every number here comes from that source, from a
paper section named on the spot, or from arithmetic performed in the guide on
stated assumptions.

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

DuckDB runs these in one fixed, hand-tuned order, times every one, and hands
the plan to a verifier after every one. The wrapper is short and is where the
discipline lives:

```cpp
// src/optimizer/optimizer.cpp — RunOptimizer and Verify, 136-155
   136  void Optimizer::RunOptimizer(OptimizerType type, const std::function<void()> &callback) {
   // ... 137-142: bail on interrupt; skip if this optimizer is disabled ...
   143  	auto &profiler = QueryProfiler::Get(context);
   144  	{
   145  		auto optimizer_timer = profiler.StartTimerInternal("optimizer." + StringUtil::Lower(EnumUtil::ToString(type)));
   146  		callback();
   147  	}
   148  	if (plan) {
   149  		Verify(*plan);
   150  	}
   151  }
   152
   153  void Optimizer::Verify(LogicalOperator &op) {
   154  	LogicalPlanVerifier::Verify(context, op);
   155  }
```

Line 149 is the one to look at: *every* pass is followed by a call into the
logical-plan verifier. What that call costs depends on a setting:

```cpp
// src/planner/logical_plan_verifier.cpp — LogicalPlanVerifier::Verify, 326-335
   326  void LogicalPlanVerifier::Verify(ClientContext &context, LogicalOperator &op) {
   327  	if (!Settings::Get<DebugVerifyColumnBindingsSetting>(context)) {
   328  		return;
   329  	}
   330  	string first_error;
   331  	auto result = VerifyAlwaysInternal(op, first_error);
   332  	if (result.HasError()) {
   333  		throw InternalException("%s", first_error);
   334  	}
   335  }
```

Line 327 is the gate. `debug_verify_column_bindings` is a debug setting whose
default is `"false"` (`src/include/duckdb/main/settings.hpp:899-909`, default
at :906), so in an ordinary run the hook after each pass is one setting lookup
and an early return. Turn it on and every pass is followed by a full check of
the plan's column bindings and types, which throws the moment a rewrite leaves
a dangling reference — the hook is there so that a broken rewrite is caught at
the pass that broke it. Line 145 is the other always-on half: every pass is
separately timed, which is why `EXPLAIN ANALYZE` can tell you which optimizer
cost you the millisecond.

The list itself lives in `Optimizer::RunBuiltInOptimizers` (:229), reached from
`Optimizer::Optimize` (:565) at :592. **Count them before you trust any summary
of this file: there are 44 `RunOptimizer(OptimizerType::…)` calls between :252
and :549, covering 41 distinct optimizer types** — `CTE_INLINING`,
`UNUSED_COLUMNS` and `COLUMN_LIFETIME` each run twice, at different points.
The order tells a story:

```
 :252 expression rewriter → :259 cte inlining → :271 FILTER PULLUP →
 :277 FILTER PUSHDOWN → :301 in-clause → :307 deliminator (decorrelation) →
 :342 projection pullup → :348 outer-join simplification →
 :355 JOIN_ORDER → :361 aggregate reuse → … → :392 unused columns →
 :404 common subexpressions → :417 build/probe side → :433 limit pushdown →
 :450 TOP_N → … → :484 projection placement → :505 unused columns, again →
 … → :519 reorder filter → :531 join filter pushdown → :543 type pushdown →
 :549 scalar-function pushdown
```

Three things to notice. **Pullup runs before pushdown** (:271 before :277) — it
looks backwards, but hoisting a filter out of a subtree first lets pushdown then
sink it into *both* branches of a join it could not otherwise cross. **Join
ordering runs mid-pipeline** (:355), on a plan already scrubbed of noise; the
comment above it (:353-354) notes that the join-order pass "also rewrites cross
products + filters into joins and performs filter pushdowns", so the pipeline's
two halves are not perfectly separated. And **build/probe side selection is a
separate, later pass** (:417) — Step 7 comes back to what that costs.

This is an order-dependent heuristic pipeline, not a fixpoint engine — contrast
DataFusion's run-until-nothing-changes loop and Cascades' memo (this topic's
other guides). The list's own answer to "a later pass created work for an
earlier one" is to run the earlier one again, by hand. The second
`UNUSED_COLUMNS` at :505 is the clearest case: it is guarded by
`if (removed_expressions)` at :504, and the comment at :502-503 gives the
reason — "statistics propagation removes filters, join conditions and aggregate
children that it proves redundant. this can leave columns in a scan that
nothing references any more - prune them again". A few of the other passes
show what the list is for: `AGGREGATE_REUSE` (:361) runs right after join
ordering ([#24543](https://github.com/duckdb/duckdb/pull/24543));
`REDUNDANT_ORDER_KEYS` (:380) drops `ORDER BY` keys that follow a unique
prefix ([#25878](https://github.com/duckdb/duckdb/pull/25878));
`PROJECTION_PLACEMENT` (:484) keeps variable-size projections below joins
([#24914](https://github.com/duckdb/duckdb/pull/24914)); and
`SCALAR_FN_PUSHDOWN` (:549), last, pushes scalar functions such as `strlen`
into the file readers ([#23528](https://github.com/duckdb/duckdb/pull/23528)).

Why it matters: a fixed order is a fixed set of bugs. If pass B only fires on
output pass A produces, and B runs first, the rewrite is simply never found —
and nothing in the system reports that. The guarded re-run at :504-505 is the
fix applied once, for one known pair; every pair nobody noticed stays broken.

### Step 3 — filter pushdown mechanics: a bag of filters sinking until blocked

> **In:** the plan as it stands at :277, the FILTER_PUSHDOWN slot in Step 2's
> list.
> **Out:** the same plan with every filter as deep as it can legally go — and,
> where a filter proves a NULL-padded row impossible, with an outer join
> rewritten to an inner one. That plan is what Step 4 reads relations out of.

Pushdown is not "move one filter one step". DuckDB's implementation carries a
*bag* of accumulated filter expressions down the tree: at each operator it asks
"can these pass through you?", pushes the ones that can, and deposits the rest
as a `Filter` node right above the blocker. `FilterPushdown::Rewrite` (:108)
first separates out *barrier* filters — expressions marked as not allowed to
move below an operator that can remove rows — and re-applies them above any such
operator (:110-117); everything else goes to a bare `switch` on operator type,
one case per rule, and the default case is the honest one:

```cpp
// src/optimizer/filter_pushdown.cpp — the default arm of Rewrite, 160-163,
// and the function it calls, 317-325
   160  	default:
   161  		return FinishPushdown(std::move(op));
   162  	}
   163  }
   // ... 164-316: PushdownJoin, PushFilters, PushFinalFilters, … ...
   317  unique_ptr<LogicalOperator> FilterPushdown::FinishPushdown(unique_ptr<LogicalOperator> op) {
   318  	// unhandled type, first perform filter pushdown in its children
   319  	for (auto &child : op->children) {
   320  		FilterPushdown pushdown(optimizer, convert_mark_joins, projection_mode);
   321  		child = pushdown.Rewrite(std::move(child));
   322  	}
   323  	// now push any existing filters
   324  	return PushFinalFilters(std::move(op));
   325  }
```

Line 320 is the argument: an operator this pass does not understand gets a
*fresh, empty* `FilterPushdown` for each child, so the current bag of filters
cannot cross it, and :324 deposits that bag above the blocker. Unknown operator
⇒ nothing sinks through. That is the safe default, and it is why adding an
operator to DuckDB cannot silently break pushdown correctness.

**Outer joins** are where the interesting case lives. A **left outer join**
emits every left row, padding the right columns with NULL when nothing matched
— so a filter on the right-hand columns cannot simply sink below the join: the
rows it would remove are *created by* the join, not present in its input. But
`PushdownLeftJoin` (`src/optimizer/pushdown/pushdown_left_join.cpp:111`) does
something cleverer than refuse:

- :137 classifies each filter by which side its bindings come from;
- :146 pushes LEFT-side filters into the left child, unconditionally;
- :157 asks `FilterRemovesNull(...)` of a right-side filter — *would this
  predicate reject a NULL-padded row?* If yes, no padded row can survive the
  filter anyway, so the outer join is downgraded to an inner join at :159 and
  the whole thing is re-run as `PushdownInnerJoin` (:169), where everything
  sinks;
- :172 keeps the filters that do *not* remove NULLs, and they stay above the
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

> **In:** the rewritten plan from Step 3, at the JOIN_ORDER slot (:355).
> **Out:** *two* datasets, and they go to different places. (a) A **query
> graph** — relations and predicate edges — consumed by the enumerator in
> Step 5. (b) A per-relation `RelationStats` — row counts and distinct counts —
> consumed by the estimator in Step 6. Everything after this point sees one or
> the other, never the plan tree.

`QueryGraphManager::Build` (`query_graph_manager.cpp:219`) is the fork, and it
is short enough to read as a table of contents:

- :222 `relation_manager.ExtractJoinRelations(...)` walks the plan and pulls out
  the **base relations** — the leaves that will be re-ordered — plus a
  `can_reorder` flag;
- :224-227 bails out entirely if there are fewer than two relations or something
  said "don't reorder me";
- :229 `relation_manager.ExtractEdges(...)` turns the collected filter operators
  into **edges**: one per join predicate, tagged with the relations on each side
  — and also returns the *join operators* that are not plain inner joins;
- :232-244 builds a conflict descriptor for each of those operators (below);
- :246-251 binds each predicate's endpoints, builds the predicate model, and
  materialises the hypergraph (`CreateHyperGraphEdges` :399, which adds each
  ordinary predicate in *both* directions at :424-425).

The result is a **query graph**: one node per base relation, one edge per join
predicate connecting two of them.

Inner joins commute and associate freely; LEFT, SEMI and ANTI joins do not —
moving a relation across a LEFT join can change which rows get NULL-padded.
DuckDB still reorders around them: `JoinIsReorderable`
(`relation_manager.cpp:233`) accepts INNER, SEMI, LEFT and ANTI comparison joins
(:249-252), and each non-inner one becomes a `JoinOrderOperator` whose header
comment (`src/include/duckdb/optimizer/join_order/join_order_operator.hpp:31-34`)
names its method: "CD-C from *On the Correct and Complete Enumeration of the
Core Search Space*" — Moerkotte, Fender and Eich's **conflict detector**. A
conflict detector widens each operator's *syntactic eligibility set* (the
relations its predicate mentions) into a *total eligibility set* (the relations
that must be on its side for the reordering to stay correct), plus conflict
rules checked when a candidate join is formed. Those operators are emitted into
the graph from their total sets at :452-453, so the enumerator of Step 5 simply
never sees a pair that would reorder a LEFT join illegally.

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
> the `plans` memo and read back by `QueryGraphManager::Reconstruct` (:498).
> No algorithm choice is made here; that is Step 2's :417 pass.

**Dynamic programming** (DP) means solving a big problem by combining stored
solutions to smaller subproblems, each solved once. Here the subproblems are
*sets of relations*: the best plan for a set S must be a join of the best plans
for two disjoint subsets whose union is S. A **memo** is the table that stores
them — keyed by relation set, holding the best plan found for it.

DuckDB's memo is `plans`, and `EmitPair` is the whole memo discipline in one
function:

```cpp
// src/optimizer/join_order/plan_enumerator.cpp — inside EmitPair, 200-217
   200  	auto &new_set = query_graph_manager.set_manager.Union(left, right);
   201  	// create the join tree based on combining the two plans
   202  	auto new_plan = CreateJoinTree(new_set, info, *left_plan->second, *right_plan->second);
   203  	if (!new_plan) {
   204  		return nullptr;
   205  	}
   206  	// check if this plan is the optimal plan we found for this set of relations
   207  	auto entry = plans.find(new_set);
   208  	auto new_cost = new_plan->cost;
   209  	double old_cost = NumericLimits<double>::Maximum();
   210  	if (entry != plans.end()) {
   211  		old_cost = entry->second->cost;
   212  	}
   213  	if (entry == plans.end() || new_cost < old_cost) {
   214  		// the new plan costs less than the old plan. Update our DP table.
   215  		plans[new_set] = std::move(new_plan);
   216  		return plans[new_set].get();
   217  	}
   // ... 218-234: a tiebreaker for equal-cost plans, to keep LEFT joins LEFT ...
```

Lines 203-205 are where Step 4's conflict rules bite: `CreateJoinTree` returns
nothing for a pair no operator may legally join, and the pair is dropped. The
line that defines the whole architecture is 215: the memo is keyed by
`new_set` and holds **exactly one plan per relation set**, the cheapest. That is
the design decision postgres does *not* make (see `reading-postgres-optimizer.md`
— it keeps one plan per interesting order too), and Step 7 explains why DuckDB
can afford it.

Unlike Selinger's original left-deep-only search, this enumerator considers
**bushy** trees — joins whose *both* inputs are themselves joins, as opposed to
**left-deep** trees where the right input of every join is a base relation.
Graph-pattern queries especially want bushy plans. The file names its own
source: the comment at :580-582 says the enumeration is "a straight
implementation of the paper *Dynamic Programming Strikes Back* by Guido
Moerkotte and Thomas Neumann" — i.e. DPhyp, the hypergraph member of the
DPccp family, whose function names (`EmitCSG` :253, `EnumerateCSGRecursive`
:348, `EnumerateCmpRecursive` :305, `TryEmitPair` :237, `EmitPair` :192) you will
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
   5        20        32          90       (plan_enumerator.cpp:243)
   9       120     1,024       9,330       ← clique just fits
  10       165     2,304      28,501       ← clique blows it by 2.85×
  12       286    11,264     261,625
  15       560         —           —
```

A chain never troubles it. A 9-relation clique emits 9,330 pairs and completes.
A 10-relation clique emits 28,501 and does not. That is escape hatch one:

```cpp
// src/optimizer/join_order/plan_enumerator.cpp — TryEmitPair, 237-251
   237  bool PlanEnumerator::TryEmitPair(JoinRelationSet &left, JoinRelationSet &right,
   238                                   const vector<reference<NeighborInfo>> &info) {
   239  	pairs++;
   // ... 240-242: comment on keeping emission going until a final plan is produced ...
   243  	if (pairs >= 10000) {
   244  		// when the amount of pairs gets too large we exit the dynamic programming and resort to a greedy algorithm
   245  		// FIXME: simple heuristic currently
   246  		// at 10K pairs stop searching exactly and switch to heuristic
   247  		return false;
   248  	}
   249  	EmitPair(left, right, info);
   250  	return true;
   251  }
```

Line 243 is the budget and line 239 is the counter it spends. `false` propagates
up through `EnumerateCmpRecursive` (:325-326) and `SolveJoinOrderExactly` (:385)
as a plain "I gave up".

Escape hatch two is cruder and fires *first*, on relation count alone:

```cpp
// src/optimizer/join_order/plan_enumerator.cpp — SolveJoinOrder, 583-603
   583  bool PlanEnumerator::SolveJoinOrder() {
   584  	bool force_no_cross_product = Settings::Get<DebugForceNoCrossProductSetting>(query_graph_manager.context);
   585  	auto swap_to_approximate_threshold =
   586  	    Settings::Get<ApproximateJoinOrderThresholdSetting>(query_graph_manager.context);
   587
   588  	// first try to solve the join order exactly
   589  	bool solved;
   590  	if (query_graph_manager.relation_manager.NumRelations() >= swap_to_approximate_threshold) {
   591  		solved = SolveJoinOrderApproximately();
   592  	} else {
   593  		auto completed_exactly = SolveJoinOrderExactly();
   594  		if (completed_exactly && !HasCompletePlan() && ActivateRequiredCrossProducts()) {
   595  			completed_exactly = SolveJoinOrderExactly();
   596  		}
   597  		if (!completed_exactly || !HasCompletePlan()) {
   598  			// Exact enumeration either reached its pair budget or could not connect the graph.
   599  			solved = SolveJoinOrderApproximately();
   600  		} else {
   601  			solved = true;
   602  		}
   603  	}
```

Line 590 is the gate, and `approximate_join_order_threshold` defaults to **12**
(`src/include/duckdb/main/settings.hpp:308-320`, default at :316) — the *same*
number as postgres's `geqo_threshold`. So the two hatches divide the work: the
relation gate catches every query with 12 or more tables regardless of shape,
and the 10,000-pair budget catches the dense graphs below that; :597 sends both
"ran out of budget" and "finished without a plan covering every relation" to
the heuristic. Lines 594-595 are a third, quieter path: when exact search
finishes but the graph is disconnected, `ActivateRequiredCrossProducts` adds
the cross-product edges the query needs and the exact search runs once more. A
15-relation *chain* emits only 560 pairs and would be trivially solvable
exactly — but at n = 15 the :590 gate has already sent it to the heuristic.

The heuristic is `SolveJoinOrderApproximately` (:408), and the comment at
:409-411 names it: **Greedy Operator Ordering** — start with every base relation
as its own tree, then repeatedly combine the pair with the lowest cost. The
comment at :417-419 states how it keeps that cheap: "Pair costs are
deterministic while the query graph is unchanged, so we evaluate each pair once
and keep the viable candidates in a list, instead of re-emitting every pair in
every step". Count the expensive calls — `EmitPair`, which builds and costs a
join — for r relations: r(r−1)/2 to fill the candidate list, then after the
k-th merge one new candidate per surviving relation, r−1−k of them. For
r = 15 that is 105 + (13 + 12 + … + 1) = 105 + 91 = **196** costed pairs for
the whole greedy run, where costing every remaining pair at every step would
take Σ C(r−k+1, 2) over the 14 steps = 560. Choosing the minimum is still a
scan of the candidate list per step. Greedy can be badly wrong, but a mediocre
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
// 898-921 (the leading comment names the method's source)
   898  // Cardinality is calculated using logic based on
   899  // https://blobs.duckdb.org/papers/tom-ebergen-msc-thesis-join-order-optimization-with-almost-no-statistics.pdf
   // ... 900-904: INNER equality predicates use transitive equality classes; composite
   //              same-pair equalities can apply an FK/PK cap; disconnected predicate
   //              subgraphs are merged by cross product; LEFT/SEMI/ANTI adjust the numerator ...
   905  template <>
   906  double CardinalityEstimator::EstimateCardinalityWithSet(JoinRelationSet &new_set) {
   907  	double result;
   908  	auto it = state->relation_set_2_cardinality.find(new_set);
   909  	if (it != state->relation_set_2_cardinality.end()) {
   910  		result = it->second.cardinality_before_filters;
   911  	} else {
   // ... 912: comment on zero cardinalities and tdoms ...
   913  		auto denom = GetDenominator(new_set);
   // ... 914-915: comment — semi/anti joins leave the RHS out of the numerator ...
   916  		auto numerator = GetNumerator(denom.numerator_relations);
   917  		result = numerator / denom.denominator;
   918  		state->relation_set_2_cardinality[new_set] = CardinalityHelper(result);
   919  	}
   920  	return ApplyOrFilterSelectivities(new_set, result);
   921  }
```

Line 917 is the entire model. `GetNumerator` (:337-350) is a plain product of
the base relations' row counts — line 347, `numerator *= cardinality_before_filters`.
`GetDenominator` (:889) walks the edges and multiplies in one **total domain**
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

One constant deserves care, because it is the most-repeated wrong claim about
this file. DuckDB does have a `DEFAULT_SELECTIVITY`, but it is **0.2** —
`src/include/duckdb/optimizer/relation_statistics/relation_statistics_helper.hpp:32`,
`static constexpr double DEFAULT_SELECTIVITY = 0.2;` — **not** postgres's
0.005. It is 40× larger, and it is not a general "unknown predicate" fallback
either. The statistics helpers live in their own directory,
`src/optimizer/relation_statistics/`, shared by the join-order pass and by other
passes that want relation statistics, and the constant is multiplied in at
eight sites, in four situations:

1. `relation_statistics_scan.cpp:159-162` — a base table with non-optional
   filters, none of which its column statistics could estimate, gets
   `max(base_cardinality × 0.2, 1)`.
2. `relation_statistics_operator.cpp:178-187` — a `LOGICAL_FILTER` whose
   statistics are derived from its child's: `max(child × 0.2, 1)`, whatever
   the filter says.
3. `cardinality_estimator.cpp:923-930` — `ApplyOrFilterSelectivities`
   multiplies a relation set's estimate by 0.2 (:926) once per OR filter
   covering it.
4. Five sites in `relation_manager.cpp` — :396-399, :460-463, :481-484,
   :501-504 and :580-583 — one per kind of relation (a generic relation with
   children, a non-reorderable operator, an aggregate, a window, a table scan),
   each multiplying the relation's cardinality by 0.2 once when
   `datasource_filters` is non-empty. That list is filled at :413-416: a
   `LOGICAL_FILTER` found above the relation whose child is non-reorderable,
   i.e. a filter the join-order pass cannot turn into hypergraph edges; the
   comment at :577-578 says it plainly — "if there is another logical filter
   that could not be pushed down into the table scan, apply another
   selectivity."

And once as its complement: `relation_statistics_scan.cpp:248-253` gives a
`NOT LIKE` filter (`!~~`) on a column that cannot hold NULL a selectivity of
1 − 0.2 = 0.8.

Situations 1 and 4 compound. A 10,000-row table scan with a filter its
statistics cannot estimate is `max(10,000 × 0.2, 1)` = 2,000 rows at site 1;
if a second filter sits above it that the join-order pass could not decompose,
site 4's table-scan arm (:580-583) multiplies again: 2,000 × 0.2 = **400**
rows, regardless of what either filter tests. Everything else goes through the
numerator/denominator above.

Why it matters: these estimates are the *only* signal ranking the plans Step 5
enumerates, and the guess above is arithmetic over two statistics per column.
There is no histogram anywhere in this path.

### Step 7 — the cost model is (almost) just cardinality

> **In:** Step 6's estimate for the combined set, plus the already-computed
> costs of the two child plans from the Step 5 memo.
> **Out:** one `double` — the number `EmitPair` compares at :208-213 to decide
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
`DPJoinNode` is constructed with `cost(0)` (`join_node.cpp:11`) and
`InitLeafPlans` sets `join_node->cost = 0` explicitly at
`plan_enumerator.cpp:572` — so base-table scans are *free* in this model and a
plan's cost is precisely the sum of its intermediate join outputs. Cout,
exactly. Lines 44-46 are the one exception, and the
comment at :38-39 explains it honestly: a LEFT join's output cardinality equals
its left input's, so without a term for the right input, joining a huge RHS
early looks free. (A summary that says "cost is cardinality, that's it" is
right about :43 and wrong about :45; the file has both.)

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
`BUILD_SIDE_PROBE_SIDE` pass at `optimizer.cpp:417`, using column-lifetime
information the join-order pass did not have.

Why it matters: every knob you might want to tune in this optimizer — a CPU
weight, an IO constant, a sort penalty — does not exist. The only thing you can
improve is Step 6.

## Where each step lives in the code

Read in this order: `optimizer.cpp`, then `filter_pushdown.cpp`, then the
`join_order/` subdirectory — the payoff. All line numbers are `d54cbbe3df`
(v2.0.0).

| Step | File | Lines | What |
|---|---|---|---|
| 2 | `src/optimizer/optimizer.cpp` | 136-155 | `RunOptimizer` — times each pass and `Verify`s the plan after it |
| 2 | `src/planner/logical_plan_verifier.cpp` | 326-335 | `LogicalPlanVerifier::Verify` — gated by `debug_verify_column_bindings` at :327 |
| 2 | `src/optimizer/optimizer.cpp` | 229 | `RunBuiltInOptimizers` — the pass list's own function |
| 2 | `src/optimizer/optimizer.cpp` | 252-549 | the 44 `RunOptimizer` calls, in order: pullup :271, pushdown :277, JOIN_ORDER :355, build/probe :417, TOP_N :450, the guarded second unused-columns :504-505 |
| 2 | `src/optimizer/optimizer.cpp` | 565-605 | `Optimizer::Optimize` — extensions, then `RunBuiltInOptimizers` at :592 |
| 3 | `src/optimizer/filter_pushdown.cpp` | 108-163 | `Rewrite` — barrier filters :110-117, then the dispatch table, one arm per operator |
| 3 | `src/optimizer/filter_pushdown.cpp` | 317-325 | `FinishPushdown` — the safe default: fresh child pushdown, filters deposited |
| 3 | `src/optimizer/pushdown/pushdown_left_join.cpp` | 111-176 | side classification :137, LEFT push :146, `FilterRemovesNull` → INNER :157-169 |
| 4 | `src/optimizer/join_order/query_graph_manager.cpp` | 219-253 | `Build` — relations :222, edges :229, conflict descriptors :232-244, hypergraph :251 |
| 4 | `src/optimizer/join_order/query_graph_manager.cpp` | 399-463 | `CreateHyperGraphEdges` — each predicate added in both directions (:424-425); operator edges from total eligibility sets (:452-453) |
| 4 | `src/optimizer/join_order/relation_manager.cpp` | 233, 404, 858 | `JoinIsReorderable`, `ExtractJoinRelations`, `ExtractEdges` — where the fork's two datasets are built |
| 4 | `src/include/duckdb/optimizer/join_order/join_order_operator.hpp` | 31-66 | `JoinOrderOperator` — the CD-C conflict descriptor for LEFT/SEMI/ANTI joins |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 192-235 | `EmitPair` — the memo; :215 keeps one plan per relation set |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 237-251 | `TryEmitPair` — the 10,000-pair budget at :243 |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 253, 305, 348, 385 | `EmitCSG`, `EnumerateCmpRecursive`, `EnumerateCSGRecursive`, `SolveJoinOrderExactly` — the DPhyp enumeration |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 408-523 | `SolveJoinOrderApproximately` — Greedy Operator Ordering, each pair costed once (:417-419) |
| 5 | `src/optimizer/join_order/plan_enumerator.cpp` | 580-603 | the DPhyp citation, and `SolveJoinOrder`'s escape hatches |
| 5 | `src/include/duckdb/main/settings.hpp` | 308-320 | `approximate_join_order_threshold`, default `"12"` at :316 |
| 6 | `src/optimizer/join_order/cardinality_estimator.cpp` | 898-921 | `EstimateCardinalityWithSet` — `numerator / denominator` at :917 |
| 6 | `src/optimizer/join_order/cardinality_estimator.cpp` | 337-350, 889-896 | `GetNumerator` (product of base rows), `GetDenominator` (total domains) |
| 6 | `src/include/duckdb/optimizer/relation_statistics/relation_statistics_helper.hpp` | 32 | `DEFAULT_SELECTIVITY = 0.2` |
| 6 | `src/optimizer/relation_statistics/relation_statistics_scan.cpp` | 159-162, 248-253 | the base-table fallback (× 0.2) and the `NOT LIKE` complement (× 0.8) |
| 7 | `src/optimizer/join_order/cost_model.cpp` | 37-48 | `ComputeCost` — Cout at :43/:47, the LEFT-join correction at :44-46 |
| 7 | `src/optimizer/join_order/join_node.cpp` | 9-12 | a leaf `DPJoinNode` is built with `cost(0)` — base scans are free |

Suggested route: `optimizer.cpp:565` → `:229` and read the list top to bottom
→ `filter_pushdown.cpp:108` → `pushdown_left_join.cpp:111` → then
`join_order/`: `query_graph_manager.cpp:219` → `plan_enumerator.cpp:583` (start
at the *end*, where the hatches are) → `:385` → `:192` →
`cardinality_estimator.cpp:906` → `cost_model.cpp:40`.

## Questions for notes.md

1. Why does pullup-then-pushdown beat pushdown alone? Find one operator in
   `src/optimizer/pullup/` where hoisting first enables a deeper sink, and name
   the file.
2. The memo keeps one best plan per relation set (`plan_enumerator.cpp:215`).
   What plan property does that discard that postgres keeps (hint: interesting
   orders) — and why does DuckDB get away with it? Which physical operator's
   dominance is the answer?
3. Step 5's table says a 9-clique emits 9,330 csg-cmp-pairs and a 10-clique
   28,501. At what n does a *star* schema cross the 10,000-pair budget, and
   does the :590 relation gate fire before or after that? Work it from the
   table.
4. Cost is output cardinality only (`cost_model.cpp:43`), so build-vs-probe
   side is chosen later, at `optimizer.cpp:417`. What does splitting
   order-choice from side-choice lose? Construct a case where the best order
   under Cout has the worse build side.
5. M10: a Cypher chain `(a)-[:R]->(b)-[:S]->(c)` is a chain query graph over
   edge relations. Which DuckDB piece maps to anchor-node selection — the
   enumerator (`plan_enumerator.cpp`) or the cardinality estimator? Justify
   with the fork in Step 4.

## Takeaway

The pipeline is 44 passes in a fixed order, each timed and each followed by a
verifier hook; the join search is DPhyp, with a conflict detector keeping
non-inner joins legal, a 12-relation gate and a 10,000-pair budget behind it; the cost model is
Cout plus one LEFT-join correction; and the estimator is a product of row
counts over a product of distinct counts, with no histogram in sight. For M10:
copy the fork (Step 4) and the budget (Step 5), and expect every plan bug you
have to be an estimate bug.

## Done when

Answer each before unfolding it.

- [ ] You can name the pass order in coarse buckets, and say why filter pullup runs before filter pushdown.

  <details><summary>Answer</summary>

  Coarse buckets, in the order `RunBuiltInOptimizers` (`optimizer.cpp:229`)
  runs them: expression-level simplification (:252), CTE handling (:259),
  filter movement (pullup :271, pushdown :277), subquery decorrelation
  (deliminator :307), structural simplification (projection pullup :342, outer
  join simplification :348), **join ordering** (:355) and the aggregate and
  join rewrites right after it, column pruning (:392) and
  common-subexpression work (:404), physical-ish choices (build/probe side
  :417), then limit/top-n (:433, :450), statistics-driven work and a second
  column pruning (:505, only when statistics propagation removed expressions),
  and a final tail out to scalar-function pushdown at :549. Forty-four
  `RunOptimizer` calls, 41 distinct types — `CTE_INLINING`, `UNUSED_COLUMNS`
  and `COLUMN_LIFETIME` each appear twice.

  Pullup runs first because it *un*-blocks pushdown. A filter stranded inside
  one branch of a subtree can often be hoisted to a point where it applies to
  both branches; once hoisted, the pushdown pass at :277 can sink it into each
  of them, deeper than it started. Running pushdown alone would leave it where
  it was. The pipeline's order is expert knowledge encoded as a list, which is
  also its weakness: a rewrite that only becomes possible after a later pass is
  simply never found, and nothing reports that.

  </details>

- [ ] You can explain DPhyp, the greedy fallback, and both escape hatches — with the actual thresholds.

  <details><summary>Answer</summary>

  The enumerator walks *connected subgraph / connected complement* pairs of the
  query graph, smallest first, and for each pair calls `EmitPair`
  (`plan_enumerator.cpp:192`), which builds the join and keeps it in the `plans`
  memo at :215 only if it beats what is already stored for that relation set.
  One plan per set, cheapest wins. The file cites *Dynamic Programming Strikes
  Back* (Moerkotte and Neumann) at :580-582 as its source.

  Two hatches, in the order they fire. `SolveJoinOrder` (:583) first compares the
  relation count against `approximate_join_order_threshold`, default **12**
  (`settings.hpp:316`), at :590 — twelve or more tables goes straight to
  greedy, whatever the shape. Otherwise it tries exact, and `TryEmitPair`
  (:237) aborts the moment its `pairs` counter reaches **10,000** (:243),
  returning `false` all the way up so :597 falls through to greedy at :599.

  The greedy is `SolveJoinOrderApproximately` (:408): Greedy Operator Ordering,
  which starts with every relation as its own tree and repeatedly merges the
  cheapest pair. Its comment at :417-419 says each pair is costed once and kept
  in a candidate list, so for r relations it costs r(r−1)/2 pairs up front and
  r−1−k more after the k-th merge — 196 for r = 15. Concretely: a 9-relation
  clique emits 9,330 csg-cmp-pairs and is solved exactly; a 10-relation clique
  emits 28,501 and is not; a 15-relation chain emits only 560 but is sent to
  greedy anyway, by the :590 gate.

  </details>

- [ ] You can write DuckDB's cardinality formula from memory and run it on three tables, with and without the independence assumption.

  <details><summary>Answer</summary>

  `card(S) = (Π over R in S of |R|) / (Π over equality classes E of tdom(E))` —
  `cardinality_estimator.cpp:917`, with `GetNumerator` (:337-350) supplying the
  product of base row counts at :347 and `GetDenominator` (:889) supplying one
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
  `src/include/duckdb/optimizer/relation_statistics/relation_statistics_helper.hpp:32`.
  Postgres's `DEFAULT_EQ_SEL` is 0.005; the two are not the same constant and
  DuckDB's is 40× larger. Anything that calls DuckDB's "its version of
  postgres's famous 0.005" is repeating a claim the source does not support.

  It is also not a general fallback for predicates the estimator cannot reason
  about. It is multiplied in at eight sites, in four situations. At
  `relation_statistics_scan.cpp:159-162`, a base table that has non-optional
  filters but none its statistics could estimate gets
  `max(base_cardinality × 0.2, 1)`. At `relation_statistics_operator.cpp:178-187`,
  a `LOGICAL_FILTER` whose statistics are derived from its child gets
  `max(child × 0.2, 1)`. At `cardinality_estimator.cpp:923-930`,
  `ApplyOrFilterSelectivities` multiplies a relation set's estimate by 0.2 once
  per OR filter covering it. And at five sites in `relation_manager.cpp`
  (:396-399, :460-463, :481-484, :501-504, :580-583), a relation with a filter
  above it that the join-order pass could not decompose — collected into
  `datasource_filters` at :413-416 — has its cardinality multiplied by 0.2 once.
  It also appears once as its complement: a `NOT LIKE` on a non-NULL column
  keeps 0.8 (`relation_statistics_scan.cpp:248-253`). The first and last
  situations compound — a 10,000-row scan becomes 2,000 and then 400.
  Everything else in the join-order path goes through
  `numerator / denominator` at `cardinality_estimator.cpp:917`.

  </details>

- [ ] You can say why the join-order search cannot fix a bad estimate, and where in the code the two are wired together.

  <details><summary>Answer</summary>

  Because search and estimation are not two independent quality knobs — the
  search *consumes* the estimate as its only ranking signal. `ComputeCost`
  (`cost_model.cpp:40`) calls `EstimateCardinalityWithSet` at :42, adds the
  children's costs at :47, and returns; `EmitPair` (`plan_enumerator.cpp:192`)
  compares that number at :208-213 and keeps the smaller. There is no other
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
- [duckdb](https://github.com/duckdb/duckdb) v2.0.0 at `d54cbbe3df` —
  `src/optimizer/`: `optimizer.cpp` (the pass pipeline, read :229-553 top to
  bottom), `filter_pushdown.cpp` + `pushdown/`, `join_order/`
  (`plan_enumerator.cpp`, `cardinality_estimator.cpp`, `cost_model.cpp`) and
  `relation_statistics/`; ~2 h.
- Tom Ebergen, *Join Order Optimization with (Almost) No Statistics* (MSc
  thesis) — the method `cardinality_estimator.cpp:898-899` cites for its
  numerator/denominator model.
- Moerkotte and Neumann, *Dynamic Programming Strikes Back* (SIGMOD 2008) — the
  enumeration algorithm `plan_enumerator.cpp:580-582` says it implements.
- Moerkotte, Fender and Eich, *On the Correct and Complete Enumeration of the
  Core Search Space* (SIGMOD 2013) — the conflict detector (CD-C)
  `join_order_operator.hpp:31-34` says it follows.

**Papers**
- Leis, Gubichev, Mirchev, Boncz, Kemper, Neumann — *How Good Are Query
  Optimizers, Really?* (VLDB 2015). Used here for three figures, each from a
  named section: §6.1 (average worst-to-best plan ratio 101× / 115× / 48,120×
  by index configuration), §5.4 (a trivial main-memory cost model with true
  cardinalities beats PostgreSQL's own by 34% in geometric mean), and Table 3
  (exhaustive DP scores 1.00/1.00/1.00 with true cardinalities and
  1.66/169/186,367 with PostgreSQL's). Read in full via
  `reading-how-good-optimizers.md`.
