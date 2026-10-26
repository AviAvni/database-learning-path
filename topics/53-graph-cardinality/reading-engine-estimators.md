# What four graph engines estimate: two products, one constant, one score

The papers of the previous chapters propose estimators; this one reads the
ones that ship. Kùzu and Neo4j multiply averages. Memgraph multiplies a
constant. FalkorDB — the engine this repository's reader works on, in both
its C form and its Rust rewrite — does not estimate a multi-hop pattern
at all: it scores the pattern's endpoints by whether they are bound,
filtered, labelled, and how many nodes the label has. Where "multi-hop
estimation dies" (the question left open in topic 13's
[notes](../13-graph-engines/notes.md) and
[reading-ldbc-snb.md](../13-graph-engines/reading-ldbc-snb.md)) is therefore
an answer with two parts: in three engines at the independence assumption,
which lane 1 measures; in the fourth, before it is ever attempted.

All code anchors are read at the commits in the pin table of
[resources/codebases.md](../../resources/codebases.md): `kuzu` at `89f0263`,
`neo4j` at `eccd584a`, `memgraph` at `8f87f6a`, `FalkorDB` at `ccb449a9a`
and `falkordb-rs-next-gen` at `9d28bdc6`, read with
`tools/pinned-source.py`. Kùzu's pin is the commit topic 13's
[reading-kuzu.md](../13-graph-engines/reading-kuzu.md) was read at.

## The problem in one sentence

When a planner must choose where to start a multi-hop `MATCH` and in which
order to expand, which numbers does each engine actually use to guess how
many rows each choice produces — and what do those numbers assume?

## The concepts, step by step

### Step 1 — the independence product, the common core

> **In:** lane 1's graph (four labels, five edge types, shared popularity)
> and its exact path counts. **Out:** the one formula Steps 2–3 implement
> and the by-hop q-errors it earns.

For a path `(a)-[:t0]->(b)-[:t1]->(c)…` with every node labelled, the
estimate that Kùzu and Neo4j compute is

```
est(t0 … t_{k-1}) = |E_t0| · Π_{i=1}^{k-1} |E_ti| / |L_src(ti)|
```

— the first edge type's count, times, per further hop, the **average
out-degree** of the next type (its edge count over the number of nodes
carrying its source label). Worked on lane 1's seed-1 graph with
`|E| = [34,374, 53,343, 80,401, 51,749, 28,132]` and label sizes
`[3,000, 6,000, 12,000, 6,000]`:

```
est(r0 r1)       = 34,374 · 53,343 / 6,000                  = 305,602.0      (true 3,009,074)
est(r0 r1 r2)    = 305,602.0 · 80,401 / 12,000              = 2,047,559.2    (true 124,840,475)
est(r0 r1 r2 r3) = 2,047,559.2 · 51,749 / 6,000             = 17,659,856.7   (true 6,150,191,151)
```

The q-errors are 9.8, 61.0 and 348.3, all underestimates. Pooled over five
seeds and every label-consistent path (30, 35 and 45 patterns), the median
q-error is **9.6, 57.5 and 324.0** at two, three and four hops, with 100%
of the estimates below the truth — and **1.1, 1.1, 1.1** on the
control graph where popularity is independent (see the
[README](README.md)). The estimator is not bad; it is exact on its own
premise and loses a factor of ~6 per hop when the premise (a node's in-degree
and out-degree are unrelated) fails.

### Step 2 — Kùzu: the extension rate, applied to a factorized plan

> **In:** Step 1's formula. **Out:** where it sits in Kùzu's planner and
> the three details that are not in the formula: the table-level
> divisor, factorization, and the constants for everything else.

Kùzu's per-hop factor is `getExtensionRate`:

```cpp
// kuzu src/planner/join_order/cardinality_estimator.cpp, pinned 89f0263
   214  double CardinalityEstimator::getExtensionRate(const RelExpression& rel,
   215      const NodeExpression& boundNode, const Transaction* transaction) const {
   216      auto numBoundNodes = static_cast<double>(getNumNodes(transaction, boundNode.getTableIDs()));
   217      auto numRels = static_cast<double>(getNumRels(transaction, rel.getInnerRelTableIDs()));
   218      KU_ASSERT(numBoundNodes > 0);
   219      auto oneHopExtensionRate = numRels / atLeastOne(numBoundNodes);
   220      switch (rel.getRelType()) {
   221      case QueryRelType::NON_RECURSIVE: {
   222          return oneHopExtensionRate;
   223      }
```

The line to read is 219, `numRels / atLeastOne(numBoundNodes)`:
`numRels` is the number of rows of the relationship table(s) (`:204-212`,
summed over the tables the pattern could use) and `numBoundNodes` the node
count of the *table(s)* the bound variable ranges over (`:194-202`). That is
Step 1's average out-degree, with the table as the unit — in Kùzu's schema a
node label is a node table, so a labelled endpoint divides by its label's
size; an unlabelled one by the sum over all node tables. Variable-length
patterns multiply by `max(upperBound, 1)` and a scale factor (`:224-239`),
a heuristic, not a measurement.

The rate is not applied where you might expect:

```cpp
// kuzu src/planner/plan/append_extend.cpp, pinned 89f0263
    86      auto transaction = Transaction::Get(*clientContext);
    87      const auto extensionRate = cardinalityEstimator.getExtensionRate(*rel, *boundNode, transaction);
    88      extend->setCardinality(plan.getLastOperator()->getCardinality());
    89      plan.setCost(CostModel::computeExtendCost(plan));
    90      auto group = extend->getSchema()->getGroup(nbrNode->getInternalID());
    91      group->setMultiplier(extensionRate);
```

`extend->setCardinality(plan.getLastOperator()->getCardinality())` (line 88)
leaves the operator's cardinality unchanged and the rate goes onto a
*multiplier* of the output group (line 91): Kùzu's executor is
**factorized**, keeping a node's neighbour list as a group instead of
materialising one tuple per neighbour, so the cardinality grows only when
the group is **flattened** (`estimateFlatten`, `cardinality_estimator.cpp:
133-137`, `childOp.getCardinality() * group->cardinalityMultiplier`). The
independence product is the same; where its error lands in the cost model
depends on where the planner flattens.

The other formulas are the same family, with constants where no statistic
exists. A hash join whose keys are node IDs estimates
`probe · JoinKeysFlatCardinality(build) / Π domain sizes` (`:96-98`); any
other join `probe · build · 0.01` per equality condition (`:101-104`, with
`PlannerKnobs::EQUALITY_PREDICATE_SELECTIVITY = 0.01` and
`NON_EQUALITY_PREDICATE_SELECTIVITY = 0.1`, `src/include/common/
constants.h:159-160`); an equality filter on a column divides by its number
of distinct values when the table statistics have one, and by 100 when they
do not (`:174-187`). The intersect used for cyclic patterns takes the *minimum*
of "a filter at 0.1 of the probe side" and "independent joins on every
join-key domain" (`:117-130`) — Step 1's closing probability, hedged against
the flat 0.1.

### Step 3 — Neo4j: node cardinalities times per-relationship multipliers

> **In:** Step 1's formula. **Out:** Neo4j's version of it, the statistic it
> reads, and a proof that for a labelled path it is the same number.

Neo4j's `AssumeIndependenceQueryGraphCardinalityModel` computes a query
graph's cardinality as three factors, the last two shown here:

```scala
// neo4j community/cypher/cypher-planner/src/main/scala/org/neo4j/cypher/internal/compiler/planner/logical/cardinality/assumeIndependence/AssumeIndependenceQueryGraphCardinalityModel.scala, pinned eccd584a
   197      val nodesCardinality =
   198        queryGraph.patternNodes
   199          .diff(boundNodesAndArguments.boundNodes)
   200          .toList
   201          .map { node =>
   202            if (boundNodesAndArguments.argumentIds.contains(node)) {
   203              // In case the node is passed as an argument in the query graph (or indeed the endpoint of a relationship passed as an argument),
   204              // then we apply the selectivity of the additional labels defined in this query graph but not in the previous ones.
   205              // For example: MATCH (n:A) OPTIONAL MATCH (n:B) <- we would apply the selectivity of label B here
   206              Cardinality(getArgumentSelectivity(context, predicates.localOnlyLabelInfo, node).factor)
   207            } else
   208              getNodeCardinality(context, predicates.localOnlyLabelInfo, node).getOrElse(nodeWithNoLabelsCardinality)
   209          }.product(NumericCardinality)
   210
   211      val otherPredicatesSelectivity =
   212        context.predicatesSelectivity(predicates.allLabelInfo, predicates.otherPredicates)
   213
   214      nodesCardinality *
   215        nodeConnectionMultipliers.product(NumericMultiplier) *
   216        otherPredicatesSelectivity
```

(lines 176–191, not shown, fold the query graph's node connections —
relationships, quantified path patterns — into `nodeConnectionMultipliers`
and thread a set of *bound* nodes through the fold.) The product at 214–216
is `nodesCardinality · Π multipliers · selectivity of the remaining
predicates`, where `nodesCardinality` (197–209) covers only the pattern nodes
that no connection has bound (`patternNodes.diff(boundNodes)`, line 199) —
isolated nodes. Each relationship's multiplier is computed as its endpoints
become bound:

```scala
// neo4j community/cypher/cypher-planner/src/main/scala/org/neo4j/cypher/internal/compiler/planner/logical/cardinality/assumeIndependence/NodeConnectionCardinalityModel.scala, pinned eccd584a
   142      // First calculate the cardinality of the left node
   143      val leftNodeCardinality = getBoundEndpointCardinality(context, predicates, nodeConnection.left)
   144      // Then calculate the cardinality of the right node, having marked the left node as bound, in cases both endpoints are the same node
   145      val rightNodeCardinality =
   146        copy(boundNodes = boundNodes.incl(nodeConnection.left))
   147          .getBoundEndpointCardinality(context, predicates, nodeConnection.right)
   148      val multiplier =
   149        Multiplier.ofDivision(
   150          dividend = nodeConnectionCardinality,
   151          divisor = leftNodeCardinality * rightNodeCardinality
   152        ).getOrElse(Multiplier.ZERO)
   153      (copy(boundNodes = boundNodes.union(Set(nodeConnection.left, nodeConnection.right))), multiplier)
```

`nodeConnectionCardinality / (leftNodeCardinality · rightNodeCardinality)`
(lines 148–152): the relationship's count over the product of its endpoint
cardinalities, where an endpoint already bound counts as its label's
cardinality and an unbound one as 1 (`:156-167`). The relationship count
itself is one number read from the store:

```scala
// neo4j community/cypher/cypher/src/main/scala/org/neo4j/cypher/internal/spi/TransactionBoundGraphStatistics.scala, pinned eccd584a
   144      override def patternStepCardinality(
   145        fromLabel: Option[LabelId],
   146        relTypeId: Option[RelTypeId],
   147        toLabel: Option[LabelId]
   148      ): Cardinality =
   149        Cardinality(read.estimateCountsForRelationships(fromLabel, relTypeId, toLabel))
```

`estimateCountsForRelationships(fromLabel, relType, toLabel)` is a lookup
in the **counts store** of the number of `(:from)-[:type]->(:to)` edges —
exactly the catalog entry `F1[t]` of lane 3, kept per *label triple*. When
a pattern has several labels on an endpoint, the model computes the
cardinality for each combination and takes the **minimum**
(`PatternRelationshipCardinalityModel.scala:334`).

Now the proof that this is Step 1's number. Take
`(a:A)-[:r]->(b:B)-[:s]->(c:C)`, and let `E_r`, `E_s` be the label-typed
counts. When the first relationship is processed nothing is bound, so both
endpoint cardinalities are 1 (`Cardinality.SINGLE`, line 166) and its
multiplier is `E_r / 1 = E_r`; that binds `a` and `b`. For the second, `b`
is bound and costs `|B|` (line 162) while `c` is not and costs 1, so the
multiplier is `E_s / |B|`. Every node is covered by a connection, so
`nodesCardinality` is the empty product 1. The estimate is
`E_r · E_s / |B|`. Step 1's formula for the same path is
`E_r · E_s / |L_src(s)|` and `L_src(s) = B`.
On lane 1's graph, `E_{r0} · E_{r1} / |L_1| = 34,374 · 53,343 / 6,000 =
305,602.0`: Neo4j's model and Kùzu's give the identical estimate, and so does
the `indep_path` in `experiments/src/est.rs`. **Two engines, one estimator**;
the difference between them is whether the factor lands in a flat or a
factorized cost model.

### Step 4 — Memgraph: a constant per Expand

> **In:** Step 1's formula. **Out:** what Memgraph multiplies instead of an
> average degree, and what a supplied statistic changes.

Memgraph's cost estimator walks the plan's operators and multiplies a
running cardinality at each:

```cpp
// memgraph src/query/plan/cost_estimator.hpp, pinned 8f87f6a
   368    bool PostVisit(Expand &expand) override {
   369      auto card_param = CardParam::kExpand;
   370      auto stats = GetStatsFor(expand.input_symbol_);
   371
   372      if (stats) {
   373        card_param = stats->degree;
   374      }
   375
   376      cardinality_ *= card_param;
   377      IncrementCost(CostParam::kExpand);
   378
   379      return true;
   380    }
```

`card_param` starts as the constant `CardParam::kExpand` (line 369) and is
replaced by `stats->degree` (line 373) only when the input symbol has
supplied `SymbolStatistics{count, degree}` (`:29-32`).

```cpp
// memgraph src/query/plan/cost_constants.hpp, pinned 8f87f6a
    46  namespace CardParam {
    47  inline constexpr double kExpand{3.0};
    48  inline constexpr double kExpandVariable{9.0};
    49  inline constexpr double kFilter{0.25};
    50  inline constexpr double kEdgeUniquenessFilter{0.95};
```

`kExpand = 3.0`: every `Expand` is assumed to triple the row count, and a
filter to keep a quarter (`kFilter = 0.25`). A scan by label multiplies by the
label's vertex count (`:114`) and a scan by edge type by the edge count
(`:158`). For a `k`-hop path from a label scan the estimate is `|L_start| · 3^k`.

On lane 1's graph that is 27,000, 81,000 and 243,000 for `r0 r1`,
`r0 r1 r2` and `r0 r1 r2 r3` from `L0` (3,000 nodes), against true counts
of 3.0 M, 124.8 M and 6.15 B — q-errors 111, 1,541 and 25,309 in the seed-1
detail, **62.2, 822.1 and 12,425.7** as medians pooled over seeds. The
control graph shows the cost of a constant that has nothing to do with the
data: median q-errors **6.4, 18.6, 40.1** even though popularity there is
independent, because this graph's average out-degrees are 11.5, 8.9, 6.7
and 8.6 for `r0` to `r3`, not 3. A constant of 3 fits a graph whose average
degree is 3; the engine's way out is the statistics argument, which this
guide did not trace to its producer.

### Step 5 — FalkorDB: no estimate, a score and a label count

> **In:** the question "what would FalkorDB's planner say about lane 1's
> pattern?" **Out:** the answer — it never produces a number for the
> pattern — and the three places statistics do appear.

FalkorDB's C planner orders a pattern's algebraic expressions with
`orderExpressions` (`traverse_order.c:204`), which scores each expression
in three phases. The first phase scores by labels:

```c
// FalkorDB src/execution_plan/optimizations/traverse_order_utils.c, pinned ccb449a9a
    36  // score expression based on its source and destination labels
    37  // score = src label count + dest label count
    38  int TraverseOrder_LabelsScore
    39  (
    40  	AlgebraicExpression *exp,
    41  	const QueryGraph *qg
    42  ) {
    43  	ASSERT(qg  != NULL);
    44  	ASSERT(exp != NULL);
    45
    46  	int        score      = 0;
    47  	const char *src       = AlgebraicExpression_Src(exp);
    48  	const char *dest      = AlgebraicExpression_Dest(exp);
    49  	QGNode     *src_node  = QueryGraph_GetNodeByAlias(qg, src);
    50  	QGNode     *dest_node = QueryGraph_GetNodeByAlias(qg, dest);
    51
    52  	score += QGNode_LabelCount(src_node);
    53  	score += QGNode_LabelCount(dest_node);
```

`QGNode_LabelCount` is the *number of labels written on the query node* —
`(:A:B)` counts 2, `(a)` counts 0 — not the number of nodes with that label.
So phase 1 prefers expressions whose endpoints are more heavily
label-constrained, and carries no information about the data. Phase 2 adds
points for filtered endpoints (`+2` plus twice the number of independent
predicates, `+1` for a filtered edge, `:90-125`) and phase 3 for variables
already bound (`+1` each, `:129-157`); each phase's score is added to the
maximum of the previous one, so a bound variable beats a filtered one beats
a labelled one (`:225-238`).

The statistics that do exist are node counts. The `costBaseLabelScan`
optimization (registered at `optimizer.c:57`) may change which end of a
chain is scanned:

```c
// FalkorDB src/execution_plan/optimizations/cost_base_label_scan.c, pinned ccb449a9a
   159  	// determine nim number of entities for both source node and dest node
   160  	// src_min  = MIN(NVALS(src_lbls))
   161  	// dest_min = MIN(NVALS(dest_lbls))
   162  	const char *min_lbl    = NULL;
   163  	LabelID     min_lbl_id = GRAPH_NO_LABEL;
   164
   165  	// src_min is initialized with the label-scan operation's matrix nvlas
   166  	uint64_t src_min  = Graph_LabeledNodeCount(g, scan_ctx->label_id);
   167  	uint64_t dest_min = UINT64_MAX;
// ... 168–215: elided ...
   216  	// check if we should replace the current label scan operation
   217  	if(dest_min >= src_min) {
   218  		// source will produce less entities, keep things as they are
   219  		return false;
   220  	}
```

The comparison at 217 is `dest_min >= src_min`: scan the destination's
smallest label instead of the source's when it has fewer nodes
(`Graph_LabeledNodeCount`). That is the whole of the data-driven choice:
**which end of the pattern has the smaller label**. Under
`src/execution_plan` the data statistics read are node counts per label
(`Graph_LabeledNodeCount` in `cost_base_label_scan.c`, `reduce_count.c:108`,
`utilize_indices.c:517`, `op_cond_var_len_traverse.c:186`) and edge totals to
answer a bare `count(*)` without traversing (`reduce_count.c:207-212`); no
pass reads a relationship matrix's `nvals`, an average degree or a degree
histogram to price a hop. Execution
multiplies the matrices in the order the planner left them:

```c
// FalkorDB src/arithmetic/algebraic_expression/algebraic_expression_mul.c, pinned ccb449a9a
    36  	for(uint i = 0; i < child_count; i++) {
    37  		c = CHILD_AT(exp, i) ;
    38  		ASSERT(c->type == AL_OPERAND) ;
    39
    40  		M = c->operand.matrix ;
    41
    42  		// first time A is set
    43  		if(A == NULL) {
    44  			A = M ;
    45  			continue ;
    46  		}
    47
    48  		// both A and M are valid matrices, perform multiplication
    49  		info = Delta_mxm(res, semiring, A, M);
    50  		res_modified = true ;
    51  		// setup for next iteration
    52  		A = res ;
    53
    54  		// exit early if 'res' is empty 0 * A = 0
    55  		info = Delta_Matrix_nvals(&nvals, res);
    56  		ASSERT(info == GrB_SUCCESS) ;
    57  		if(nvals == 0) break ;
    58  	}
```

`_Eval_Mul` is a left-to-right fold (`A = res` at line 52); the one data-
dependent line is 57, `if(nvals == 0) break` — an early exit when a partial
product is empty, not a choice of association. The semiring is
`GxB_ANY_PAIR_BOOL` (line 33): an intermediate holds *which* `(src, dst)`
pairs are connected, not how many paths connect them.

The Rust rewrite makes the same decision in the same shape:

```rust
// falkordb-rs-next-gen graph/src/planner/optimizer/select_scan_node.rs, pinned 9d28bdc6
    79  fn score_endpoint(
    80      node: &Arc<QueryNode<Arc<String>, Variable>>,
    81      filtered_vars: &HashSet<u32>,
    82      bound_vars: &HashSet<u32>,
    83      graph: &Graph,
    84  ) -> (u32, u64) {
    85      let mut score = 0u32;
    86      if bound_vars.contains(&node.alias.id) {
    87          score += 3;
    88      }
    89      if filtered_vars.contains(&node.alias.id) {
    90          score += 2;
    91      }
    92      // Node attribute filters (e.g. {name: "Nicolas Cage"}) also count as filters.
    93      if node.attrs.root().num_children() > 0 {
    94          score += 2;
    95      }
    96      if !node.labels.is_empty() {
    97          score += 1;
    98      }
    99      // Cardinality: minimum label node count (lower is better).
   100      // For nodes with no labels, use u64::MAX so labeled nodes win ties.
   101      let cardinality = if node.labels.is_empty() {
   102          u64::MAX
   103      } else {
   104          node.labels
   105              .iter()
   106              .map(|l| graph.label_node_count(l))
   107              .min()
   108              .unwrap_or(u64::MAX)
   109      };
   110      (score, cardinality)
   111  }
```

`score_endpoint` returns `(score, cardinality)` — bound 3, filtered 2 (twice
for inline attributes, `:93-95`), labelled 1, tie-broken by the smallest
label node count (`:99-109`) — and the module doc (`:10-19`) states the
same four-level priority. A chain is *reversed* so that it starts at the
winning endpoint (`:3-8`). The rewrite's planner
reads one statistic, `label_node_count` (`select_scan_node.rs:106`,
`reduce_count.rs:98-108`); a grep of `graph/src/planner/**` for
`cardinal|estimate|selectiv|nvals` finds only comments and those uses.

Read against lane 1: FalkorDB's choice among expansion orders is made
without the quantity whose error lane 1 measures. That is not
automatically a defect — Step 6 and the README's plan-choice table say why
the estimate often does not change the choice — but it means FalkorDB has
no way to notice the case that *does* (a hub reached by an unselective
pattern), and nothing to be wrong 348× about.

### Step 6 — what each engine would need to be wrong less

> **In:** Steps 1–5. **Out:** a table of what each engine stores, how it
> combines it, and the smallest change that moves it to lane 3's catalog.

| engine | statistic read | combination rule | lane-1 analogue | smallest step to a 2-edge catalog |
|---|---|---|---|---|
| Kùzu | rows per rel table, nodes per node table | product of averages, per-hop (`:219`) | independence, median q 9.6 / 57.5 / 324.0 | add per-(rel, rel) path counts |
| Neo4j | counts store: `(label)-[type]->(label)` | node cardinalities × per-relationship ratios (`:214-216`) | independence, identical number | extend the counts store to label-type-label-type-label |
| Memgraph | vertices per label, edges per type; optional per-symbol degree | constant 3.0 per `Expand` unless a degree is supplied (`:369-376`) | 62.2 / 822.1 / 12,425.7 | wire `SymbolStatistics.degree` from degree counts |
| FalkorDB | nodes per label | none: a score and a label-count tie-break | no estimate | per-relation `nvals` is free; a 2-path count needs one reduction |

The last cell deserves a note for the reader who works on FalkorDB: the
number of two-hop paths `a -ta-> b -tb-> c` is `Σ_b indeg_ta(b) ·
outdeg_tb(b)` — the dot product of one matrix's column counts with the next's
row counts — which is exactly what lane 1's sanity line checks against the
frontier DP. Computed once per relationship-type pair with two reductions
over the matrices' degree vectors, it fills lane 3's `F2` table without
running any matrix product (a product under the `ANY_PAIR` semiring above
would count connected pairs, the wrong quantity).

## How to read the code (with the concepts in hand)

1. **Kùzu:** `cardinality_estimator.cpp` is 247 lines — read all of it, then
   `append_extend.cpp:80-97` for where `getExtensionRate` is called.
2. **Neo4j:** start at `AssumeIndependenceQueryGraphCardinalityModel.scala:
   126-217`, then `NodeConnectionCardinalityModel.scala:37-65` (the
   relationship case), then `PatternRelationshipCardinalityModel.scala:
   220-335`. Skip the quantified-path-pattern branches on a first pass.
3. **Memgraph:** `cost_estimator.hpp:89-120` for the visitor's shape, then
   the `Expand` and scan overloads.
4. **FalkorDB:** `traverse_order.c:204-260` (the driver), then the scoring
   in `traverse_order_utils.c`, then `cost_base_label_scan.c:397-418`.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | `est.rs` `indep_path`; lane 1 tables | the by-hop q-errors |
| 3 | `est.rs` `indep_triangle` | the closing-edge ratio `E / (|L_c|·|L_a|)` Neo4j's `bindEndpoints` divides by |
| 4 | `est.rs` `memgraph_const` | the 3^k row |
| 6 | lane 3, `catalog.rs` (stub) | `F1`, `F2` |

## Questions (answer in notes.md)

1. A Neo4j query `(a:A)-[:r]->(b:B)-[:s]->(c:C)` on a graph with
   `|A| = 1,000`, `|B| = 4,000`, `|C| = 2,000`, `E_r = 10,000`, `E_s = 16,000`
   (typed label triples). What does the model estimate (work the two
   multipliers, not just the closed form)? Redo it with Kùzu's rule and
   confirm the same number.
2. Why does Kùzu's `Extend` leave the cardinality unchanged and put the
   rate in a multiplier? What would break if the multiplier were applied at
   the Extend?
3. Memgraph's `kExpand = 3.0` and lane 1's average out-degrees are 11.5,
   8.9, 6.7 and 8.6 for `r0` to `r3`. Which direction does the constant err
   on this graph, and does the error grow or shrink with the number of hops?
   What average degree would make it exact?
4. FalkorDB's `TraverseOrder_LabelsScore` counts labels on a node, not
   nodes in a label. Give a pattern for which that picks the worse start,
   and say which statistic `cost_base_label_scan` would need to see to
   repair it.
5. Compute the number of two-hop paths `r0 r1` on the 6-node graph with
   `r0: 0→3, 1→3, 2→4` and `r1: 3→5, 4→5` from degree counts alone.

## Done when

Answer each before unfolding it.

- [ ] You can write the independence product for a labelled path and show that Neo4j's and Kùzu's computations reduce to it.

  <details><summary>Answer</summary>

  `|E_t0| · Π |E_ti| / |L_src(ti)|`. Kùzu multiplies the running
  cardinality by `numRels / numBoundNodes` per hop
  (`cardinality_estimator.cpp:219`). Neo4j's first relationship has both
  endpoints unbound (cost 1), so its multiplier is `E_r`; the second has its
  source bound at cost `|B|`, so `E_s / |B|`; no node is left unconnected, so
  the product is `E_r E_s / |B|` (`NodeConnectionCardinalityModel.scala:
  148-152, 156-167`; `AssumeIndependenceQueryGraphCardinalityModel.scala:
  214-216`). On lane 1's seed-1 graph both are 305,602.0 for `r0 r1`.

  </details>

- [ ] You can say what Kùzu's factorization does to where the estimate lands.

  <details><summary>Answer</summary>

  `append_extend.cpp:88-91` leaves the extend's cardinality at its input's
  and sets the rate as the group's `cardinalityMultiplier`; the rows are
  counted when the group is flattened (`estimateFlatten`, `:133-137`). The
  per-hop factor is unchanged — the independence product — but the planner
  sees the cardinality grow at the flatten, not at the extend.

  </details>

- [ ] You can state what Memgraph assumes per `Expand` and what lane 1 shows it costs.

  <details><summary>Answer</summary>

  A constant 3.0 (`cost_constants.hpp:47`) unless `SymbolStatistics.degree`
  is supplied (`cost_estimator.hpp:372-374`); `|L_start| · 3^k`. On lane 1 it
  underestimates by medians 62.2, 822.1, 12,425.7 at two, three and four hops
  on the shared graph, and 6.4, 18.6, 40.1 on the independent graph, where
  the independence estimator is within 1.1. The error is the constant being
  unrelated to the data's average degrees (11.5, 8.9, 6.7, 8.6 for `r0` to
  `r3`), not correlation.

  </details>

- [ ] You can explain what FalkorDB's planner uses instead of a cardinality estimate, and what it cannot see.

  <details><summary>Answer</summary>

  A priority score: bound variable beats filtered beats labelled
  (`traverse_order_utils.c:225-238`), where "labelled" counts the labels on
  the node (`:36-53`), and a tie-break on the smallest label's node count
  (`cost_base_label_scan.c:166-167, 216-220`; in the Rust rewrite
  `select_scan_node.rs:99-109`). It reads no relationship-matrix size, degree
  or histogram, so it cannot tell a hub-heavy hop from a thin one.

  </details>

- [ ] You can say how a two-hop path count could be computed in FalkorDB without a matrix product.

  <details><summary>Answer</summary>

  `Σ_b indeg_ta(b) · outdeg_tb(b)`: reduce each relationship matrix to its
  column counts (in-degrees) and row counts (out-degrees) and take the dot
  product over the shared node label. Lane 1's sanity line checks this
  identity against the frontier DP for every 2-walk. A product under
  `GxB_ANY_PAIR_BOOL` (`algebraic_expression_mul.c:33`) would count
  connected pairs rather than paths.

  </details>

## References

- Kùzu: `src/planner/join_order/cardinality_estimator.cpp`,
  `src/planner/plan/append_extend.cpp`, `src/include/common/constants.h`
  at `89f0263`. Neo4j: the `cardinality/assumeIndependence/` package of
  `cypher-planner` and `TransactionBoundGraphStatistics.scala` at
  `eccd584a`. Memgraph: `src/query/plan/cost_estimator.hpp` and
  `cost_constants.hpp` at `8f87f6a`. FalkorDB: `src/execution_plan/
  optimizations/{traverse_order,traverse_order_utils,cost_base_label_scan}.c`
  and `src/arithmetic/algebraic_expression/algebraic_expression_mul.c` at
  `ccb449a9a`; `graph/src/planner/optimizer/select_scan_node.rs` of
  `falkordb-rs-next-gen` at `9d28bdc6`.
- Earlier chapters of this topic: [reading-gcare.md](reading-gcare.md),
  [reading-pessimistic-bounds.md](reading-pessimistic-bounds.md),
  [reading-glogue-gopt.md](reading-glogue-gopt.md),
  [reading-color.md](reading-color.md).
- Topic 13's engine guides: [reading-kuzu.md](../13-graph-engines/reading-kuzu.md),
  [reading-neo4j-record-store.md](../13-graph-engines/reading-neo4j-record-store.md),
  [reading-memgraph-storage.md](../13-graph-engines/reading-memgraph-storage.md).
