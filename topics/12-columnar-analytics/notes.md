# Topic 12 notes — columnar storage & analytics

## Predictions (fill BEFORE running scan_bench)

Raw baseline context: 100M u64 = 800 MB; this Mac's bandwidth ≈ ? GB/s
(check topic 0 baselines) → raw sum floor ≈ ? s.

| shape | encoding | predicted vs raw (faster/slower, ×) | actual |
|---|---|---|---|
| sorted low-card | rle sum (no decode) | | |
| sorted low-card | rle decode+sum | | |
| shuffled low-card | dict sum (codes only) | | |
| small-range random | bitpack decode+sum | | |

| question | prediction | actual |
|---|---|---|
| best raw-equiv GB/s seen (does anything beat the memory bus?) | | |
| sizes: rle / dict / bitpack per shape | | |
| dict "codes only" sum: bound by 4-byte code reads or the counts array? | | |

## Implementation log

- [ ] Rle encode/decode/sum-on-encoding; maximal-runs test green
- [ ] Dict encode/decode; sorted-dedup test green
- [ ] BitPacked encode/decode/get; width-0, FOR, random-access tests green
- [ ] scan_bench full table recorded above
- [ ] stretch: DictPacked cascade (bit-pack the codes) — sizes before/after

Surprises / dead ends:

## ClickBench-on-DuckDB (see reading-clickhouse-paper.md §experiments)

| query | time | rows pruned by zone maps | hot column compression (storage_info) |
|---|---|---|---|
| Q0 | | | |
| Q3 | | | |
| Q8 | | | |
| Q13 | | | |
| Q20 | | | |

## Questions from the reading guides

### DuckDB compression (reading-duckdb-compression.md)

1. BtrBlocks sampling vs full analyze — what sampling risks:
2. fetch_row on DELTA_FOR — cost, and why OLAP tolerates it:
3. RLE vs dict on 50% NULLs (validity changes the answer how):
4. Zone-map always-true (filter removal) — when it beats skipping:
5. Bitpacking mode for graph node-id payload columns:

### ClickHouse MergeTree (reading-clickhouse-mergetree.md)

1. Worst-case over-read for a point query (granularity 8192, 3 cols):
2. Why marks need two offsets:
3. ORDER BY (user_id,ts) vs (ts,user_id) — zone maps on col 2:
4. Too-eager vs too-lazy merging — topic 4 analogue:
5. Part-equivalent for mergeable property segments + SummingMergeTree-style degree stats:

### Arrow + Parquet (reading-arrow-parquet.md)

1. Why Arrow has ~no encodings (what delta breaks for kernels):
2. RLE-hybrid rationale + worst case vs PLAIN:
3. BYTE_STREAM_SPLIT = columns-beat-rows one level down:
4. Truncated string max stats — the increment-the-prefix bug:
5. M12 optional properties: validity bitmap vs roaring presence, 1% vs 99%:

### C-Store + SIGMOD '06 (reading-cstore-compression.md)

1. Run-shortcuttable aggregates (min/max/count/avg yes-ish; distinct/median?):
2. What made ClickHouse projections affordable when C-Store's weren't:
3. WS/RS/tuple-mover → topic 4 vocabulary map:
4. Position lists vs bitmaps — selectivity crossover:
5. Process-compressed plan for `n.country = 'IL'` at 1% selectivity:

### BtrBlocks + FSST (reading-btrblocks-fsst.md)

1. URLs / country codes / UUIDs — winner per case + cascade for URLs:
2. Why FSST's table must be static:
3. Ingest-cost/ratio/burden triangle: sample vs analyze vs declare:
4. FSST worst-case inflation vs RLE-hybrid worst case:
5. String property cascade + where predicate-on-encoded works:

### ClickHouse paper (reading-clickhouse-paper.md)

1. Where ClickHouse barely wins and why:
2. Merge-starvation failure mode — topic 4 stall analogue:
3. Part-shipping vs WAL-shipping tradeoffs:
4. **M12 decision**: who chooses encodings for graph property columns
   (declare / analyze / sample) — commit + reason:
5. clickhouse-local vs DuckDB niche:

## Cross-topic threads

- Compression IS performance because analytics is memory-bound —
  topic 11's bandwidth lesson cashed in.
- MergeTree = topic 4's LSM with scan-shaped choices (sparse index,
  merge-time work); "too many parts" = write stalls.
- Selection vectors / late materialization = C-Store position lists —
  same idea, three names, twenty years.
- Vector-type flags (topic 11) = SIGMOD '06's compressed-block API.
- fetch_row constraint = why zstd loses to lightweight encodings —
  random access shapes the menu (LMDB/B-tree echo from topic 3).

## M12 log (columnar properties + zone maps)

- [ ] per-label property columns, node id → value; optional props via
      validity (decide after arrow-parquet Q5)
- [ ] encodings from encodings.rs (dict strings, FOR/bitpack ints)
- [ ] zone maps per segment; measure `WHERE n.age > 65` before/after
- [ ] encoding chooser decision recorded (ClickHouse-paper Q4)
- [ ] structure/payload split doc: matrices = topology, columns = properties

## Done when

- All encoding tests green; scan_bench table filled; at least one
  encoding beats the memory bus (raw-equiv GB/s > bandwidth).
- ClickBench-on-DuckDB table filled.
- M12 encoding-chooser decision written.
