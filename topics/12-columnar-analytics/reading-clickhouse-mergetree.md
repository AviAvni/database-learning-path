# MergeTree: brute force, organized

ClickHouse's storage engine is topic 4's LSM shapes at analytics scale: immutable sorted
parts, background merges, and — because the workload is scans, not point reads — an index
that is deliberately **sparse**.

`src/Storages/MergeTree/` is enormous. Read only what is anchored here. Every anchor below
was checked against **ClickHouse/ClickHouse@4d598fb2c**; confirm the pin before you start:

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

`MergeTreeDataPartCompact.h:8-16` states it directly: "In compact format all columns are
stored in one file (`data.bin`). Data is split in granules and columns are serialized
sequentially in one granule… It's considered to store only small parts in compact format
(up to 10M)." The threshold is `default_min_bytes_for_wide_part = 10485760` — **10 MiB** —
at `MergeTreeSettings.cpp:34`, wired to the `min_bytes_for_wide_part` setting at `:76`.

The reason is file-count economics, not query performance: a thousand small inserts into a
200-column table would otherwise create 200,000 tiny files. Compact parts are transient —
merges promote them to Wide once they exceed 10 MiB.

Even inside a Compact part, ClickHouse tries to keep columnar skipping alive:
`compress_per_column_in_compact_parts` (default `true`,
`MergeTreeSettings.cpp:878-883`) starts a new compressed block for each column within a
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
`MergeTreeSettings.cpp:70` declares `index_granularity` with default `8192`. But read the
doc string on the next line: "**Maximum** number of data rows between the marks of an
index." There is a second, independent cap: `index_granularity_bytes`, default
`10 * 1024 * 1024` = **10 MiB** (`MergeTreeSettings.cpp:1676`), described as "Maximum size
of data granules in bytes", with a floor of `min_index_granularity_bytes = 1024`
(`:1681`). Mixed (adaptive) granularity is on by default —
`enable_mixed_granularity_parts` at `:1714`, whose doc explains it "improves ClickHouse
performance when selecting data from tables with big rows (tens and hundreds of
megabytes)".

So a granule holds `min(8192 rows, whatever fits in 10 MiB)`. Work it:

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
`IMergeTreeDataPart.h:424` is `getIndex()`, and `:425` is `loadIndexToCache()`, which
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
(`MergeTreeDataSelectExecutor.cpp:1725`, called at `:189` and `:1070`) branches on
`key_condition.matchesExactContinuousRange()` at `:2131`:

**Case A — the predicate is one continuous key interval** (`user_id = 42`,
`user_id BETWEEN 10 AND 20` on `ORDER BY (user_id, …)`). Binary search for the left and
right endpoints. The code says so at `:2180-2182`: "In case when SELECT's predicate defines
a single continuous interval of keys, we can use binary search algorithm to find the left
and right endpoint key marks of such interval. The returned value is the minimum range of
marks, containing all keys for which KeyCondition holds." It is tagged
`SearchAlgorithm::BinarySearch` at `:2184`.

**Case B — anything else** (`user_id % 2 = 0`; or a predicate on the *second* key column
only). Binary search is meaningless because the qualifying granules are not contiguous, so
ClickHouse runs a **generic exclusion search** (`:2136-2176`): recursively split each mark
range into `merge_tree_coarse_index_granularity` subranges — default **8**,
`src/Core/Settings.cpp:1593` — and discard any subrange in which the condition provably
cannot be true.

```
// ILLUSTRATION — not quoted from ClickHouse/ClickHouse; this is the shape of the two
// branches in MergeTreeDataSelectExecutor.cpp:2131-2200 rendered in Rust. The real
// binary-search loop starts at :2190; the exclusion search is delegated to
// genericExclusionSearch() at :2156 (src/Storages/MergeTree/GenericExclusionSearch.h).
fn mark_ranges(idx: &[Key], cond: &KeyCondition) -> Vec<MarkRange> {
    if cond.matches_exact_continuous_range() {          // :2131
        // one interval: find its two endpoints, return the minimal covering range
        let lo = lower_bound(idx, cond.left());
        let hi = upper_bound(idx, cond.right());
        vec![MarkRange { begin: lo, end: hi }]          // :2184 BinarySearch
    } else {
        // no interval to bracket: split coarsely and drop what cannot match
        generic_exclusion_search(idx, cond, /* coarse = */ 8)  // :2156
    }
}
```

Two details worth carrying away, because they are the difference between the idea and a
production implementation:

- The exclusion search has a **step budget**,
  `merge_tree_generic_exclusion_search_max_steps` (`Settings.cpp:1600`, default `0` =
  unlimited). Its doc at `:1603` is unusually candid about the trade: "When it is
  exhausted, the ranges that were not fully analyzed are **accepted as a whole**, so the
  query stays correct but may read more granules than an unlimited search would select."
  Index analysis is itself a cost centre that can be capped, and the failure mode is
  reading too much, never reading too little.
- Surviving ranges that are close together get **fused**, because a seek costs more than
  reading through the gap. `min_marks_for_seek` is computed at `:2143` from
  `merge_tree_min_rows_for_seek` / `merge_tree_min_bytes_for_seek`
  (`Settings.cpp:1579`, `:1586`, both default `0`): "If the distance between two data
  blocks to be read in one file is less than … then ClickHouse does not seek through the
  file but reads the data sequentially."

### Step 6 — Marks: two offsets, because compression blocks ≠ granules

> **In:** a `MarkRange` from Step 5 and a column file.
> **Out:** a byte position to seek to, and the reason it takes two numbers rather than one.

Column files are a sequence of independently compressed blocks. Block boundaries are chosen
by **size**, granule boundaries by **rows**, and the two grids do not align — so a mark
carries two coordinates:

```c
// ClickHouse/ClickHouse@4d598fb2c — src/Formats/MarkInCompressedFile.h:14-21
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
(`:108`) and `max_compress_block_size` = **1,048,576** (1 MiB, `:123`). The `:108` doc
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
// ClickHouse/ClickHouse@4d598fb2c — src/Formats/MarkInCompressedFile.h:51-63
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
(`MergeTreeDataMergerMutator.cpp:272`) makes the choice. The knobs and their defaults, all
in `MergeTreeSettings.cpp`, turn the abstract dial into numbers:

| Setting | Default | Line | What it bounds |
| --- | --- | --- | --- |
| `merge_selector_base` | 5.0 | `:860` | "Affects write amplification of assigned merges" |
| `max_parts_to_merge_at_once` | 100 | `:637` | fan-in of a single merge |
| `max_bytes_to_merge_at_max_space_in_pool` | 150 GiB | `:475` | largest merge ever attempted |
| `parts_to_delay_insert` | 1000 | `:886` | inserts get an artificial sleep past this |
| `parts_to_throw_insert` | 3000 | `:908` | inserts are **rejected** past this |

The last two are the "too lazy" failure mode made concrete, and they are back-pressure, not
tuning: `:893-894` says ClickHouse "artificially executes `INSERT` longer (adds 'sleep') so
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

### Step 8 — Codec chains: the user declares, nothing analyzes

> **In:** a column and a `CODEC(...)` clause in the DDL.
> **Out:** a chain of transforms applied left to right, and a self-describing header that
> lets the reader undo them.

Each column carries a declared chain of codecs. `CODEC(Delta, LZ4)` means delta-encode
(**delta encoding**: store each value's difference from the previous one), then LZ4 the
result. Composition is literal — `CompressionCodecMultiple::doCompressData`
(`src/Compression/CompressionCodecMultiple.cpp:44-67`) loops the codecs in order, swapping
its input and output buffers each time (`:60-61`), and writes a header of
`[codec count][method byte × N]` before the data (`:49`, `:55`, `:64`). Decompression walks
the same list **backwards** — `for (int idx = compression_methods_size - 1; idx >= 0;
--idx)` at `:86`.

The overhead is exactly `1 + codecs.size()` bytes per compressed block (`:66`). On a
64 KiB block with a two-codec chain that is 3 bytes, or **0.0046%** — the reason nobody
thinks about it.

The menu (`src/Compression/`) includes the general-purpose codecs — LZ4
(`CompressionCodecLZ4.cpp`, and the default: `CompressionFactory.cpp:257` does
`default_codec = get("LZ4", {})`) and ZSTD — plus a set of *preprocessors* that shrink
nothing by themselves: `Delta`, `DoubleDelta` (deltas of deltas — near-zero for regular
timestamps), `Gorilla` (XOR consecutive floats — sensor values barely change), `FPC`,
`GCD`, `T64`, and `ALP` (topic 30 material).

"Preprocessor" is the codebase's own word, not a gloss. `CompressionCodecDelta.cpp:30`
declares `bool isCompression() const override { return false; }`, and `:36` returns the
description "Preprocessor (should be followed by some compression codec). Stores difference
between neighboring values; good for monotonically increasing or decreasing data." This is
the exact same idea as Parquet's `BYTE_STREAM_SPLIT`, which also does not shrink anything —
both exist to make the *next* stage's job easier.

**Why it matters:** this is the third answer to "who picks the encoding". ClickHouse makes
*you* declare the chain (or takes the LZ4 default); DuckDB analyses every column
(`reading-duckdb-compression.md`); BtrBlocks samples 1% of each block
(`reading-btrblocks-fsst.md`). Which is right depends entirely on who knows the data's
shape, and each system's answer is really a statement about who its users are.

---

## Where each step lives in the code

All paths relative to the repo root of **ClickHouse/ClickHouse@4d598fb2c**.

| Step | Anchor | What you will see |
| --- | --- | --- |
| 2 | `src/Storages/MergeTree/MergeTreeDataPartCompact.h:8-16` | the Compact layout, in a comment |
| 2 | `src/Storages/MergeTree/MergeTreeSettings.cpp:34`, `:76` | `default_min_bytes_for_wide_part` = 10 MiB |
| 3 | `src/Storages/MergeTree/MergeTreeSettings.cpp:70` | `index_granularity` = 8192 — note "Maximum" at `:71` |
| 3 | `src/Storages/MergeTree/MergeTreeSettings.cpp:1676`, `:1681`, `:1714` | the 10 MiB byte cap and adaptive granularity |
| 4 | `src/Storages/MergeTree/IMergeTreeDataPart.h:424`, `:425` | `getIndex()` / `loadIndexToCache()` |
| 5 | `src/Storages/MergeTree/MergeTreeDataSelectExecutor.cpp:1725` | `markRangesFromPKRange`, used at `:189` and `:1070` |
| 5 | same file, `:2131`, `:2136-2176`, `:2178-2200` | the two search algorithms and the choice between them |
| 5 | `src/Core/Settings.cpp:1593`, `:1600`, `:1579`, `:1586` | coarse granularity 8, step budget, seek thresholds |
| 6 | `src/Formats/MarkInCompressedFile.h:14-21` | the two-offset mark |
| 6 | same file, `:38-41`, `:51-67` | FOR + bit-packing applied to the marks themselves |
| 6 | `src/Core/Settings.cpp:108`, `:123` | compression block sizes, with a worked example in the doc |
| 7 | `src/Storages/MergeTree/MergeTreeDataMergerMutator.cpp:272` | `selectPartsToMerge` |
| 7 | `src/Storages/MergeTree/MergeTask.h:78-82`, `:84` | merge priority, and the resumable task |
| 7 | `src/Storages/MergeTree/MergeTreeSettings.cpp:475`, `:637`, `:860`, `:886`, `:908` | the merge and back-pressure limits |
| 8 | `src/Compression/CompressionCodecMultiple.cpp:44-67`, `:86` | chain composition, and undoing it in reverse |
| 8 | `src/Compression/CompressionCodecDelta.cpp:30`, `:36` | a codec that admits it is not a compressor |
| 8 | `src/Compression/CompressionFactory.cpp:257` | LZ4 is the default |

**Read order:** `MergeTreeSettings.cpp:70` and `:1676` (the two granule caps) →
`markRangesFromPKRange` (the read path is the payload; skim to `:2131` and read both
branches) → `MarkInCompressedFile.h` end to end, it is 156 lines →
`selectPartsToMerge` → `CompressionCodecMultiple.cpp`. `ReplacingMergeTree`,
`SummingMergeTree` and `AggregatingMergeTree` are siblings in
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
   instead — `MergeTreeSettings.cpp:1676`). Why is that fine here and fatal for OLTP?
2. **Two offsets per mark: why can't it be one?** Use the `min_compress_block_size` worked
   example at `src/Core/Settings.cpp:109-117`: for a `UInt32` column at 8192 rows/granule,
   which marks have a non-zero `offset_in_decompressed_block`, and what is its value? Then
   explain why `MarkInCompressedFile.h:55` can say "y is usually zero".
3. **`ORDER BY (user_id, ts)` vs `(ts, user_id)`** — which queries does each serve? Now
   connect it to Step 5: `Settings.cpp:1601` says the generic exclusion search runs "when
   it uses key columns other than the first one". Which of your two orderings forces which
   algorithm, for `WHERE ts > now() - 1h`?
4. **Merge heuristics.** What goes wrong with too-eager merging (write amp) versus too-lazy
   (read amp)? Put numbers on the lazy end using `parts_to_delay_insert` = 1000 and
   `parts_to_throw_insert` = 3000 (`MergeTreeSettings.cpp:886`, `:908`): what does a client
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
declared by the person who actually knows the data. Each decision is legible only in terms
of the workload — which is the general lesson worth carrying to the next system.

---

## Done when

Answer each before unfolding it.

- [ ] Someone tells you a MergeTree granule is 8192 rows. When are they wrong, and what
      makes them wrong?

<details><summary>Answer</summary>

Whenever the rows are wide. `index_granularity` = 8192 is a **maximum** — its own doc
string at `MergeTreeSettings.cpp:71` reads "Maximum number of data rows between the marks
of an index". A second cap, `index_granularity_bytes`, defaults to 10 MiB
(`:1676`), and adaptive granularity is on by default via `enable_mixed_granularity_parts`
(`:1714`). A granule is `min(8192 rows, ~10 MiB)`.

Concretely: at 100 B/row the row cap binds (8192 rows ≈ 800 KiB). At 4 KiB/row the byte cap
binds and a granule is 2560 rows. At 1 MiB/row it is 10 rows. Without the byte cap, a
"granule" of 8192 × 1 MiB would be an 8 GB read quantum.
</details>

- [ ] `WHERE user_id = 42` and `WHERE user_id % 2 = 0` on `ORDER BY user_id` take different
      code paths through the index. Name both and say what determines the choice.

<details><summary>Answer</summary>

The switch is `key_condition.matchesExactContinuousRange()` at
`MergeTreeDataSelectExecutor.cpp:2131`.

`user_id = 42` is a single continuous key interval, so ClickHouse binary-searches for the
left and right endpoint marks (`:2178-2200`, tagged `SearchAlgorithm::BinarySearch` at
`:2184`). The comment at `:2180-2182` describes exactly this.

`user_id % 2 = 0` is not an interval — qualifying granules are scattered — so there is
nothing to bracket. ClickHouse runs a **generic exclusion search** (`:2136-2176`),
recursively splitting each mark range into `merge_tree_coarse_index_granularity` subranges
(default 8, `src/Core/Settings.cpp:1593`) and discarding subranges where the condition
provably cannot hold. It is bounded by `merge_tree_generic_exclusion_search_max_steps`
(`Settings.cpp:1600`); when the budget runs out, unanalysed ranges are "accepted as a
whole" (`:1603`) — correct, but reading more granules than necessary.
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
`MergeTreeSettings.cpp:886`) inserts are artificially slowed — the doc at `:893-894` says
ClickHouse "adds 'sleep' so that the background merge process can merge parts faster than
they are added". At **3000** (`parts_to_throw_insert`, `:908`) inserts are rejected
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
type system: `CompressionCodecDelta.cpp:30` is `bool isCompression() const override
{ return false; }`, and the description at `:36` is "Preprocessor (should be followed by
some compression codec)."

`Delta` turns a monotonically increasing sequence into a run of small, similar numbers,
which LZ4 or ZSTD can then encode in far fewer bytes. `BYTE_STREAM_SPLIT` scatters each
double's 8 bytes into 8 separate streams so that the (highly repetitive) exponent bytes sit
next to each other. Same move, different axis: neither removes information, both increase
*local* similarity so a general-purpose compressor finds more matches.

The practical consequence is that `CODEC(Delta)` alone is close to a no-op — the chain
mechanism (`CompressionCodecMultiple.cpp:44-67`) exists precisely so these can be composed
with a real compressor, and decompression undoes them in reverse order (`:86`).
</details>

---

## References

**Papers**

- The VLDB '24 system paper gets its own chapter:
  [reading-clickhouse-paper.md](reading-clickhouse-paper.md) — read it after this code
  walk; it supplies the *why* for every *what* above.

**Code** — all at `ClickHouse/ClickHouse@4d598fb2c`

- `src/Storages/MergeTree/` — `MergeTreeSettings.cpp`, `IMergeTreeDataPart.h`,
  `MergeTreeDataPartCompact.h`, `MergeTreeDataSelectExecutor.cpp`,
  `MergeTreeDataMergerMutator.cpp`, `MergeTask.h`
- `src/Formats/MarkInCompressedFile.h` — 156 lines, read all of it
- `src/Core/Settings.cpp` — the query-level knobs (`:108`, `:123`, `:1579`, `:1586`,
  `:1593`, `:1600`)
- `src/Compression/` — `CompressionCodecMultiple.cpp`, `CompressionCodecDelta.cpp`,
  `CompressionFactory.cpp`

**In this topic**

- [reading-arrow-parquet.md](reading-arrow-parquet.md) — the same two-level addressing
  problem, solved in a file format
- [reading-duckdb-compression.md](reading-duckdb-compression.md) and
  [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md) — the other two answers to "who
  picks the encoding"
- `FINDINGS.md` row 12 — the measured scan floor (24–57 GB/s on a ~150 GB/s machine) and
  the 19,047,619 GB/s hoisted-loop bug
