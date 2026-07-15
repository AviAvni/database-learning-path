# MergeTree: brute force, organized

ClickHouse's storage engine is topic 4's LSM shapes at analytics
scale: immutable sorted parts, background merges, and — because the
workload is scans, not point reads — an index that is deliberately
SPARSE. Before you open `src/Storages/MergeTree/` (the codebase is
huge; read ONLY what's anchored below), this chapter builds the
machine step by step: what a part is, what's inside one, why reads
happen 8192 rows at a time, how a sparse index answers "which 8192
rows" instead of "which row", why every mark is two offsets, what the
background merges do, and who picks the compression. Then it hands
you the file and line anchors.

## The problem in one sentence

Serve `GROUP BY` scans over billions of rows arriving at millions of
inserts per second — a B-tree that pays a page write per row can't
ingest that, and a per-row index would be bigger than the data, so
ClickHouse indexes only every 8192nd row and makes scanning the rest
cheap enough not to care.

## The concepts, step by step

### Step 1 — the part: an insert writes new files, never modifies old ones

A **part** is one self-contained directory of files holding a batch of
rows, sorted by the table's declared sort key (`ORDER BY`); every
INSERT creates a brand-new part, and existing parts are never modified
— they are **immutable**. A table is just the set of its current
parts, and background **merges** combine small parts into bigger ones
(and delete the inputs).

```
 table = set of immutable sorted PARTS (sorted by ORDER BY key)
 INSERT -> writes a NEW part (no in-place anything; topic 4's
           immutability), background MERGES combine parts
```

This is an LSM (log-structured merge design, topic 4: absorb writes
into new sorted files, merge in background) where: memtable ≈ the
insert block, SSTable ≈ part, compaction ≈ merge — but no WAL-per-row,
no point-read path. Why it matters: ingest is pure sequential file
writes at disk bandwidth, and everything read-side can assume sorted,
immutable data.

### Step 2 — inside a part: one file per column, sorted by the ORDER BY key

Within a part, each column is stored in its own file — the columnar
split — and all files are ordered by the same sort key, so row *i* of
every column file belongs to the same logical row:

```
 part  = one directory: one file per column + primary.idx + marks
```

The `ORDER BY` key you declare at table creation decides the physical
sort — and therefore the clustering — of every part forever. That is
the price of admission ClickHouse states upfront: you must know your
main filter column at schema time (DuckDB's zone maps have the same
clustering dependency, just undeclared). Why it matters: sorting is
what makes the sparse index (Step 4) and the compression both work.

### Step 3 — the granule: reads happen 8192 rows at a time

A **granule** is the read quantum — a fixed slice of 8192 consecutive
rows (`index_granularity`); the engine never reads or indexes anything
smaller. A **mark** is the per-granule, per-column bookmark saying
where that granule's bytes start in the column file.

```
 granule = 8192 rows (index_granularity)
 mark  = (offset_in_compressed_file, offset_in_decompressed_block)
         one mark per granule per column
```

The bet behind the number: decompressing and scanning 8192 rows with
vectorized code costs microseconds, so it is never worth tracking
anything finer. Why it matters: every read-side structure now scales
with `rows / 8192` — three orders of magnitude smaller than the data.

### Step 4 — the sparse primary index: which granules, not which row

The **sparse primary index** (`primary.idx`) stores only the sort key
of the FIRST row of each granule — one entry per 8192 rows, so it's
8192× smaller than the key column and always stays in memory. A
predicate on the key becomes two binary searches over this array,
producing a *range of granules* to read:

```rust
// primary_idx[g] = ORDER BY key of granule g's FIRST row — 8192x
// smaller than the data, always in memory
fn mark_range(primary_idx: &[Key], lo: &Key, hi: &Key) -> Range<usize> {
    let first = primary_idx.partition_point(|k| k < lo).saturating_sub(1);
    let last = primary_idx.partition_point(|k| k <= hi);
    first..last   // for each granule: seek marks[g].compressed_offset,
}                 // decompress the block, skip to row — then just scan
```

```
 WHERE user_id = 42 (ORDER BY user_id):
 primary.idx: [1, 800, 1600, ...]  -> binary search -> granules 3..4
 read marks[3..4] per needed column -> decompress ~16K rows, scan them
```

Sparse = you always over-read up to a granule; the bet is that
decompress+scan of 8192 rows is cheap (vectorized) and the index stays
resident. A B-tree answers "which row"; this answers "which 8192
rows". Why it matters: for a scan workload the over-read is noise, and
in exchange the entire index for a 10-billion-row table is ~1.2M
entries — RAM-resident forever.

### Step 5 — marks: two offsets, because compression blocks ≠ granules

Column files are stored as a sequence of independently compressed
blocks, and a granule's rows can start in the *middle* of one — so a
mark must carry two coordinates: seek to `offset_in_compressed_file`,
decompress that block, then skip `offset_in_decompressed_block` bytes
to reach the granule's first row. One offset can't work because
compression block boundaries are chosen by size (~64 KB–1 MB), not by
row count, and the two grids don't align. Why it matters: it's the
concrete cost of layering block compression under a row-addressed
index — every format that compresses in blocks (Parquet pages, next
chapter) grows the same two-level addressing.

### Step 6 — merges: the metabolic cycle, and work done during them

Background merges continuously take several parts and merge-sort them
into one bigger part — the LSM compaction — steering between two
failure modes: merge too eagerly and you rewrite the same rows over
and over (write amplification); too lazily and scans must visit too
many parts (read amplification). Topic 4's dial, at part granularity.

The distinctly ClickHouse move: since a merge already streams every
row through memory, **do other work while you're there**. Specialized
engines run computation inside the merge — `ReplacingMergeTree` dedups
rows, `SummingMergeTree` / `AggregatingMergeTree` pre-aggregate them —
compaction-as-computation, and the mechanism behind materialized views
(the paper's answer to "scans are still too slow": precompute during
ingest/merge). That's the architecture triangle: brute-force scan
speed (ClickHouse) vs precomputation (Pinot/Druid star-trees) vs
embedded convenience (DuckDB). It's also the trick FalkorDB could
steal for graph statistics. Why it matters: merge bandwidth is the
system's metabolism — background IO converted into query speed.

### Step 7 — codec chains: the user declares, nothing analyzes

Each column carries a declared chain of codecs that compose left to
right — `CODEC(Delta, LZ4)` means delta-encode (store differences from
the previous value), then LZ4 the result. The menu includes
time-series specials: DoubleDelta (deltas of deltas — near-zero for
regular timestamps), Gorilla (XOR consecutive floats — sensor values
barely change), FPC, GCD, ALP (topic 30 material). Contrast the
previous chapter: ClickHouse makes YOU declare the chain (or takes the
default LZ4) — there is no analyze-and-score pass. Why it matters:
it's the third answer to "who picks the encoding" — user-declared
(ClickHouse) vs full-analyze (DuckDB) vs sampled (BtrBlocks) — and the
right answer depends on who knows the data's shape.

## Where each step lives in the code

All under `src/Storages/MergeTree/` unless noted; a fresh shallow
clone is enough.

- **Step 3** — `index_granularity = 8192`: `MergeTreeSettings.cpp:70`.
- **Step 4** — the in-memory index: `IMergeTreeDataPart.h:424`
  `getIndex` / `:425` `loadIndexToCache`. Predicate → granule ranges:
  `MergeTreeDataSelectExecutor::markRangesFromPKRange` (`:1725`, used
  at `:189`) — turns a predicate into a list of `MarkRanges` to read.
- **Step 5** — the two-offset mark:
  `src/Formats/MarkInCompressedFile.h:17`.
- **Step 6** — merge selection:
  `MergeTreeDataMergerMutator::selectPartsToMerge` (`:272`) — the
  heuristics balancing write amplification vs part count — plus
  `MergeTask.h:84`. The specialized engines (ReplacingMergeTree,
  AggregatingMergeTree, SummingMergeTree) are siblings in the same
  directory.
- **Step 7** — codec chains: `src/Compression/`, composition in
  `CompressionCodecMultiple.cpp`.

Read order: settings → `markRangesFromPKRange` (the read path is the
payload) → `MarkInCompressedFile.h` → merge selection → codecs. The
design rationale behind all of it is the VLDB '24 paper —
[reading-clickhouse-paper.md](reading-clickhouse-paper.md), read after
this code walk.

## Questions for notes.md

1. Sparse index over-read: worst case rows decompressed for a point
   query with granularity 8192 and a 3-column read? Why is that fine
   here and fatal for OLTP?
2. Two offsets per mark: why can't it be one? (Compression block
   boundaries ≠ granule boundaries.)
3. ORDER BY choice: `(user_id, ts)` vs `(ts, user_id)` — which queries
   does each serve, and what happens to zone maps on the second column?
   (Same clustering lesson as DuckDB zone maps, but declared upfront.)
4. Merge heuristics: what goes wrong with too-eager merging
   (write amp) vs too-lazy (read amp)? Topic 4's leveled-vs-tiered, at
   part granularity.
5. M12/M22: FalkorDB stores matrices per relationship type. What's the
   "part" equivalent if property columns become mergeable segments —
   and could a merge pre-aggregate degree stats the way
   SummingMergeTree does?

## Done when

You can draw part → granule → mark → compressed block, walk a point
query through the sparse index, and name what ClickHouse traded away
(point reads, in-place updates) for scan throughput.

## References

**Papers**
- The VLDB '24 system paper gets its own chapter:
  [reading-clickhouse-paper.md](reading-clickhouse-paper.md) — read it
  after this code walk

**Code**
- [ClickHouse](https://github.com/ClickHouse/ClickHouse) —
  `src/Storages/MergeTree/` (the anchors above:
  `MergeTreeSettings.cpp`, `IMergeTreeDataPart.h`,
  `MergeTreeDataSelectExecutor.cpp`,
  `MergeTreeDataMergerMutator.cpp`, `MergeTask.h`),
  `src/Formats/MarkInCompressedFile.h`, and `src/Compression/` for the
  codec chains; a fresh shallow clone is enough
