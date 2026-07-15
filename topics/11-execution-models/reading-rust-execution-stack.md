# Vectorized in Rust: polars-stream morsels and DataFusion streams

Two Rust codebases answering the same design questions DuckDB answered
in C++ — and the closest templates for M11's runtime. polars-stream
makes morsels a first-class type driven by an async graph; DataFusion
keeps Volcano's shape with a vector payload and async clothes. Before
the code, this chapter builds the five design decisions the two systems
embody — the batch contract, async scheduling, SIMD kernel shape, static
vs dynamic parallelism, and the group-by-as-arrays pattern — then maps
each to its file:line.

## The problem in one sentence

A vectorized engine in Rust must decide four things — what the batch
type carries, who schedules operators (async runtime or hand-rolled
state machine), how work splits across cores, and what a kernel looks
like under nulls and SIMD — and these two codebases picked differently
on almost every axis, which is exactly what makes reading both worth it.

## The concepts, step by step

### Step 1 — the batch contract: what travels between operators

Every vectorized engine moves data in **batches** (a fixed-capacity set
of column arrays plus a row count — DuckDB's DataChunk, seen in this
topic's other guides), but the *contract* attached to the batch differs
per system: DuckDB's `DataChunk` is 2048 rows and nothing else; polars'
`Morsel` is a DataFrame plus a sequence number plus a backpressure
token; DataFusion's Arrow `RecordBatch` is ~8K rows with ordering left
to a stream contract. What the batch carries determines what the
scheduler must reconstruct later — ordering, flow control, provenance —
so read each system's batch type first; it's the systems' design in
miniature.

### Step 2 — polars-stream: the morsel as a first-class type

polars-stream (the streaming executor behind `.lazy().collect()`)
promotes the work unit to a type: `Morsel` = `DataFrame` + `MorselSeq` +
`SourceToken`. Each piece answers a question the paper version
(reading-morsel-parallelism.md) left implicit:

- **`MorselSeq`** — a sequence number: parallel workers pull morsels in
  whatever order, and order-sensitive sinks (ORDER BY, LIMIT) reassemble
  by seq while order-insensitive ones ignore it. DuckDB keeps this
  implicit; polars writes it down.
- **`SourceToken`** — backpressure: a sink can ask sources to stop
  producing (topic 7's output-buffer problem, solved politely instead of
  by killing clients).
- `get_ideal_morsel_size` is a config knob, not a compile-time constant
  — contrast `STANDARD_VECTOR_SIZE = 2048`.

The physical plan is an explicit `Graph` of nodes connected by pipes;
nodes are **async tasks** (cooperatively-scheduled functions that yield
while waiting) and pipes are channels — pipeline parallelism falls out
of the async runtime rather than a hand-rolled scheduler. What async
buys: blocking sources (network, files) integrate for free. What it
costs: poll overhead and fuzzier buffer ownership (question 1 below).

### Step 3 — what a SIMD kernel actually looks like

Down at the leaf, a kernel is a loop that must survive three hazards:
nulls, selectivity, and the compiler failing to vectorize. polars-compute's
float sum shows the production answers:

- **Masked variants**: every kernel comes in a pair —
  `sum_block_vectorized` and `sum_block_vectorized_with_mask` — because
  columns have null masks (a bitmask marking missing values). The masked
  sum SELECTS values into SIMD lanes (blend the value or 0.0 per lane)
  rather than branching per element — no branch misprediction at 50%
  nulls. This masked/unmasked pairing is the columnar equivalent of
  selection vectors.
- **Multiple independent accumulators**: fixed-size blocks accumulated
  into several SIMD registers in parallel, reduced once at the end.
  One accumulator would serialize on the ~4-cycle add latency; 4–8
  independent ones keep the arithmetic ports full — topic 0's MLP
  lesson applied to arithmetic instead of memory.
- `vector_horizontal_sum` — the final reduce of one SIMD register to a
  scalar, shaped "to map to good shuffle instructions".
- The fine print: float addition isn't associative, so the vectorized
  sum ≠ the sequential sum bit-for-bit. Engines document this away.

### Step 4 — DataFusion: Volcano's shape, async clothes, static partitions

DataFusion keeps the iterator model's *shape* and changes the unit: the
`ExecutionPlan` trait's `execute(partition, ctx)` returns a
`SendableRecordBatchStream` — that's `open()` returning a stream, and
the stream's `poll_next` is `next()`. Volcano survived; what changed is
the payload (Arrow `RecordBatch`, ~8K rows) and the dispatch (async
poll, amortized over the batch, so the per-call cost stops mattering —
one poll per 8K rows is 0.01 ns/row even at 100 ns/poll).

Parallelism is **partition-per-stream**: `execute(i)` for i in 0..N
spawns one task per partition — STATIC partitioning, the very thing
morsel-driven scheduling exists to avoid. Skew hurts more than in
DuckDB/polars-stream; `RepartitionExec` operators patch it up mid-plan
by reshuffling batches across partitions.

### Step 5 — group-by as array arithmetic: intern, then index flat states

The heart of any aggregation engine, and DataFusion's
`GroupedHashAggregateStream` is the cleanest statement of the modern
shape. Per input batch: **intern** the group keys — one hash-table probe
per row that maps each key to a dense integer group index (0, 1, 2, …
in first-seen order) — then update aggregate states that live in flat
columnar arrays indexed by that integer:

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

Two wins: 4 aggregates share ONE probe per row instead of probing 4
times (question 4), and states are dense arrays — cache-friendly,
SIMD-able, no per-group heap objects to chase. This is exactly the shape
your `vectorized.rs` group-by should have.

### Step 6 — the comparison that matters

| | DuckDB | polars-stream | DataFusion |
|---|---|---|---|
| unit | DataChunk 2048 | Morsel (config) | RecordBatch ~8K |
| parallelism | morsel pull | async graph + tokens | static partitions |
| scheduling | own scheduler | async runtime | tokio |
| ordering | implicit | MorselSeq | stream contract |

No row of this table has a free winner: morsel pulling beats static
partitions on skew but demands your own scheduler; async graphs
integrate blocking sources but give up precise control of buffers; an
explicit `MorselSeq` costs a u64 per batch and buys ordered sinks. M11
must fill in a fourth column — that's the point of reading all three.

## Where each step lives in the code

- **Steps 1–2 — polars-stream** (`crates/polars-stream/src/`):
  `morsel.rs:82` — `Morsel`; `MorselSeq` at `:21`;
  `get_ideal_morsel_size` at `:11`. The graph: `graph.rs:21,:165`
  (`Graph`, `GraphNode`), pipes in `pipe.rs`, `execute.rs:301`
  `execute_graph` drives it. Skim `nodes/` for the operator
  implementations.
- **Step 3 — polars-compute**
  (`crates/polars-compute/src/float_sum.rs`): `:44`
  `vector_horizontal_sum`; `:67` `SumBlock` trait with
  `sum_block_vectorized` + `sum_block_vectorized_with_mask` — the mask
  is a `BitMask`, selected into lanes, not branched.
- **Step 4 — DataFusion**
  (`datafusion/physical-plan/src/execution_plan.rs`): `trait
  ExecutionPlan` :97; `execute(partition, ctx) ->
  SendableRecordBatchStream` (:478). `RepartitionExec` for the
  skew patch.
- **Step 5 — DataFusion aggregates**
  (`aggregates/grouped_hash_stream.rs:275`) —
  `GroupedHashAggregateStream`; `poll_next` (`:641`) pulls input
  batches; key interning via `group_values/mod.rs:90` (`trait
  GroupValues`); states updated with vectorized
  `update_batch(values, group_indices)`.

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
