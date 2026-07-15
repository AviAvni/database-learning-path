# DuckDB's execution engine: 2048 rows at a time

The vectorized reference, in production C++ — every X100 idea from this
topic's papers appears here with a file:line. Before the code, this
chapter builds the machine in six steps: the chunk, the vector type
flags, selection vectors, pipelines, the executor protocol, and the
salt-tagged join hash table — the data plane first, then the control
plane, then the operator where the tricks pay off. Then it hands you the
anchors to watch each step run.

## The problem in one sentence

Run an analytical query over 100M rows without paying the per-row
interpretation tax (~20–100 ns × 100M rows × 5 operators = minutes of
overhead) and without materializing whole 800 MB intermediate columns to
RAM — the answer is to move data in 2048-row units that fit in cache.

## The concepts, step by step

### Step 1 — the DataChunk: `next()` returns 2048 rows

DuckDB keeps the classic iterator structure (operators composed in a
tree, data flowing up), but the unit that moves between operators is a
**DataChunk**: a set of **vectors** — one contiguous array per column —
plus a row count, at most `STANDARD_VECTOR_SIZE = 2048` rows. All
per-call overhead (dispatch, operator state checks) now divides by 2048,
and the loops inside each operator are tight `for` loops over arrays —
auto-vectorizable, prefetcher-friendly. Why 2048 and not a million: a
chunk of 8 columns × 8 bytes × 2048 rows = 128 KB — sized so an
operator's working set stays in L1/L2 *between* operators. It's the
cache ladder of topic 0 turned into an engine constant, and the single
most consequential number in the codebase.

### Step 2 — vector type flags: metadata instead of work

A vector doesn't have to be a plain array. Each carries a type flag, and
kernels dispatch on it — representing structure instead of expanding it:

```
 FLAT        plain columnar array
 CONSTANT    one value stands for the whole vector (literals, and any
             op whose inputs were constant — never expanded)
 DICTIONARY  selection vector over another vector (filter output,
             decompressed dictionary data — flows through unexpanded)
 SEQUENCE    start + increment (row ids)
 FSST        still-compressed strings (topic 12)
```

The payoff is arithmetic: `2 * price` with a CONSTANT `2` runs one loop
over `price` and never materializes 2048 copies of the literal;
dictionary-compressed data flows through the engine without being
decompressed. The cost is combinatorial: a binary kernel faces
{flat, constant, dictionary}² input shapes. Question to hold: how does
DuckDB avoid writing 9 loops per operation? (Look for
`UnifiedVectorFormat` / `ToUnifiedFormat` — the normalize-then-one-loop
dodge, at the price of an indirection.)

### Step 3 — selection vectors: filtering without copying

A filter that copied its ~50 survivors out of 2048 rows into fresh
arrays would pay a copy per operator per chunk. Instead a filter's
entire output is a **SelectionVector** — a small index array `sel[]`
naming the surviving row positions — over the *same untouched* data
vectors. Every downstream kernel takes `(data, sel, count)` and iterates
`sel` instead of 0..2048; zero bytes of column data move until some
operator genuinely must materialize:

```rust
// every kernel takes (data, sel, count); a filter's OUTPUT is a new sel
fn filter_lt(v: &[i64], t: i64, sel: &[u32], out_sel: &mut [u32]) -> usize {
    let mut n = 0;
    for &i in sel {
        out_sel[n] = i;                    // branch-free: write always,
        n += (v[i as usize] < t) as usize; // advance only on match
    }
    n   // survivor count — the data vectors are untouched, zero copies
}
```

Note the branch-free body: write unconditionally, advance the counter
only on match — no branch for the predictor to miss on 50%-selective
data (topic 0's branch_misprediction lesson, in production). A
DICTIONARY vector (Step 2) is this same trick promoted to a vector
representation.

### Step 4 — pipelines: the plan splits at materialization points

Some operators can stream chunk-by-chunk (filter, projection); others
must consume *all* input before producing anything — a hash-join build
must see the whole build side, a sort must see every row. These are
**materialization points**, and they cut the plan into **pipelines**:
each pipeline is a source (scan, or a previous pipeline's output) → a
chain of streaming operators → a **sink** (the materializing operator).
For `SELECT k, SUM(v) FROM t JOIN s ... GROUP BY k`:

```
 pipeline 1:  scan(s) ──────────────────► build hash table   (sink)
 pipeline 2:  scan(t) → probe HT → ...  ► hash aggregate     (sink)
              (runs only after pipeline 1's sink is complete)
```

Pipelines are the scheduling unit: each can be run by many threads in
parallel, and dependencies (build before probe) gate execution. This is
also where morsel-driven parallelism plugs in
(reading-morsel-parallelism.md): the source hands out row-group-sized
work units (122880 rows = 60 vectors) that worker threads pull.

### Step 5 — the executor protocol: push within, pull between

Inside a pipeline task, DuckDB is **push-based**: the executor fetches a
chunk from the source and pushes it through the operator chain into the
sink. Between tasks it's pull-based: workers pull work units from the
source. (Compare textbook Volcano: pull all the way down.) Pushing needs
a protocol for operators whose output size doesn't match their input —
each operator call returns an `OperatorResultType`:

- `NEED_MORE_INPUT` — done with this chunk, push me the next;
- `HAVE_MORE_OUTPUT` — I wasn't finished with this input (a join that
  exploded one 2048-row chunk into many output chunks); call me again
  with the SAME input before fetching more;
- `FINISHED` — this pipeline can stop early (a LIMIT was satisfied).

`HAVE_MORE_OUTPUT` exists because operators must not buffer unbounded
output internally — memory stays bounded at ~one chunk per operator,
and the ownership of chunks stays with the executor (question 3 below).

### Step 6 — the join hash table: salt bits before pointer chases

The hash join is where the vectorized machinery pays off. Build side:
each thread collects its chunks into partitioned row-format storage
(thread-local, no contention), merged at the end — the morsel-driven
two-phase pattern. The table itself stores 8-byte entries = a pointer to
the tuple + **salt** bits (a few bits of the key's hash smuggled into
the entry — topic 2's bit-smuggling): a probe compares the salt FIRST,
and since most non-matching probes fail the salt compare, they are
rejected without ever dereferencing the pointer — no cache miss on the
tuple. The probe is vectorized end to end: hash all 2048 keys, gather
all their buckets, salt-compare en masse, build a selection vector of
candidates, compare actual keys only for those. Batching the bucket
gathers is exactly what lets the core overlap the cache misses —
memory-level parallelism, the reason vectorized probes win in the
VLDB'18 shootout (reading-compiled-vs-vectorized.md).

## Where each step lives in the code

Read in this order: the vector types (the data plane), then the pipeline
executor (the control plane), then the join hash table.

- **Step 1**: `src/include/duckdb/common/vector_size.hpp:16–20` —
  `STANDARD_VECTOR_SIZE = 2048`;
  `src/include/duckdb/common/types/data_chunk.hpp:44` — `DataChunk` =
  a set of vectors + count. This is what `next()` returns.
- **Step 2**: `src/include/duckdb/common/enums/vector_type.hpp:15` —
  the vector kinds; chase `UnifiedVectorFormat` / `ToUnifiedFormat`
  from any kernel.
- **Step 3**: `src/include/duckdb/common/types/selection_vector.hpp:31`
  — `SelectionVector`, the filter-without-copying mechanism.
- **Step 4**: `src/parallel/pipeline.cpp:136` —
  `Pipeline::ScheduleParallel`: asks source AND sink whether they
  support parallelism, creates one `PipelineTask` per allowed thread;
  `:95` sequential fallback.
  `src/execution/operator/scan/physical_table_scan.cpp:77` —
  `MaxThreads` from the source's global state: ~one unit per row group
  (122880 rows, `storage_info.hpp:26`) — DuckDB's morsel size.
- **Step 5**: `src/parallel/pipeline_executor.cpp:260` —
  `Execute(max_chunks)`: the main loop. Fetch a chunk from source
  (`:281`), push it through the operator chain — `ExecutePushInternal
  :375`, which walks operators via `Execute(input, result, idx) :483` —
  into the sink. The `OperatorResultType` protocol lives here.
- **Step 6**: `src/execution/join_hashtable.cpp` — build side `Sink`
  collects chunks into partitioned row-format storage
  (`sink_collection`; `:169` `Combine` merges thread-local partitions).
  `ht_entry_t::ExtractSalt` `:195` — the 8-byte pointer+salt entry.
  Probe (`ProbeState`, header `:206`): hash a whole chunk
  (`VectorOperations::CombineHash` `:393`), gather entries, salt-compare
  en masse, then compare actual keys via selection vector; chains
  handled with `ResidualPredicateProbeState` selection juggling (header
  `:74–:80`).

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

## References

**Code**
- [duckdb](https://github.com/duckdb/duckdb) — the data plane:
  `src/include/duckdb/common/vector_size.hpp`,
  `enums/vector_type.hpp`, `types/data_chunk.hpp`,
  `types/selection_vector.hpp`; the control plane:
  `src/parallel/pipeline.cpp`, `src/parallel/pipeline_executor.cpp`;
  the payoff: `src/execution/join_hashtable.cpp`; ~2 h
