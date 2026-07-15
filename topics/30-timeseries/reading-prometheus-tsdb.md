# Prometheus TSDB: an LSM with time as the key

Every TSDB concept — head, WAL, immutable time blocks, label index,
bounded out-of-order — exists in `prometheus/tsdb/` in a form you can
read in an afternoon, which makes it the best single codebase for this
topic. Its design-doc lineage (Fabian Reinartz's "Writing a Time Series
Database from Scratch") explains every choice. This chapter builds the
engine step by step — the workload shape, the LSM skeleton, the head,
the out-of-order quarantine, the label index, and the compaction/retention
ladder — then hands you the Go anchors. Read it as topic 4's LSM wearing
a metrics costume, and as the reference for our `head.rs` and `index.rs`
stubs.

## The problem in one sentence

Ingest ~1M tiny samples per second across millions of series — 99.9% of
them arriving in time order — while answering "this label selector, over
this time range, aggregated" in milliseconds, and deleting old data
without ever rewriting anything.

## The concepts, step by step

### Step 1 — the workload dictates the design

A **series** is one metric identified by its **label set** (key=value
pairs: `http_requests{job="api", pod="api-7"}`); a **sample** is one
(timestamp, value) point in a series. The workload's regularities
(topic 30 README §0) are extreme: writes are append-mostly and
time-ordered, each series receives a sample every ~10 s, and every read
is a *time range* × a *label selector*, usually aggregated. Everything
below is one of those regularities cashed in: time-ordering → in-order
fast path, per-series arrival → per-series compressed chunks
(reading-gorilla.md), range reads → time-partitioned files, label
selection → an inverted index over labels.

### Step 2 — the skeleton is an LSM with time as the key

An LSM (topic 4) buffers recent writes in memory, logs them for crash
safety, flushes immutable sorted files, and merges those files in the
background. Prometheus is exactly that machine with time as the sort key:

```
   scrape ──► Head (in-memory, ~3h)          disk
              ┌──────────────────────┐       ┌─────────────────────────┐
              │ memSeries per series │  cut  │ 2h Block (immutable)    │
              │  └ Gorilla chunks    │ ────► │  ├ chunks/  (the data)  │
              │ WAL (crash recovery) │       │  ├ index    (postings + │
              │ MemPostings          │       │  │           series)    │
              │ OOO buffer (window)  │       │  └ meta.json min/max t  │
              └──────────────────────┘       └─────────────────────────┘
                                             compaction: 2h -> 6h -> 18h…
                                             retention: DELETE = rm -r block
```

head = memtable, WAL = WAL, blocks = SSTs sorted/partitioned by time,
compaction merges adjacent time ranges — and retention is dropping the
oldest "level", the cheapest delete in databases (Step 6). Because the
key is time and time only moves forward, new data never overlaps old
blocks (almost — Step 4 handles the exceptions).

### Step 3 — the head: one memSeries per series, chunks within

The **head** holds the last ~3 hours. Series are keyed by a hash of the
label set; each **memSeries** owns its chain of Gorilla chunks, cutting a
new chunk every ~120 samples (see reading-gorilla.md for why). Before any
sample lands in a chunk, it's appended to the **WAL** (write-ahead log —
batched (series, samples) records; crash recovery replays it into the
head). The in-order fast path is one comparison and a bitstream append —
that's the whole cost of the 99.9% case, and it's what per-series
10-second arrival buys: each series' encoder state (`t_prev, delta_prev,
v_prev`) is hot and private.

### Step 4 — out-of-order: bounded, opt-in, quarantined

The Gorilla encoder physically cannot accept a timestamp older than its
last (the dod state machine only moves forward), so disorder needs a
policy. Prometheus's is a **bounded OOO window** (opt-in via
`OutOfOrderTimeWindow`): a sample older than `max_time − window` is
refused outright (`ErrTooOldSample`); one inside the window goes into
*separate* OOO chunks, merged with the main chunks at query and
compaction time. Disorder is quarantined so the in-order path never pays
for it. The condensed contract — exactly what `head.rs` implements:

```rust
fn append(&mut self, series: SeriesId, t: i64, v: f64) -> Result<()> {
    let s = self.series.get_mut(series);
    if t >= s.max_time() {
        self.wal.log(series, t, v);           // durability first
        return s.open_chunk().push(t, v);     // in-order fast path: the 99.9%
    }
    if t < s.max_time() - self.ooo_window {
        return Err(TooOldSample);             // beyond the watermark: refused
    }
    self.wal.log(series, t, v);
    s.ooo_chunks.insert(t, v)                 // disorder is QUARANTINED — merged
}                                             // at query/compaction time, so the
                                              // in-order path never pays for it
```

The window is a **watermark** (topic 27's bounded-disorder-then-seal
move): a promise that data older than X is final, which is what lets
blocks be immutable.

### Step 5 — the label index: an inverted index over labels

Selector queries (`job="api", status="500"`) need "which series carry
this label?" without scanning all series. **MemPostings** is an
**inverted index** (topic 23: term → sorted list of document ids, here
label pair → sorted list of series ids):
`map[label name]map[value][]seriesID`. Evaluating a selector =
intersecting the sorted id lists, one per label pair — k-way merge over
sorted integers, our `index.rs` stub. Every block carries a frozen copy
of the same structure in its `index` file. The failure mode is built into
the data model: **every unique label set is a new series** — a new
memSeries, new postings entries, new index rows in every block. A
`user_id` label turns 1 metric into 10M series (the cardinality bomb our
`cardinality_bomb_is_visible` test counts), and **churn** (rolling
deployments replacing `pod` values) inflates cardinality over *time*
even when instantaneous cardinality is fine — old series linger in the
head and index until truncation.

### Step 6 — blocks, compaction, retention: the lifecycle

Every 2 hours the head is **cut**: its oldest span is written out as an
immutable **block** — a directory with `chunks/` (the data), `index`
(postings + series), and `meta.json` (min/max time, the pruning
metadata) — and the WAL is truncated behind it (topic 5's checkpoint,
verbatim). Compaction merges adjacent blocks into exponentially larger
time ranges (2h → 6h → 18h…) — topic 4's size-tiered compaction with
time units instead of bytes — which caps per-query block counts and
merges the OOO chunks in. Retention is the payoff of time partitioning:
deleting old data = `rm -r` the oldest block directory. No tombstones, no
rewrite, no vacuum — the delete costs one directory unlink because the
partition key *is* the age.

## Where each step lives in the code

1. `head.go:71` — `type Head`: the memtable (Steps 2–3). Series are keyed
   by a hash of the label set; each `memSeries` owns its chunk chain.
   Chunks cut at ~120 samples (see reading-gorilla.md).
2. `head_append.go:436` — `Append`: the hot path (Steps 3–4). In-order →
   straight into the series' open chunk. `:481` returns
   `storage.ErrOutOfOrderSample`; the decision ladder at `:688-693`
   distinguishes `ErrTooOldSample` (outside the OOO window — refused)
   from in-window OOO. This is exactly your `head.rs` contract.
3. `head.go:168` — `OutOfOrderTimeWindow`: OOO support is *opt-in and
   bounded* (Step 4). `ooo_head.go` keeps OOO samples in separate chunks
   merged at query/compaction time — disorder is quarantined so the
   in-order path never pays for it.
4. `index/postings.go:60` — `MemPostings`:
   `map[label name]map[value][]seriesID`, sorted ids (Step 5). `Add`
   (`:403`) appends under lock. Selector evaluation = sorted-list
   intersection — your `index.rs`, and topic 23's inverted index with
   labels as terms.
5. `db.go:56` — `DefaultBlockDuration = 2h`, and `compact.go:41`
   `ExponentialBlockRanges`: blocks merge into exponentially larger time
   ranges (Step 6). Compare topic 4's size-tiered compaction — same math,
   time units instead of bytes.
6. `wal.go` / `head_wal.go` — WAL records are (series, samples) batches;
   crash recovery replays into the head (Step 3). Checkpointing truncates
   the WAL once a block is cut — topic 5's story verbatim (Step 6).

## Questions to answer while reading

1. Why can prometheus get away with one WAL for all series (no per-series
   ordering issue), while the chunks must be strictly per-series?
2. The head holds ~3h but blocks are 2h. Walk through why the overlap
   exists (what happens to samples arriving during a block cut?).
3. MemPostings intersects *sorted* id lists. Prometheus also keeps a
   special all-postings key. Derive when `job=~".+"` (match-everything)
   is served by that key vs when a regex forces value-by-value expansion —
   and what that costs at 10M series.
4. OOO chunks are merged at *read* time before compaction folds them in.
   What does a query over the OOO window pay, and why is that acceptable?
   (Compare our `flush`-time merge — we pay at flush instead.)
5. Retention deletes whole blocks. What query-visible anomaly can that
   create near the retention boundary, and why is it tolerated?
6. M30 mapping: FalkorDB property history needs per-entity chunks like
   memSeries. What is the analogue of the label index — and does graph
   topology (adjacency) belong in the "labels" (indexed dimensions) or in
   the "values" (payload)?

## References

**Papers**
- Fabian Reinartz — "Writing a Time Series Database from Scratch"
  (design doc / blog, 2017) — the rationale behind every structure in
  the code walk; read it first if the layout feels arbitrary

**Code**
- [prometheus](https://github.com/prometheus/prometheus) `tsdb/` —
  start at `head.go`, `head_append.go`, `index/postings.go`,
  `compact.go`; the whole engine is an afternoon of Go
