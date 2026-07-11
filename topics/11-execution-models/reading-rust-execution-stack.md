# Vectorized in Rust: polars-stream morsels and DataFusion streams

Two Rust answers to the same design questions DuckDB answered in C++ —
and the closest templates for M11's runtime. polars-stream makes morsels
a first-class type driven by an async graph; DataFusion keeps Volcano's
shape with a vector payload and async clothes.

## 1. polars-stream: morsels as a first-class type

`crates/polars-stream/src/`:

- `morsel.rs:82` — `Morsel` = `DataFrame` + `MorselSeq` (`:21`) +
  `SourceToken`. The sequence number is what DuckDB keeps implicit:
  order-sensitive sinks reassemble by seq, order-insensitive ones ignore
  it. `get_ideal_morsel_size` (`:11`) is a config knob, not a compile
  constant — contrast STANDARD_VECTOR_SIZE.
- `SourceToken` = backpressure: a sink can ask sources to stop. (Topic 7's
  output-buffer problem, solved politely instead of by killing clients.)
- `graph.rs:21,:165` — the physical plan is an explicit `Graph` of
  `GraphNode`s connected by pipes (`pipe.rs`); `execute.rs:301`
  `execute_graph` drives it. Nodes are async tasks; pipes are channels —
  the pipeline parallelism falls out of the async runtime rather than a
  hand-rolled scheduler. Skim `nodes/` for the operator implementations.

## 2. polars-compute: what a SIMD kernel actually looks like

`crates/polars-compute/src/float_sum.rs`:

- `:44` `vector_horizontal_sum` — reduce a SIMD register to a scalar,
  shaped "to map to good shuffle instructions".
- `:67` `SumBlock` trait: `sum_block_vectorized` +
  `sum_block_vectorized_with_mask` — every kernel comes in a masked
  variant (nulls!). The mask is a `BitMask`, and the masked sum SELECTS
  into the lanes rather than branching per element. This masked/unmasked
  pairing is the columnar equivalent of your selection vectors.
- Note the block structure: fixed-size blocks accumulated in multiple
  independent SIMD accumulators (ILP — the MLP lesson from topic 0
  applied to arithmetic ports), reduced once at the end. Also: float
  summation order changes the answer — vectorized sum ≠ sequential sum
  bit-for-bit. Engines document this away.

## 3. DataFusion: Volcano shape, vector payload, async clothes

- `datafusion/physical-plan/src/execution_plan.rs:97` — `trait
  ExecutionPlan`; `execute(partition, ctx) -> SendableRecordBatchStream`
  (`:478`). It's `open()` returning a stream; `poll_next` is `next()`.
  Volcano's SHAPE survived — what changed is the unit (Arrow
  `RecordBatch`, ~8K rows) and the dispatch (async poll, amortized over
  the batch, so the per-call cost stops mattering).
- Partition-per-stream parallelism: `execute(i)` for i in
  0..N partitions, one task each — STATIC partitioning, not
  morsel-pulling. Skew hurts more than DuckDB/polars-stream; repartition
  operators (`RepartitionExec`) patch it up mid-plan.
- `aggregates/grouped_hash_stream.rs:275` — `GroupedHashAggregateStream`,
  the engine's heart: `poll_next` (`:641`) pulls input batches, and for
  each batch INTERNS the group keys (`group_values/mod.rs:90`, `trait
  GroupValues`) → dense group indices; aggregate states are flat columnar
  arrays indexed by group id, updated with a vectorized
  `update_batch(values, group_indices)`. No per-group objects — the
  group-by IS array arithmetic. This is exactly the shape your
  `vectorized.rs` group-by should have.

```rust
// group-by IS array arithmetic: intern keys → dense ids → flat states
fn update_batch(&mut self, keys: &Column, vals: &[i64]) {
    let gids = self.group_values.intern(keys); // ONE HT probe per row,
    for (i, &g) in gids.iter().enumerate() {   // shared by all aggregates
        self.sums[g] += vals[i];               // states are flat arrays
        self.counts[g] += 1;                   // indexed by group id —
    }                                          // no per-group heap objects
}
```

## The comparison that matters

| | DuckDB | polars-stream | DataFusion |
|---|---|---|---|
| unit | DataChunk 2048 | Morsel (config) | RecordBatch ~8K |
| parallelism | morsel pull | async graph + tokens | static partitions |
| scheduling | own scheduler | async runtime | tokio |
| ordering | implicit | MorselSeq | stream contract |

## Questions for notes.md

1. Async operators (polars/DF) vs hand-rolled state machines (DuckDB's
   OperatorResultType): what does async buy (blocking sources) and cost
   (poll overhead, buffer ownership)? Which fits M11 — remember topic 7's
   one-threadpool decision.
2. MorselSeq: which graph query results are order-sensitive? (ORDER BY
   obviously — anything else in Cypher? LIMIT without ORDER BY?)
3. The masked-kernel pattern: your batches will have selection vectors
   instead of null masks. Same trick? When does select-in-lanes beat
   compact-then-compute? (Selectivity threshold — guess, then bench.)
4. DataFusion interning group keys per batch: why is
   hash-once-then-index cheaper than hashing per aggregate? Count the
   HT probes for 4 aggregates either way.
5. M11: FalkorDB's Expand does one GraphBLAS SpMV per batch — which of
   the three systems' operator contracts fits "one call produces a
   whole matrix of results" best?

## Done when

You can name the batch unit + parallelism strategy of all three systems
from the table WITHOUT the table, and describe the
intern-then-flat-arrays group-by shape in two sentences.

## References

**Code**
- [polars](https://github.com/pola-rs/polars) —
  `crates/polars-stream/src/` (`morsel.rs`, `graph.rs`, `execute.rs`,
  `nodes/`) and `crates/polars-compute/src/float_sum.rs` for what a
  SIMD kernel actually looks like
- [datafusion](https://github.com/apache/datafusion) —
  `datafusion/physical-plan/src/execution_plan.rs` (the trait) and
  `aggregates/` (`GroupedHashAggregateStream`, `group_values/`) — the
  engine's heart; ~1.5 h for both
