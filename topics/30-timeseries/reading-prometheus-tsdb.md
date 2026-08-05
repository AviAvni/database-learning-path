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

Every code anchor below is prometheus at `f282b5c` (the commit this repo
pins in `resources/codebases.md`), quoted with the line numbers it occupies
at that commit. Where a figure is a *default* rather than a hard constant,
this chapter says so — several of prometheus's famous numbers are tunable.

## The problem in one sentence

Ingest ~1M tiny samples per second across millions of series — 99.9% of
them arriving in time order — while answering "this label selector, over
this time range, aggregated" in milliseconds, and deleting old data
without ever rewriting anything.

## The concepts, step by step

### Step 1 — the workload dictates the design

> **In:** nothing yet — this step fixes the metrics workload (append-mostly,
> time-ordered, range-and-selector reads) that every later choice is a
> response to.
> **Out:** the four regularities Steps 2–6 each cash in — time-ordering,
> per-series arrival, range reads, label selection.

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

> **In:** the four workload regularities from Step 1.
> **Out:** the head/WAL/block/compaction skeleton whose four organs Steps
> 3–6 open up one at a time — the map for the rest of the chapter.

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

> **In:** the LSM skeleton from Step 2, zoomed to the in-memory head.
> **Out:** the in-order fast path — one comparison plus a bitstream append —
> that Step 4's disorder policy has to protect.

The **head** holds the last ~3 hours. Series are keyed by a hash of the
label set; each **memSeries** owns its chain of Gorilla chunks, cutting a
new chunk at a **default** of 120 samples (`DefaultSamplesPerChunk`,
`tsdb/head.go:236` — a default, not a hard cap; see reading-gorilla.md for
the time-prediction cut and the 240 hard ceiling). Before any sample lands
in a chunk, it's appended to the **WAL** (write-ahead log — batched
(series, samples) records; crash recovery replays it into the head). The
in-order fast path is one comparison and a bitstream append — that's the
whole cost of the 99.9% case, and it's what per-series 10-second arrival
buys: each series' encoder state (`t_prev, delta_prev, v_prev`) is hot and
private.

### Step 4 — out-of-order: bounded, opt-in, quarantined

> **In:** the in-order fast path from Step 3, which the Gorilla encoder
> physically cannot rewind.
> **Out:** the three-way decision — accept in-order, quarantine in-window,
> refuse too-old — that `appendable()` makes on every sample, and the
> watermark that lets blocks stay immutable.

The Gorilla encoder physically cannot accept a timestamp older than its
last (the dod state machine only moves forward), so disorder needs a
policy. Prometheus's is a **bounded OOO window** (opt-in via
`OutOfOrderTimeWindow`, `tsdb/head.go:168`): a sample older than
`headMaxt − window` is refused outright (`ErrTooOldSample`); one inside the
window goes into *separate* OOO chunks, merged with the main chunks at query
and compaction time. Disorder is quarantined so the in-order path never pays
for it. The condensed contract our `head.rs` stub implements:

```rust
// ILLUSTRATION — head.rs stub; the shipped logic is
// prometheus tsdb/head_append.go:654 (memSeries.appendable).
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
    s.ooo_chunks.insert(t, v)                 // in-window: QUARANTINED
}
```

The shipped decision ladder is `memSeries.appendable`. The load-bearing
lines are 662 (in-order: `t > msMaxt`), 682 (in-window OOO), and 688
(too-old → `ErrTooOldSample`):

```go
// prometheus tsdb/head_append.go — memSeries.appendable, the OOO ladder, 654-693
   654  func (s *memSeries) appendable(t int64, v float64, headMaxt, minValidTime, oooTimeWindow int64) (isOOO bool, oooDelta int64, err error) {
   655  	// Check if we can append in the in-order chunk.
   656  	if t >= minValidTime {
   // ... 657-660: freshly created series with no samples -> accept ...
   661  		msMaxt := s.maxTime()
   662  		if t > msMaxt {
   663  			return false, 0, nil          // in-order fast path: the 99.9%
   664  		}
   // ... 665-678: t == msMaxt is an exact duplicate; tolerated, not errored ...
   679  	}
   681  	// The sample cannot go in the in-order chunk. Check the out-of-order chunk.
   682  	if oooTimeWindow > 0 && t >= headMaxt-oooTimeWindow {
   683  		return true, headMaxt - t, nil    // in-window: QUARANTINE in OOO chunks
   684  	}
   686  	// The sample cannot go in both in-order and out-of-order chunk.
   687  	if oooTimeWindow > 0 {
   688  		return true, headMaxt - t, storage.ErrTooOldSample   // beyond watermark
   689  	}
   // ... 690-693: OOO disabled -> ErrOutOfBounds / ErrOutOfOrderSample ...
   694  }
```

The window is a **watermark** (topic 27's bounded-disorder-then-seal
move): a promise that data older than `headMaxt − window` is final, which is
what lets blocks be immutable — once sealed, nothing may append into their
range.

### Step 5 — the label index: an inverted index over labels

> **In:** the head full of per-series chunks from Step 3, plus the need to
> find series by label without scanning all of them.
> **Out:** MemPostings — a label→sorted-series-id map whose selector
> evaluation is a k-way sorted-list intersection — and the cardinality
> failure mode built into the data model.

Selector queries (`job="api", status="500"`) need "which series carry
this label?" without scanning all series. **MemPostings** is an
**inverted index** (topic 23: term → sorted list of document ids, here
label pair → sorted list of series ids). The load-bearing field is the
doubly-nested map at `tsdb/index/postings.go:70`:

```go
// prometheus tsdb/index/postings.go — MemPostings, the index itself, 60-70
   60  type MemPostings struct {
   61  	mtx sync.RWMutex
   62
   // ... 63-69: doc comment; note the known addFor data race, issue #15317 ...
   70  	m map[string]map[string][]storage.SeriesRef
```

Evaluating a selector = intersecting the sorted id lists, one per label
pair — k-way merge over sorted integers, our `index.rs` stub. `Add`
(`postings.go:403`) inserts a new series under every one of its label pairs
*and* under a special all-postings key (`postings.go:409`, `addFor(id,
allPostingsKey)`) — the key that answers match-everything selectors without
expanding every value (Q3). Every block carries a frozen copy of the same
structure in its `index` file. The failure mode is built into the data
model: **every unique label set is a new series** — a new memSeries, new
postings entries, new index rows in every block. A `user_id` label turns 1
metric into 10M series (the cardinality bomb our `cardinality_bomb_is_visible`
test counts), and **churn** (rolling deployments replacing `pod` values)
inflates cardinality over *time* even when instantaneous cardinality is fine
— old series linger in the head and index until truncation.

### Step 6 — blocks, compaction, retention: the lifecycle

> **In:** a head that has accumulated ~3 hours of chunks and postings.
> **Out:** the cut→compact→retire ladder that turns memory into immutable
> time-partitioned blocks and makes deletion an `rm -r`.

Every 2 hours (`DefaultBlockDuration`, `tsdb/db.go:56` —
`int64(2 * time.Hour / time.Millisecond)`) the head is **cut**: its oldest
span is written out as an immutable **block** — a directory with `chunks/`
(the data), `index` (postings + series), and `meta.json` (min/max time, the
pruning metadata) — and the WAL is truncated behind it (`truncateWAL`,
`tsdb/head.go:1485`; topic 5's checkpoint, verbatim). Compaction merges
adjacent blocks into exponentially larger time ranges via
`ExponentialBlockRanges` (`tsdb/compact.go:41`; 2h → 6h → 18h…) — topic 4's
size-tiered compaction with time units instead of bytes — which caps
per-query block counts and merges the OOO chunks in. Retention is the payoff
of time partitioning: deleting old data = `rm -r` the oldest block
directory. No tombstones, no rewrite, no vacuum — the delete costs one
directory unlink because the partition key *is* the age.

## Where each step lives in the code

prometheus `tsdb/` at `f282b5c`:

| Anchor | What | Step | Constant or default |
|---|---|---|---|
| `head.go:71` | `type Head struct` — the memtable | 2, 3 | — |
| `head.go:236` | `DefaultSamplesPerChunk = 120` — chunk cut target | 3 | **default** |
| `head.go:168` | `OutOfOrderTimeWindow atomic.Int64` — OOO is opt-in | 4 | **default** (0 = off) |
| `head_append.go:436` | `headAppender.Append` — the hot path entry | 3, 4 | — |
| `head_append.go:654-693` | `memSeries.appendable` — the OOO decision ladder | 4 | — |
| `head_append.go:682`, `:688` | in-window OOO vs `ErrTooOldSample` | 4 | — |
| `index/postings.go:60`, `:70` | `MemPostings`, the nested label map | 5 | — |
| `index/postings.go:403`, `:409` | `Add`, and the `allPostingsKey` insert | 5 | — |
| `db.go:56` | `DefaultBlockDuration = 2h` — the block boundary | 6 | **default** |
| `compact.go:41` | `ExponentialBlockRanges` — 2h→6h→18h merge | 6 | — |
| `head_wal.go:80` | `loadWAL` — crash recovery replay | 3, 6 | — |
| `head.go:1485` | `truncateWAL` — checkpoint behind a cut block | 6 | — |

Note the earlier draft's `wal.go` does not exist: the WAL implementation is
`tsdb/wlog/wlog.go`, replayed by `head_wal.go:80` and truncated by
`head.go:1485`.

## Questions to answer while reading

1. Why can prometheus get away with one WAL for all series (no per-series
   ordering issue), while the chunks must be strictly per-series?
2. The head holds ~3h but blocks are 2h. Walk through why the overlap
   exists (what happens to samples arriving during a block cut?).
3. MemPostings intersects *sorted* id lists. Prometheus also keeps a
   special all-postings key (`postings.go:409`). Derive when `job=~".+"`
   (match-everything) is served by that key vs when a regex forces
   value-by-value expansion — and what that costs at 10M series.
4. OOO chunks are merged at *read* time before compaction folds them in.
   What does a query over the OOO window pay, and why is that acceptable?
   (Compare our `flush`-time merge — we pay at flush instead.)
5. Retention deletes whole blocks. What query-visible anomaly can that
   create near the retention boundary, and why is it tolerated?
6. M30 mapping: FalkorDB property history needs per-entity chunks like
   memSeries. What is the analogue of the label index — and does graph
   topology (adjacency) belong in the "labels" (indexed dimensions) or in
   the "values" (payload)?

## Done when

Answer each before unfolding it.

- [ ] You can explain the head block against persistent blocks and what compaction does between them.

  <details><summary>Answer</summary>

  The **head** is the in-memory memtable (`head.go:71`) holding ~3 hours:
  one `memSeries` per series, each owning a chain of Gorilla chunks, plus
  the WAL and MemPostings. Every `DefaultBlockDuration` = 2h (`db.go:56`)
  the oldest span is **cut** into an immutable on-disk **block** — a
  directory of `chunks/`, an `index`, and `meta.json`. Blocks never change
  after they are written. **Compaction** (`ExponentialBlockRanges`,
  `compact.go:41`) then merges *adjacent* blocks into exponentially larger
  time ranges (2h→6h→18h…), which folds OOO chunks in, deduplicates, and
  caps how many blocks a range query must open. It is topic 4's size-tiered
  compaction with time as the unit instead of bytes.

  </details>

- [ ] You can explain why series churn is the scaling problem rather than sample volume.

  <details><summary>Answer</summary>

  Samples are cheap: an in-order sample is one comparison
  (`appendable`, `head_append.go:662`) plus a bitstream append into an
  already-hot per-series encoder. Series are expensive: **every unique label
  set is a new series** — a new `memSeries`, new entries under each of its
  label pairs in `MemPostings` (`postings.go:70`), and new rows in every
  block's index. High instantaneous cardinality (a `user_id` label → 10M
  series) blows up memory and index size; **churn** — rolling deploys that
  keep minting new `pod` values — inflates cardinality *over time* even when
  the live count is modest, because retired series linger in the head and in
  sealed blocks until truncation and retention age them out. Doubling the
  scrape rate doubles samples; adding one high-cardinality label multiplies
  series, and series are what the index is sized by.

  </details>

- [ ] You can explain the inverted index over labels and why a selector is a postings intersection (topic 23, again).

  <details><summary>Answer</summary>

  `MemPostings.m` is `map[labelName]map[labelValue][]seriesRef` — for each
  label pair, the *sorted* list of series ids that carry it
  (`postings.go:70`), an inverted index exactly like topic 23's term →
  doclist. A selector `job="api", status="500"` fetches the two sorted id
  lists and **intersects** them; more label matchers means a k-way merge
  over sorted integer lists, which is linear in the list sizes and needs no
  per-series scan. `job=~".+"` is served by the `allPostingsKey`
  (`postings.go:409`) when it means "everything"; a narrower regex has to
  union the lists of each matching value instead, which is where a
  high-cardinality label gets expensive.

  </details>

- [ ] You can say what out-of-order samples cost and why the head block's design decides it.

  <details><summary>Answer</summary>

  A Gorilla chunk's dod/XOR state machine only moves forward, so an
  out-of-order sample can never be appended to the live chunk. Prometheus
  makes disorder opt-in and bounded: `appendable` (`head_append.go:654`)
  accepts `t > msMaxt` in-order for free (:662), routes a sample within
  `headMaxt − OutOfOrderTimeWindow` into *separate* OOO chunks (:682), and
  refuses anything older with `ErrTooOldSample` (:688). The in-window
  samples are quarantined and only merged at query and compaction time, so
  the 99.9% in-order path pays nothing for the exceptions; a query touching
  the OOO window pays a merge of the OOO chunks against the in-order ones.
  The window is a watermark — declaring data older than it final — which is
  the precondition for sealed blocks being immutable.

  </details>

## Takeaway

Prometheus is topic 4's LSM with time as the sort key: head = memtable, WAL =
WAL, 2h blocks = time-partitioned SSTs, `ExponentialBlockRanges` =
size-tiered compaction, retention = `rm -r`. The two things the metrics
costume adds are the per-series Gorilla chunk (reading-gorilla.md) and the
label inverted index whose cardinality is the real scaling axis. For the
capstone (M30), the memSeries → per-entity-chunk and MemPostings →
property-index mappings are the whole port; the open question (Q6) is whether
graph adjacency is an indexed dimension or a payload.

## References

**Papers**
- Fabian Reinartz — "Writing a Time Series Database from Scratch"
  (design doc / blog, 2017) — the rationale behind every structure in
  the code walk; read it first if the layout feels arbitrary.

**Code**
- [prometheus](https://github.com/prometheus/prometheus) `tsdb/` (pinned at
  `f282b5c`) — start at `head.go`, `head_append.go` (`appendable` at
  `:654`), `index/postings.go`, `compact.go`; the WAL is `wlog/wlog.go`,
  replayed by `head_wal.go`. The whole engine is an afternoon of Go.
