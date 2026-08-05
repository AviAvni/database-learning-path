# DuckDB's encoding zoo: analyze, score, commit

Who picks the encoding? ClickHouse says *you do*, in the DDL. BtrBlocks says *a sampler
does*. DuckDB's answer is the third one: **nobody guesses — run every candidate encoder over
the real data, take the smallest estimate, and only then compress.** It is
benchmark-before-committing, wired into a production storage engine's checkpoint path.

Before you open the C++, this chapter builds the ideas one at a time: what a lightweight
encoding is, the four nested units the decision is made for, the two-pass lifecycle that
makes racing affordable, the deliberate bias in the scoring, the random-access constraint
that shapes the whole menu, two encoders end to end, the string stack, and the zone maps
that filter pushdown physically lands on. Then it hands you file-and-line anchors for each
piece.

All anchors are `duckdb/duckdb@6c0c1a68`; check them with
`tools/pinned-source.py show duckdb/duckdb <path> -r A:B`.

Read [reading-cstore-compression.md](reading-cstore-compression.md) first if you have not.
Step 6 below is Abadi's 2006 thesis running in production, and it is much more striking when
you have read the paper it descends from.

---

## The problem in one sentence

Analytic scans are memory-bound, so bytes moved ≈ time — an encoding that shrinks a column
6× can make the scan several times faster — but the right encoding differs per column *and*
per 2,048 rows, a wrong guess can *inflate* the data, and the only way to know which is
right is to try them all on the actual bytes.

---

## The concepts, step by step

### Step 1 — Lightweight encoding: compression the scan can execute over

> **In:** a column of values, and a scan loop that has to touch every one of them.
> **Out:** the distinction between an encoding and a block compressor, and why only one of
> them belongs inside the loop.

An **encoding** here is not gzip. It is a reversible rewrite of a column's values that
exploits a *pattern in the data* — repetition, a narrow range, few distinct values — and
whose decode is a handful of arithmetic instructions, cheap enough to run inside the scan
loop. A **block compressor** (gzip / zstd class) treats bytes as opaque, achieves better
ratios on average, and must inflate a whole block before you can read anything in it.

Two terms you will need throughout, defined here:

- **Run-length encoding (RLE)** — store each maximal run of equal values as one
  `(value, count)` pair.
- **Bit-packing** — store integers in `ceil(log2(range))` bits instead of the type's full
  width, with no byte alignment between them.

Both appear in DuckDB's menu; so do **frame-of-reference**, **delta encoding**, **dictionary
encoding** and **FSST**, each defined at the step that uses it.

DuckDB's registered menu, in the order the engine considers them
(`src/function/compression_config.cpp:17-35`): `CONSTANT`, `UNCOMPRESSED`, `RLE`,
`BITPACKING`, `DICTIONARY`, `CHIMP`, `PATAS`, `ALP`, `ALPRD`, `FSST`, `ZSTD`, `ROARING`,
`EMPTY` (all-valid validity masks), `DICT_FSST`. Thirteen real functions plus the `AUTO`
sentinel. Exactly one of them — `ZSTD` — is a block compressor, and Step 5 explains why it
is there at all.

**Why it matters:** since a scan's cost is bytes moved, a lightweight encoding is not a
space feature that costs time; it *is* the performance feature. That inversion is this
topic's thesis, and DuckDB is where you can watch it being decided per column.

### Step 2 — Four nested units, and the one the decision is made for

> **In:** a table being checkpointed to disk.
> **Out:** the four sizes that matter, with the arithmetic relating them.

| Unit | Size | Defined at |
| --- | --- | --- |
| **row group** — a horizontal slice of the table | **122,880** rows | `src/include/duckdb/storage/storage_info.hpp:26` — `#define DEFAULT_ROW_GROUP_SIZE 122880ULL` |
| **column segment** — one column's data within a row group, encoded one way | ≤ a row group | the checkpoint unit; see Step 3 |
| **vector** — the execution and analyze unit | **2,048** values | `src/include/duckdb/common/vector_size.hpp:16` — `#define DEFAULT_STANDARD_VECTOR_SIZE 2048U` |
| **bitpacking group** — the *mode* unit inside a bit-packed segment | **2,048** values | `src/storage/compression/bitpacking.cpp:25` |

The relation is exact, and the codebase asserts it: `storage_info.hpp:394` refuses to compile
unless `DEFAULT_ROW_GROUP_SIZE % STANDARD_VECTOR_SIZE == 0`. So

```
122,880 / 2,048 = 60 vectors per row group
```

— sixty `analyze` calls per candidate encoder per column, and sixty independently-moded
bitpacking groups inside a bit-packed segment.

```
 table
 └─ row group (122,880 rows = 60 vectors)      <- the encoding decision unit
    ├─ column "ts"   -> segment encoded as BITPACKING, 60 groups, each with its own mode
    ├─ column "city" -> segment encoded as DICT_FSST
    └─ column "id"   -> segment encoded as BITPACKING
```

Note the third line of `bitpacking.cpp:25`: the group size is
`STANDARD_VECTOR_SIZE > 512 ? STANDARD_VECTOR_SIZE : 2048`, so on a build with a smaller
vector size the group is still 2,048 — the mode granularity is pinned independently of the
execution granularity.

**Why it matters:** data shape drifts within a table — early rows sorted, late rows random;
one column low-cardinality, the next unique. A global choice is always wrong somewhere.
Per-row-group choice bounds the damage of a bad fit to 122,880 rows, and bitpacking's
per-group modes bound it further, to 2,048.

### Step 3 — analyze → score → compress: one scan, every candidate

> **In:** a column being checkpointed, and the thirteen registered functions.
> **Out:** the selection loop, and the four ways a candidate can lose.

The contract is documented in the framework header itself,
`src/include/duckdb/function/compression_function.hpp:130-138`:

> "The analyze functions are used to determine whether or not to use this compression
> method… 1. The `init_analyze` is called to initialize the analyze state of every candidate
> compression method. 2. The `analyze` method is called with all of the input data in the
> order in which it must be stored. `analyze` can return 'false'. In that case, the
> compression method is taken out of consideration early. 3. The `final_analyze` method is
> called, which should return a score for the compression method… The system then decides
> which compression function to use based on the analyzed score."

Three typedefs implement it — `compression_init_analyze_t` at `:139`,
`compression_analyze_t` at `:140`, `compression_final_analyze_t` at `:141` — and the winner's
`compression_compress_data_t` at `:148`.

The loop that drives them is `ColumnDataCheckpointer::DetectBestCompressionMethod`,
`src/storage/table/column_data_checkpointer.cpp:172-278`. The shape is not one pass per
encoder; it is **one scan, feeding every candidate the same vector**:

```rust
// ILLUSTRATION — Rust sketch of C++ control flow, not quoted. The real loop is
// duckdb/duckdb@6c0c1a68 src/storage/table/column_data_checkpointer.cpp:172-278;
// the single shared scan is at :200-217 and the score comparison at :245-256.
fn detect_best(col: &Column, mut candidates: Vec<Encoder>) -> Encoder {
    let mut states: Vec<Option<State>> = candidates.iter().map(|e| e.init_analyze()).collect();

    for vector in col.vectors() {                  // ONE pass over the data (:200)
        for (enc, st) in candidates.iter().zip(states.iter_mut()) {
            if let Some(s) = st {
                if !enc.analyze(s, vector) { *st = None; }   // dropped for good (:211-214)
            }
        }
    }

    let mut best = (usize::MAX, None);
    for (enc, st) in candidates.iter().zip(states) {
        let Some(s) = st else { continue };
        let score = enc.final_analyze(s);           // ESTIMATED bytes (:245)
        if score == INVALID_INDEX { continue }      // self-disqualified (:248-250)
        if score < best.0 { best = (score, Some(enc)); }     // strict < : ties go to the
    }                                                        // earlier-registered function
    best.1.expect("no suitable compression method")          // FatalException at :265-268
}
```

Four ways to lose, all in that loop:

1. **Drop out mid-scan** — `analyze` returns `false` and the state is nulled for the rest of
   the pass (`:211-214`), so a hopeless candidate costs nothing further. `BitpackingAnalyze`
   at `bitpacking.cpp:318-334` does this when one group would not fit in a block, and again
   whenever a value overflows the state.
2. **Self-disqualify at the end** — `final_analyze` returns `DConstants::INVALID_INDEX`
   (`:247-250`). `BitpackingFinalAnalyze` at `bitpacking.cpp:337-344` does exactly this when
   its final `Flush` fails.
3. **Lose on score** — `:252`, `score < best_score`, strictly. On a tie the **earlier**
   entry in `compression_config.cpp:17-35` wins, which is why `CONSTANT` and `UNCOMPRESSED`
   head the list.
4. **Never run at all** — `:196`, `skip_scan`. If the column's DDL names a compression type,
   the analyze scan is skipped outright. That is the ClickHouse-style declaration escape
   hatch, sitting inside the automatic system. `PRAGMA force_compression` (`:185-188`) is the
   session-level version, and is how you run this topic's experiments.

The cost is honest and stated by the design: this is a **two-pass** ingest. DuckDB reads the
whole column once to choose and once to compress. And if nothing qualifies, `:265-268`
throws — which is why `UNCOMPRESSED` is in the menu, as the candidate that always scores.

**Why it matters:** it is the same discipline this repo's `verify.sh` enforces on itself —
measure, then commit — implemented in a storage engine's hot path. And it is the direct
alternative to the two other answers in this topic, which Step 4 prices.

### Step 4 — The score is deliberately biased, and sometimes sampled rather than measured

> **In:** a `final_analyze` about to return an estimated byte count.
> **Out:** the two ways that number is not a byte count, and why both are correct.

**The bias.** Dictionary encoding's `final_analyze`, `dictionary_compression.cpp:85-98`,
computes the real required space and then inflates it on the way out:

```cpp
// duckdb/duckdb@6c0c1a68 src/storage/compression/dictionary_compression.cpp:92-97
   92  	auto width = BitpackingPrimitives::MinimumBitWidth(state.current_unique_count + 1);
   93  	auto req_space = DictionaryCompression::RequiredSpace(state.current_tuple_count, state.current_unique_count,
   94  	                                                      state.current_dict_size, width);
   95
   96  	const auto total_space = state.segment_count * state.info.GetBlockSize() + req_space;
   97  	return LossyNumericCast<idx_t>(DictionaryCompression::MINIMUM_COMPRESSION_RATIO * float(total_space));
```

with `MINIMUM_COMPRESSION_RATIO = 1.2F`
(`src/include/duckdb/storage/compression/dictionary/common.hpp:20`). FSST does the same,
`src/storage/compression/fsst.cpp:37` and `:202`. So both schemes report themselves **20%
larger than they are**, and must beat the alternatives by more than 20% to be chosen.

Work it on this topic's shared column — 1,000,000 `INT64` values, 200 distinct, average run
8. Dictionary's true cost (Step 8 derives it) is 1,001,600 B and bit-packing's is
≈1,259,664 B:

```
true:      1,259,664 / 1,001,600 = 1.258   dictionary wins by 25.8%
reported:  1,259,664 / 1,201,920 = 1.048   dictionary wins by  4.8%
                       ^ 1,001,600 x 1.2
```

Still a win — but a 5% margin instead of a 26% one, and a column only slightly less
favourable would flip. The bias buys back the costs that do not appear in a byte count:
dictionary and FSST both add an indirection on every decoded value, and `fetch_row` on them
is slower than on a bit-packed segment. A scheme whose score is *its own size* would be
chosen too often.

**The sampling.** The header says `analyze` "is called with all of the input data", and for
RLE, bit-packing and dictionary that is literally true — every one of the 60 vectors. Two
encoders quietly do less:

| Encoder | Sample | Fraction of a row group |
| --- | --- | --- |
| FSST — `fsst.cpp:38`, `ANALYSIS_SAMPLE_SIZE = 0.25` | 25% of the strings | 25% |
| ALP — `src/include/duckdb/storage/compression/alp/alp_constants.hpp:19-23` | `RG_SAMPLES = 8` vectors × `SAMPLES_PER_VECTOR = 32` values | `8 × 32 / 122,880` = **0.208%** |
| BtrBlocks, for comparison (SIGMOD 2023 §3.1) | 10 runs of 64 values per 64,000-value block | 1% |

ALP's sampling is *five times sparser than BtrBlocks'*, in the system that is otherwise the
poster child for exhaustive analysis. Its stride is spelled out at `:22-23`: "We calculate
how many equidistant vector we must jump within a rowgroup", `(122,880 / 8) / 2,048` =
`15,360 / 2,048` = **7** vectors between samples.

**Why it matters:** "DuckDB analyzes, BtrBlocks samples" is the tidy version of the story and
it is not quite true. The real design rule is *sample when training the model is the
expensive part* — FSST has to build a symbol table, ALP has to search an exponent/factor pair
— and *measure exhaustively when the analyze pass is just counting*. Bring that distinction
to question 1 rather than the tidy version.

### Step 5 — `fetch_row`: the random-access constraint that shapes the menu

> **In:** an operator that wants row 1907 of a segment and nothing else.
> **Out:** the contract entry that decides which encodings are admissible at all.

Every encoding must implement `compression_fetch_row_t`
(`compression_function.hpp:171-173`), documented one line above as "Function prototype used
for reading a single value". Late-materialized fetches and index joins ask for single rows,
not vectors — Step 4 of [reading-cstore-compression.md](reading-cstore-compression.md) is
where that pattern comes from.

The header states the consequence explicitly at `:174-176`, in the doc comment for
`compression_skip_t`: "Function prototype used for skipping 'skip_count' values, **non-trivial
if random-access is not supported for the compressed data.**"

That single requirement explains the menu's shape. RLE can binary-search its run counts;
bit-packing computes a bit offset; dictionary indexes its selection buffer; FSST decodes one
string because its symbol table is static and stateless (see
[reading-btrblocks-fsst.md](reading-btrblocks-fsst.md)). A block compressor cannot: fetching
one row means inflating the whole block, and its internal state is path-dependent.

The framework has two more entries that matter here, both added since the original design:
`compression_select_t` at `:164-166`, "reading a subset of the values of a vector indicated
by a selection vector", and `compression_filter_t` at `:167-170`, "**applying a filter to a
vector while scanning that vector**". Those are filter pushdown reaching all the way into
the encoding — topic 10's plan-level rewrite, landing on physical bytes. Step 6 shows what
RLE does with it.

**Why it matters:** the storage format is negotiated with the *executor*, not chosen for
ratio alone. Zstd is in the menu (`compression_config.cpp:29`) and it does win sometimes —
but it wins on score, against a 20% handicap on its rivals, and it pays for every point
lookup afterwards.

### Step 6 — RLE end to end: Abadi 2006, shipped

> **In:** `src/storage/compression/rle.cpp`, 638 lines.
> **Out:** the whole framework contract in its simplest instance, plus two optimisations you
> have already read the paper for.

RLE is the smallest encoder that exercises every part of the contract. Read it first; every
other encoder repeats its registration pattern.

**Analyze** is a run counter. `RLEAnalyzeState` at `:86-91` wraps an `RLEState<T>`;
`RLEAnalyze` at `:99-110` walks the vector calling `Update`, which increments `seen_count`
on each new value. **Score**, `RLEFinalAnalyze` at `:113-116`, is one line:

```
return (sizeof(rle_count_t) + sizeof(T)) * rle_state.state.seen_count;
```

Bytes = runs × (count size + value size). No bias multiplier — RLE reports its true size.
Note what it is *not*: C-Store's RLE record is a **triple** `(value, start_pos, run_length)`
(C-Store §3.1), because C-Store needed positions addressable. DuckDB stores a **pair** and
reconstructs positions by walking the counts, which is a third less space and a slower
`fetch_row`.

**Compress** writes two arrays into the segment — values from the header forward, counts from
`rle_count_offset` on (`RLECompressState` at `:126`, the scan state's `data_pointer` and
`index_pointer` at `:313-314`).

**Registration**, `GetRLEFunction` at `:568-576`, is the pattern to grep for in every other
encoder: one `CompressionFunction` constructor call bundling `RLEInitAnalyze`, `RLEAnalyze`,
`RLEFinalAnalyze`, the three compress functions, `RLEInitScan`, `RLEScan`, `RLEScanPartial`,
`RLEFetchRow`, `RLESkip`, and — at `:574-575` — `RLESelect` and `RLEFilter`. (`:578-584`
then disables `filter` for `BOOL`.)

Now the two things worth the trip. Both are the 2006 paper, in C++:

**`isOneValue()`, by another name.** `CanEmitConstantVector` at `:333-347` asks whether the
current run covers an entire 2,048-value vector; if so `RLEScanConstant` at `:349-359` sets
`VectorType::CONSTANT_VECTOR` and writes **one** value:

```
result.SetVectorType(VectorType::CONSTANT_VECTOR);
result_data[0] = scan_state.data_pointer[scan_state.entry_pos];
scan_state.position_in_entry += scan_count;
```

2,048 rows produced, one value written. Abadi §5.2's first block property, shipped.

**Predicate once per run.** `RLEFilter` at `:447-490` is the clearest statement of the whole
thesis anywhere in this topic. Its own comments:

> "we haven't applied the filter yet — **apply the filter to all RLE values at once**"
> (`:456-457`)
> "**execute the filter over all runs at once**" (`:463`)
> "early-out, **no runs match the filter so the filter can never pass**" (`:478`)

It builds a `matching_runs` bool array over the run *values* (`:460-475`), caches it on the
scan state (`:310`), and can abandon the entire segment at `:477-481` without decoding a
single row. A predicate over a segment with 125,000 runs and 1,000,000 rows is evaluated
125,000 times, not 1,000,000. That is Abadi §6.2's `num_tuples / avg_run_len`, nineteen years
later, in a shipping engine.

**Why it matters:** if you read only one file in DuckDB's compression directory, read this
one. It is where the paper stops being history.

### Step 7 — Bit-packing: four modes decided by one comparison

> **In:** a group of 2,048 integers.
> **Out:** the four modes, the single arithmetic test that chooses between them, and the
> function that is analyze and compress at the same time.

`BitpackingMode` is an enum of six values, four of them real
(`src/include/duckdb/storage/compression/bitpacking.hpp:15`):

```
enum class BitpackingMode : uint8_t { INVALID, AUTO, CONSTANT, CONSTANT_DELTA, DELTA_FOR, FOR };
```

Two definitions first. **Frame of reference (FOR)** stores the group's minimum once, then
bit-packs each value's offset from it — turning 1,000,000,007 … 1,000,000,900 into 10-bit
offsets, 64 bits down to 10, a 6.4× cut. **Delta encoding** stores each value's difference
from its predecessor; `DELTA_FOR` delta-encodes first and then applies FOR to the deltas,
which is what catches timestamps and sequences.

The mode is chosen in `BitpackingState::Flush`, `bitpacking.cpp:204-271`, and it is a
**priority cascade**, not a race:

| Order | Line | Test | Mode | What it stores |
| --- | --- | --- | --- | --- |
| 1 | `:209` | `all_invalid \|\| maximum == minimum` | `CONSTANT` | one value + a metadata word |
| 2 | `:219` | `maximum_delta == minimum_delta` | `CONSTANT_DELTA` | base + delta + a metadata word |
| 3 | `:230-237` | `!prefer_for` | `DELTA_FOR` | FOR value + width + delta offset + packed deltas |
| 4 | `:257` | `can_do_for` | `FOR` | packed offsets + FOR value + width |
| — | `:270` | none of the above | `return false` | the encoder disqualifies itself |

The one comparison that decides 3 versus 4 is `:230-235`:

```
delta_required_bitwidth   = MinimumBitWidth(min_max_delta_diff)
regular_required_bitwidth = MinimumBitWidth(min_max_diff)
prefer_for = can_do_for && delta_required_bitwidth >= regular_required_bitwidth
```

Ties go to plain `FOR` — note the `>=`. That is the right default: `DELTA_FOR` stores an
extra `sizeof(T)` delta offset (`:249`) and its decode needs a prefix sum (`:664`, `:824`,
`:899`), so at equal width it is strictly worse.

And the detail worth the whole step: **analyze and compress are the same function.**
`BitpackingFinalAnalyze` at `:337-344` calls `Flush<EmptyBitpackingWriter>()` and returns
`total_size`. `EmptyBitpackingWriter` at `:47-63` is a struct whose `WriteConstant`,
`WriteConstantDelta`, `WriteDeltaFor` and `WriteFor` all have empty bodies. So the estimate is
not a model of the encoder — it is the encoder, with the stores compiled out. The score cannot
drift from reality, because there is only one code path.

Each group's chosen mode is packed into a single 32-bit word with its offset — `EncodeMeta`
at `:34-39` puts the mode in the high 8 bits, the offset in the low 24 (`0x00FFFFFF`), and
`DecodeMeta` at `:40-45` pulls them back out. Four bytes of metadata per 2,048 values.

**Why it matters:** this is the score-then-commit discipline of Step 3 recursed one level
down, at 2,048-value granularity, and implemented so that the two passes cannot disagree.
It is the pattern to copy the next time you write an estimator.

### Step 8 — The string stack: dictionary retired, FSST, both, then give up

> **In:** a `VARCHAR` column.
> **Out:** the cascade of string encodings, and one fact the docs will not tell you.

**Dictionary encoding** stores each distinct string once in a dictionary and replaces the
column with integer ids into it. The segment layout is drawn in a comment at
`dictionary_compression.cpp:14-44`: a header, a **selection buffer** (`uint16_t` per tuple →
index-buffer slot), an **index buffer** (`uint16_t` per distinct string → offset into the
dictionary), and the dictionary itself, "the string data without lengths". Its score,
`:85-98`, bit-packs the ids at `MinimumBitWidth(unique_count + 1)` — mind the `+1`, which
means 255 distinct values need **9** bits, not 8 — and applies the 1.2× bias of Step 4.

Now the fact worth checking the source for. `DictionaryCompressionStorage::StringInitAnalyze`
at `:70-78` opens with:

```cpp
// duckdb/duckdb@6c0c1a68 src/storage/compression/dictionary_compression.cpp:70-77
   70  unique_ptr<AnalyzeState> DictionaryCompressionStorage::StringInitAnalyze(ColumnData &col_data, PhysicalType type) {
   71  	auto &storage_manager = col_data.GetStorageManager();
   72  	if (StorageManager::TargetAtLeastVersion(StorageVersion::V1_3_0, storage_manager.GetStorageVersion())) {
   73  		// dict_fsst introduced - disable dictionary
   74  		return nullptr;
   75  	}
   76
   77  	return make_uniq<DictionaryAnalyzeState>(col_data.GetBlockManager());
```

Plain `DICTIONARY` is **retired** for storage version 1.3.0 and later. It returns a null
analyze state, which the loop at `column_data_checkpointer.cpp:237-239` skips. It is still in
the menu, and still the encoder to read for the mechanism — but on a database you create
today it never wins, because `DICT_FSST` (`compression_config.cpp:33-34`) supersedes it: a
dictionary whose entries are themselves FSST-compressed, in `src/storage/compression/dict_fsst/`.

**FSST** — a fast static symbol table mapping up to 255 substrings of 1–8 bytes to one-byte
codes — catches the case dictionary encoding cannot: strings that are *distinct but similar*,
like URLs and email addresses, where there is nothing to deduplicate but plenty to
substitute. Its DuckDB integration is in `fsst.cpp`; the paper and the algorithm get their own
chapter, [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md). Three numbers to carry over:
`MINIMUM_COMPRESSION_RATIO = 1.2` at `:37`, `ANALYSIS_SAMPLE_SIZE = 0.25` at `:38`, and
`duckdb_fsst_decompress` at `:470`.

**Zstd** (`src/storage/compression/zstd.cpp`, registered at `compression_config.cpp:29`) is
the heavyweight fallback for whatever nothing else catches — accepted because sometimes ratio
genuinely beats access, and constrained by the `fetch_row` cost of Step 5.

**Why it matters:** the string stack is where the ratio-versus-access trade is sharpest,
because strings are where the ratios are largest. It is also a live part of the tree —
`DICT_FSST` replacing `DICTIONARY` is a change you can date from the source, and the kind of
thing a guide written from documentation would miss.

### Step 9 — Zone maps: five answers, not three

> **In:** a filter and a segment about to be scanned.
> **Out:** the three-valued-logic result the storage layer returns, and the two extra cases
> NULLs force.

A **zone map** (equivalently a **min-max index**) is a per-segment summary of the values it
holds; before scanning, the engine checks the filter against it and may skip the segment
entirely — no read, no decode:

```
 WHERE ts BETWEEN '2026-01-01' AND '2026-01-02'
 seg 0 [ts: 2025-11-01 .. 2025-12-04]  -> skip (no read, no decode)
 seg 1 [ts: 2025-12-04 .. 2026-01-05]  -> scan
 seg 2 [ts: 2026-01-05 .. 2026-02-11]  -> skip
```

`ColumnData::CheckZonemap` at `src/storage/table/column_data.cpp:423-462` does it, delegating
to `expr_filter.CheckStatistics(...)` at `:442-443`. The return type is
`FilterPropagateResult`, and it has **five** values, not three
(`src/include/duckdb/common/enums/filter_propagate_result.hpp:15-21`):

| Value | Meaning | What the scan does |
| --- | --- | --- |
| `NO_PRUNING_POSSIBLE` | the range straddles the predicate | scan, and evaluate the filter per row |
| `FILTER_ALWAYS_TRUE` | every value in the segment passes | scan, and **drop the filter** — do not test rows |
| `FILTER_ALWAYS_FALSE` | no value can pass | skip the segment |
| `FILTER_TRUE_OR_NULL` | passes except that NULLs are unresolved | scan; only the validity mask needs testing |
| `FILTER_FALSE_OR_NULL` | fails except for NULLs | as above, inverted |

The last two exist because SQL comparison is three-valued: a min/max range says nothing about
NULLs, so a filter that is decided for every *value* may still be undecided for every *row*.
`FILTER_ALWAYS_TRUE` is the underrated one — it removes per-row filter evaluation entirely,
which is pure win on a highly selective-in-reverse predicate where skipping is impossible.

Three implementation details that change how you reason about this:

- **`state.segment_checked` (`:424`, set at `:433`).** The zone map is consulted once per
  segment per scan — except for **dynamic filters**, detected at `:431-432`, which are
  re-checked every time "as it can always change". Those are the filters a join build side
  tightens mid-query; a static `segment_checked` would freeze them at their loosest.
- **Updates invalidate pruning (`:448-461`).** If the column has an update segment, the
  filter is evaluated against the update statistics too, and unless both agree the result is
  downgraded to `NO_PRUNING_POSSIBLE`. Pruning is only sound over data the statistics
  actually cover.
- **String zone maps are prefixes.** `src/include/duckdb/storage/statistics/string_stats.hpp:34`
  sets `CURRENT_MAX_STRING_MINMAX_SIZE = 12` (the legacy format used 8, `:35`), and
  `string_stats.cpp:391-396` marks anything longer `TRUNCATED_STATS`. So a min/max on a URL
  column compares the first 12 bytes — `https://www.` for most of the web, which prunes
  nothing.

The catch that governs all of it: zone maps only prune when the data is **clustered on the
filter column**. On randomly ordered data every zone spans the whole domain and nothing
skips — the same clustering premise C-Store built projections for and ClickHouse enforces
with a mandatory `ORDER BY`.

**Why it matters:** this is where topic 10's filter pushdown physically lands. A plan-level
rewrite becomes a storage-level skip — or does not, depending on a property of the data no
optimizer controls.

### Step 10 — Do the sizes yourself, and say which GB/s

> **In:** one concrete column — 1,000,000 `INT64` values, 200 distinct, average run 8, value
> range 900 wide.
> **Out:** each encoder's score, with the multiplication shown, and an unambiguous way to
> report the scan rate.

Baseline: `1,000,000 × 8 B` = **8,000,000 B**. Average run 8 ⇒ `1,000,000 / 8` =
**125,000 runs**.

**RLE**, scored by `rle.cpp:113-116` as `(sizeof(rle_count_t) + sizeof(T)) × seen_count`.
`rle_count_t` is 4 bytes, `T` is 8:

```
125,000 x (4 + 8) = 1,500,000 B          8,000,000 / 1,500,000 = 5.33x
```

**Bit-packing**, `FOR` mode. The width is `ceil(log2(900))` = **10 bits**, because
2⁹ = 512 < 900 ≤ 1024 = 2¹⁰. Groups: `1,000,000 / 2,048` = 488.28 ⇒ **489 groups**. Per group,
`bitpacking.cpp:263-265` charges the packed payload plus `sizeof(T)` for the frame of
reference plus an aligned width field:

```
payload/group:  2,048 x 10 bits = 20,480 bits = 2,560 B
overhead/group: 8 B frame of reference + 8 B aligned width field = 16 B
per group:      2,576 B
total:          489 x 2,576 = 1,259,664 B      8,000,000 / 1,259,664 = 6.35x
```

**Dictionary**, scored by `dictionary_compression.cpp:85-98`. Width is
`MinimumBitWidth(unique_count + 1)` = `MinimumBitWidth(201)` = **8 bits**:

```
selection buffer: 1,000,000 x 8 bits = 1,000,000 B
dictionary:             200 x 8 B    =     1,600 B
                                       -----------
true:                                    1,001,600 B    ratio 7.99x
reported (x1.2):                         1,201,920 B    the score the loop compares
```

So on this column the ranking the checkpointer sees is dictionary 1,201,920 < bit-packing
1,259,664 < RLE 1,500,000 < uncompressed 8,000,000 — dictionary wins, by 4.8%. Lengthen the
average run to 32 and RLE's score falls to `31,250 × 12` = 375,000 B and it wins by 3.2×. The
same column, a different sort order, a different encoder: that is the whole argument for
deciding per row group.

**Now the bandwidth, and the ambiguity.** `FINDINGS.md` row 12 records this topic's measured
scan floor: **24–57 GB/s on a machine with roughly 150 GB/s of memory bandwidth**. Take the
dictionary-encoded column above, at 7.99×, and describe one scan two ways:

```
physical bytes read:      1,001,600 B      logical values produced: 8,000,000 B
at 24 GB/s logical   =>   3.0 GB/s of compressed bytes actually moved
at  3.0 GB/s physical =>   24 GB/s of "effective" throughput
```

Both sentences are true of the same query. A report that does not say which one it means is
not a measurement. And `FINDINGS.md` row 12 keeps this topic's cautionary case, where the
number is not merely ambiguous but impossible: a hoisted timing loop once printed
**19,047,619 GB/s**, which is `19,047,619 / 150` ≈ **127,000×** the machine's peak memory
bandwidth. Sanity-check every throughput against the hardware ceiling before you write it
down.

**Why it matters:** you can now predict which encoder wins for a column before running
`PRAGMA storage_info`, and check the engine against your own arithmetic. That is the point of
the exercise lanes in `experiments/`.

---

## Where each step lives in the code

Read the framework header first — the lifecycle contract is documented in it — then the
selection loop, then the encoders, then zone maps.

| File (`duckdb/duckdb@6c0c1a68`) | Role (steps) |
| --- | --- |
| `src/function/compression_config.cpp` | the menu and its order (1, 3) |
| `src/include/duckdb/storage/storage_info.hpp`, `src/include/duckdb/common/vector_size.hpp` | the units (2) |
| `src/include/duckdb/function/compression_function.hpp` | the lifecycle contract (3, 5) |
| `src/storage/table/column_data_checkpointer.cpp` | the selection loop (3, 4) |
| `src/storage/compression/rle.cpp` | the simplest complete encoder (6) |
| `src/storage/compression/bitpacking.cpp` | four modes in one `Flush` (7) |
| `src/storage/compression/dictionary_compression.cpp`, `fsst.cpp`, `dict_fsst/`, `zstd.cpp` | the string stack (8) |
| `src/storage/table/column_data.cpp`, `src/storage/statistics/string_stats.cpp` | zone maps (9) |

- **Step 1** — `compression_config.cpp:17-35`, the `internal_compression_methods` array. The
  order is the tie-break order.
- **Step 2** — `storage_info.hpp:26` (`DEFAULT_ROW_GROUP_SIZE 122880ULL`), `:394` (the
  divisibility assert); `vector_size.hpp:16` (`2048U`); `bitpacking.cpp:25`
  (`BITPACKING_METADATA_GROUP_SIZE`).
- **Step 3** — `compression_function.hpp:130-138` documents the lifecycle; `:139`
  `init_analyze`, `:140` `analyze`, `:141` `final_analyze`, `:148` `compress_data`. The loop
  is `column_data_checkpointer.cpp:172-278`: the shared scan at `:200-217`, drop-out at
  `:211-214`, `skip_scan` at `:196`, `PRAGMA force_compression` at `:185-188`, the score
  comparison at `:245-256`, the "no suitable method" throw at `:265-268`.
- **Step 4** — `dictionary/common.hpp:20` and `fsst.cpp:37` (`MINIMUM_COMPRESSION_RATIO
  = 1.2F`); `dictionary_compression.cpp:97` and `fsst.cpp:202` apply it. Sampling:
  `fsst.cpp:38` (`ANALYSIS_SAMPLE_SIZE = 0.25`), `alp_constants.hpp:19-23` (8 vectors × 32
  values, jumping 7 vectors).
- **Step 5** — `compression_function.hpp:171-173` `fetch_row`; `:174-176` `skip`, whose doc
  states the random-access constraint; `:164-166` `select`; `:167-170` `filter`.
- **Step 6** — `rle.cpp`: `RLEAnalyzeState :86-91`, `RLEAnalyze :99-110`, `RLEFinalAnalyze
  :113-116` (the score), `RLECompressState :126`, scan-state pointers `:313-314`,
  `CanEmitConstantVector :333-347`, `RLEScanConstant :349-359`, `RLEFilter :447-490` (the
  per-run predicate and the whole-segment early-out at `:477-481`), registration
  `GetRLEFunction :568-576`.
- **Step 7** — `bitpacking.hpp:15` the enum; `bitpacking.cpp` `EncodeMeta :34-39` /
  `DecodeMeta :40-45`, `EmptyBitpackingWriter :47-63`, `Flush :204-271` (CONSTANT `:209`,
  CONSTANT_DELTA `:219`, the width comparison `:230-235`, DELTA_FOR `:237`, FOR `:257`,
  give-up `:270`), `BitpackingAnalyze :318-334`, `BitpackingFinalAnalyze :337-344`,
  `ForceBitpackingModeSetting :312`.
- **Step 8** — `dictionary_compression.cpp:14-44` the layout, `:48-65` the storage struct,
  `:70-78` the V1.3.0 retirement, `:85-98` the score; `fsst.cpp:37,:38,:470`; `dict_fsst/`;
  `zstd.cpp`.
- **Step 9** — `column_data.cpp:423-462` `CheckZonemap` (`segment_checked` `:424`/`:433`,
  dynamic filters `:431-432`, the update downgrade `:448-461`);
  `filter_propagate_result.hpp:15-21` the five results; `string_stats.hpp:34-35` and
  `string_stats.cpp:391-396` for prefix truncation.

---

## Questions for notes.md

1. **The analyze pass doubles ingest cost.** What does BtrBlocks do instead, and what does
   it risk? Then complicate it: DuckDB already samples for two encoders (FSST at 25%, ALP at
   0.208% — Step 4). What distinguishes the encoders that sample from the ones that measure
   everything, and does that rule explain BtrBlocks' choice too?
2. **`fetch_row` on `DELTA_FOR`.** Decoding row 1907 of a 2,048-value group requires what,
   exactly? (Follow `bitpacking.cpp:824` and `:899`.) Why is the cost acceptable for OLAP —
   think about *when* `fetch_row` runs under late materialization, i.e. after the filter, on
   survivors only. Then say what would change if the same segment served an OLTP point-lookup
   workload.
3. **RLE score versus dictionary score on a column of 50% NULLs.** Which wins, and why does
   validity change the answer? Note that DuckDB stores validity as its own column with its own
   compression function (`COMPRESSION_EMPTY`, `compression_config.cpp:31-32`, plus
   `COMPRESSION_ROARING`), so the NULLs may not be in the data column's score at all.
4. **A zone map returning `FILTER_ALWAYS_TRUE` removes the filter.** When does that matter
   more than segment skipping? (Think selectivity near 100% — the cost being removed is
   per-row filter *evaluation*, not I/O.) Then account for `FILTER_TRUE_OR_NULL`: what does
   the scan still have to do, and what does it get to skip?
5. **M12.** Which of the four bit-packing modes fits the node-id payload columns in a graph
   adjacency structure, where ids are dense-ish and clustered by creation time? Work the
   width for a plausible id range with Step 10's arithmetic, and say which mode `Flush`'s
   cascade would actually reach and why — including whether `DELTA_FOR` or `FOR` wins the
   `:235` comparison for your numbers.

---

## Takeaway

DuckDB's answer to "who picks the encoding" is *the data does, measured, per 122,880 rows* —
and the implementation is more interesting than the slogan. The analyze pass is one shared
scan, not one per candidate. The scores are deliberately 20% pessimistic for the schemes that
cost something at decode time. Two encoders sample rather than measure, because training a
model is the expensive part. And bit-packing's estimator *is* its compressor with the writes
compiled out, so the two passes cannot disagree.

The transferable idea is the last one. Every system that estimates a cost and then does the
work has two implementations of the same logic and a bug waiting in the gap between them.
`Flush<EmptyBitpackingWriter>` closes that gap by construction, and it costs one template
parameter.

---

## Done when

Answer each before unfolding it.

- [ ] Recite the analyze → score → compress lifecycle, and name the four ways a candidate
      encoder can fail to be chosen.

<details><summary>Answer</summary>

`init_analyze` per candidate → `analyze` per vector, on every candidate, from **one** shared
scan → `final_analyze` returns an estimated byte count → the smallest score wins → the winner
runs `compress_data` over the same data again. Documented at
`compression_function.hpp:130-138`, implemented at `column_data_checkpointer.cpp:172-278`.

Four ways to lose:
1. **Drop out mid-scan** — `analyze` returns false; the state is nulled and the function
   removed for the remainder of the pass (`:211-214`).
2. **Self-disqualify** — `final_analyze` returns `DConstants::INVALID_INDEX` (`:247-250`), as
   `BitpackingFinalAnalyze` does at `bitpacking.cpp:341` when `Flush` fails.
3. **Lose on score** — `:252` compares with strict `<`, so ties go to the earlier entry in
   `compression_config.cpp:17-35`.
4. **Never be considered** — `:196`'s `skip_scan`, when the DDL or `PRAGMA force_compression`
   names a type outright.

And the safety net: `:265-268` throws `FatalException` if nothing qualifies, which is why
`UNCOMPRESSED` is permanently in the menu.

</details>

- [ ] `final_analyze` is supposed to return estimated bytes. Give two encoders where the
      number it returns is not the estimated bytes, and say why each is right to lie.

<details><summary>Answer</summary>

**Dictionary** (`dictionary_compression.cpp:97`) and **FSST** (`fsst.cpp:202`) both multiply
their honest estimate by `MINIMUM_COMPRESSION_RATIO = 1.2`
(`dictionary/common.hpp:20`, `fsst.cpp:37`) before returning it. They report themselves 20%
bigger than they are, so they must beat the alternatives by more than 20% to win.

Right, because a byte count is not the whole cost. Both add an indirection to every decoded
value, and both make `fetch_row` more expensive than a bit-packed segment's arithmetic. A
scheme scored purely on size would be picked at margins where it loses on time.

A different kind of not-really-bytes: **ALP** samples 256 of a row group's 122,880 values
(`alp_constants.hpp:19-23` — 8 vectors × 32 values, 0.208%) and **FSST** analyses 25%
(`fsst.cpp:38`). Their scores are extrapolations, not measurements. The rule is that encoders
whose *training* is expensive sample; encoders whose analyze pass is just counting (RLE,
bit-packing, dictionary) measure everything.

</details>

- [ ] `RLEFilter` is the 2006 paper running in production. Say what it does and what the
      complexity is.

<details><summary>Answer</summary>

`rle.cpp:447-490`. On the first filtered scan of a segment it evaluates the predicate over
the **run values array**, once — its own comments say "apply the filter to all RLE values at
once" (`:456-457`) and "execute the filter over all runs at once" (`:463`) — records the
result in a `matching_runs` bool array cached on the scan state (`:310`, `:460-475`), and
then, at `:477-481`, returns `sel_count = 0` immediately if no run matched, abandoning the
whole segment without decoding a row.

Complexity: `O(runs)`, not `O(rows)`. On this topic's shared column — 1,000,000 values,
125,000 runs — the predicate is evaluated 125,000 times instead of 1,000,000, an 8× cut that
grows linearly with average run length.

That is exactly Abadi §6.2's accounting, which puts the aggregation cost at
`num_tuples / avg_run_len` for RLE against `num_tuples` uncompressed. Its sibling is
`CanEmitConstantVector` at `:333-347`: when a run spans a full 2,048-value vector,
`RLEScanConstant` (`:349-359`) emits a `CONSTANT_VECTOR` holding one value — Abadi's
`isOneValue()` block property, by another name.

</details>

- [ ] Bit-packing has four modes but only one arithmetic comparison decides between the two
      interesting ones. Give it, and say which way ties go and why.

<details><summary>Answer</summary>

`bitpacking.cpp:230-235`:

```
delta_required_bitwidth   = MinimumBitWidth(min_max_delta_diff)
regular_required_bitwidth = MinimumBitWidth(min_max_diff)
prefer_for = can_do_for && delta_required_bitwidth >= regular_required_bitwidth
```

If `prefer_for` is false, `Flush` takes the `DELTA_FOR` branch at `:237`; otherwise it falls
through to `FOR` at `:257`. `CONSTANT` (`:209`) and `CONSTANT_DELTA` (`:219`) are cheap
special cases tested before either.

Ties go to plain **`FOR`**, because of the `>=`. That is correct: `DELTA_FOR` stores an extra
`sizeof(T)` delta offset (`:249`) and its decode needs a prefix sum over the group
(`:664`, `:824`, `:899`), so at equal bit width it is strictly more expensive in both space
and time.

The structural point is that `Flush` is a **priority cascade**, not a race — it takes the
first mode that applies rather than scoring all four. And `BitpackingFinalAnalyze:337-344`
runs this very function with `EmptyBitpackingWriter` (`:47-63`, all method bodies empty), so
the analyze estimate and the compressor are literally the same code.

</details>

- [ ] `CheckZonemap` returns five results, not three. Name the two extra ones and say what
      forces them to exist.

<details><summary>Answer</summary>

`filter_propagate_result.hpp:15-21`: `NO_PRUNING_POSSIBLE`, `FILTER_ALWAYS_TRUE`,
`FILTER_ALWAYS_FALSE`, and the two extras — **`FILTER_TRUE_OR_NULL`** and
**`FILTER_FALSE_OR_NULL`**.

They exist because SQL comparison is three-valued and a min/max range says nothing about
NULLs. A zone map can prove that every non-NULL value in a segment satisfies the predicate,
but a row whose value is NULL still evaluates to unknown, so the segment cannot be blanket
accepted; the scan still has to consult the validity mask, but it can skip evaluating the
predicate itself. Without these two states that whole case collapses to
`NO_PRUNING_POSSIBLE` and the saving is lost.

Two related things `column_data.cpp:423-462` reveals: the check is memoised per segment via
`state.segment_checked` (`:424`, `:433`) **except** for dynamic filters (`:431-432`), which
can tighten mid-query and are re-checked; and if the column has updates, the result is
downgraded to `NO_PRUNING_POSSIBLE` unless the update statistics agree (`:448-461`). And
string zone maps are only the first 12 bytes (`string_stats.hpp:34`, truncation marked at
`string_stats.cpp:391-396`), so a URL column's min/max usually compares `https://www.` and
prunes nothing.

</details>

---

## References

**Code** — all anchors at `duckdb/duckdb@6c0c1a68`; verify with
`tools/pinned-source.py show duckdb/duckdb <path> -r A:B`.

- [duckdb/duckdb](https://github.com/duckdb/duckdb). Read in this order:
  `src/include/duckdb/function/compression_function.hpp` (the lifecycle contract is in the
  header comments), `src/storage/table/column_data_checkpointer.cpp` (the selection loop),
  then `src/storage/compression/rle.cpp` and `bitpacking.cpp`, then
  `dictionary_compression.cpp` / `fsst.cpp` / `dict_fsst/` / `zstd.cpp`, then
  `src/storage/table/column_data.cpp` for zone maps. `src/function/compression_config.cpp` is
  the index to all of it.
- `PRAGMA storage_info('<table>')` reports the encoding actually chosen per segment;
  `PRAGMA force_compression` overrides the race. Both are how you turn this chapter into an
  experiment.

**Papers**

- Abadi, Madden, Ferreira. *Integrating Compression and Execution in Column-Oriented Database
  Systems*. SIGMOD 2006 — §5.2's block-properties API is what Step 6's `RLEFilter` and
  `CanEmitConstantVector` implement. Covered in
  [reading-cstore-compression.md](reading-cstore-compression.md).
- Kuschewski, Sauerwein, Alhomssi, Leis. *BtrBlocks: Efficient Columnar Compression for Data
  Lakes*. SIGMOD 2023 — §3.1's sampler, for question 1. Covered in
  [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md).

**In this topic**

- [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md) — FSST itself, and the sampling
  alternative
- [reading-clickhouse-mergetree.md](reading-clickhouse-mergetree.md) — the third answer to
  "who picks the encoding": the user, in the DDL
- `FINDINGS.md` row 12 — the measured scan floor (24–57 GB/s on a ~150 GB/s machine) and the
  19,047,619 GB/s hoisted-loop bug
