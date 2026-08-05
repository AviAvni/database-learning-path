# VictoriaMetrics & InfluxDB 3: two rebuttals to Prometheus

Same problem as prometheus, two opposite bets. VictoriaMetrics doubles
down on a custom LSM — tighter codecs, explicit parts and merges, its
own format end to end. InfluxDB 3 (the productized IOx, in Rust) deletes
the custom engine entirely and rebuilds on Parquet + object storage —
topic 28's stack wearing a TSDB hat. This chapter builds each design
step by step and reads them against each other: together they show which
parts of a TSDB are essential and which are just a storage engine.

The code anchors are VictoriaMetrics at `c1e39b2` and influxdb (the
InfluxDB 3 / IOx repo) at `d783411`, the two commits this repo pins in
`resources/codebases.md`, quoted with the line numbers they occupy there.
Every claim below names which of the two systems it belongs to — they agree
on less than they look like they do.

## The problem in one sentence

Prometheus's engine is one point in a design space — custom chunk
format, local disk, 2 h blocks — and these two systems each move a
different axis to its extreme: VictoriaMetrics asks how much a fully
custom vertically-integrated LSM can save (bytes, allocations, index
lookups), InfluxDB 3 asks how little custom engine you need once Parquet
and object storage exist.

## The concepts, step by step

### Step 1 — the same machine, said out loud (VictoriaMetrics)

> **In:** prometheus's head/block LSM from reading-prometheus-tsdb.md.
> **Out:** VM's renamed-but-identical skeleton — rawRows → parts → merges,
> monthly partitions — and the two deltas (per-CPU ingest, coarse partitions)
> Steps 2–3 build on.

VictoriaMetrics is the prometheus architecture with the LSM vocabulary
made explicit — where prometheus says head and blocks, VM says raw rows,
**parts** (immutable sorted files), and merge workers:

```
 ingest ──► rawRows shards (per-CPU)  ──convert──► parts (immutable)
            partition.go:75, :46, :72               merge workers compact
            8MB in-memory buffers                    parts within a PARTITION
                                     partitions are MONTHLY directories
                                     retention = drop old partitions (table.go:131)
```

Two deltas from prometheus worth noticing immediately: ingest is sharded
**per CPU** — `rawRowsShardsPerPartition = cgroup.AvailableCPUs()`
(`lib/storage/partition.go:46`), each core owning an 8 MB rawRows buffer
(`maxRawRowsPerShard = (8 << 20) / …`, `:72`) so there is no cross-core
contention on the hot path — and the time partitions are **monthly**
directories rather than 2 h blocks, because VM sells long retention:
retention still equals "drop the oldest partition" (`startRetentionWatcher`,
`lib/storage/table.go:131`), just at coarser grain (Q2).

### Step 2 — VM's codec: integers first, then lossy on purpose

> **In:** VM's per-part storage from Step 1, holding raw f64 values.
> **Out:** the two-stage value codec — decimal scaling to int64, then
> lossy delta-of-delta varints — and the mixed-magnitude failure mode Q1
> hunts.

VM's value codec starts by *leaving floating point*: values are scaled to
int64 via decimal encoding (`AppendFloatToDecimal`, `lib/decimal/decimal.go:173`
— 12.34 → 1234 with exponent −2, per block), so that arithmetic prediction
works on them. Then **nearest_delta2** applies the same predictor as Gorilla
(delta-of-delta) but encodes the errors as **zigzag varints** (zigzag maps
signed to unsigned so small negatives stay small; varints are byte-aligned)
instead of a bitstream — batchable, SIMD-friendly, cheaper to decode. The
twist is `precisionBits`: dod bits below the noise floor you care about are
*dropped* — the codec is **optionally lossy**. Gorilla is exact; VM lets you
buy ratio with honesty about float noise. The marshal function makes both
the predictor and the fast/lossy split explicit:

```go
// VictoriaMetrics lib/encoding/nearest_delta2.go — marshalInt64NearestDelta2, 15-45
   15  func marshalInt64NearestDelta2(dst []byte, src []int64, precisionBits uint8) (result []byte, firstValue int64) {
   // ... 16-22: require >= 2 items, validate precisionBits, seed firstValue ...
   23  	firstValue = src[0]
   24  	d1 := src[1] - src[0]
   25  	dst = MarshalVarInt64(dst, d1)                 // first delta, byte-aligned varint
   26  	v := src[1]
   27  	src = src[2:]
   28  	is := GetInt64s(len(src))
   29  	if precisionBits == 64 {
   30  		// Fast path.
   31  		for i, next := range src {
   32  			d2 := next - v - d1                    // delta-of-delta, same as Gorilla
   33  			d1 += d2
   34  			v += d1
   35  			is.A[i] = d2                           // EXACT: no bits dropped
   36  		}
   37  	} else {
   38  		// Slower path.
   39  		trailingZeros := getTrailingZeros(v, precisionBits)
   40  		for i, next := range src {
   41  			d2, tzs := nearestDelta(next-v, d1, precisionBits, trailingZeros)  // LOSSY
   42  			trailingZeros = tzs
   43  			d1 += d2
   44  			v += d1
   45  			is.A[i] = d2
```

Line 32 is the exact fast path; line 41 (`nearestDelta`) is the lossy path
that trims dod bits below `precisionBits`. The integer detour has a failure
mode — mixed magnitudes in one block break the shared decimal exponent, which
`CalibrateScale` (`lib/decimal/decimal.go:13`) has to reconcile (Q1) — and
`precisionBits` is also the paper-over for it.

### Step 3 — VM's index: cache the query, not just the postings

> **In:** VM's high-cardinality label index, hit by expensive selectors.
> **Out:** the tagFilters→metricIDs cache layered over the postings, its
> churn-driven invalidation failure mode, and dedup-at-merge — the last two
> both "fold the cost into compaction" moves.

VM targets higher cardinality than prometheus, so the label index
(`index_db`) gets a second layer: a **tagFilters → metricIDs cache** —
`tagFiltersToMetricIDsCache *lrucache.Cache` (`lib/storage/index_db.go:125`),
keyed by the *whole selector* and storing the resulting series-id set,
sitting in front of the inverted index. Selector evaluation at 100M+ series
is expensive enough to warrant a query-shaped cache; the price is
invalidation — registering any new series can change any selector's answer,
so high **churn** (new series arriving constantly) is exactly what defeats it
(Q3 — the same failure shape as topic 8's plan-cache invalidation).
Out-of-order and duplicate handling get the same philosophy as prometheus but
a different location: `DeduplicateSamples(…, dedupInterval int64)`
(`lib/storage/dedup.go:30`) folds dedup into merges at a **configurable**
`dedupInterval` (set it to the scrape interval and it collapses double-scrapes)
— off the hot path.

### Step 4 — IOx: delete the engine, keep the pipeline (InfluxDB 3)

> **In:** the same TSDB requirements, but a green field with Parquet, Arrow,
> object storage and DataFusion already available.
> **Out:** the IOx pipeline — WAL on object store → `QueryableBuffer` (Arrow)
> → Parquet — built from commodity parts, and the components Step 5's
> disorder story runs through.

InfluxDB 3 / IOx makes the opposite bet: no custom chunk format, no
custom file format, no custom query engine. The TSDB dissolves into
topic 28's stack — **Parquet** (the standard immutable columnar file
format with per-column encodings and min/max statistics) on **object
storage**, with **Arrow** (the standard in-memory columnar
representation) for recent data and **DataFusion** (a Rust SQL engine
over Arrow/Parquet) for queries:

```
 write ──► WAL (object store)  ──snapshot──► Parquet files (object store)
           influxdb3_wal/src/lib.rs:78        sorted, time-partitioned
                │  (flush_buffer; SnapshotTracker decides when)
                ▼
           QueryableBuffer (Arrow, in-memory)
           queryable_buffer.rs:41
           serves recent data; DataFusion executes SQL over buffer+Parquet
```

`flush_buffer` (`influxdb3_wal/src/lib.rs:78`, a trait method — its doc at
`:74-77` says "If it is time for a snapshot, it will tell the notifier to
start the snapshot") hands off to a `SnapshotTracker` that decides when
accumulated WAL periods become a Parquet snapshot. The in-memory head is a
struct of Arrow batches and cache providers:

```rust
// influxdb3_write/src/write_buffer/queryable_buffer.rs — QueryableBuffer, 41-52
   41  pub struct QueryableBuffer {
   42      pub(crate) executor: Arc<Executor>,
   43      catalog: Arc<Catalog>,
   // ... 44-47: distinct/last-cache providers, persister, persisted_files ...
   48      buffer: Arc<RwLock<BufferState>>,        // the Arrow head, in memory
   49      parquet_cache: Option<Arc<dyn ParquetCacheOracle>>,  // prewarm read cache
   // ... 50-52: snapshot concurrency limit + persisted-snapshot notify channel ...
```

The shapes are all still here — WAL for durability-fast (topic 28's landing
zone), an in-memory head (the `QueryableBuffer` `buffer` field, `:48`),
immutable time-partitioned files, a catalog with min/max pruning, and an
optional `ParquetCacheOracle` (`:49`) prewarming the read cache — implemented
by commodity components instead of bespoke ones.

### Step 5 — IOx's out-of-order story: sort at snapshot

> **In:** the Arrow `QueryableBuffer` from Step 4, accepting writes in any
> order.
> **Out:** the sort-at-snapshot mechanism (`sort_dedupe_persist`) that makes
> Parquet files come out clean, and the three-system disorder ladder that
> reframes how much of Gorilla's win was really sorting.

The Arrow buffer accepts disorder freely; when accumulated WAL periods are
snapshotted, the data is **sorted by the table's sort key (series, then
time) before writing Parquet**, so files come out clean. That is
`sort_dedupe_persist` (`queryable_buffer.rs:567`), which its own comment
describes as "Dedupe and sort using the COMPACT query built into iox_query"
and which passes `sort_key: Some(persist_job.sort_key…)` to the writer
(`:600`); it is called from the snapshot path at `:327`. Late data arriving
after its snapshot lands in *new* files whose time ranges overlap old ones;
the query layer merges overlapping files, and compaction later rewrites them
away. Compare the ladder across the three systems: prometheus pays for
disorder at *read* time (OOO chunk merge), our `head.rs` pays at *flush*, IOx
pays at *snapshot + compaction* — same quarantine, three different bills. And
note Q5's sleeper: that (series, time) sort is itself a big fraction of what
made Gorilla look good — a codec only compresses a neighbour-similar stream
if something first put the neighbours next to each other.

### Step 6 — the bet, side by side

> **In:** the two fully-built designs from Steps 1–5.
> **Out:** the single cost/leverage trade they price oppositely, and the
> capstone question (Q6) of which to pick for hot vs cold history.

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

VictoriaMetrics (Go, `c1e39b2`):

| Anchor | What | Step |
|---|---|---|
| `lib/storage/partition.go:75` | `type partition` — the LSM said out loud | 1 |
| `lib/storage/partition.go:46` | `rawRowsShardsPerPartition = AvailableCPUs()` — per-CPU ingest | 1 |
| `lib/storage/partition.go:72` | `maxRawRowsPerShard` — the 8 MB buffer | 1 |
| `lib/storage/table.go:131` | `startRetentionWatcher` — retention = drop partitions | 1 |
| `lib/encoding/nearest_delta2.go:15` | `marshalInt64NearestDelta2` — dod as varints | 2 |
| `lib/decimal/decimal.go:173`, `:13` | `AppendFloatToDecimal`, `CalibrateScale` — float→int64 | 2 |
| `lib/storage/index_db.go:125` | `tagFiltersToMetricIDsCache` — the query cache | 3 |
| `lib/storage/dedup.go:30` | `DeduplicateSamples` — dedup at a configurable interval, at merge | 3 |

InfluxDB 3 / IOx (Rust, `d783411`):

| Anchor | What | Step |
|---|---|---|
| `influxdb3_wal/src/lib.rs:78` | `flush_buffer` trait method — WAL flush, snapshot trigger | 4, 5 |
| `queryable_buffer.rs:41` | `QueryableBuffer` — the Arrow head | 4 |
| `queryable_buffer.rs:49` | `parquet_cache: Option<…ParquetCacheOracle>` — read prewarm | 4 |
| `queryable_buffer.rs:567` | `sort_dedupe_persist` — sort by sort_key at snapshot | 5 |
| `queryable_buffer.rs:327` | the snapshot call site into `sort_dedupe_persist` | 5 |

Note the honest caveats: `flush_buffer` is a *trait* signature (the "if it is
time for a snapshot" logic lives in the doc and the `SnapshotTracker`
implementation, not this line), and `sort_dedupe_persist` reuses iox_query's
COMPACT rather than any bespoke IOx sort.

## Questions to answer while reading

1. VM scales floats to int64 via decimal encoding before delta2. What
   float values break that (hint: mixed magnitudes in one block), and how
   does `precisionBits` (and `CalibrateScale`, `decimal.go:13`) paper over
   it?
2. Monthly partitions (VM) vs 2h blocks (prometheus): derive how each
   choice follows from the retention story each system sells.
3. The tagFilters cache is invalidated by new series. Why is that
   invalidation *the* high-churn failure mode, and what does it share
   with topic 8's plan-cache invalidation?
4. IOx: a query for the last 5 minutes must see WAL-buffered data not yet
   in Parquet. Trace which component serves it (`QueryableBuffer`, `:41`)
   and what the consistency story is between buffer and files during a
   snapshot.
5. Parquet delta + dictionary + zstd vs Gorilla on a gauge: predict the
   ratio gap, then reconcile with the fact that IOx sorts by (series,
   time) before writing (`sort_dedupe_persist`, `:567`) — how much of
   Gorilla's win was really *sorting*?
6. M30 mapping: FalkorDB's history could be custom chunks (VM-style) or
   Parquet-on-object-store (IOx-style, M28 already built the substrate).
   Which do you pick for `MATCH ... AT TIME t` and why does the answer
   differ for hot recent history vs year-old history?

## Done when

Answer each before unfolding it.

- [ ] You can say what both systems agree on with Prometheus, before naming what they reject.

  <details><summary>Answer</summary>

  All three are the same LSM: an in-memory head (VM rawRows shards,
  `partition.go:46`; IOx `QueryableBuffer`, `queryable_buffer.rs:41`), a
  write-ahead log for durability-first ingest, immutable time-partitioned
  files (VM parts in monthly partitions; IOx Parquet), background compaction
  merging those files, and retention implemented as dropping the oldest
  partition (`table.go:131`). They also agree on quarantining disorder off
  the hot path. What they reject is the *storage engine*: VM rejects
  prometheus's chunk/index formats for tighter custom ones; IOx rejects
  having a custom engine at all, substituting Parquet + Arrow + DataFusion.
  The essential TSDB is the pipeline; the codec and file format are
  swappable.

  </details>

- [ ] You can explain VM's codec ladder: integers first, then lossy *on purpose* — and say what "on purpose" means for a monitoring workload.

  <details><summary>Answer</summary>

  Stage one: `AppendFloatToDecimal` (`decimal.go:173`) scales each block's
  f64s to int64 with a shared decimal exponent (12.34 → 1234, e=−2), so
  arithmetic prediction applies. Stage two: `marshalInt64NearestDelta2`
  (`nearest_delta2.go:15`) stores the delta-of-delta (`d2 := next - v - d1`,
  `:32`) as zigzag varints. `precisionBits < 64` takes the `nearestDelta`
  path (`:41`) and *drops dod bits below the noise floor*. "On purpose"
  means monitoring data is already noisy at the bottom of the mantissa — a
  CPU-percentage's 12th significant digit is meaningless jitter — so
  discarding those bits costs nothing a dashboard can see while buying real
  ratio. Gorilla is bit-exact and cannot make that trade; VM offers it as a
  knob.

  </details>

- [ ] You can explain why VM caches the query rather than only the postings, and what that assumes about query shape.

  <details><summary>Answer</summary>

  `tagFiltersToMetricIDsCache` (`index_db.go:125`) is keyed by the whole
  selector and stores the resolved series-id set, in front of the postings
  index. It assumes queries *repeat their shape* — the same dashboard panel
  re-running `job="api", status="500"` every 15 s — so the expensive
  postings intersection at 100M+ series is paid once and reused. The
  assumption breaks under churn: any new series registration can change what
  a selector matches, so the cache must be invalidated, and a workload that
  mints series constantly (rolling deploys, per-request labels) keeps
  invalidating it — the same self-defeat as a plan cache under constant
  schema/stat change (topic 8). It is a bet that read shape is stable and
  write shape is not.

  </details>

- [ ] You can state IOx's bet — delete the engine, keep the pipeline (Parquet plus DataFusion) — and what it inherits for free from topic 12.

  <details><summary>Answer</summary>

  IOx keeps the TSDB *pipeline* (WAL → Arrow head → immutable
  time-partitioned files → compaction → catalog pruning) but implements
  every box with commodity parts: Parquet files on object storage, Arrow
  record batches in the `QueryableBuffer` (`queryable_buffer.rs:41`),
  DataFusion for SQL. From topic 12 (columnar/vectorized execution) it
  inherits for free the things it would otherwise have to build: per-column
  encodings (delta, dictionary, RLE, zstd), min/max column statistics for
  predicate pushdown, and vectorized scan — all standard in Parquet/Arrow.
  The payoff is that the format *is* the API: any Parquet reader queries the
  history directly. The cost is giving up a codec tuned to exactly this
  workload.

  </details>

- [ ] You can explain IOx's sort-at-snapshot approach to out-of-order data and contrast it with a head block that must accept writes in place.

  <details><summary>Answer</summary>

  The Arrow buffer accepts writes in any order; disorder is resolved at
  snapshot by `sort_dedupe_persist` (`queryable_buffer.rs:567`), which runs
  iox_query's COMPACT to dedupe and sort by the table sort key (series,
  time) before the Parquet file is written (`:600`), so files come out
  clean. Late data after a snapshot simply lands in a *new* overlapping file
  that the query layer merges and compaction later folds away. Contrast the
  Gorilla head (reading-prometheus-tsdb.md): its chunk encoder cannot rewind,
  so it must reject or quarantine an out-of-order sample *at append time* —
  it cannot buffer-then-sort because the bitstream is already committed. IOx
  can defer because Arrow batches are mutable until they are frozen to
  Parquet.

  </details>

- [ ] You can fill in the side-by-side bet table from memory, and say which bet you would make for the capstone.

  <details><summary>Answer</summary>

  The table: codec (custom/lossy vs Parquet-standard), storage (managed
  local disk vs object store), query (PromQL engine vs DataFusion SQL),
  ecosystem (own format vs anything-reads-Parquet), bet (vertical
  integration on cost vs commodity leverage). For M30 the defensible split is
  IOx-style for cold, year-old history — it already sits on M28's Parquet +
  object-store substrate, and old data is read rarely and scanned in ranges,
  exactly Parquet's strength — and VM-style tight custom chunks only for hot
  recent history where per-point decode latency and allocation dominate. The
  answer differs by age because the cost that matters flips from ingest/query
  latency (hot) to storage bytes and ecosystem reach (cold).

  </details>

- [ ] You have a predicted bytes-per-sample for VM's codec set against this topic's measured 11.00 B/sample baseline.

  <details><summary>Answer</summary>

  VM's `nearest_delta2` stores the same delta-of-delta as Gorilla but as a
  zigzag varint, so on a steady counter or slow gauge the per-sample dod is
  small and encodes in ~1 byte, with the timestamp column doing the same —
  landing well under the 11.00 B/sample baseline (our `baseline.rs`,
  [measured](../../FINDINGS.md), topic 30), in the same low-single-digit
  range as Gorilla's ~1.37 B/point once `precisionBits` trims float noise.
  The honest caveat: the varint is byte-aligned, so it cannot reach
  Gorilla's sub-bit constant-series figure (2 bits/sample) — VM trades a
  little peak ratio for batchable, SIMD-friendly decode. On full-entropy
  random values both codecs are stuck near or above raw, because a lossless
  dod of noise is still noise; that is the constant the baseline exposes.

  </details>

## Takeaway

Two systems, one lesson: the TSDB *pipeline* — WAL, in-memory head, immutable
time-partitioned files, compaction, retention-by-partition-drop — is
essential and identical everywhere; the *storage engine* (codec + file
format) is a swappable component. VM swaps in a tighter, optionally-lossy
custom codec and a query-shaped index cache; IOx swaps in Parquet + Arrow +
DataFusion and gets the whole columnar ecosystem for free. For the capstone
(M30) the choice is not either/or but age-tiered (Q6): commodity Parquet for
cold history on M28's substrate, custom chunks only where hot-path latency
pays for them.

## References

**Papers**
- None — both systems are documented in code and blog posts rather than
  papers; the IOx design discussions on the InfluxData blog are the
  closest thing to a paper for the Parquet bet.

**Code**
- [VictoriaMetrics](https://github.com/VictoriaMetrics/VictoriaMetrics)
  (Go, pinned `c1e39b2`) — `lib/storage/partition.go`,
  `lib/encoding/nearest_delta2.go`, `lib/decimal/decimal.go`,
  `lib/storage/index_db.go`, `lib/storage/dedup.go`.
- [influxdb](https://github.com/influxdata/influxdb) (Rust, pinned
  `d783411` — the repo is InfluxDB 3, the productized IOx) —
  `influxdb3_wal/src/lib.rs`,
  `influxdb3_write/src/write_buffer/queryable_buffer.rs`.
