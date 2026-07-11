# Reading guide — DuckDB `src/execution/`: the vectorized reference (~2 h)

Local clone: `~/repos/duckdb`. Read in this order: the vector types (the
data plane), then the pipeline executor (the control plane), then the
join hash table (where the tricks pay off).

## 1. Vectors and chunks (the data plane)

- `src/include/duckdb/common/vector_size.hpp:16–20` —
  `STANDARD_VECTOR_SIZE = 2048`. The single most consequential constant
  in the engine; everything below flows through 2048-row units.
- `src/include/duckdb/common/enums/vector_type.hpp:15` — the vector
  kinds:

```
 FLAT        plain columnar array
 CONSTANT    one value stands for the whole vector (literals, and any
             op whose inputs were constant — never expanded)
 DICTIONARY  selection vector over another vector (filter output,
             decompressed dictionary data — flows through unexpanded)
 SEQUENCE    start + increment (row ids)
 FSST        still-compressed strings (topic 12)
```

- `src/include/duckdb/common/types/data_chunk.hpp:44` — `DataChunk` =
  a set of vectors + count. This is what `next()` returns.
- `src/include/duckdb/common/types/selection_vector.hpp:31` —
  `SelectionVector`: the filter-without-copying mechanism. A kernel takes
  `(Vector, sel, count)`; a filter's whole output is a new sel over the
  same buffers.

Question to hold: every kernel must handle
{flat, constant, dictionary}². How does DuckDB avoid writing 9 loops per
operation? (Look for `UnifiedVectorFormat` / `ToUnifiedFormat` — the
normalize-then-one-loop dodge, at the price of an indirection.)

## 2. Pipelines (the control plane)

Plans are split into PIPELINES at materialization points (hash table
builds, sorts). Each pipeline = source → streaming operators → sink.

- `src/parallel/pipeline.cpp:136` — `Pipeline::ScheduleParallel`: asks
  source AND sink whether they support parallelism, creates one
  `PipelineTask` per allowed thread; `:95` sequential fallback.
- `src/execution/operator/scan/physical_table_scan.cpp:77` —
  `MaxThreads` comes from the source's global state: for a table scan,
  ~one unit per row group (122880 rows,
  `storage_info.hpp:26`) — DuckDB's morsel size.
- `src/parallel/pipeline_executor.cpp:260` — `Execute(max_chunks)`: the
  main loop. Fetch a chunk from source (`:281`), push it through the
  operator chain — `ExecutePushInternal :375`, which walks operators via
  `Execute(input, result, idx) :483` — into the sink. Note the
  `OperatorResultType` protocol: `HAVE_MORE_OUTPUT` (operator wasn't
  done with this input — e.g. a join that exploded one chunk into many),
  `NEED_MORE_INPUT`, `FINISHED`.
- Push-based inside a task, pull-based between tasks: within a pipeline
  DuckDB pushes chunks sink-ward, but workers PULL work units from the
  source. Compare with textbook Volcano (pull all the way down).

## 3. The join hash table (`src/execution/join_hashtable.cpp`)

- Build side: `Sink` collects chunks into partitioned row-format storage
  (`sink_collection`, `:169` Combine merges thread-local partitions —
  morsel-driven two-phase in action).
- The HT proper: 8-byte entries = pointer + salt
  (`ht_entry_t::ExtractSalt` `:195`) — compare hash-salt bits BEFORE
  chasing the tuple pointer; most non-matches are rejected without a
  cache miss. (Topic 2's SwissTable H2 / topic 8's tagged pointers,
  again.)
- Probe (`ProbeState`, header `:206`): vectorized — hash a whole chunk
  (`VectorOperations::CombineHash` `:393`), gather entries, salt-compare
  en masse, build a selection vector of candidates, then compare actual
  keys only for those. Chains handled with `ResidualPredicateProbeState`
  selection juggling (header `:74–:80`).

## Questions for notes.md

1. Why 2048 and not 64K (X100 used ~1K)? Compute: chunk bytes for 8
   columns × 8 B at each size vs your measured L2 (topic 0 ladder).
2. CONSTANT vectors: trace `2 * price` where price is FLAT and 2 is
   CONSTANT — which loop runs? What would a Volcano engine do per row?
3. `HAVE_MORE_OUTPUT`: which operators need it and why can't they just
   buffer internally? (Memory bound + who owns the chunk.)
4. The salt trick: with 64-bit hashes and k salt bits, what fraction of
   non-matching probes still chase a pointer? Pick k.
5. M11: your Expand operator explodes one source node into deg(n)
   results — that's `HAVE_MORE_OUTPUT` shaped. Sketch the state it must
   keep between calls.

## Done when

You can draw a pipeline for `SELECT k, SUM(v) FROM t JOIN s ... GROUP BY k`
(two pipelines, which is the sink of which), and explain selection
vectors + the salt trick in two sentences each.
