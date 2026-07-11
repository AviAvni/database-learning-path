# Topic 12 — Columnar Storage & Analytics

DuckDB/ClickHouse-style OLAP. The thesis of this topic: **compression IS
performance**. Encoded data is smaller, so scans move fewer bytes — and
with lightweight encodings, scanning compressed data is often FASTER
than scanning raw, because analytics is memory-bound (topic 11's
lesson) and decode cost < bandwidth saved.

```
 row store (OLTP)                column store (OLAP)
 ┌──────────────────┐            ┌────┐┌────┐┌────┐
 │ id │ name │ city │  1 row =   │ id ││name││city│   1 column =
 │ id │ name │ city │  1 place   │ id ││name││city│   1 place
 │ id │ name │ city │            │ id ││name││city│
 └──────────────────┘            └────┘└────┘└────┘
 point lookup: 1 cache line      SELECT sum(x): reads ONLY x,
 analytics: reads everything     10-100x less I/O + it compresses
```

Columns compress because a column is SELF-SIMILAR: same type, similar
values, sorted or clustered. Rows interleave types and kill every trick
below.

## 1. The lightweight encoding zoo

Not gzip. These are encodings the SCAN can execute over directly:

| encoding | idea | wins on | decode cost |
|---|---|---|---|
| RLE | (value, run_length) pairs | sorted / low-cardinality | ~free, can even predicate-push on runs |
| Dictionary | ids into a distinct-value dict | strings, low NDV | array index; comparisons become int == |
| Bit-packing | ints in ceil(log2(max-min+1)) bits | small int ranges | shift+mask, SIMD-able |
| FOR (frame of reference) | store min + deltas from it | clustered values | add a constant |
| Delta | diffs from previous value | timestamps, sequences | prefix sum (SIMD-able, but sequential-ish) |
| FSST | string symbol table: 8-byte substrings → 1-byte codes | med-cardinality strings dictionary can't dedup | table lookup per code; random access preserved |

DuckDB stacks them: bitpacking.cpp implements CONSTANT / FOR / DELTA_FOR
as MODES of one function; dictionary ids get bit-packed; FSST codes get
dictionary'd (`dict_fsst/`). BtrBlocks (SIGMOD '23) makes the stacking
recursive and picks per-block by SAMPLING each encoder.

## 2. How a system picks: analyze → score → compress

DuckDB's compression framework (`compression_function.hpp:130–141`):
every candidate encoder gets an `analyze` pass over the column data,
returns a SCORE (estimated compressed size); the cheapest wins, per
row-group per column. Forced via `PRAGMA force_compression` for your
experiments. This is the benchmark-before-choosing discipline built into
the storage engine.

## 3. Zone maps (min/max pruning)

Per-segment min/max stats let the scan SKIP segments that can't match:

```
 WHERE ts BETWEEN '2026-01-01' AND '2026-01-02'
 seg 0 [ts: 2025-11-01 .. 2025-12-04]  -> skip (no read, no decode)
 seg 1 [ts: 2025-12-04 .. 2026-01-05]  -> scan
 seg 2 [ts: 2026-01-05 .. 2026-02-11]  -> skip
```

Effective ONLY if data is clustered on the filter column — zone maps on
random data prune nothing (every zone spans the whole domain). That's
why ClickHouse makes you declare ORDER BY at table creation. DuckDB:
`ColumnData::CheckZonemap` (column_data.cpp:423), stats per segment.
Parquet: min/max per column chunk + page.

## 4. The formats: Arrow (memory) vs Parquet (disk)

- **Arrow**: columnar IN MEMORY, designed for zero-copy compute:
  `ArrayData` (arrow-data/src/data.rs:208) = type + buffers (values,
  validity bitmap, offsets). No encoding beyond dictionary — layout IS
  the contract, kernels (topic 11's polars-compute) run on it directly.
- **Parquet**: columnar ON DISK: file → row groups → column chunks →
  pages, each page encoded (PLAIN / RLE_DICTIONARY /
  DELTA_BINARY_PACKED / BYTE_STREAM_SPLIT, parquet/src/basic.rs:397+)
  then optionally block-compressed (snappy/zstd). Min/max stats per
  chunk and page (metadata/mod.rs:630, :808).
- The boundary: Parquet optimizes bytes-at-rest + selective reads;
  Arrow optimizes compute. Decode once at the boundary — unless the
  engine can execute ON the encoding (DuckDB scans over compressed
  segments; late materialization below).

## 5. Late materialization

Keep data encoded/columnar as deep into the plan as possible:

- filter on dictionary column → compare dictionary CODES (int ==), only
  decode survivors;
- DuckDB's DICTIONARY/FSST vector types (topic 11) carry compressed data
  THROUGH operators;
- join produces row ids; fetch payload columns only for matches
  (C-Store's original pitch).

## 6. Architectures compared

| | ClickHouse MergeTree | DuckDB | Pinot/Druid |
|---|---|---|---|
| shape | LSM-flavored: sorted PARTS merged in background (topic 4!) | single file, row groups of 122880 | ingest-time indexing, segments |
| primary index | SPARSE: one key per 8192-row granule, binary-search marks | zone maps only | per-segment inverted/star-tree |
| ordering | ORDER BY key, physical sort | insertion order (+ optional sort) | time-partitioned |
| niche | fastest brute-force scans | embedded analytics | real-time slice-and-dice |

MergeTree = topic 4's LSM ideas at analytics scale: immutable sorted
parts, background merges, but the "memtable" is a whole part and the
index is sparse because scans, not point reads, are the workload.

## Experiments (`experiments/`)

1. `encodings.rs` — YOU implement RLE, dictionary, and bit-packing
   encode/decode for `Vec<u64>`; tests fix round-trips and exact
   compressed sizes.
2. `scan_bench` — PROVIDED: `sum()` over raw vs encoded columns for
   three data shapes (sorted-ish / low-NDV / random). The headline: does
   decode-while-scanning beat raw's bandwidth? Includes a
   sum-WITHOUT-decoding path for RLE (value × run_length) — operate on
   the encoding.
3. `duckdb-clickbench.md` — run 5 ClickBench queries on DuckDB, EXPLAIN
   ANALYZE, note which compression each hot column chose
   (`PRAGMA storage_info`).

## Reading guides

| guide | what it walks |
|---|---|
| [reading-duckdb-compression.md](reading-duckdb-compression.md) | analyze/score/compress framework, bitpacking modes, FSST, zone maps |
| [reading-clickhouse-mergetree.md](reading-clickhouse-mergetree.md) | parts, granules, sparse index, merges |
| [reading-arrow-parquet.md](reading-arrow-parquet.md) | arrow-rs layout + parquet-rs encodings/stats |
| [reading-cstore-compression.md](reading-cstore-compression.md) | C-Store (VLDB '05) + SIGMOD '06 compression-aware execution |
| [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md) | BtrBlocks sampling + FSST symbol tables |
| [reading-clickhouse-paper.md](reading-clickhouse-paper.md) | VLDB '24 system paper |

## Capstone M12

Columnar attribute storage + zone-map pruning for property filters:

- [ ] properties stored as columns per label (node id → value), not
      per-node maps — the schema question: sparse columns for optional
      properties (validity bitmap, Arrow-style)
- [ ] encodings: dictionary for strings, FOR/bitpack for ints — reuse
      encodings.rs
- [ ] zone maps per column segment; `WHERE n.age > 65` prunes segments
      before decode
- [ ] the FalkorDB angle: matrices index STRUCTURE (topology), columns
      store PAYLOAD (properties) — the split mirrors
      Parquet-stats/Arrow-compute; measure a property-filter query
      before/after zone maps
