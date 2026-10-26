# MergeTree: brute force, organized

ClickHouse's storage engine is topic 4's LSM shapes at analytics scale: immutable sorted
parts, background merges, and — because the workload is scans, not point reads — an index
that is deliberately **sparse**.

`src/Storages/MergeTree/` is enormous. Read only what is anchored here. Every anchor below
was checked against **ClickHouse/ClickHouse@c6428eb** (the `v26.10.2.103-stable` release);
confirm the revision before you start:

```
python3 tools/pinned-source.py ref clickhouse
```

Two words used constantly below. A **zone map** (or **min-max index**) is a small summary —
usually just the minimum and maximum value — attached to a chunk of rows, letting a reader
skip the chunk when a predicate cannot be satisfied inside those bounds. **Write
amplification** is the total bytes written to storage divided by the bytes of user data
inserted; **read amplification** is the analogous ratio on the read side.

---

## The problem in one sentence

Serve `GROUP BY` scans over billions of rows arriving at millions of inserts per second —
a B-tree that pays a page write per row cannot ingest that, and a per-row index would rival
the data in size, so ClickHouse indexes only every 8192nd row and makes scanning the rest
cheap enough not to care.

Put a number on the second clause. A dense index over 10 billion `UInt64` keys costs
`10,000,000,000 × 8 B` = **80 GB** before any tree overhead — it cannot stay in memory. One
entry per 8192 rows costs `10,000,000,000 / 8192` = 1,220,703 entries ×  8 B = **9.8 MB**,
which is smaller than a CPU's last-level cache on a large server. That factor of 8192 is
the whole design, and everything in this guide is a consequence of it.

---

## The concepts, step by step

### Step 1 — The part: an insert writes new files, never modifies old ones

> **In:** a batch of rows arriving in an `INSERT`.
> **Out:** a new immutable directory on disk, and a table that is now the set of its parts.

A **part** is one self-contained directory of files holding a batch of rows, sorted by the
table's declared sort key (`ORDER BY`). Every `INSERT` creates a brand-new part; existing
parts are never modified — they are **immutable**. A table *is* the set of its currently
active parts, and background **merges** combine small parts into bigger ones, then delete
the inputs.

```
 table = set of immutable sorted PARTS (each sorted by the ORDER BY key)
 INSERT ──▶ writes a NEW part          (no in-place anything)
 background MERGE ──▶ reads N parts, writes 1, drops the N
```

This is an LSM design (topic 4: absorb writes into new sorted files, merge in the
background) with the vocabulary shifted: memtable ≈ the insert block, SSTable ≈ part,
compaction ≈ merge. What it drops relative to a key-value LSM is just as important: no
write-ahead log per row and no point-read path worth optimising.

**Why it matters:** ingest becomes pure sequential file writes at disk bandwidth, and every
read-side structure may assume its input is sorted and will never change under it. Steps
3–6 all cash in that assumption.

### Step 2 — Inside a part: Wide or Compact, and why the answer is not always "one file per column"

> **In:** one part directory.
> **Out:** the file layout inside it, and the size threshold that switches between two
> layouts.

The textbook answer is "one file per column" — that is the **column store** arrangement,
where each column's values are contiguous, as opposed to a **row store** where a row's
fields are contiguous. ClickHouse does that, but only above a size threshold. There are two
part formats:

| Format | Layout | When |
| --- | --- | --- |
| **Wide** | one `.bin` per column, one marks file per column | part ≥ `min_bytes_for_wide_part` |
| **Compact** | *all* columns in a single `data.bin`, all marks in `data.mrk3` | part below that threshold |

`MergeTreeDataPartCompact.h:8-14` states it directly: "In compact format all columns are
stored in one file (`data.bin`). Data is split in granules and columns are serialized
sequentially in one granule… It's considered to store only small parts in compact format
(up to 10M)." The threshold is `default_min_bytes_for_wide_part = 10485760` — **10 MiB** —
at `MergeTreeSettings.cpp:37`, wired to the `min_bytes_for_wide_part` setting at `:80`. (A
ClickHouse Cloud build swaps in 1 GiB at `:39`; the open-source build is the 10 MiB one.)

The reason is file-count economics, not query performance: a thousand small inserts into a
200-column table would otherwise create 200,000 tiny files. Compact parts are transient —
merges promote them to Wide once they exceed 10 MiB.

Even inside a Compact part, ClickHouse tries to keep columnar skipping alive:
`compress_per_column_in_compact_parts` (default `true`,
`MergeTreeSettings.cpp:1162-1168`) starts a new compressed block for each column within a
granule so a reader can still skip columns it does not need — at the cost of compression
ratio, exactly as the setting's own doc says.

The `ORDER BY` key you declare at table creation fixes the physical sort — and therefore
the clustering — of every part forever. That is the price of admission ClickHouse states
upfront: you must know your main filter column at schema time. (DuckDB's zone maps have
the same clustering dependency; it is simply never declared.)

### Step 3 — The granule: at most 8192 rows *and* at most 10 MiB

> **In:** a sorted part.
> **Out:** the read quantum, and the two independent caps that define it.

A **granule** is the read quantum: the engine never reads or indexes anything smaller. A
**mark** is the per-granule, per-column bookmark saying where that granule's bytes begin.

The number everyone quotes is 8192, and it is real —
`MergeTreeSettings.cpp:74` declares `index_granularity` with default `8192`. But read the
doc string on the next line: "**Maximum** number of data rows between the marks of an
index." There is a second, independent cap: `index_granularity_bytes`, default
`10 * 1024 * 1024` = **10 MiB** (`MergeTreeSettings.cpp:2163`), described as "Maximum size
of data granules in bytes", with a floor of `min_index_granularity_bytes = 1024`
(`:2168`). Mixed (adaptive) granularity is on by default —
`enable_mixed_granularity_parts` at `:2223`, whose doc explains it "improves ClickHouse
performance when selecting data from tables with big rows (tens and hundreds of
megabytes)".

So a granule holds `min(8192 rows, whatever fits in 10 MiB)` — the doc of the neighbouring
`use_const_adaptive_granularity` setting spells the rule out at `:2175-2178`: "rows per
granule is `index_granularity_bytes` divided by the average size of a row in that block,
capped at `index_granularity`". Work it:

| Average row size | Rows in 10 MiB | Granule size |
| --- | --- | --- |
| 100 B | 104,857 | **8192 rows** (the row cap binds) |
| 1 KiB | 10,240 | **8192 rows** (the row cap binds, barely) |
| 4 KiB | 2,560 | **2560 rows** (the byte cap binds) |
| 1 MiB | 10 | **10 rows** |

The bet behind 8192: decompressing and scanning that many rows with vectorized code costs
microseconds, so tracking anything finer buys nothing. The byte cap exists because that bet
is about *bytes touched*, not rows, and a table of 1 MiB blobs would otherwise make a
"granule" mean 8 GB.

**Why it matters:** every read-side structure now scales with `rows / 8192` rather than
with `rows` — three orders of magnitude smaller. Step 4 spends that budget.

### Step 4 — The sparse primary index: which granules, not which row

> **In:** the sorted key column and the granule boundaries from Step 3.
> **Out:** an in-memory array of one key per granule, small enough to keep resident
> forever.

The **sparse primary index** (`primary.idx`) stores the sort key of the **first row of each
granule** — one entry per 8192 rows. It is loaded into memory and kept there;
`IMergeTreeDataPart.h:549` is `getIndex()`, and `:550` is `loadIndexToCache()`, which
places it in a dedicated `PrimaryIndexCache`.

```
 rows:      0 .... 8191 | 8192 .. 16383 | 16384 .. 24575 | ...
 granule:        0      |       1       |       2        |
 primary.idx:  key[0]   |   key[8192]   |   key[16384]   |   ← one entry per granule
```

The arithmetic from the problem statement, now in context: 10 billion rows, `UInt64` key,
8192-row granules → 1,220,703 entries × 8 B = **9.8 MB**, against **80 GB** for a dense
index. A compound key of three `UInt64` columns triples it to 29.3 MB — still resident.

This is what "sparse" costs and buys. A B-tree answers *which row*. This answers *which
8192 rows*, and you always over-read up to a full granule. For a scan workload that
over-read is noise; for OLTP it would be fatal, which is the honest reason ClickHouse is
not an OLTP engine.

### Step 5 — From predicate to mark ranges: two different search algorithms

> **In:** the in-memory index from Step 4 and a `WHERE` clause.
> **Out:** a list of `MarkRange`s to read — computed by one of two algorithms, depending on
> the predicate's *shape*.

This is the read path's payload, and it is where the popular summary of MergeTree is
wrong. "The index is binary-searched" is true only for one class of predicate.
`MergeTreeDataSelectExecutor::markRangesFromPKRange`
(`MergeTreeDataSelectExecutor.cpp:2244`, reached from the call sites at `:208` and `:1216`)
asks `key_condition.matchesExactContinuousRange()` at `:2703` and branches on the answer at
`:2708`. (The same question is put to two optional offset conditions on `_part_offset` and
the query-wide row offset at `:2704-2705`; with no such filter both count as exact, so for an
ordinary `WHERE` on the sort key the key condition alone decides.)

**Case A — the predicate is one continuous key interval** (`user_id = 42`,
`user_id BETWEEN 10 AND 20` on `ORDER BY (user_id, …)`). Binary search for the left and
right endpoints. The code says so at `:2785-2787`: "In case when SELECT's predicate defines
a single continuous interval of keys, we can use binary search algorithm to find the left
and right endpoint key marks of such interval. The returned value is the minimum range of
marks, containing all keys for which KeyCondition holds". It is tagged `SearchAlgorithm::BinarySearch` at `:2789`.

**Case B — anything else** (`user_id % 2 = 0`; or a predicate on the *second* key column
only). Binary search is meaningless because the qualifying granules are not contiguous, so
ClickHouse runs a **generic exclusion search** (`:2708-2782`): recursively split each mark
range into `merge_tree_coarse_index_granularity` subranges — default **8**,
`src/Core/Settings.cpp:1935` — and discard any subrange in which the condition provably
cannot be true.

```
// ILLUSTRATION — not quoted from ClickHouse/ClickHouse; this is the shape of the two
// branches in MergeTreeDataSelectExecutor.cpp:2703-2936 rendered in Rust. The real
// binary search starts at :2795 (left endpoint loop at :2808); the exclusion search is
// delegated to genericExclusionSearch() at :2733 / :2763
// (src/Storages/MergeTree/GenericExclusionSearch.h).
fn mark_ranges(idx: &[Key], cond: &KeyCondition) -> Vec<MarkRange> {
    if cond.matches_exact_continuous_range() {          // :2703, branch at :2708
        // one interval: find its two endpoints, return the minimal covering range
        let lo = lower_bound(idx, cond.left());
        let hi = upper_bound(idx, cond.right());
        vec![MarkRange { begin: lo, end: hi }]          // :2789 BinarySearch
    } else {
        // no interval to bracket: split coarsely and drop what cannot match
        generic_exclusion_search(idx, cond, /* coarse = */ 8)  // :2763
    }
}
```

Two details worth carrying away, because they are the difference between the idea and a
production implementation:

- The exclusion search has a **step budget**,
  `merge_tree_generic_exclusion_search_max_steps` (`Settings.cpp:1942`, default `0` =
  unlimited). Its doc at `:1945` is unusually candid about the trade: "When it is
  exhausted, the ranges that were not fully analyzed are **accepted as a whole**, so the
  query stays correct but may read more granules than an unlimited search would select."
  Index analysis is itself a cost centre that can be capped, and the failure mode is
  reading too much, never reading too little.
- Surviving ranges that are close together get **fused**, because a seek costs more than
  reading through the gap. `min_marks_for_seek` is computed at `:2715` from
  `merge_tree_min_rows_for_seek` / `merge_tree_min_bytes_for_seek`
  (`Settings.cpp:1921`, `:1928`, both default `0`): "If the distance between two data
  blocks to be read in one file is less than … then ClickHouse does not seek through the
  file but reads the data sequentially."

### Step 6 — Marks: two offsets, because compression blocks ≠ granules

> **In:** a `MarkRange` from Step 5 and a column file.
> **Out:** a byte position to seek to, and the reason it takes two numbers rather than one.

Column files are a sequence of independently compressed blocks. Block boundaries are chosen
by **size**, granule boundaries by **rows**, and the two grids do not align — so a mark
carries two coordinates:

```c
// ClickHouse/ClickHouse@c6428eb — src/Formats/MarkInCompressedFile.h:14-21
    14  /** Mark is the position in the compressed file. The compressed file consists of adjacent compressed blocks.
    15    * Mark is a tuple - the offset in the file to the start of the compressed block, the offset in the decompressed block to the start of the data.
    16    */
    17  struct MarkInCompressedFile
    18  {
    19      size_t offset_in_compressed_file;
    20      size_t offset_in_decompressed_block;
    21  
```

Read: seek to `offset_in_compressed_file`, decompress that block, then skip
`offset_in_decompressed_block` bytes to reach the granule's first row.

The block sizes come from `src/Core/Settings.cpp`: `min_compress_block_size` = **65,536**
(`:146`) and `max_compress_block_size` = **1,048,576** (1 MiB, `:161`). The `:146` doc
contains the worked example that makes the two-offset design obvious, and it is worth
quoting because it *is* the arithmetic:

> "We are writing a UInt32-type column (4 bytes per value). When writing 8192 rows, the
> total will be 32 KB of data. Since `min_compress_block_size` = 65,536, a compressed block
> will be formed for **every two marks**."
>
> "We are writing a URL column with the String type (average size of 60 bytes per value).
> When writing 8192 rows, the average will be slightly less than 500 KB of data. Since this
> is more than 65,536, a compressed block will be formed **for each mark**."

So on the `UInt32` column, every odd-numbered granule starts 32,768 bytes into its block —
`offset_in_decompressed_block = 32768` — and one offset could not have found it. On the URL
column, every mark's second offset is 0. That is the whole story, and it is also the
concrete cost of layering block compression under a row-addressed index. Parquet grows the
same two-level addressing for the same reason (see `reading-arrow-parquet.md`).

Then there is a detail that rewards reading the header to the end. The in-memory array of
marks is **itself compressed with the schemes this topic is about**:

```c
// ClickHouse/ClickHouse@c6428eb — src/Formats/MarkInCompressedFile.h:51-63
    51      /** We need to store a sequence of marks, each consisting of two 64-bit integers:
    52       * offset_in_compressed_file and offset_in_decompressed_block. We'll call them x and y for
    53       * convenience, since compression doesn't care what they mean. The compression exploits the
    54       * following regularities:
    55       *  * y is usually zero.
    56       *  * x usually increases steadily.
    57       *  * Differences between x values in nearby marks usually fit in much fewer than 64 bits.
    58       *
    59       * We split the sequence of marks into blocks, each containing MARKS_PER_BLOCK marks.
    60       * (Not to be confused with data blocks.)
    61       * For each mark, we store the difference [value] - [min value in the block], for each of the
    62       * two values in the mark. Each block specifies the number of bits to use for these differences
    63       * for all marks in this block.
```

Subtract a per-block minimum, then bit-pack the residuals at a per-block width: that is
**frame-of-reference plus bit-packing**, the same pair of schemes Parquet and DuckDB apply
to user data, applied here to the index. The measured payoff is in the class comment at
`:38-41`: "~3 bytes/mark for integer columns, ~5 bytes/mark for string columns, ~0.3
bytes/mark for trivial marks in auxiliary dict files of LowCardinality columns" — against
16 bytes for the naive two-`size_t` struct, a **5.3×** reduction on integer columns. For
the 10-billion-row table: 1,220,703 marks × 3 columns × 3 B = **11 MB** instead of 58.6 MB.

### Step 7 — Merges: the metabolic cycle, and the work done during it

> **In:** a growing pile of parts.
> **Out:** fewer, larger parts — plus, optionally, computed results.

Background merges take several parts and merge-sort them into one bigger part. This is
topic 4's compaction dial at part granularity, steering between two failure modes: merge
too eagerly and you rewrite the same rows repeatedly (write amplification); too lazily and
every scan must visit too many parts (read amplification).

`MergeTreeDataMergerMutator::selectPartsToMerge`
(`MergeTreeDataMergerMutator.cpp:328`) makes the choice. The knobs and their defaults, all
in `MergeTreeSettings.cpp`, turn the abstract dial into numbers:

| Setting | Default | Line | What it bounds |
| --- | --- | --- | --- |
| `merge_selector_base` | 5.0 | `:1090` | "Affects write amplification of assigned merges" |
| `max_parts_to_merge_at_once` | 100 | `:777` | fan-in of a single merge |
| `max_bytes_to_merge_at_max_space_in_pool` | 150 GiB | `:541` | largest merge ever attempted |
| `parts_to_delay_insert` | 1000 | `:1171` | inserts get an artificial sleep past this |
| `parts_to_throw_insert` | 3000 | `:1193` | inserts are **rejected** past this |

The last two are the "too lazy" failure mode made concrete, and they are back-pressure, not
tuning: `:1178-1179` says ClickHouse "artificially executes `INSERT` longer (adds 'sleep') so
that the background merge process can merge parts faster than they are added". If merges
cannot keep up, ingest is throttled and then refused — the system chooses to stop accepting
writes rather than let read amplification grow without bound.

Merges are also prioritised, and the comment at `MergeTask.h:78-82` explains the policy in
plain words: "A priority is simple - the lower the size of the merge, the higher priority.
So, if ClickHouse wants to merge some really big parts into a bigger part, then it will be
executed for a long time, because the result of the merge is not really needed immediately.
It is better to merge small parts as soon as possible." The `MergeTask` class itself is at
`MergeTask.h:84`; it is a resumable state machine so a huge merge can be suspended between
blocks.

The distinctly ClickHouse move: since a merge already streams every row through memory,
**do other work while you are there**. Specialized engines run computation inside the
merge — `ReplacingMergeTree` deduplicates rows, `SummingMergeTree` and
`AggregatingMergeTree` pre-aggregate them. Compaction-as-computation, and the mechanism
behind incrementally maintained materialized views. That is the architecture triangle:
brute-force scan speed (ClickHouse) versus precomputation (Pinot/Druid star-trees) versus
embedded convenience (DuckDB) — and it is the trick FalkorDB could steal for graph
statistics.

### Step 8 — Codec chains: the user declares, the size picks the default, an opt-in analyzes

> **In:** a column, its `CODEC(...)` clause in the DDL (or the lack of one), and the size of
> the part being written.
> **Out:** a chain of transforms applied left to right, and a self-describing header that
> lets the reader undo them.

Each column carries a declared chain of codecs. `CODEC(Delta, LZ4)` means delta-encode
(**delta encoding**: store each value's difference from the previous one), then LZ4 the
result. Composition is literal — `CompressionCodecMultiple::compressBody`
(`src/Compression/CompressionCodecMultiple.cpp:132-180`) first writes a header of
`[codec count][method byte × N]` (`:138-140`), then runs the codecs in order, each stage's
output becoming the next stage's input (`:147-163`). Intermediate outputs alternate between
two scratch buffers (`buffers[idx % 2]`, `:157-158`); the **last** stage writes straight into
the caller's destination whenever its worst-case output fits there (`:151-152`), so the
common chain pays no final copy. Decompression, `doDecompressData` at `:182-261`, walks the
same list **backwards** — `for (int idx = compression_methods_size - 1; idx >= 0; --idx)` at
`:204` — and mirrors the trick: the first stage decodes the caller's block where it lies and
the last decodes into the caller's output, unless that codec needs padding past the end of
its buffer (`:197-200`, `:219-249`). `LZ4` (`CompressionCodecLZ4.cpp:27`) and `ZXC`
(`CompressionCodecZXC.h:30`) are the codecs that do: both reserve 64 bytes of slack, because
their decoders copy in wide chunks that may read and write past the end of the data
(`LZ4_decompress_faster.h:33-38`; for ZXC, "32-byte SIMD 'wild' loads", `:23-26`).

The header overhead is exactly `1 + codecs.size()` bytes per compressed block — the
`payload_offset` at `:142`, and the `sizeof(UInt8) + chain.size()` in the reserve
calculation at `:104`. On a 64 KiB block with a two-codec chain that is 3 bytes, or
**0.0046%** — the reason nobody thinks about it.

The menu (`src/Compression/`) includes the general-purpose codecs — LZ4, ZSTD, and ZXC
("Asymmetric LZ codec (zxc): slow compression, very fast decompression, ratio between LZ4
and ZSTD", `CompressionCodecZXC.h:44`) — plus a set of *preprocessors* that shrink nothing
by themselves: `Delta`, `DoubleDelta` (deltas of deltas — near-zero for regular
timestamps), `Gorilla` (XOR consecutive floats — sensor values barely change), `FPC`,
`GCD`, `T64`, and `ALP` (topic 30 material).

"Preprocessor" is the codebase's own word, not a gloss. `CompressionCodecDelta.cpp:33`
declares `bool isCompression() const override { return false; }`, and `:39` returns the
description "Preprocessor (should be followed by some compression codec). Stores difference
between neighboring values; good for monotonically increasing or decreasing data." This is
the exact same idea as Parquet's `BYTE_STREAM_SPLIT`, which also does not shrink anything —
both exist to make the *next* stage's job easier.

**A column with no `CODEC` clause** gets a default, and the default depends on the part's
size. `CompressionCodecFactory`'s own default codec is `ZSTD(3)`
(`CompressionFactory.cpp:372`), but a MergeTree table without a `<compression>` server
config takes its codec from `CompressionCodecSelector::choose`
(`src/Storages/CompressionCodecSelector.h:106-122`), which uses LZ4 below
`min_part_size_for_default_codec` = `100 * 1024 * 1024` = **100 MiB** (`:60`) and `ZSTD(3)`
at or above it (`:113-115`). The size it compares is the one known *before* writing
(`:45-49`): a fresh insert passes `0` and is LZ4; a merge passes the size of its source
parts, so the big parts background merges produce cross the line and are rewritten as
`ZSTD(3)`. The reasoning is in the header comment at `:40-43` — "This keeps compression cheap
for small, frequently rewritten data while getting the better ratio for the bulk of the
data." It is Step 7's merge cycle doing one more job: the cheap codec on the hot, short-lived
parts, the dense one on the parts that will sit on disk.

**And an analyzing mode exists, as an opt-in.** The MergeTree setting
`enable_adaptive_codec_selection` (`MergeTreeSettings.cpp:2654-2661`; tier `EXPERIMENTAL`,
default `false`, introduced in 26.8 per its history entry) makes merges and mutations
choose a codec **per block** for columns that use the default codec (no `CODEC` clause, or
`CODEC(Default)`). `CompressionCodecAdaptive` (`src/Compression/CompressionCodecAdaptive.h:32-65`)
evaluates every candidate on the block and keeps the smallest output (`:44-48`; a codec that
can report its output size without producing it is only run in full if it wins). The pool is built
by `poolForType` (`:24-28`): `NONE` first, so an incompressible block is stored raw; the
table's default codec second, so the result is never worse than the default; then
type-specific codecs, each alone and — when the default is a general-purpose compressor —
followed by it, ordered by descending decompression speed (`:26-27`) — and a tie goes to the
earliest entry (`:45`), so it resolves to the faster read and `NONE` beats an equal-sized
compressor. The wrapper never reaches disk (`:32-34`): each block carries the
winner's method byte, and a reader decodes it with no knowledge that a choice was made.

**Why it matters:** this is the third answer to "who picks the encoding". ClickHouse makes
*you* declare the chain — or, if you do not, picks by part size between LZ4 and `ZSTD(3)`;
DuckDB analyses every column (`reading-duckdb-compression.md`); BtrBlocks samples 1% of each
block (`reading-btrblocks-fsst.md`). ClickHouse's own adaptive mode is the DuckDB answer at
block granularity, and it is off by default. Which is right depends entirely on who knows
the data's shape, and each system's answer is really a statement about who its users are.

---

## Where each step lives in the code

All paths relative to the repo root of **ClickHouse/ClickHouse@c6428eb**
(`v26.10.2.103-stable`).

| Step | Anchor | What you will see |
| --- | --- | --- |
| 2 | `src/Storages/MergeTree/MergeTreeDataPartCompact.h:8-14` | the Compact layout, in a comment |
| 2 | `src/Storages/MergeTree/MergeTreeSettings.cpp:37`, `:80` | `default_min_bytes_for_wide_part` = 10 MiB |
| 3 | `src/Storages/MergeTree/MergeTreeSettings.cpp:74` | `index_granularity` = 8192 — note "Maximum" at `:75` |
| 3 | `src/Storages/MergeTree/MergeTreeSettings.cpp:2163`, `:2168`, `:2175-2178`, `:2223` | the 10 MiB byte cap, its floor, the rows-per-granule rule, adaptive granularity |
| 4 | `src/Storages/MergeTree/IMergeTreeDataPart.h:549`, `:550` | `getIndex()` / `loadIndexToCache()` |
| 5 | `src/Storages/MergeTree/MergeTreeDataSelectExecutor.cpp:2244` | `markRangesFromPKRange`, reached from `:208` and `:1216` |
| 5 | same file, `:2703`, `:2708-2782`, `:2783-2936` | the choice, then the two search algorithms |
| 5 | `src/Core/Settings.cpp:1935`, `:1942`, `:1921`, `:1928` | coarse granularity 8, step budget, seek thresholds |
| 6 | `src/Formats/MarkInCompressedFile.h:14-21` | the two-offset mark |
| 6 | same file, `:38-41`, `:51-67` | FOR + bit-packing applied to the marks themselves |
| 6 | `src/Core/Settings.cpp:146`, `:161` | compression block sizes, with a worked example in the doc (`:151-155`) |
| 7 | `src/Storages/MergeTree/MergeTreeDataMergerMutator.cpp:328` | `selectPartsToMerge` |
| 7 | `src/Storages/MergeTree/MergeTask.h:78-82`, `:84` | merge priority, and the resumable task |
| 7 | `src/Storages/MergeTree/MergeTreeSettings.cpp:541`, `:777`, `:1090`, `:1171`, `:1193` | the merge and back-pressure limits |
| 8 | `src/Compression/CompressionCodecMultiple.cpp:132-180`, `:182-261` | chain composition, and undoing it in reverse (`:204`) |
| 8 | `src/Compression/CompressionCodecDelta.cpp:33`, `:39` | a codec that admits it is not a compressor |
| 8 | `src/Storages/CompressionCodecSelector.h:40-60`, `:106-122` | the size-aware default: LZ4 below 100 MiB, `ZSTD(3)` above |
| 8 | `src/Compression/CompressionFactory.cpp:372` | the factory default, `ZSTD(3)` |
| 8 | `src/Compression/CompressionCodecAdaptive.h:11-65`; `MergeTreeSettings.cpp:2654` | per-block adaptive selection, and the setting that enables it |

**Read order:** `MergeTreeSettings.cpp:74` and `:2163` (the two granule caps) →
`markRangesFromPKRange` (the read path is the payload; skim to `:2703` and read both
branches) → `MarkInCompressedFile.h` end to end, it is 156 lines →
`selectPartsToMerge` → `CompressionCodecMultiple.cpp` → `CompressionCodecSelector.h`.
`ReplacingMergeTree`, `SummingMergeTree` and `AggregatingMergeTree` are siblings in
`src/Storages/MergeTree/` if you want to see compaction-as-computation.

Use `tools/pinned-source.py grep` and `show -r A:B` rather than cloning; whole-file `show`
on this repository will bury you.

---

## Work the numbers yourself

Do this before opening the code. All four calculations use one table: **10 billion rows**,
`ORDER BY user_id` (a `UInt64`), and a query that reads three `UInt64` columns.

**1. The index.** `10e9 / 8192` = **1,220,703 granules**. `primary.idx` = 1,220,703 × 8 B
= **9.8 MB**, resident forever. Dense alternative: 10e9 × 8 B = **80 GB**. Ratio: 8192×,
by construction.

**2. The marks.** Three columns × 1,220,703 granules = 3,662,109 marks. Naive
(`2 × size_t`) = 58.6 MB; at the measured ~3 B/mark for integer columns
(`MarkInCompressedFile.h:39`) = **11 MB**. Note that marks scale with *columns × granules*,
so a 200-column table has 244 million marks — which is why compressing them mattered enough
to write a bespoke scheme.

**3. The point-query over-read.** `WHERE user_id = 42` matching exactly one row still
decompresses one full granule of each of the three columns: `8192 × 8 B × 3` =
**196,608 bytes** = 192 KiB, to return 24 bytes of payload. Read amplification **8192×**.
For a scan of a billion rows that ratio is invisible; for an OLTP workload doing 100,000
point lookups per second it is 19.7 GB/s of pure waste. Same number, opposite verdict —
which is the entire argument for why OLTP and OLAP engines cannot be the same engine.

**4. Which GB/s?** `FINDINGS.md` row 12 records this topic's measured scan floor: **24–57
GB/s on a machine with roughly 150 GB/s of memory bandwidth**. Those are *logical* bytes —
values processed after decoding. A MergeTree scan of an LZ4'd column at, say, 4× compression
reads a quarter as many bytes off disk as it processes, so the same scan can report "40
GB/s" (logical) while moving 10 GB/s of real traffic, or report "10 GB/s" (physical) for
identical work. Both numbers are correct; neither is meaningful alone. The discipline is to
say which bytes you counted, every time — and to sanity-check against the hardware, since
`FINDINGS.md` row 12 also preserves this topic's own **19,047,619 GB/s**, printed by a
hoisted timing loop, which is about **127,000×** the machine's peak bandwidth and therefore
impossible on its face. An implausible bandwidth is a bug in the benchmark, not a discovery.

---

## Questions for notes.md

1. **Sparse index over-read.** Worst case rows decompressed for a point query with
   granularity 8192 and a 3-column read? Compute it in bytes for `UInt64` columns, then
   redo it for a table whose rows average 4 KiB (where `index_granularity_bytes` binds
   instead — `MergeTreeSettings.cpp:2163`). Why is that fine here and fatal for OLTP?
2. **Two offsets per mark: why can't it be one?** Use the `min_compress_block_size` worked
   example at `src/Core/Settings.cpp:151-155`: for a `UInt32` column at 8192 rows/granule,
   which marks have a non-zero `offset_in_decompressed_block`, and what is its value? Then
   explain why `MarkInCompressedFile.h:55` can say "y is usually zero".
3. **`ORDER BY (user_id, ts)` vs `(ts, user_id)`** — which queries does each serve? Now
   connect it to Step 5: `Settings.cpp:1943` says the generic exclusion search runs "when
   it uses key columns other than the first one". Which of your two orderings forces which
   algorithm, for `WHERE ts > now() - 1h`?
4. **Merge heuristics.** What goes wrong with too-eager merging (write amp) versus too-lazy
   (read amp)? Put numbers on the lazy end using `parts_to_delay_insert` = 1000 and
   `parts_to_throw_insert` = 3000 (`MergeTreeSettings.cpp:1171`, `:1193`): what does a client
   observe as the part count crosses each? Compare with topic 4's leveled-vs-tiered dial.
5. **M12/M22.** FalkorDB stores matrices per relationship type. What is the "part"
   equivalent if property columns become mergeable segments — and could a merge
   pre-aggregate degree statistics the way `SummingMergeTree` does? Say what you would give
   up (hint: the same thing ClickHouse gave up in Step 1).

---

## Takeaway

MergeTree is what a storage engine looks like when you accept that you will never do a fast
point read and optimise everything else without that constraint. Sorted immutable parts
make ingest sequential; an index at 1/8192 resolution stays in RAM at any table size;
granules make the read quantum big enough that vectorized code amortises every per-call
cost; two-offset marks pay the small, unavoidable tax for layering block compression
underneath; merges are both garbage collection and a compute opportunity; and codecs are
declared by the person who actually knows the data — with a size-aware default, cheap LZ4
for young parts and `ZSTD(3)` for merged ones, for everyone who does not. Each decision is legible only in terms
of the workload — which is the general lesson worth carrying to the next system.

---

## Done when

Answer each before unfolding it.

- [ ] Someone tells you a MergeTree granule is 8192 rows. When are they wrong, and what
      makes them wrong?

<details><summary>Answer</summary>

Whenever the rows are wide. `index_granularity` = 8192 is a **maximum** — its own doc
string at `MergeTreeSettings.cpp:75` reads "Maximum number of data rows between the marks
of an index". A second cap, `index_granularity_bytes`, defaults to 10 MiB
(`:2163`), and adaptive granularity is on by default via `enable_mixed_granularity_parts`
(`:2223`). A granule is `min(8192 rows, ~10 MiB)`.

Concretely: at 100 B/row the row cap binds (8192 rows ≈ 800 KiB). At 4 KiB/row the byte cap
binds and a granule is 2560 rows. At 1 MiB/row it is 10 rows. Without the byte cap, a
"granule" of 8192 × 1 MiB would be an 8 GB read quantum.

</details>

- [ ] `WHERE user_id = 42` and `WHERE user_id % 2 = 0` on `ORDER BY user_id` take different
      code paths through the index. Name both and say what determines the choice.

<details><summary>Answer</summary>

The switch is `key_condition.matchesExactContinuousRange()` at
`MergeTreeDataSelectExecutor.cpp:2703`, branched on at `:2708`.

`user_id = 42` is a single continuous key interval, so ClickHouse binary-searches for the
left and right endpoint marks (`:2783-2936`, tagged `SearchAlgorithm::BinarySearch` at
`:2789`). The comment at `:2785-2787` describes exactly this.

`user_id % 2 = 0` is not an interval — qualifying granules are scattered — so there is
nothing to bracket. ClickHouse runs a **generic exclusion search** (`:2708-2782`),
recursively splitting each mark range into `merge_tree_coarse_index_granularity` subranges
(default 8, `src/Core/Settings.cpp:1935`) and discarding subranges where the condition
provably cannot hold. It is bounded by `merge_tree_generic_exclusion_search_max_steps`
(`Settings.cpp:1942`); when the budget runs out, unanalysed ranges are "accepted as a
whole" (`:1945`) — correct, but reading more granules than necessary.

</details>

- [ ] A mark is two 64-bit offsets = 16 bytes. A 200-column table with 10 billion rows has
      244 million marks. Why is that not 3.9 GB of RAM?

<details><summary>Answer</summary>

Because the in-memory mark array is compressed with this topic's own schemes.
`MarkInCompressedFile.h:51-63` lists the regularities it exploits: `y` (the offset within
the decompressed block) is usually zero, `x` (the offset in the file) increases steadily,
and differences between neighbouring `x` values fit in far fewer than 64 bits. So marks are
split into fixed-size blocks; each block stores the per-block minimum of `x` and `y` and
then the residuals bit-packed at a per-block width — **frame-of-reference plus
bit-packing**, exactly what Parquet and DuckDB apply to user data.

Measured result, from the class comment at `:38-41`: ~3 bytes/mark for integer columns,
~5 for string columns, ~0.3 for trivial marks in LowCardinality dictionary files. At 3
B/mark the 244 million marks cost ~732 MB rather than 3.9 GB, and random access is still
O(1) (`:33-34`).

</details>

- [ ] Ingest outruns merges. Trace what a client sees, in order, and say what ClickHouse is
      protecting.

<details><summary>Answer</summary>

Active parts in one partition accumulate. At **1000** (`parts_to_delay_insert`,
`MergeTreeSettings.cpp:1171`) inserts are artificially slowed — the doc at `:1178-1179` says
ClickHouse "adds 'sleep' so that the background merge process can merge parts faster than
they are added". At **3000** (`parts_to_throw_insert`, `:1193`) inserts are rejected
outright.

It is protecting read amplification. Every query must consult every active part's index and
merge their outputs, so query cost grows with part count. Rather than let scans degrade
without bound, the engine converts the problem into back-pressure on writers — a
deliberate choice to fail the ingest path loudly instead of the query path quietly. This is
topic 4's write-vs-read amplification dial with the thresholds written down.

</details>

- [ ] ClickHouse's `Delta` codec and Parquet's `BYTE_STREAM_SPLIT` both shrink nothing.
      Why does either exist?

<details><summary>Answer</summary>

Because they are **preprocessors** for the stage that follows. ClickHouse says so in its own
type system: `CompressionCodecDelta.cpp:33` is `bool isCompression() const override
{ return false; }`, and the description at `:39` is "Preprocessor (should be followed by
some compression codec)."

`Delta` turns a monotonically increasing sequence into a run of small, similar numbers,
which LZ4 or ZSTD can then encode in far fewer bytes. `BYTE_STREAM_SPLIT` scatters each
double's 8 bytes into 8 separate streams so that the (highly repetitive) exponent bytes sit
next to each other. Same move, different axis: neither removes information, both increase
*local* similarity so a general-purpose compressor finds more matches.

The practical consequence is that `CODEC(Delta)` alone is close to a no-op — the chain
mechanism (`CompressionCodecMultiple.cpp:132-180`) exists precisely so these can be composed
with a real compressor, and decompression undoes them in reverse order (`:204`).

</details>

- [ ] A table declares no `CODEC` anywhere and runs on a server with no `<compression>`
      config. Which codec compresses a freshly inserted part, which compresses the 5 GB part
      a merge later produces from it, and why does ClickHouse bother to differ?

<details><summary>Answer</summary>

The fresh insert is **LZ4**; the merged part is **`ZSTD(3)`**. `CompressionCodecSelector::choose`
(`src/Storages/CompressionCodecSelector.h:106-122`) picks LZ4 when the size it is given is
below `min_part_size_for_default_codec` = 100 MiB (`:60`, `:113-115`), and otherwise the
factory default, which is `ZSTD(3)` (`CompressionFactory.cpp:372`). The size is the one
known before writing (`:45-49`): an insert has none yet and passes `0`, a merge passes the
total size of its source parts — 5 GB is far above 100 MiB.

The reason is Step 7's merge cycle. Small parts are short-lived: they are read soon and
rewritten by the next merge, so paying ZSTD's slower compression on them buys a ratio that
is thrown away minutes later. Large merged parts are where the bytes live for a long time,
so the better ratio pays for itself. The header comment at `:40-43` says exactly this. The
per-block adaptive alternative (`enable_adaptive_codec_selection`,
`MergeTreeSettings.cpp:2654`) is experimental and off by default, so it plays no part here.

</details>

---

## References

**Papers**

- The VLDB '24 system paper gets its own chapter:
  [reading-clickhouse-paper.md](reading-clickhouse-paper.md) — read it after this code
  walk; it supplies the *why* for every *what* above.

**Code** — all at `ClickHouse/ClickHouse@c6428eb` (`v26.10.2.103-stable`)

- `src/Storages/MergeTree/` — `MergeTreeSettings.cpp`, `IMergeTreeDataPart.h`,
  `MergeTreeDataPartCompact.h`, `MergeTreeDataSelectExecutor.cpp`,
  `MergeTreeDataMergerMutator.cpp`, `MergeTask.h`
- `src/Formats/MarkInCompressedFile.h` — 156 lines, read all of it
- `src/Core/Settings.cpp` — the query-level knobs (`:146`, `:161`, `:1921`, `:1928`,
  `:1935`, `:1942`)
- `src/Compression/` — `CompressionCodecMultiple.cpp`, `CompressionCodecDelta.cpp`,
  `CompressionFactory.cpp`, `CompressionCodecAdaptive.h`
- `src/Storages/CompressionCodecSelector.h` — the size-aware default codec

**In this topic**

- [reading-arrow-parquet.md](reading-arrow-parquet.md) — the same two-level addressing
  problem, solved in a file format
- [reading-duckdb-compression.md](reading-duckdb-compression.md) and
  [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md) — the other two answers to "who
  picks the encoding"
- `FINDINGS.md` row 12 — the measured scan floor (24–57 GB/s on a ~150 GB/s machine) and
  the 19,047,619 GB/s hoisted-loop bug
