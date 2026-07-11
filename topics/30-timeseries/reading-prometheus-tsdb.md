# Prometheus TSDB: an LSM with time as the key

Every TSDB concept — head, WAL, immutable time blocks, label index,
bounded out-of-order — exists in `prometheus/tsdb/` in a form you can
read in an afternoon, which makes it the best single codebase for this
topic. Its design-doc lineage (Fabian Reinartz's "Writing a Time Series
Database from Scratch") explains every choice. Read it as topic 4's LSM
wearing a metrics costume, and as the reference for our `head.rs` and
`index.rs` stubs.

## The architecture

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

It's topic 4's LSM with time as the key: head = memtable, WAL = WAL,
blocks = SSTs sorted/partitioned by time, compaction merges adjacent time
ranges, and retention is dropping the oldest "level" — the cheapest
delete in databases.

## Code walk

1. `head.go:71` — `type Head`: the memtable. Series are keyed by a hash
   of the label set; each `memSeries` owns its chunk chain. Chunks cut at
   ~120 samples (see reading-gorilla.md).
2. `head_append.go:436` — `Append`: the hot path. In-order → straight
   into the series' open chunk. `:481` returns
   `storage.ErrOutOfOrderSample`; the decision ladder at `:688-693`
   distinguishes `ErrTooOldSample` (outside the OOO window — refused)
   from in-window OOO. This is exactly your `head.rs` contract.
3. `head.go:168` — `OutOfOrderTimeWindow`: OOO support is *opt-in and
   bounded*. `ooo_head.go` keeps OOO samples in separate chunks merged at
   query/compaction time — disorder is quarantined so the in-order path
   never pays for it.
4. `index/postings.go:60` — `MemPostings`: `map[label name]map[value][]seriesID`,
   sorted ids. `Add` (`:403`) appends under lock. Selector evaluation =
   sorted-list intersection — your `index.rs`, and topic 23's inverted
   index with labels as terms.
5. `db.go:56` — `DefaultBlockDuration = 2h`, and `compact.go:41`
   `ExponentialBlockRanges`: blocks merge into exponentially larger time
   ranges. Compare topic 4's size-tiered compaction — same math, time
   units instead of bytes.
6. `wal.go` / `head_wal.go` — WAL records are (series, samples) batches;
   crash recovery replays into the head. Checkpointing truncates the WAL
   once a block is cut — topic 5's story verbatim.

The ingestion contract, condensed from `head_append.go`'s decision
ladder (this is exactly what `head.rs` implements):

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

## Where it hurts (the famous failure modes)

- **High cardinality**: every unique label set is a new series — a new
  memSeries, new postings entries, new index rows in every block. A
  `user_id` label turns 1 metric into 10M series. Your
  `cardinality_bomb_is_visible` test counts this directly.
- **Churn**: rolling deployments replace `pod` label values; old series
  linger in the head + index until truncation. Cardinality over *time*
  hurts even when instantaneous cardinality is fine.

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
