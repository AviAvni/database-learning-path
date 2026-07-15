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

## The problem in one sentence

The mainstream TSDB design quietly assumes a durable storage layer
underneath and queries that scan every sample they aggregate — Monarch
can't have the first (it monitors the storage layers it would depend on)
and BtrDB can't afford the second (at 100M+ samples/s/stream, "plot 3
years" touching every sample is dead on arrival).

## The concepts, step by step

### Step 1 — Monarch's constraint: you can't depend on what you monitor

Monarch monitors Google — including Bigtable, Colossus, and Spanner, the
storage systems a durable TSDB would naturally be built on. That's a
**circular dependency**: a monitoring system that's down when Bigtable is
down is worthless precisely when it's needed most. The forced choice:
**memory first, durability traded down** — data lives in RAM at the
leaves, is logged to disk lazily, and queries never wait for the log.
Alerting availability is bought with the loss of guaranteed history (a
leaf crash can drop recent samples), a trade no bank would take and every
monitoring team should (Q1 asks what queries die).

### Step 2 — autonomy by geography: zones that survive partitions

The same must-stay-up reasoning shapes the topology: ingestion and
alerting must keep working *inside* a network partition, so Monarch is a
hierarchy of **autonomous zones** — each zone's leaves ingest and answer
queries for locally-monitored targets with no cross-zone dependency, and
a global query layer federates over them when the network permits:

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

Prometheus *pulls*: a scraper polls each target on its own schedule,
which means the scraper controls timestamp regularity — the fixed
scrape interval is exactly what makes Gorilla's delta-of-delta mostly
zero. Monarch is *push*: targets stream samples to leaves, because at
planetary scale and across failure domains a central puller is itself a
liability. The cost is losing the regularity guarantee — a push system
must cope with whatever timestamps arrive. Every TSDB's ingestion story
is downstream of this one choice; note prometheus chose pull *for* the
regularity.

### Step 4 — typed schemas: the cure for cardinality is structure

Prometheus models everything as string labels, and topic 30's cardinality
bomb (a `user_id` label = 10M series) is the bill. Monarch instead gives
series **typed schemas** — declared key fields with types — and, more
important, **distribution-typed values**: a latency histogram is *one*
series whose values are histogram objects, not ~10 separate
`le=`-labelled bucket series as in prometheus. The lesson generalizes:
the cure for the label-cardinality bomb is schema, not more index — move
structure out of the series *key* and into the *value type* (Q2 prices
the query-time consequence for quantiles).

### Step 5 — query pushdown: ship aggregates, not samples

A global query ("p99 latency of service X across all zones") could pull
every sample to one place — at Monarch's scale, absurd. Instead the query
plan is **pushed down** the hierarchy: leaves aggregate their own data,
zones combine leaf results, the global layer combines zone results — each
hop ships *partial aggregates* (counts, sums, distribution sketches), not
samples. Topic 13's move-the-computation-to-the-data, at monitoring
scale. This is also why distribution values (Step 4) matter: histograms
merge associatively, so partial aggregation is lossless for quantiles.

### Step 6 — BtrDB's regime, and the aggregate tree

BtrDB serves power-grid synchrophasors: 100M+ samples/s per stream,
nanosecond timestamps, and queries like "plot 3 years at screen
resolution" that touch *every* sample if evaluated naively. Its answer is
to make the index precompute the answers: a tree partitioning the time
axis (root spans `[t0, t0 + 2^62)` ns, 64-way fanout at every level)
where **each internal node stores the min/mean/max/count of its entire
subtree**:

```
                     root: [t0, t0+2^62) ns
                    ┌ min/mean/max/count ┐            each node: 64 children,
              child │ min/mean/max/count │ child      each holding STATISTICAL
                    └ ... 64-way fanout ─┘            SUMMARIES of its subtree
                              ...
                    leaves: the raw samples
```

A query at resolution `r` descends only until a node's time span fits
under `r`, then takes the precomputed summary — **query cost ∝ pixels,
not samples** (Q3 derives it):

```rust
// Descend only until a node's span fits under the requested resolution.
fn query(node: &Node, range: TimeRange, res_ns: u64, out: &mut Vec<Stats>) {
    for child in node.children_overlapping(range) {
        if child.span_ns() <= res_ns {
            out.push(child.stats);            // precomputed min/mean/max/count —
        } else {                              // never touch the raw samples
            query(child, range, res_ns, out); // one of 64 ways, O(log₆₄ depth)
        }
    }
}
```

Downsampling isn't a batch job (prometheus recording rules, VM
downsampling) — it's the *index structure itself*, always current. The
"obviously wasteful" ~1.5× space for summaries buys O(log n)
any-resolution reads: Gorilla optimizes bytes/sample, BtrDB optimizes
bytes *read per query* — different objective, different tree.

### Step 7 — copy-on-write versions: disorder and corrections as history

BtrDB's tree is **copy-on-write** (topic 3's CoW B-tree): an insert
rewrites the path from leaf to root and publishes a *new root*, so every
commit is a retained **version**. Out-of-order data and corrections —
routine in telemetry, where a field device uploads yesterday's backlog —
are just inserts producing new versions, no quarantine window needed. And
because versions are diffable, "what changed between v1000 and v1200" is
computable — the changed-ranges API that makes downstream incremental
computation (topic 27's IVM) natural. The catch that keeps this from
rescuing prometheus-shaped workloads: it's one tree *per stream*, priced
for few fat streams, not 10M skinny ones (Q4).

## How to read the papers (with the concepts in hand)

- **Monarch §1–3** — the memory-first argument (Step 1), zones and
  autonomy (Step 2), push ingestion (Step 3), and the typed data model
  with distribution values (Step 4). Read §1's motivation carefully —
  every architectural oddity traces back to the circularity constraint.
- **Monarch's query sections** — pushdown and hierarchical evaluation
  (Step 5); pairs with topic 13. Watch for how distribution values make
  partial aggregation lossless.
- **BtrDB (FAST '16)** — short and dense; the aggregate tree (Step 6)
  and CoW versioning (Step 7) are the whole paper. Work the cost
  derivation of Q3 while the tree diagram is fresh, then read their
  changed-ranges/IVM discussion against topic 27.

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
   trick NOT rescue prometheus-shaped workloads (hint: series count —
   one tree per stream at 10M streams)?
5. Both papers reject the label-selector data model (Monarch: schemas;
   BtrDB: few fat streams + external metadata). Argue which parts of the
   prometheus model are essential vs incidental for infrastructure
   monitoring.
6. M30 mapping: `MATCH ... AT TIME t` needs point-in-time; but "how did
   this subgraph evolve" wants BtrDB-style multi-resolution over edge
   churn (edges-added-per-hour rollups). Sketch where an aggregate tree
   over the M27 changelog would live in FalkorDB.

## References

**Papers**
- Adams et al. — "Monarch: Google's Planet-Scale In-Memory Time Series
  Database" (VLDB 2020) — §1-3 for the memory-first/push/schema choices;
  the query pushdown section pairs with topic 13
- Andersen & Culler — "BTrDB: Optimizing Storage System Design for
  Timeseries Processing" (FAST 2016) — short and dense; the aggregate
  tree and CoW versioning are the whole paper

**Code**
- No repo clones — read both papers for the design points that bracket
  the Gorilla/Prometheus middle
