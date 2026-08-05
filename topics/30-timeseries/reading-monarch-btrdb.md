# Monarch & BtrDB: the extremes that bracket the middle

Two design points far outside the Gorilla/Prometheus mainstream, read
for what they prove is possible: Monarch shows what monitoring looks
like when it must not depend on anything it monitors (planet-scale,
memory-first, push-based), and BtrDB shows what happens when the *index
is the downsampler* (query cost proportional to pixels, not samples).
This chapter builds each system's defining move step by step — the
circularity constraint, push ingestion, typed schemas, query pushdown,
the aggregate tree, and CoW versioning — then routes you through both
papers. Paper-only chapter — there are no repo clones here.

The two sources are Adams et al., **"Monarch: Google's Planet-Scale
In-Memory Time Series Database," VLDB 2020**, and Andersen & Culler,
**"BTrDB: Optimizing Storage System Design for Timeseries Processing,"
FAST 2016**. Every number below is attributed to a section, table or
figure of one of them; the two papers describe different systems, so each
claim also names which. Because there is no code here, the one pseudocode
block is marked as illustration, not quoted.

## The problem in one sentence

The mainstream TSDB design quietly assumes a durable storage layer
underneath and queries that scan every sample they aggregate — Monarch
can't have the first (it monitors the storage layers it would depend on)
and BtrDB can't afford the second (at 120 Hz per stream accumulating into
tens of billions of readings, "plot 3 years" touching every sample is dead
on arrival).

## The concepts, step by step

### Step 1 — Monarch's constraint: you can't depend on what you monitor

> **In:** the mainstream assumption that a TSDB sits on durable storage
> (Gorilla on HBase, prometheus on local disk).
> **Out:** Monarch's forced inversion — memory-first, durability traded down
> — and the availability-over-completeness trade Steps 2–3 elaborate.

Monarch monitors Google — including Bigtable, Colossus, and Spanner, the
storage systems a durable TSDB would naturally be built on. That's a
**circular dependency**: a monitoring system that's down when Bigtable is
down is worthless precisely when it's needed most. Monarch names this
explicitly — it "stores data in memory and avoids hard dependencies"
(§4.1), and keeps the persistent store off the alerting path "to avoid a
potentially dangerous circular [dependency]" (§2). The forced choice:
**memory first, durability traded down** — data lives in RAM at the leaves,
is logged to disk lazily, and queries never wait for the log (§4.1: "leaves
do not wait for acknowledgement when writing to the recovery logs").
Alerting availability is bought with the loss of guaranteed history (a leaf
crash can drop recent samples; §2: the system "drops delayed writes and
returns partial data for queries"), a trade no bank would take and every
monitoring team should (Q1 asks what queries die).

### Step 2 — autonomy by geography: zones that survive partitions

> **In:** the memory-first, must-stay-up mandate from Step 1.
> **Out:** the autonomous-zone topology and its deliberately weak
> cross-zone consistency — the substrate push ingestion (Step 3) and
> pushdown (Step 5) run over.

The same must-stay-up reasoning shapes the topology: ingestion and
alerting must keep working *inside* a network partition, so Monarch is a
hierarchy of **autonomous zones** — §2: "Each Monarch zone is autonomous,"
its leaves ingesting and answering queries for locally-monitored targets
with no cross-zone dependency, and a global query layer federating over them
when the network permits:

```
              global query layer (query pushdown, hierarchical)
                 ┌────────────┬────────────┐
        zone A   │   zone B   │   zone C   │   <- autonomous per zone:
        leaves   │   leaves   │   leaves   │      ingest keeps working
        (RAM)    │   (RAM)    │   (RAM)    │      through partitions
```

Consistency is deliberately weak — zones don't coordinate on writes at
all; a global query is best-effort over whatever zones answer. For
monitoring, fresh-but-partial beats complete-but-stale.

### Step 3 — push, not pull

> **In:** the autonomous zones from Step 2, which each accept their own
> targets' data.
> **Out:** the push ingestion model (§4.1) and the regularity guarantee it
> gives up — the exact guarantee Gorilla's codec depended on.

Prometheus *pulls*: a scraper polls each target on its own schedule,
which means the scraper controls timestamp regularity — the fixed
scrape interval is exactly what makes Gorilla's delta-of-delta mostly
zero (reading-gorilla.md). Monarch is *push*: §4.1's Data Collection
Overview has "a client sends data to one of the nearby ingestion routers,"
which route to the destination zone's leaves — because at planetary scale
and across failure domains a central puller is itself a liability. The cost
is losing the regularity guarantee — a push system must cope with whatever
timestamps arrive. Every TSDB's ingestion story is downstream of this one
choice; note prometheus chose pull *for* the regularity.

### Step 4 — typed schemas: the cure for cardinality is structure

> **In:** Monarch's push-ingested series from Step 3, and prometheus's
> string-label cardinality bomb (reading-prometheus-tsdb.md).
> **Out:** typed schemas and distribution-valued series (§3) — the move that
> collapses a histogram's ~10 label-series into one, and the precondition
> for lossless pushdown in Step 5.

Prometheus models everything as string labels, and topic 30's cardinality
bomb (a `user_id` label = 10M series) is the bill. Monarch instead gives
series **typed schemas** — declared key columns with types (§3, Figure 2's
`ComputeTask` target schema) — and, more important, **distribution-typed
values**: §3 supports "a distribution (i.e., histogram) value type," so a
latency histogram is *one* series whose values are histogram objects, not
~10 separate `le=`-labelled bucket series as in prometheus. The lesson
generalizes: the cure for the label-cardinality bomb is schema, not more
index — move structure out of the series *key* and into the *value type*
(Q2 prices the query-time consequence for quantiles).

### Step 5 — query pushdown: ship aggregates, not samples

> **In:** the typed, distribution-valued, zone-partitioned series from
> Steps 2–4.
> **Out:** hierarchical pushdown (§5.3) as the query dual of the ingestion
> hierarchy — and why distribution values are what make it lossless.

A global query ("p99 latency of service X across all zones") could pull
every sample to one place — at Monarch's scale, absurd. Instead §5.3 (Query
Pushdown) has Monarch "push down evaluation of a query's table operations as
close to the source data as possible": leaves aggregate their own data,
zones combine leaf results, the global layer combines zone results — each
hop ships *partial aggregates* (counts, sums, distribution sketches), not
samples. Topic 13's move-the-computation-to-the-data, at monitoring scale.
This is also why distribution values (Step 4) matter: histograms merge
associatively, so partial aggregation is lossless for quantiles — you can
combine per-leaf histograms and still read an exact p99, which you could not
do from per-leaf pre-computed p99s.

### Step 6 — BtrDB's regime, and the aggregate tree

> **In:** a different world — dense sensor telemetry, not fleet monitoring
> — where a single query can address billions of samples.
> **Out:** the time-partitioned aggregate tree whose internal nodes are
> summaries, making query cost proportional to resolution rather than sample
> count.

BtrDB serves power-grid synchrophasors: each uPMU device produces 12 streams
of **120 Hz** data (§1, abstract), timestamps at nanosecond resolution (the
GPS limit). A server targets 1000 devices = **1.4 million inserted points/s**
(§1), and the paper demonstrates **53 million inserted / 119 million queried
values/s on a four-node cluster** (abstract, §7) — *not* 100M/s per stream;
the per-stream rate is 120 Hz. The scale problem is the accumulated total:
Figure 1a plots a year of voltage as **50 billion readings**, which a naive
"plot at screen resolution" would touch every one of. Its answer is to make
the index precompute the answers: a **time-partitioning tree** whose root
logically spans −2⁶⁰ to 3·2⁶⁰ ns (§4 — roughly 1933–2079, a width of 2⁶²
ns), conceptually a binary tree but implemented **k-ary with K=64** for fewer
IO ops (§4, "although conceptually a binary tree … a k-ary tree"), where
**each internal node stores the (min, mean, max, count) of its entire
subtree** (§3: the statistical record is `(Time, Min, Mean, Max, Count)`):

```
                     root: [−2^60, 3·2^60) ns  (width 2^62)
                    ┌ min/mean/max/count ┐            each node: K=64 children,
              child │ min/mean/max/count │ child      each holding STATISTICAL
                    └ ... 64-way fanout ─┘            SUMMARIES of its subtree
                              ...
                    leaves: the raw samples
```

A query at resolution `r` descends only until a node's time span fits
under `r`, then takes the precomputed summary — **query cost ∝ pixels,
not samples** (§3: statistical queries return records at "a given temporal
resolution," and Q3 derives the cost). The pseudocode, illustration only —
there is no BtrDB clone in this repo:

```
ILLUSTRATION — pseudocode for BtrDB §3/§4; no repo clone exists for this
paper-only chapter, so there is no file:line to anchor it to.
fn query(node, range, res_ns, out):
    for child in node.children_overlapping(range):
        if child.span_ns <= res_ns:
            out.push(child.stats)       # precomputed min/mean/max/count —
        else:                           # never touch the raw samples
            query(child, range, res_ns, out)   # one of 64 ways, O(log_64 depth)
```

Downsampling isn't a batch job (prometheus recording rules, VM
downsampling) — it's the *index structure itself*, always current. And the
summaries are nearly free in space: §4 measures internal nodes at **< 0.3%
of the total footprint** for a single-version K=64 tree, and §7's production
figure is **5.514 bytes per reading including all statistical and historical
overheads — a 2.9× compression** versus the 16-byte raw tuple. Gorilla
optimizes bytes/sample, BtrDB optimizes bytes *read per query* — different
objective, different tree, and here the tree is almost pure upside.

### Step 7 — copy-on-write versions: disorder and corrections as history

> **In:** the aggregate tree from Step 6.
> **Out:** CoW versioning as the mechanism that turns out-of-order inserts
> and corrections into cheap new versions plus a diffable changelog — and the
> per-stream cost model that keeps it from rescuing prometheus.

BtrDB's tree is **copy-on-write** (topic 3's CoW B-tree): an insert
rewrites the path from leaf to root and publishes a *new root* (§4: each
insert "forms an overlay … accessible via a new root node"), so every commit
is a retained **version**. Out-of-order data and corrections — routine in
telemetry, where a field device uploads yesterday's backlog — are just
inserts producing new versions, no quarantine window needed (§7: "BTrDB
allows data to be inserted in arbitrary order"). And because versions are
diffable, "what changed between v1000 and v1200" is computable from **just 8
bytes of state** (the last version processed, §4/§7) — the changed-ranges API
that makes downstream incremental computation (topic 27's IVM) natural,
"spanning a year of data in under 200ms" (§1). The catch that keeps this from
rescuing prometheus-shaped workloads: it's one tree *per stream*, priced for
few fat streams (1.4M points/s into ~12k streams per device fleet), not 10M
skinny ones (Q4).

## Where each step lives in the papers

Monarch (VLDB 2020):

| Section | What | Step |
|---|---|---|
| §1, §2 | memory-first, circular-dependency, "partial data" trade | 1 |
| §2 | autonomous zones ("Each Monarch zone is autonomous") | 2 |
| §4.1 Data Collection | push ingestion via ingestion routers; logs not awaited | 1, 3 |
| §3 Data Model, Figure 2 | typed key columns + distribution value type | 4 |
| §5.3 Query Pushdown | ship partial aggregates down the hierarchy | 5 |

BtrDB (FAST 2016):

| Section | What | Step |
|---|---|---|
| §1, abstract | 120 Hz/stream, 1.4M/s/server, 53M/119M cluster; year = 50B readings | 6 |
| §4 | time-partitioning tree, root −2⁶⁰..3·2⁶⁰ ns, K=64 k-ary | 6 |
| §3 | statistical record `(Time, Min, Mean, Max, Count)`; resolution queries | 6 |
| §4, §7 | internal nodes < 0.3% footprint; 5.514 B/reading, 2.9× compression | 6 |
| §4, §7 | copy-on-write versions; changed-ranges from 8 bytes of state | 7 |

Read §1 of Monarch carefully — every architectural oddity traces back to the
circularity constraint of Step 1. BtrDB is short and dense; work Q3's cost
derivation while the tree diagram is fresh, then read its changed-ranges/IVM
discussion against topic 27.

## Questions to answer while reading

1. Monarch chose RAM + lazy logs; Gorilla chose RAM + HBase behind.
   Both are "monitoring must not depend on what it monitors." What
   *queries* does Monarch give up that a durable TSDB answers (hint:
   long-range historical joins)?
2. Distribution-typed values change the cardinality equation: a latency
   histogram is ONE series in Monarch but ~10 (le buckets) in prometheus.
   What does each choice cost at query time (quantile computation)?
3. Derive BtrDB's query cost for "mean over [a,b] at 1000 points" —
   show it's O(1000 · log₆₄(range/resolution)) and independent of sample
   count.
4. BtrDB's CoW versions make OOO inserts cheap-ish. Why does the same
   trick NOT rescue prometheus-shaped workloads (hint: one tree per stream
   at 10M streams)?
5. Both papers reject the label-selector data model (Monarch: schemas;
   BtrDB: few fat streams + external metadata). Argue which parts of the
   prometheus model are essential vs incidental for infrastructure
   monitoring.
6. M30 mapping: `MATCH ... AT TIME t` needs point-in-time; but "how did
   this subgraph evolve" wants BtrDB-style multi-resolution over edge
   churn (edges-added-per-hour rollups). Sketch where an aggregate tree
   over the M27 changelog would live in FalkorDB.

## Done when

Answer each before unfolding it.

- [ ] You can state Monarch's founding constraint — you cannot depend on what you monitor — and name two design choices that fall directly out of it.

  <details><summary>Answer</summary>

  The constraint: Monarch monitors Google's storage systems (Bigtable,
  Colossus, Spanner), so building durable-first on them would be a circular
  dependency — down exactly when monitoring is most needed. Two choices that
  fall out of it: (1) **memory-first storage** — data lives in RAM at the
  leaves and queries never wait for the recovery log (§4.1, "leaves do not
  wait for acknowledgement when writing to the recovery logs"), trading
  guaranteed history for alerting availability; (2) **autonomous zones**
  (§2) that ingest and answer locally with no cross-zone write coordination,
  so a network partition can't take ingestion down. Push ingestion (§4.1) is
  a third. All of them prefer fresh-but-partial to complete-but-stale.

  </details>

- [ ] You can explain autonomy by geography: what a zone owns and what it keeps working through a partition.

  <details><summary>Answer</summary>

  A zone owns the leaves that ingest and store (in RAM) the data for its
  locally-monitored targets, plus the ability to evaluate queries and fire
  alerts over that local data — §2's "Each Monarch zone is autonomous."
  Through a partition that severs a zone from the global layer, the zone
  keeps ingesting and alerting on its own targets, because it depends on
  nothing outside itself for those paths. What it loses is participation in
  *global* queries until the network heals — a cross-zone query becomes
  best-effort over whichever zones answer, returning partial data rather than
  blocking. The design spends global completeness to buy local availability.

  </details>

- [ ] You can say why push beats pull at Monarch's scale, and what it costs.

  <details><summary>Answer</summary>

  Pull needs a central scheduler that knows and reaches every target; at
  planetary scale and across failure domains that puller is a
  single-point-of-liability and a cross-domain dependency Monarch's
  circularity constraint forbids. Push (§4.1: clients send to nearby
  ingestion routers) keeps ingestion local and lets each target drive its own
  data in, surviving partitions. The cost is the loss of the *regularity
  guarantee*: a puller imposes a fixed scrape interval, which is exactly what
  makes Gorilla's delta-of-delta mostly zero (reading-gorilla.md); a push
  system must accept whatever timestamps arrive, so it cannot assume the
  clean cadence the timestamp codec loves.

  </details>

- [ ] You can explain why typed schemas are the cure for cardinality, and connect that to the tag-index cost you will measure in `index.rs`.

  <details><summary>Answer</summary>

  Prometheus encodes every dimension as a string label, so each unique label
  set is a new series and each new series is new rows in the inverted index —
  the cardinality bomb your `index.rs` postings intersection pays for, and a
  histogram becomes ~10 `le=`-labelled series. Monarch (§3, Figure 2) instead
  declares **typed key columns** and, crucially, a **distribution value
  type**, so a latency histogram is *one* series whose value is a histogram
  object. That moves structure out of the index key and into the value type:
  fewer series, fewer postings entries, smaller index. The cure is schema,
  not a bigger index — you shrink the very thing `index.rs` measures rather
  than indexing it faster.

  </details>

- [ ] You can explain query pushdown as shipping aggregates rather than samples, and say what class of query it cannot serve.

  <details><summary>Answer</summary>

  §5.3: Monarch pushes a query's table operations "as close to the source
  data as possible" — leaves aggregate their own data, zones combine leaf
  partials, the global layer combines zone partials — so each hop ships
  counts/sums/distribution sketches, not raw samples (topic 13's
  computation-to-the-data). It is lossless for anything that combines
  associatively, including quantiles via merged distributions (Step 4). What
  it *cannot* serve cheaply is a query that needs raw cross-source rows
  together — arbitrary joins across zones, or per-sample correlation between
  series held in different leaves — because those cannot be reduced to a
  mergeable partial before the data meets, so the pushdown has nothing to
  push and would have to centralize the samples it was built to avoid moving.

  </details>

- [ ] You can describe BtrDB's aggregate tree and say which regime it is built for that Monarch is not.

  <details><summary>Answer</summary>

  BtrDB (§4) stores each stream as a time-partitioning k-ary tree (K=64, root
  spanning −2⁶⁰..3·2⁶⁰ ns) whose every internal node carries the (min, mean,
  max, count) of its whole subtree (§3). A query at resolution `r` descends
  only until a node's span fits under `r` and reads the precomputed summary,
  so cost is proportional to the number of returned records (≈ pixels), not
  the sample count — and the summaries cost < 0.3% of footprint (§4). It is
  built for the **few-fat-streams, dense-history, arbitrary-resolution**
  regime: 120 Hz synchrophasors accumulating tens of billions of readings,
  queried as "plot years at screen resolution." Monarch is the opposite
  regime — enormous *numbers* of shallow, recent series for fleet monitoring
  and alerting — which is why it optimizes availability and pushdown, not
  multi-resolution scans over deep per-stream history.

  </details>

- [ ] You can explain copy-on-write versioning as treating corrections as history, and connect it to the out-of-order tax this topic's `head.rs` lane prices.

  <details><summary>Answer</summary>

  Each BtrDB insert rewrites the leaf-to-root path and publishes a new root
  (§4), so a correction or a late backfill is simply a new **version** layered
  over the old, not a mutation — history is retained and diffable, and
  "what changed between two versions" costs 8 bytes of state (§7). Disorder is
  free because the tree never had to accept writes *in place*: it copies.
  Contrast the Gorilla-chunk head your `head.rs` lane models
  (reading-prometheus-tsdb.md): its bitstream encoder cannot rewind, so an
  out-of-order sample must be refused or quarantined at append time and merged
  later — the OOO tax. BtrDB pays for disorder with write amplification (a new
  path per commit) instead of a quarantine window; prometheus pays with a
  bounded window and a merge. Same problem, opposite bill — and BtrDB's only
  works because it is one tree per stream, not 10M.

  </details>

## Takeaway

Two extremes that bracket the mainstream. Monarch proves a TSDB can drop
durability and central pulling entirely when the constraint is "never depend
on what you monitor," paying with partial history and weak consistency and
buying planet-scale availability plus schema-killed cardinality. BtrDB proves
the index can *be* the downsampler — summaries in every internal node make
query cost track resolution not sample count, for < 0.3% space — when the
regime is few fat streams of dense history. For the capstone (M30) the useful
import is BtrDB's aggregate tree over the M27 changelog (Q6): point-in-time
`AT TIME t` is one query, but "how did this subgraph evolve" wants
multi-resolution rollups, which is exactly the tree Monarch never needed and
BtrDB is built from.

## References

**Papers**
- Adams et al. — "Monarch: Google's Planet-Scale In-Memory Time Series
  Database" (VLDB 2020). §1–§2 for the memory-first/circular-dependency
  choice, §4.1 for push ingestion and lazy logs, §3 (Figure 2) for the typed
  schema and distribution value type, §5.3 for query pushdown.
- Andersen & Culler — "BTrDB: Optimizing Storage System Design for
  Timeseries Processing" (FAST 2016). §3–§4 for the aggregate tree, the
  `(Time, Min, Mean, Max, Count)` record and CoW versioning; §7 for the
  53M/119M throughput, the 5.514 B/reading (2.9×) footprint, and
  arbitrary-order inserts.

**Code**
- No repo clones — read both papers for the design points that bracket
  the Gorilla/Prometheus middle.
