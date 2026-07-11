# Reading guide — VictoriaMetrics + InfluxDB 3 (IOx): two rebuttals to Prometheus

Code: [`~/repos/VictoriaMetrics`](https://github.com/VictoriaMetrics/VictoriaMetrics) (Go), [`~/repos/influxdb`](https://github.com/influxdata/influxdb) (Rust — the
repo is InfluxDB 3, the productized IOx). Same problem, two opposite
bets: VM doubles down on a custom LSM; InfluxDB 3 deletes the custom
engine and rebuilds on Parquet + object storage (topic 28's stack).

## VictoriaMetrics: the LSM said out loud

```
 ingest ──► rawRows shards (per-CPU)  ──convert──► parts (immutable)
            partition.go:75, :72                     merge workers compact
            8MB in-memory buffers                    parts within a PARTITION
                                     partitions are MONTHLY directories
                                     retention = drop old partitions (table.go:131)
```

- `lib/storage/partition.go:75` — `type partition`: rawRows buffered per
  CPU (`:46`), converted to sorted immutable parts in the background.
  Explicitly the LSM vocabulary prometheus hides: parts, merges, levels.
- `lib/encoding/nearest_delta2.go:15` — the value codec: delta-of-delta
  as *int64s* + varint batches (values are first scaled to integers via
  decimal.go). Contrast Gorilla: byte-aligned varints, batch-friendly,
  SIMD-able — and `precisionBits` makes it **optionally lossy** (drop
  mantissa bits below the precision you care about). Gorilla is exact;
  VM lets you buy ratio with honesty about float noise.
- `lib/storage/index_db.go:124` — tagFilters→metricIDs cache in front of
  the label index: selector evaluation is expensive enough at VM's
  cardinality targets to warrant a query-shaped cache, invalidated on
  new-series registration.
- `lib/storage/dedup.go` — dedup at scrape-interval granularity during
  merges: OOO and duplicate handling folded into compaction, not the hot
  path — same quarantine philosophy as prometheus, different location.

## InfluxDB 3 / IOx: the TSDB dissolves into topic 28

```
 write ──► WAL (object store)  ──snapshot──► Parquet files (object store)
           influxdb3_wal/src/lib.rs:75-98      sorted, time-partitioned
                │                              catalog tracks file min/max t
                ▼
           QueryableBuffer (Arrow, in-memory)
           influxdb3_write/src/write_buffer/queryable_buffer.rs:41
           serves recent data; DataFusion executes SQL over buffer+Parquet
```

- `influxdb3_wal/src/lib.rs:75-98` — the WAL flushes on a period; the
  `SnapshotTracker` decides when accumulated WAL periods become a Parquet
  snapshot. The landing-zone pattern from topic 28: durable-fast first,
  columnar-later.
- `queryable_buffer.rs:41` — `QueryableBuffer`: the head block, but it's
  Arrow record batches, and "flush" means *write Parquet + update
  catalog*, with an optional `ParquetCacheOracle` (`:49`) prewarming the
  read cache — topic 28's cache-fixes-the-median.
- Out-of-order: absorbed by sorting at snapshot time — the buffer accepts
  disorder, Parquet files come out time-sorted. Late data past a
  snapshot lands in *new* files that overlap old time ranges; the
  query layer merges (and compaction later rewrites).
- The bet: Parquet's general-purpose encodings (delta, dictionary, zstd)
  + pruning-by-min/max-stats are close enough to Gorilla, and in exchange
  every SQL engine on earth can read your history directly.

## The trade, in one table

| | VictoriaMetrics | InfluxDB 3 |
|---|---|---|
| codec | custom, tighter, optionally lossy | Parquet, standard, good enough |
| storage | local disks it manages | object store (topic 28 economics) |
| query | PromQL-compatible engine | SQL via DataFusion |
| ecosystem | its own format, its own tools | anything that reads Parquet |
| bet | vertical integration wins on cost | commodity formats win on leverage |

## Questions to answer while reading

1. VM scales floats to int64 via decimal encoding before delta2. What
   float values break that (hint: mixed magnitudes in one block), and how
   does `precisionBits` paper over it?
2. Monthly partitions (VM) vs 2h blocks (prometheus): derive how each
   choice follows from the retention story each system sells.
3. The tagFilters cache is invalidated by new series. Why is that
   invalidation *the* high-churn failure mode, and what does it share
   with topic 8's plan-cache invalidation?
4. IOx: a query for the last 5 minutes must see WAL-buffered data not yet
   in Parquet. Trace which component serves it and what the consistency
   story is between buffer and files during a snapshot.
5. Parquet delta + dictionary + zstd vs Gorilla on a gauge: predict the
   ratio gap, then reconcile with the fact that IOx sorts by (series,
   time) before writing — how much of Gorilla's win was really *sorting*?
6. M30 mapping: FalkorDB's history could be custom chunks (VM-style) or
   Parquet-on-object-store (IOx-style, M28 already built the substrate).
   Which do you pick for `MATCH ... AT TIME t` and why does the answer
   differ for hot recent history vs year-old history?
