# VictoriaMetrics & InfluxDB 3: two rebuttals to Prometheus

Same problem as prometheus, two opposite bets. VictoriaMetrics doubles
down on a custom LSM — tighter codecs, explicit parts and merges, its
own format end to end. InfluxDB 3 (the productized IOx, in Rust) deletes
the custom engine entirely and rebuilds on Parquet + object storage —
topic 28's stack wearing a TSDB hat. This chapter builds each design
step by step and reads them against each other: together they show which
parts of a TSDB are essential and which are just a storage engine.

## The problem in one sentence

Prometheus's engine is one point in a design space — custom chunk
format, local disk, 2 h blocks — and these two systems each move a
different axis to its extreme: VictoriaMetrics asks how much a fully
custom vertically-integrated LSM can save (bytes, allocations, index
lookups), InfluxDB 3 asks how little custom engine you need once Parquet
and object storage exist.

## The concepts, step by step

### Step 1 — the same machine, said out loud

VictoriaMetrics is the prometheus architecture with the LSM vocabulary
made explicit — where prometheus says head and blocks, VM says raw rows,
**parts** (immutable sorted files), and merge workers:

```
 ingest ──► rawRows shards (per-CPU)  ──convert──► parts (immutable)
            partition.go:75, :72                     merge workers compact
            8MB in-memory buffers                    parts within a PARTITION
                                     partitions are MONTHLY directories
                                     retention = drop old partitions (table.go:131)
```

Two deltas from prometheus worth noticing immediately: ingest is sharded
**per CPU** (each core owns an 8 MB rawRows buffer — no cross-core
contention on the hot path), and the time partitions are **monthly**
directories rather than 2 h blocks, because VM sells long retention —
retention still equals "drop the oldest partition", just at coarser
grain (Q2).

### Step 2 — VM's codec: integers first, then lossy on purpose

VM's value codec starts by *leaving floating point*: values are scaled to
int64 via decimal encoding (12.34 → 1234 with exponent −2, per block), so
that arithmetic prediction works on them. Then **nearest_delta2** applies
the same predictor as Gorilla (delta-of-delta) but encodes the errors as
**zigzag varints** (zigzag maps signed to unsigned so small negatives
stay small; varints are byte-aligned) instead of a bitstream — batchable,
SIMD-friendly, cheaper to decode. The twist is `precisionBits`: dod bits
below the noise floor you care about are *dropped* — the codec is
**optionally lossy**. Gorilla is exact; VM lets you buy ratio with
honesty about float noise:

```rust
// floats already scaled to i64 via decimal encoding
fn nearest_delta2(vals: &[i64], precision_bits: u8, out: &mut Vec<u8>) {
    let (mut prev, mut prev_delta) = (vals[0], 0i64);
    for &v in &vals[1..] {
        let delta = v - prev;
        let dod = delta - prev_delta;               // same predictor as Gorilla…
        let dod = trim_precision(dod, precision_bits); // …but LOSSY on purpose:
        out.extend(zigzag_varint(dod));             // drop bits below the noise floor
        prev_delta = delta; prev = v;               // byte-aligned varints, not a
    }                                               // bitstream => batch/SIMD friendly
}
```

The integer detour has a failure mode — mixed magnitudes in one block
break the shared decimal exponent (Q1) — and `precisionBits` is also the
paper-over for it.

### Step 3 — VM's index: cache the query, not just the postings

VM targets higher cardinality than prometheus, so the label index
(`index_db`) gets a second layer: a **tagFilters → metricIDs cache** —
a cache keyed by the *whole selector*, storing the resulting series-id
set, sitting in front of the inverted index. Selector evaluation at 100M+
series is expensive enough to warrant a query-shaped cache; the price is
invalidation — registering any new series can change any selector's
answer, so high **churn** (new series arriving constantly) is exactly
what defeats it (Q3 — the same failure shape as topic 8's plan-cache
invalidation). Out-of-order and duplicate handling get the same
philosophy as prometheus but a different location: **dedup at
scrape-interval granularity during merges** — folded into compaction,
off the hot path.

### Step 4 — IOx: delete the engine, keep the pipeline

InfluxDB 3 / IOx makes the opposite bet: no custom chunk format, no
custom file format, no custom query engine. The TSDB dissolves into
topic 28's stack — **Parquet** (the standard immutable columnar file
format with per-column encodings and min/max statistics) on **object
storage**, with **Arrow** (the standard in-memory columnar
representation) for recent data and **DataFusion** (a Rust SQL engine
over Arrow/Parquet) for queries:

```
 write ──► WAL (object store)  ──snapshot──► Parquet files (object store)
           influxdb3_wal/src/lib.rs:75-98      sorted, time-partitioned
                │                              catalog tracks file min/max t
                ▼
           QueryableBuffer (Arrow, in-memory)
           influxdb3_write/src/write_buffer/queryable_buffer.rs:41
           serves recent data; DataFusion executes SQL over buffer+Parquet
```

The shapes are all still here — WAL for durability-fast (topic 28's
landing zone), an in-memory head (the `QueryableBuffer`), immutable
time-partitioned files, a catalog with min/max pruning — implemented by
commodity components instead of bespoke ones.

### Step 5 — IOx's out-of-order story: sort at snapshot

The Arrow buffer accepts disorder freely; when accumulated WAL periods
are snapshotted, the data is **sorted by (series, time) before writing
Parquet**, so files come out clean. Late data arriving after its
snapshot lands in *new* files whose time ranges overlap old ones; the
query layer merges overlapping files, and compaction later rewrites them
away. Compare the ladder across the three systems: prometheus pays for
disorder at *read* time (OOO chunk merge), our `head.rs` pays at *flush*,
IOx pays at *snapshot + compaction* — same quarantine, three different
bills. And note Q5's sleeper: that (series, time) sort is itself a big
fraction of what made Gorilla look good.

### Step 6 — the bet, side by side

The two systems price the same trade oppositely — vertical integration
vs commodity leverage:

| | VictoriaMetrics | InfluxDB 3 |
|---|---|---|
| codec | custom, tighter, optionally lossy | Parquet, standard, good enough |
| storage | local disks it manages | object store (topic 28 economics) |
| query | PromQL-compatible engine | SQL via DataFusion |
| ecosystem | its own format, its own tools | anything that reads Parquet |
| bet | vertical integration wins on cost | commodity formats win on leverage |

VM's claim: at metrics scale, the custom codec and index savings compound
into a hardware bill no general-purpose format matches. IOx's claim:
Parquet's delta + dictionary + zstd encodings plus min/max pruning get
close enough (Q5), and in exchange every SQL engine on earth can read
your history directly — the format *is* the API.

## Where each step lives in the code

VictoriaMetrics (Go) anchors:

- `lib/storage/partition.go:75` — `type partition`: rawRows buffered per
  CPU (`:46`), converted to sorted immutable parts in the background
  (Step 1). Explicitly the LSM vocabulary prometheus hides: parts,
  merges, levels. Retention = drop old partitions (`table.go:131`).
- `lib/encoding/nearest_delta2.go:15` — the value codec (Step 2):
  delta-of-delta as *int64s* + varint batches (values are first scaled
  to integers via decimal.go); `precisionBits` makes it optionally
  lossy.
- `lib/storage/index_db.go:124` — tagFilters→metricIDs cache in front of
  the label index (Step 3), invalidated on new-series registration.
- `lib/storage/dedup.go` — dedup at scrape-interval granularity during
  merges (Step 3): OOO and duplicate handling folded into compaction,
  not the hot path — same quarantine philosophy as prometheus, different
  location.

InfluxDB 3 (Rust) anchors:

- `influxdb3_wal/src/lib.rs:75-98` — the WAL flushes on a period; the
  `SnapshotTracker` decides when accumulated WAL periods become a Parquet
  snapshot (Steps 4–5). The landing-zone pattern from topic 28:
  durable-fast first, columnar-later.
- `influxdb3_write/src/write_buffer/queryable_buffer.rs:41` —
  `QueryableBuffer`: the head block, but it's Arrow record batches, and
  "flush" means *write Parquet + update catalog*, with an optional
  `ParquetCacheOracle` (`:49`) prewarming the read cache — topic 28's
  cache-fixes-the-median (Step 4).

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

## References

**Papers**
- None — both systems are documented in code and blog posts rather than
  papers; the IOx design discussions on the InfluxData blog are the
  closest thing to a paper for the Parquet bet

**Code**
- [VictoriaMetrics](https://github.com/VictoriaMetrics/VictoriaMetrics)
  (Go) — `lib/storage/partition.go`, `lib/encoding/nearest_delta2.go`,
  `lib/storage/index_db.go`, `lib/storage/dedup.go`
- [influxdb](https://github.com/influxdata/influxdb) (Rust — the repo
  is InfluxDB 3, the productized IOx) — `influxdb3_wal/src/lib.rs`,
  `influxdb3_write/src/write_buffer/queryable_buffer.rs`
