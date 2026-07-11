# Reading guide — Monarch (VLDB '20) + BtrDB (FAST '16): the two extremes

Papers: *Monarch: Google's Planet-Scale In-Memory Time Series Database*
(VLDB 2020); *BtrDB: Optimizing Storage System Design for Timeseries
Processing* (FAST 2016). No repo clones — read for the design points that
bracket the Gorilla/Prometheus middle.

## Monarch: what breaks at planetary scale

Monarch monitors Google — including the storage systems a durable TSDB
would depend on. That circularity forces the defining choice: **memory
first, durability traded down** (logged lazily, queries don't wait for
it). A monitoring system that's down when Bigtable is down is worthless.

```
              global query layer (query pushdown, hierarchical)
                 ┌────────────┬────────────┐
        zone A   │   zone B   │   zone C   │   <- autonomous per zone:
        leaves   │   leaves   │   leaves   │      ingest keeps working
        (RAM)    │   (RAM)    │   (RAM)    │      through partitions
```

The ideas worth stealing at any scale:

- **Push, not pull**: targets stream to leaves; a scraper (prometheus)
  owns the timestamp regularity Gorilla needs, a push system must cope
  with what arrives. (Note prometheus is pull for exactly this reason.)
- **Typed schemas over string labels**: Monarch series have typed fields
  and *distribution* values (histograms as first-class values) — the
  cure for the label-cardinality bomb is schema, not more index.
- **Query pushdown**: aggregation executes at the leaves; the hierarchy
  ships partial aggregates, not samples. topic 13's
  push-the-computation-to-the-data at monitoring scale.

## BtrDB: the aggregate tree (a genuinely different idea)

Regime: power-grid synchrophasors — 100M+ samples/s/stream, nanosecond
timestamps, queries like "plot 3 years at screen resolution" that touch
*every* sample if evaluated naively.

```
                     root: [t0, t0+2^62) ns
                    ┌ min/mean/max/count ┐            each node: 64 children,
              child │ min/mean/max/count │ child      each holding STATISTICAL
                    └ ... 64-way fanout ─┘            SUMMARIES of its subtree
                              ...
                    leaves: the raw samples
```

- A time range at resolution r needs only the tree level whose node span
  ≈ r: **query cost ∝ pixels, not samples**. Downsampling isn't a batch
  job (prometheus recording rules, VM downsampling) — it's the *index
  structure itself*, always current.
- Copy-on-write versioning: every insert creates a new root (topic 3's
  CoW B-tree); out-of-order and corrections are just versions, and
  changed-ranges between versions are computable — IVM-friendly (topic 27).
- The "obviously wasteful" 1.5× space for summaries buys O(log n)
  any-resolution reads. Compare: Gorilla optimizes bytes/sample, BtrDB
  optimizes bytes *read per query* — different objective, different tree.

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
