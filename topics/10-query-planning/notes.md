# Topic 10 notes — parsing, planning, optimization

## Predictions (fill BEFORE running / reading)

### explain.rs query 3 (items ⋈ orders ⋈ users, users filtered to city=7 AND age=30)

| question | prediction | actual |
|---|---|---|
| greedy first pair (my planner) | | |
| DuckDB's first pair for same query/stats | | |
| do they agree? if not, whose estimate diverged | | |
| est vs actual card of users after both filters (independence: 10000/100/50 = 2) | | |

### DuckDB EXPLAIN comparison

Load the same schema + row counts into DuckDB, run the three explain.rs
queries with `EXPLAIN`. Note every join-order disagreement:

| query | my order | duckdb order | why |
|---|---|---|---|
| 1 | | | |
| 2 | | | |
| 3 | | | |

## Implementation log

- [ ] `parse_and_plan` — sqlparser 0.52, GenericDialect; naive left-deep plan
- [ ] `push_down` — literal filters into scans, ColEqCol into lowest covering join
- [ ] `estimate` — 1/NDV, independence multiply, |L|·|R|/max(NDV)
- [ ] `reorder_joins` — greedy smallest-pair-first
- [ ] all tests green; `join_order_flips_with_stats` was the fiddly one? notes:
- [ ] explain.rs run, DuckDB comparison table filled

Surprises / dead ends:

## Questions from the reading guides

### DuckDB optimizer (reading-duckdb-optimizer.md)

1. Pullup-then-pushdown — which pullup rule enables a deeper sink:
2. DP keeps one plan per set; what Selinger kept that DuckDB drops, and why it's ok:
3. Exact→greedy threshold — star vs chain connected-subgraph counts at n=10:
4. What splitting join-order from build/probe-side choice loses:
5. Cypher chain → which DuckDB piece is anchor selection:

### Postgres optimizer (reading-postgres-optimizer.md)

1. Query where sorted-but-pricier {AB} wins at level 3:
2. Why geqo instead of greedy (trees vs sequences):
3. Which plan flips between LIMIT 10 and full scan, why one cost number fails:
4. Super-node degree skew as join skew — what stat M10 needs:

### Rust planner stack (reading-rust-planner-stack.md)

1. Pratt parsing: precedence climb for `a = 1 AND b < 2 OR c` — draw the tree:
2. DataFusion fixpoint vs DuckDB ordered passes — which bug class each risks:
3. Why polars can skip join reordering and a Cypher engine can't:

### Selinger vs Cascades (reading-selinger-cascades.md)

1. Interesting orders = extra DP state; Cascades equivalent (enforcers + physical properties):
2. What the memo shares that Selinger's table doesn't:
3. **M10 architecture decision** — bottom-up Selinger-style or memo-based, and why:

### How Good Are Query Optimizers (reading-how-good-optimizers.md)

1. 2-table example where independence underestimates 100×:
2. Cout validates which of my engines' knobs:
3. Hash vs nested-loop regret matrix (minimax framing):
4. **graph-JOB sketch** (M10/M22 benchmark seed) — 3 Cypher queries where
   nnz-based estimation blows up (degree skew × label correlation × triangles):

## Cross-topic threads

- Cardinality ≫ cost ≫ search (VLDB'15) is fair-benchmarking (topic 0)
  applied to the optimizer itself: inject ground truth per layer to isolate blame.
- DuckDB cost_model.cpp:40 = Cout = "sum of intermediate cardinalities" —
  the cost model IS the cardinality estimate. My `estimate` is the whole game.
- Greedy fallback (:234) = the amortize-and-escape-hatch pattern again:
  exact until the state space explodes, then heuristic.

## M10 log (Cypher parser + binder + planner)

- [ ] Cypher pattern `(a:L1)-[:R]->(b:L2)` = join over edge relation; MATCH
      with k relationships = k-way join ordering
- [ ] anchor-node selection = which base relation to scan first = join order
      leaf choice; label cardinality = table stats
- [ ] decision: Selinger-style bottom-up vs memo (answer question above first)
- [ ] cardinality for Expand = nnz of sparse matrix product — record the
      formula and its independence assumption; graph-JOB queries stress it
- [ ] rewrite pass order for Cypher: label pushdown before anchor selection
      (mirrors DuckDB filter-pushdown-before-join-order)

## Done when

- All planner tests green; explain.rs vs DuckDB comparison table filled with
  at least one disagreement explained.
- Architecture decision for M10 written with a reason.
- graph-JOB sketch exists (3 queries + why each breaks independence).
