# DuckDB's execution engine: 2048 rows at a time

The vectorized reference, in production C++ — every X100 idea from this
topic's papers appears here with a file:line. Before the code, this
chapter builds the machine in six steps: the chunk, the vector type
flags, selection vectors, pipelines, the executor protocol, and the
salt-tagged join hash table — the data plane first, then the control
plane, then the operator where the tricks pay off. Then it hands you the
anchors to watch each step run.

Every anchor below is duckdb at the commit this repo pins,
`6c0c1a68` (`resources/codebases.md`). Quoted C++ carries its real line
numbers in the gutter; elisions are marked. Where a number could be
remembered wrong — the vector size, the salt width, the morsel size —
the guide quotes the constant instead of asserting it.

## The problem in one sentence

Run an analytical query over 100M rows without paying the per-row
interpretation tax — [reading-postgres-executor.md](reading-postgres-executor.md)
works it out at 2.5 s of pure dispatch for a 5-node plan, before any
column is touched — and without materializing whole 800 MB intermediate
columns to RAM. DuckDB's answer is to move data in units of 2048 rows,
sized so that the unit stays in cache between operators.

## The concepts, step by step

### Step 1 — the DataChunk: `next()` returns 2048 rows

> **In:** a Volcano operator tree, where the unit crossing every operator
> boundary is one tuple and the per-call dispatch cost is therefore paid
> per row.
> **Out:** the same tree with the unit replaced by a **DataChunk** — up
> to 2048 rows of every column at once — and the dispatch cost divided by
> 2048. This is the change that defines the model; Steps 2-6 are all
> consequences of it.

**Vectorized execution** means each operator call processes a *batch* of
rows rather than one, and the batch is laid out column-wise so the inner
loops are `for` loops over contiguous arrays. DuckDB's batch is the
`DataChunk`: a set of **vectors** — one contiguous array per column, all
of the same length — plus a row count.

```cpp
// src/include/duckdb/common/types/data_chunk.hpp — the doc comment, 26-44,
// elided to the two sentences that matter, then the class and its payload.
    26  //!  A Data Chunk represents a set of vectors.
    27  /*!
    28      The data chunk class is the intermediate representation used by the
    29     execution engine of DuckDB. It effectively represents a subset of a relation.
    30     It holds a set of vectors that all have the same length.
// ... 31-35: how Initialize allocates ...
    36     the chunk. The reason for this behavior is that the underlying vectors can
    37     become referencing vectors to other chunks as well (i.e. in the case an
    38     operator does not alter the data, such as a Filter operator which only adds a
    39     selection vector).
// ... 40-43: rest of the comment ...
    44  class DataChunk {
    45  public:
    46  	//! Creates an empty DataChunk
    47  	DUCKDB_API DataChunk();
    48  	DUCKDB_API ~DataChunk();
    49  
    50  	//! The vectors owned by the DataChunk.
    51  	vector<Vector> data;
```

Note lines 37-39 already: an operator that does not alter data — a filter
— produces its output by *adding a selection vector*, not by copying. That
is Step 3, promised in the class comment.

The batch size is a compile-time constant, and worth reading rather than
remembering, because it is overridable and because the `#ifndef` says who
is allowed to override it:

```cpp
// src/include/duckdb/common/vector_size.hpp — the whole constant, 15-25.
    15  //! The default standard vector size
    16  #define DEFAULT_STANDARD_VECTOR_SIZE 2048U
    17  
    18  //! The vector size used in the execution engine
    19  #ifndef STANDARD_VECTOR_SIZE
    20  #define STANDARD_VECTOR_SIZE DEFAULT_STANDARD_VECTOR_SIZE
    21  #endif
    22  
    23  #if (STANDARD_VECTOR_SIZE & (STANDARD_VECTOR_SIZE - 1) != 0)
    24  #error The vector size must be a power of two
    25  #endif
```

So `STANDARD_VECTOR_SIZE` is 2048 unless the build overrode it, and the
`#error` at 23-25 fixes it to a power of two — which matters later, because
the hash table's `bitmask` modulo trick and the row-group arithmetic in
Step 4 both assume it divides evenly.

**Why 2048 divides the dispatch tax to nothing.** Take the postgres
guide's arithmetic and change only the unit. Same assumptions: 100M rows,
a 5-operator plan, `c = 20` cycles per operator call (a predicted indirect
call, the callee prologue, the reload of the row pointer), `f = 4 GHz`:

```
 tuple-at-a-time: 100e6 × 5          = 500,000,000 calls
                  500e6 × 20 / 4e9   = 2.5 s        = 25 ns/row
 vector of 2048:  (100e6 / 2048) × 5 = 244,140 calls
                  244,140 × 20 / 4e9 = 0.0012 s     = 0.012 ns/row
```

Two thousand-fold on that term. A cost that dominated the query is now
below the noise floor of a single cache miss.

**Why 2048 and not 64K.** The chunk is not free to grow, because it has
to *stay resident* between operators: the filter writes it, the aggregate
reads it, and if it was evicted in between, the second read is a memory
access rather than a cache hit. At 8 columns of 8-byte values:

```
 bytes per row          = 8 cols × 8 B                  =     64 B
    64 rows  →     4 KB          2048 rows  →    128 KB
  1024 rows  →    64 KB         65536 rows  →      4 MB
```

Hold those against this machine's *measured* ladder, from
[topics/00-performance-toolbox/notes.md](../00-performance-toolbox/notes.md):
the L1d plateau reads 1.02 ns and "ends exactly" at 128 KB; 512 KB-1 MB
reads 5.3-5.8 ns; 4-8 MB reads 7.6-9.0 ns; the DRAM plateau reads ~25 ns.
A 2048-row × 8-column chunk is 128 KB — exactly one L1d. Widen the chunk
to 64K rows and the same eight columns are 4 MB, so every inter-operator
handoff is an L2 trip. Narrow it to 64 rows and you are back to paying
dispatch 32× more often. 2048 is where those two curves cross, and it is
the single most consequential number in the codebase.

**What batching does not fix.** This repo's own Volcano lane
([FINDINGS.md](../../FINDINGS.md) row 11) tops out at 103.3 M rows/s and
gets *slower* as selectivity rises — 74.7 M rows/s at 95%. The cost that
grows is per *surviving* row, not per *evaluated* predicate. Dividing
dispatch by 2048 attacks the per-call term; it does nothing on its own
about the work each surviving row still causes downstream. Steps 3 and 6
are about that half.

### Step 2 — vector type flags: metadata instead of work

> **In:** a DataChunk whose vectors are all plain arrays, so a literal
> `2` in `2 * price` must be expanded to 2048 copies of `2` before the
> multiply loop can run.
> **Out:** vectors that carry a *type flag*, so a literal is stored once,
> a filter result is stored as indices, and row ids are stored as
> `start + increment` — structure represented rather than expanded.

A vector does not have to be a plain array. Each carries a `VectorType`,
and kernels dispatch on it:

```cpp
// src/include/duckdb/common/enums/vector_type.hpp — the whole enum, 15-22.
    15  enum class VectorType : uint8_t {
    16  	FLAT_VECTOR,       // Flat vectors represent a standard uncompressed vector
    17  	FSST_VECTOR,       // Contains string data compressed with FSST
    18  	CONSTANT_VECTOR,   // Constant vector represents a single constant
    19  	DICTIONARY_VECTOR, // Dictionary vector represents a selection vector on top of another vector
    20  	SEQUENCE_VECTOR,   // Sequence vector represents a sequence with a start point and an increment
    21  	SHREDDED_VECTOR    // Shredded variant vector
    22  };
```

Six kinds, not the five this guide used to list: `SHREDDED_VECTOR` — the
shredded representation of a `VARIANT` column, where a semi-structured
value's common sub-fields are stored as real typed columns — is present at
this pin.

The payoff is arithmetic. `2 * price` with a CONSTANT `2` runs one loop
over `price` and never writes 2048 copies of the literal: 2048 × 8 B =
16 KB of stores, and 16 KB of L1 that stays available for real data,
saved per chunk per constant. Dictionary-compressed data flows through
the engine still encoded (topic 12), and a SEQUENCE row-id vector costs
16 bytes instead of 16 KB.

The cost is combinatorial: a binary kernel faces {flat, constant,
dictionary, sequence, …}² input shapes, and nobody writes 36 loops per
operation. The dodge is a normalizing view:

```cpp
// src/include/duckdb/common/vector/unified_vector_format.hpp — the payload
// of the normalized view, 22-35 elided to its data members.
    22  struct UnifiedVectorFormat {
// ... 23-30: constructors, copy deleted, move kept ...
    31  	const SelectionVector *sel;
    32  	const_data_ptr_t data;
    33  	ValidityMask validity;
    34  	SelectionVector owned_sel;
    35  	PhysicalType physical_type;
```

`Vector::ToUnifiedFormat` (`src/include/duckdb/common/types/vector.hpp:127`)
turns *any* vector kind into this `(sel, data, validity)` triple, and every
kernel then writes one loop of the shape `data[sel->get_index(i)]`. The
price is an indirection on every element — which is why the hot kernels
still specialize on FLAT and only fall back to the unified path.

### Step 3 — selection vectors: filtering without copying

> **In:** a filter that must hand its ~1024 survivors, out of a 2048-row
> chunk, to the next operator.
> **Out:** the survivors expressed as a **selection vector** — an array
> of surviving row *positions* over the same untouched data vectors — so
> that zero bytes of column data move. This is **late materialization**:
> defer the copy until an operator genuinely needs a dense array.

A **selection vector** is a small index array `sel[]` naming which
positions of the underlying vectors are live. Every downstream kernel
takes `(data, sel, count)` and iterates `sel` instead of `0..2048`:

```cpp
// src/include/duckdb/common/types/selection_vector.hpp — write, read, and
// the storage. 124-127 and 134-140 in full, then the private member at 175.
   124  	inline void set_index(idx_t idx, idx_t loc) { // NOLINT: allow casing for legacy reasons
   125  		D_ASSERT(idx < capacity);
   126  		sel_vector[idx] = UnsafeNumericCast<sel_t>(loc);
   127  	}
// ... 128-133: swap(i, j) ...
   134  	inline idx_t get_index(idx_t idx) const { // NOLINT: allow casing for legacy reasons
   135  		return sel_vector ? get_index_unsafe(idx) : idx;
   136  	}
   137  	inline idx_t get_index_unsafe(idx_t idx) const { // NOLINT: allow casing for legacy reasons
   138  		D_ASSERT(idx < capacity);
   139  		return sel_vector[idx];
   140  	}
// ... 141-174: data(), Slice, Verify, Sort ...
   175  	sel_t *sel_vector;
```

Line 135 is the piece worth stopping on: a null `sel_vector` means the
identity mapping. "No selection" is not a special case the caller must
test for — it is a selection vector whose `get_index(i)` returns `i`, so
the one kernel loop covers both the filtered and the unfiltered path.

The economics, with `sel_t` being `uint32_t`
(`src/include/duckdb/common/typedefs.hpp:30`), 8 columns of 8-byte values,
and half the chunk surviving:

```
 copy out 1024 survivors  = 1024 rows × 8 cols × 8 B = 65,536 B written
 build a selection vector = 1024 × sizeof(sel_t)     =  4,096 B written
 ratio                                                = 16× less traffic
```

— and the 16× is the *floor*, because the copy also has to be paid again
by the next filter in the chain, while a selection vector composes: filter
two selects from filter one's output positions and still moves no data.

The kernel shape, in Rust, so the loop is legible:

```rust
// ILLUSTRATION — not quoted from duckdb; the real select loops are the
// templates in src/include/duckdb/common/vector_operations/unary_executor.hpp:310
// (`SelectLoopSelSwitch`), which handle the vector-type and validity cases
// this omits. The (data, sel, count) shape and the branch-free body are real.
fn filter_lt(v: &[i64], t: i64, sel: &[u32], out_sel: &mut [u32]) -> usize {
    let mut n = 0;
    for &i in sel {
        out_sel[n] = i;                    // branch-free: write always,
        n += (v[i as usize] < t) as usize; // advance only on match
    }
    n   // survivor count — the data vectors are untouched, zero copies
}
```

The branch-free body is the point: write unconditionally, advance the
counter only on match, so there is no branch for the predictor to miss on
50%-selective data. [FINDINGS.md](../../FINDINGS.md) row 17 puts a number
on what that avoids — a branchy filter runs 0.95 GB/s where a branchless
one runs ~10 GB/s on the same data. A DICTIONARY vector (Step 2) is this
same trick promoted from a call argument to a vector representation.

### Step 4 — pipelines: the plan splits at its breakers

> **In:** an operator tree, which says what to compute but not what may
> run at the same time as what.
> **Out:** the same tree cut into **pipelines** at its **pipeline
> breakers**, each pipeline a schedulable unit with a parallelism degree
> and a dependency on the pipelines that must finish first.

Some operators **stream**: a filter or a projection can consume one chunk
and emit its result immediately. Others must consume *all* their input
before they can emit anything — a hash-join build must see every build
row before the first probe is legal; a sort must see every row before it
knows what comes first. Those are **pipeline breakers**, and the act of
accumulating their whole input into memory is **materialization**.

A **pipeline** is what lies between breakers: a **source** (a scan, or a
previous pipeline's materialized result) → a chain of streaming operators
→ a **sink** (the breaker). For `SELECT k, SUM(v) FROM t JOIN s ... GROUP BY k`:

```
 pipeline 1:  scan(s) ──────────────────► build hash table   (sink)
 pipeline 2:  scan(t) → probe HT → ...  ► hash aggregate     (sink)
              (runs only after pipeline 1's sink is complete)
```

Pipelines are the scheduling unit: each may be run by many threads at
once, and dependencies (build before probe) gate execution.
`Pipeline::ScheduleParallel` (`src/parallel/pipeline.cpp:136-153`) decides
the degree by asking *both* ends — `TryGetMaxThreads` (`:101-134`) starts
from the source's `MaxThreads()`, lets every intermediate operator lower
it, clamps to `TaskScheduler::NumberOfThreads()`, and lets the sink lower
it again — and falls back to `ScheduleSequentialTask` (`:95`) when any
participant says no.

This is where **morsel-driven parallelism** plugs in
([reading-morsel-parallelism.md](reading-morsel-parallelism.md)): rather
than partitioning the input once and pinning a partition per thread, the
source hands out small work units — **morsels** — that idle workers pull,
which is how the scheme both **work-steals** (a thread that finishes early
takes the next morsel instead of idling) and stays **NUMA-local** (a
worker prefers morsels whose pages are on its own socket). DuckDB's morsel
is a row group, and the size is computed, not hard-coded:

```cpp
// src/storage/data_table.cpp — how many parallel work units a table offers.
   276  idx_t DataTable::MaxThreads(ClientContext &context) const {
   277  	idx_t row_group_size = GetRowGroupSize();
   278  	idx_t parallel_scan_vector_count = row_group_size / STANDARD_VECTOR_SIZE;
   279  	if (ClientConfig::GetConfig(context).verify_parallelism) {
   280  		parallel_scan_vector_count = 1;
   281  	}
   282  	idx_t parallel_scan_tuple_count = STANDARD_VECTOR_SIZE * parallel_scan_vector_count;
   283  	return GetTotalRows() / parallel_scan_tuple_count + 1;
   284  }
```

With `DEFAULT_ROW_GROUP_SIZE` at 122880
(`src/include/duckdb/storage/storage_info.hpp:26`, and `:394` asserts it is
a multiple of the vector size), the arithmetic is exact:

```
 vectors per row group      = 122880 / 2048            = 60 vectors
 morsel                     = 2048 × 60                = 122,880 rows
 units for the 50 M-row lane = 50e6 / 122880 + 1       = 408 work units
```

408 units across 8 hardware threads is 51 morsels each — enough
granularity that a straggler costs at most one morsel of tail latency, and
coarse enough that the per-morsel scheduling cost is amortized over
122,880 rows. That is the "what bounds morsel size from below and above"
question, answered on this table.

### Step 5 — the executor protocol: push within, pull between

> **In:** pipelines that need to run, and the awkward fact that operators
> do not preserve cardinality — a join can turn one 2048-row input chunk
> into ten output chunks.
> **Out:** a **push**-based loop inside each task (the executor drives
> chunks *down* into the sink) wrapped in a **pull**-based one between
> tasks (workers pull morsels), plus the four-state protocol that keeps
> memory bounded when the shapes do not match.

In a **pull** model — the textbook Volcano one — control flows downward:
the root calls `next()` on its child, which calls `next()` on its child.
In a **push** model, control flows upward from the source: the executor
fetches a chunk and hands it to operator 0, then hands operator 0's result
to operator 1, and so on into the sink. DuckDB is push *inside* a pipeline
task and pull *between* tasks (workers pull morsels from the source).

The reason push needs a protocol at all is cardinality. Each operator call
returns an `OperatorResultType`:

```cpp
// src/include/duckdb/common/enums/operator_result_type.hpp — the contract,
// stated in its own comment. 15-27 in full.
    15  //! The OperatorResultType is used to indicate how data should flow around a regular (i.e. non-sink and non-source)
    16  //! physical operator
    17  //! There are four possible results:
    18  //! NEED_MORE_INPUT means the operator is done with the current input and can consume more input if available
    19  //! If there is more input the operator will be called with more input, otherwise the operator will not be called again.
    20  //! HAVE_MORE_OUTPUT means the operator is not finished yet with the current input.
    21  //! The operator will be called again with the same input.
    22  //! FINISHED means the operator has finished the entire pipeline and no more processing is necessary.
    23  //! The operator will not be called again, and neither will any other operators in this pipeline.
    24  //! BLOCKED means the operator does not want to be called right now. e.g. because its currently doing async I/O. The
    25  //! operator has set the interrupt state and the caller is expected to handle it. Note that intermediate operators
    26  //! should currently not emit this state.
    27  enum class OperatorResultType : uint8_t { NEED_MORE_INPUT, HAVE_MORE_OUTPUT, FINISHED, BLOCKED };
```

Four states, not three — `BLOCKED` (24-26) is the async-I/O escape hatch,
and the comment is explicit that intermediate operators should not emit
it, so in practice the streaming operators you will read use three.
`HAVE_MORE_OUTPUT` is the interesting one: it exists so that an operator
which explodes its input does *not* buffer the explosion internally. The
executor calls it again with the same input.

The loop that implements this:

```cpp
// src/parallel/pipeline_executor.cpp — the source-to-sink loop inside
// Execute(max_chunks) (260). 296-301 fetch, 319-323 push.
   296  		} else if (!exhausted_pipeline || next_batch_blocked) {
   297  			SourceResultType source_result = SourceResultType::BLOCKED;
   298  			if (!next_batch_blocked) {
   299  				// "Regular" path: fetch a chunk from the source and push it through the pipeline
   300  				source_chunk.Reset();
   301  				source_result = FetchFromSource(source_chunk);
// ... 302-318: BLOCKED/FINISHED handling and the batch-index path ...
   319  			if (exhausted_pipeline && source_chunk.size() == 0) {
   320  				continue;
   321  			}
   322  
   323  			result = ExecutePushInternal(source_chunk, chunk_budget);
```

`ExecutePushInternal` (`:375-422`) then loops the chunk through the
operator chain via `Execute(input, result, idx)` (`:483`) and into
`Sink` — and its own `do … while (chunk_budget.Next())` at 387-420 is the
`HAVE_MORE_OUTPUT` loop in the flesh: it re-executes the *same* input
until the operator says `NEED_MORE_INPUT` (417-419) or the budget runs out.

Why the protocol beats internal buffering, on numbers: memory in flight
per thread is one chunk per operator.

```
 5 operators × 128 KB/chunk         =  640 KB per worker thread
 × 8 worker threads                 =  5.1 MB for the whole query
 the same join buffering internally: 50e6 rows × 10× fanout × 64 B = 32 GB
```

Bounded memory is not a nice property here; it is the difference between
running and not running.

### Step 6 — the join hash table: salt bits before pointer chases

> **In:** a probe side of 2048 keys per chunk and a hash table far larger
> than cache, where the naive probe costs two *dependent* cache misses per
> key — load the entry, then dereference its pointer to compare the key.
> **Out:** a probe that answers most non-matches from the first load
> alone, because the entry's unused pointer bits carry a **salt** — a
> slice of the key's hash — and a mismatched salt proves a mismatched key.

Build side: each thread collects its chunks into thread-local partitioned
row-format storage (no contention), and the partitions are merged at the
end — `JoinHashTable::Merge` (`src/execution/join_hashtable.cpp:149-187`),
whose `sink_collection->Combine` is line 169. That is the morsel-driven
two-phase pattern of Step 4 applied to a hash table.

The table itself is **open-addressed with linear probing** — not chained —
and each slot is one 8-byte word doing two jobs:

```cpp
// src/include/duckdb/execution/ht_entry.hpp — the split, 33-37, and the
// extraction, 73-80.
    33  #else
    34  	//! Upper 16 bits are salt, lower 48 bits are the pointer
    35  	static constexpr const hash_t SALT_MASK = 0xFFFF000000000000;
    36  	static constexpr const hash_t POINTER_MASK = 0x0000FFFFFFFFFFFF;
    37  #endif
// ... 38-72: constructors, IsOccupied, GetPointer, SetPointer ...
    73  	// Returns the salt, leaves upper salt bits intact, sets lower bits to all 1's
    74  	static inline hash_t ExtractSalt(const hash_t &hash) {
    75  		return hash | POINTER_MASK;
    76  	}
    77  
    78  	inline hash_t GetSalt() const {
    79  		return ExtractSalt(value);
    80  	}
```

So the salt is **16 bits** wide (line 34, and `SALT_MASK` at 35 spells the
split out), living in the top of a pointer that only needs 48. This is
topic 2's bit-smuggling, and the payoff is a probe loop that dereferences
nothing until the salt agrees:

```cpp
// src/execution/join_hashtable.cpp — the salted probe, 243-266, inside
// ProbeForPointersInternal (232).
   243  		if (USE_SALTS) {
   244  			// increment the ht_offset of the entry as long as the next entry is occupied and salt does not match
   245  			while (true) {
   246  				const ht_entry_t entry = entries.get()[row_ht_offset];
   247  				const bool occupied = entry.IsOccupied();
   248  
   249  				// the entry is empty -> no match possible
   250  				if (!occupied) {
   251  					break;
   252  				}
   253  
   254  				const hash_t row_salt = ht_entry_t::ExtractSalt(row_hash);
   255  				const bool salt_match = entry.GetSalt() == row_salt;
   256  				if (salt_match) {
   257  					// we know that the entry is occupied and the salt matches -> compare the keys
   258  					auto row_index = GetOptionalIndex<HAS_SEL>(row_sel, i);
   259  					AddPointerToCompare(state, entry, pointers_result_v, row_ht_offset, keys_to_compare_count,
   260  					                    row_index);
   261  					break;
   262  				}
   263  
   264  				// full and salt do not match -> continue probing
   265  				IncrementAndWrap(row_ht_offset, ht.bitmask);
   266  			}
```

Line 250 breaks on an empty slot; 254-255 compare salts; only 259 records
a pointer for the key comparison to follow. A non-matching key whose salt
differs never touches the tuple.

Worked, with the real `k = 16`:

```
 spurious salt match on a non-matching key = 2^-16       = 1 / 65,536
 of a 2048-key probe chunk, expected false hits          = 0.031 keys
 tuple dereferences avoided per chunk of all-misses      ≈ 2047.97
 at the measured ~25 ns DRAM plateau (topic 0 notes)     ≈ 51 µs saved
```

That the salt is a *cache-miss* optimization and not a compare
optimization is not an inference — DuckDB turns it off when there are no
misses to save:

```cpp
// src/include/duckdb/execution/join_hashtable.hpp — when salting is worth it.
    93  	//! only compare salts with the ht entries if the capacity is larger than 8192 so
    94  	//! that it does not fit into the CPU cache
    95  	static constexpr const idx_t USE_SALT_THRESHOLD = 8192;
```

Check the threshold against the ladder: 8192 entries × 8 B = 64 KB, which
fits inside this machine's measured 128 KB L1d. Below the threshold the
entry load is an L1 hit, the pointer chase is an L2 hit, and the salt
compare would be pure added instructions — so it is compiled out
(`UseSalt()`, `:370-373`, selects between two template instantiations).

And the whole probe is vectorized, which is what makes the misses overlap:

```cpp
// src/execution/join_hashtable.cpp — GetRowPointersInternal (300): probe the
// whole vector, compare the whole vector, re-probe only the non-matches.
   323  	do {
   324  		const idx_t keys_to_compare_count = ProbeForPointers<USE_SALTS>(state, ht, entries, pointers_result_v, row_sel,
   325  		                                                                elements_to_probe_count, has_row_sel);
   326  
   327  		// if there are no keys to compare, we are done
   328  		if (keys_to_compare_count == 0) {
   329  			break;
   330  		}
   331  
   332  		// Perform row comparisons, after Match function call salt_match_sel will point to the keys that match
   333  		keys_no_match_count = 0;
   334  		const idx_t keys_match_count =
   335  		    ht.row_matcher_build.Match(keys, key_state.vector_data, state.keys_to_compare_sel, keys_to_compare_count,
   336  		                               pointers_result_v, &state.keys_no_match_sel, keys_no_match_count);
// ... 337-350: append the matches to match_sel ...
   351  		for (idx_t i = 0; i < keys_no_match_count; i++) {
   352  			const auto row_index = state.keys_no_match_sel.get_index(i);
   353  			auto ht_offset_and_salt = ht_offsets_and_salts[row_index];
   354  			IncrementAndWrap(ht_offset_and_salt, ht.bitmask | ht_entry_t::SALT_MASK);
   355  			hashes_dense[i] = ht_offset_and_salt; // populate dense again
   356  		}
   357  
   358  		// in the next iteration, we have a selection vector with the keys that do not match
   359  		row_sel = state.keys_no_match_sel;
   360  		has_row_sel = true;
   361  
   362  		elements_to_probe_count = keys_no_match_count;
   363  
   364  	} while (DUCKDB_UNLIKELY(keys_no_match_count > 0));
```

Read 351-362: linear probing's "keep walking until you find it" is
expressed as a *selection vector of the keys that have not yet resolved*,
fed back into the next round. That is Step 3's mechanism reused as control
flow. And because the 2048 entry loads at 246 are issued from a loop with
no dependency between iterations, the core can have many of them in flight
at once — **memory-level parallelism**, the reason a vectorized probe beats
a fused compiled loop that resolves one key at a time
([reading-compiled-vs-vectorized.md](reading-compiled-vs-vectorized.md)).

## Where each step lives in the code

Read in this order: the vector types (the data plane), then the pipeline
executor (the control plane), then the join hash table (where it pays).
All anchors are duckdb `6c0c1a68`.

| File | Lines | What is there | Step |
|---|---|---|---|
| `src/include/duckdb/common/vector_size.hpp` | 15-25 | `DEFAULT_STANDARD_VECTOR_SIZE 2048U`, overridable, power-of-two enforced | 1 |
| `src/include/duckdb/common/types/data_chunk.hpp` | 26-51 | `DataChunk` = `vector<Vector> data` + count; the comment already names the filter/selection-vector case | 1 |
| `src/include/duckdb/common/enums/vector_type.hpp` | 15-22 | the six `VectorType` kinds | 2 |
| `src/include/duckdb/common/vector/unified_vector_format.hpp` | 22-35 | `(sel, data, validity)` — the normalize-then-one-loop dodge | 2 |
| `src/include/duckdb/common/types/vector.hpp` | 127 | `ToUnifiedFormat` — the entry point to it | 2 |
| `src/include/duckdb/common/types/selection_vector.hpp` | 124-140, 175 | `set_index`, `get_index` (null `sel` = identity, 135), `sel_t *sel_vector` | 3 |
| `src/include/duckdb/common/typedefs.hpp` | 30 | `typedef uint32_t sel_t` — 4 bytes per selected row | 3 |
| `src/include/duckdb/common/vector_operations/unary_executor.hpp` | 310 | `SelectLoopSelSwitch` — the real select kernels | 3 |
| `src/parallel/pipeline.cpp` | 95, 101-134, 136-153 | sequential fallback; `TryGetMaxThreads`; `ScheduleParallel` | 4 |
| `src/storage/data_table.cpp` | 276-284 | `MaxThreads` — morsels are row groups, computed here | 4 |
| `src/include/duckdb/storage/storage_info.hpp` | 26, 394 | `DEFAULT_ROW_GROUP_SIZE 122880ULL`; asserted a multiple of the vector size | 4 |
| `src/include/duckdb/common/enums/operator_result_type.hpp` | 15-27 | the four-state operator contract, with its own rationale | 5 |
| `src/parallel/pipeline_executor.cpp` | 260, 301, 375-422, 483 | `Execute(max_chunks)`; `FetchFromSource`; `ExecutePushInternal`; the per-operator `Execute` | 5 |
| `src/execution/join_hashtable.cpp` | 149-187 | `Merge` — thread-local partitions combined at 169 | 6 |
| `src/include/duckdb/execution/ht_entry.hpp` | 33-37, 73-80 | 16 salt bits / 48 pointer bits in one word; `ExtractSalt` | 6 |
| `src/execution/join_hashtable.cpp` | 232-279 | `ProbeForPointersInternal` — the salted linear probe | 6 |
| `src/execution/join_hashtable.cpp` | 300-368, 370-373 | vectorized probe rounds driven by a non-match selection vector; `UseSalt()` | 6 |
| `src/include/duckdb/execution/join_hashtable.hpp` | 93-95 | `USE_SALT_THRESHOLD = 8192` — and *why*, in the comment | 6 |

## Takeaway

The whole engine is one decision propagated: make the unit 2048 rows, and
then live with the consequences. Dispatch stops mattering (Step 1), so the
representation of a batch becomes worth optimizing (Step 2). A batch is
too expensive to copy, so filters return indices instead (Step 3). Batches
must be scheduled, so the plan is cut at its breakers (Step 4) and
operators need a protocol for not matching cardinalities (Step 5). And a
batch of independent probes is exactly what a core needs to overlap cache
misses, which is where the model's largest wins actually come from (Step
6) — not from the dispatch it saved, but from the memory-level parallelism
it made possible.

What it does *not* buy you is on the other side of the ledger. This repo's
Volcano lane ([FINDINGS.md](../../FINDINGS.md) row 11) shows the
tuple-at-a-time cost rising with selectivity — 103.3 M rows/s at 50%,
74.7 M at 95% — because the expensive thing is a row *surviving*, not a
predicate being evaluated. Batching divides the per-call term by 2048; the
per-surviving-row term is what Steps 3 and 6 are for.

## Questions for notes.md

1. Why 2048 and not 64K (X100 used ~1K)? Compute: chunk bytes for 8
   columns × 8 B at each size vs your measured L2 (topic 0 ladder).
2. CONSTANT vectors: trace `2 * price` where price is FLAT and 2 is
   CONSTANT — which loop runs? What would a Volcano engine do per row?
3. `HAVE_MORE_OUTPUT`: which operators need it and why can't they just
   buffer internally? (Memory bound + who owns the chunk.)
4. The salt trick: with 64-bit hashes and k salt bits, what fraction of
   non-matching probes still chase a pointer? Pick k — then check yours
   against DuckDB's 16.
5. M11: your Expand operator explodes one source node into deg(n)
   results — that's `HAVE_MORE_OUTPUT` shaped. Sketch the state it must
   keep between calls.

## Done when

Answer each before unfolding it.

- [ ] You can say what a DataChunk is, what bounds its size from above and from below, and quote the constant rather than remembering it.

  <details><summary>Answer</summary>

  A DataChunk is a set of vectors — one contiguous array per column, all
  the same length — plus a row count; it is the unit that crosses every
  operator boundary (`data_chunk.hpp:44-51`). Its length is
  `STANDARD_VECTOR_SIZE`, which `vector_size.hpp:16` sets to
  `DEFAULT_STANDARD_VECTOR_SIZE 2048U` unless the build overrides it, and
  which lines 23-25 require to be a power of two.

  From below, the bound is dispatch amortization: at 100M rows through a
  5-operator plan and 20 cycles a call, tuple-at-a-time costs 2.5 s of pure
  dispatch (25 ns/row) and a 2048-row unit costs 0.0012 s (0.012 ns/row).
  Shrink the chunk to 64 rows and you give 32× of that back.

  From above, the bound is cache residency between operators. Eight 8-byte
  columns is 64 B/row, so 2048 rows is exactly 128 KB — the size at which
  topic 0's measured latency ladder says this machine's L1d plateau ends.
  At 65,536 rows the same chunk is 4 MB, so every handoff from one operator
  to the next reads from L2 (7.6-9.0 ns measured) rather than L1 (1.02 ns).

  </details>

- [ ] You can explain how a filter produces output without copying data, and why "no selection vector" is not a special case.

  <details><summary>Answer</summary>

  Its output is a `SelectionVector` — an array of `sel_t` (`uint32_t`,
  `typedefs.hpp:30`) holding the *positions* that survived — layered over
  the same, untouched data vectors. Downstream kernels take
  `(data, sel, count)` and iterate `sel`. For 1024 survivors of 8 8-byte
  columns that is 4,096 bytes written instead of 65,536: 16× less traffic,
  and it composes, because a second filter selects from the first's
  positions and still moves nothing.

  "No selection" is not special because `get_index`
  (`selection_vector.hpp:134-136`) returns `idx` when `sel_vector` is null.
  An absent selection vector *is* the identity mapping, so one kernel loop
  serves the filtered and unfiltered paths and there is no branch on
  "was there a filter" in the hot loop.

  </details>

- [ ] You can draw the two pipelines for `SELECT k, SUM(v) FROM t JOIN s ... GROUP BY k` and say which operator is the sink of which.

  <details><summary>Answer</summary>

  Pipeline 1: `scan(s)` → build the hash table, and the build is the sink.
  Pipeline 2: `scan(t)` → probe the hash table → the hash aggregate, which
  is *its* sink. Pipeline 2 depends on pipeline 1 having finished, because
  the build is a pipeline breaker: it cannot emit anything until it has
  seen every build row. The probe, by contrast, streams — it is an
  intermediate operator in pipeline 2, not a sink.

  The cut is always at a breaker, and a breaker is exactly an operator
  that must materialize its whole input (build, sort, the aggregate's hash
  table) before producing a first row.

  </details>

- [ ] You can explain the salt trick in two sentences, give the real salt width, and say why DuckDB switches it off for small tables.

  <details><summary>Answer</summary>

  DuckDB stores each hash-table slot as a single 8-byte word whose low 48
  bits are the tuple pointer and whose high 16 bits are a slice of the
  key's hash — the salt (`ht_entry.hpp:34-36`). A probe compares the salt
  first (`join_hashtable.cpp:254-255`), and since a differing salt proves a
  differing key, all but 2^-16 = 1/65,536 of non-matching probes are
  rejected from the entry load alone, never dereferencing the pointer.

  It is switched off below `USE_SALT_THRESHOLD = 8192`
  (`join_hashtable.hpp:93-95`) because the saving is a *cache miss*, not a
  comparison: 8192 entries × 8 B is 64 KB, which fits in this machine's
  measured 128 KB L1d, so there is no miss to avoid and the salt compare
  would be pure added work. `UseSalt()` (`:370-373`) picks between two
  template instantiations, so the branch does not exist at run time either.

  </details>

- [ ] You can say why `HAVE_MORE_OUTPUT` exists rather than letting operators buffer their own output.

  <details><summary>Answer</summary>

  Because buffering is unbounded and the executor's memory is not. An
  operator that returns `HAVE_MORE_OUTPUT`
  (`operator_result_type.hpp:20-21`) is telling the executor "call me again
  with the *same* input" — so the explosion is drained one chunk at a time
  through `ExecutePushInternal`'s loop
  (`pipeline_executor.cpp:387-420`) instead of accumulating. Peak memory is
  therefore one chunk per operator per thread: 5 operators × 128 KB × 8
  threads ≈ 5.1 MB, against 50e6 rows × 10× join fanout × 64 B = 32 GB if
  the join buffered its own output.

  The second reason is ownership: chunks belong to the executor, which
  reuses them (`final_chunk.Reset()`, `:390`). An operator that buffered
  would have to own — and allocate — its output instead.

  </details>

## References

**Code**
- [duckdb](https://github.com/duckdb/duckdb) at `6c0c1a68` — the data
  plane: `src/include/duckdb/common/vector_size.hpp`,
  `enums/vector_type.hpp`, `types/data_chunk.hpp`,
  `types/selection_vector.hpp`, `vector/unified_vector_format.hpp`; the
  control plane: `src/parallel/pipeline.cpp`,
  `src/parallel/pipeline_executor.cpp`, `src/storage/data_table.cpp`; the
  payoff: `src/execution/join_hashtable.cpp`,
  `src/include/duckdb/execution/ht_entry.hpp`; ~2 h

**In this repo**
- [FINDINGS.md](../../FINDINGS.md) row 11 — the Volcano ceiling this model
  is trying to beat, and the direction it moves in
- [FINDINGS.md](../../FINDINGS.md) row 17 — branchy vs branchless filter
  throughput, the number behind Step 3's branch-free kernel
- [topics/00-performance-toolbox/notes.md](../00-performance-toolbox/notes.md)
  — the measured cache ladder every size argument here is checked against
