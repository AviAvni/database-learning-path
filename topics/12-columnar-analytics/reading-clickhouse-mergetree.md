# Reading guide — ClickHouse MergeTree: brute force, organized (~1.5 h)

Local clone: [`~/repos/clickhouse`](https://github.com/ClickHouse/ClickHouse) (fresh shallow clone), dir
`src/Storages/MergeTree/`. This codebase is huge — read ONLY the slices
below. The goal: understand parts / granules / sparse index / merges,
and recognize topic 4's LSM shapes at analytics scale.

## 1. The mental model

```
 table = set of immutable sorted PARTS (sorted by ORDER BY key)
 part  = one directory: one file per column + primary.idx + marks
 granule = 8192 rows (index_granularity, MergeTreeSettings.cpp:70)
 mark  = (offset_in_compressed_file, offset_in_decompressed_block)
         one mark per granule per column
 INSERT -> writes a NEW part (no in-place anything; topic 4's
           immutability), background MERGES combine parts
```

An LSM tree where: memtable ≈ the insert block, SSTable ≈ part,
compaction ≈ merge — but no WAL-per-row, no point-read path, and the
index is SPARSE because the workload is scans.

## 2. The sparse primary index

- `primary.idx` = the ORDER BY key of the FIRST row of each granule —
  8192× smaller than the data; always in memory
  (`IMergeTreeDataPart.h:424` getIndex / `:425` loadIndexToCache).
- Query on the key → binary search granule RANGES:
  `MergeTreeDataSelectExecutor::markRangesFromPKRange` (:1725, used at
  `:189`) — turns a predicate into a list of `MarkRanges` to read.
- Marks (`src/Formats/MarkInCompressedFile.h:17`): two offsets, because
  granules live inside compressed blocks — seek to compressed offset,
  decompress, skip to row.

```
 WHERE user_id = 42 (ORDER BY user_id):
 primary.idx: [1, 800, 1600, ...]  -> binary search -> granules 3..4
 read marks[3..4] per needed column -> decompress ~16K rows, scan them
```

Sparse = you always over-read up to a granule; the bet is that
decompress+scan of 8192 rows is cheap (vectorized) and the index stays
resident. A B-tree answers "which row"; this answers "which 8192 rows".

## 3. Merges (topic 4 redux)

`MergeTreeDataMergerMutator::selectPartsToMerge` (:272) + `MergeTask.h:84`
— background merge selection with heuristics balancing write
amplification vs part count (too many parts = slow scans, the
read-amp/write-amp dial again). Specialized engines
(ReplacingMergeTree, AggregatingMergeTree, SummingMergeTree) do WORK
during merges — dedup, pre-aggregation — compaction-as-computation, the
trick FalkorDB could steal for graph statistics.

## 4. Codecs (`src/Compression/`)

Per-column codec CHAINS (`CompressionCodecMultiple.cpp`):
`CODEC(Delta, LZ4)` composes. The menu includes the time-series
specials: DoubleDelta, Gorilla (XOR floats), FPC, GCD, ALP — topic 30
material. Contrast DuckDB: ClickHouse makes YOU declare the chain (or
takes the default LZ4); no analyze-and-score pass.

## 5. What to take from the VLDB '24 paper framing

Materialized views (AggregatingMergeTree targets) as the answer to
"scans are still too slow": precompute during ingest/merge. The
architecture triangle: brute-force scan speed (ClickHouse) vs
precomputation (Pinot/Druid star-tree) vs embedded convenience
(DuckDB).

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
