# LDBC SNB: the graph benchmark referee

A benchmark only referees if it forces the hard parts: updates flowing
during reads, power-law data with real correlations, audited full
disclosure. LDBC SNB is that referee for graph engines — this chapter
maps its three workloads, why its correlated data generator is the
whole point, and what M22's shootout should steal from the spec.

## Why this matters

M22 runs an LDBC-style shootout against FalkorDB. Read this now so
M13's baseline engine grows toward queries a referee will actually
ask — and so you recognize which benchmark claims in vendor blogs are
apples-to-oranges (topic 0's Fair Benchmarking lesson, graph edition).

## 1. The three workloads

```
 SNB Interactive   OLTP-ish: 2-hop neighborhoods, short paths,
                   + concurrent inserts (people, posts, likes)
 SNB BI            analytics: global scans/aggregations over the graph
 Graphalytics      pure algorithms: BFS, PageRank, WCC, CDLP, SSSP
```

Interactive is the one FalkorDB-shaped engines care about: latency
per query with updates flowing. Complex reads (IC1–IC14) are mostly
anchored multi-hop patterns with property filters and aggregation —
i.e. exactly scan-anchor-then-expand plus M12's property columns.

## 2. Correlated data is the point

The datagen produces a power-law social graph WITH correlations:
people named "Wang" cluster in China, friendships correlate with
universities, activity spikes around events. Topic 10's Leis
lesson (uniform synthetic data hides planner sins) applied to graphs:

- degree distribution is power-law → supernodes exist → your engine's
  tail latency is a graph-shape property (hop_bench's 100
  highest-degree sources make the same point)
- correlated properties → cardinality estimation errors compound
  through multi-hop patterns even faster than in JOB

## 3. What the spec forces that benchmarketing skips

- **updates run during reads** — no read-only frozen CSR; this is
  why every architecture in this topic grew a delta mechanism
- audited implementations + full disclosure (drivers, warmup,
  scale factors) — results are reproducible or they're not results
- scale factors (SF1 … SF30K) with defined seed — comparisons pin SF

## 4. What to steal for M22 (record decisions in notes.md)

- the operation mix idea: complex reads + short reads + inserts at a
  spec'd ratio, driven by a workload generator with dependency
  tracking (an insert must be visible to later reads)
- 2-3 representative queries rather than all 14: one anchored 2-hop
  with filters (IC-style), one path query, one aggregation
- report: throughput at bounded p99, not just mean — the supernode
  tail is the honest number

## Questions (answer in notes.md)

1. Why does Interactive schedule inserts with timed dependencies
   instead of firing them as fast as possible?
2. Pick IC5-ish "recent posts of friends-of-friends": write the
   pattern, mark the anchor, count the expands. Which topic-13
   representation hurts most?
3. Uniform-degree graph, same edge count: which of this topic's four
   architectures looks RELATIVELY better than it deserves, and why?
4. What's the graph analogue of JOB's "cardinality errors dwarf cost
   model errors" — at which hop does estimation die?
5. Which SNB scale factor fits in this Mac's RAM as (a) memgraph
   objects, (b) CSR, (c) Delta_Matrix? Rough per-edge byte estimates.

## References

**Papers**
- Erling et al. — "The LDBC Social Network Benchmark: Interactive
  Workload" (SIGMOD 2015)
- LDBC SNB specification
  ([ldbcouncil.org/benchmarks/snb](https://ldbcouncil.org/benchmarks/snb))
  — skim the query set, read the data-generation section
- Iosup et al. — "LDBC Graphalytics" (VLDB 2016) — topic 24's referee;
  noted here for the boundary

**Code**
- [ldbc_snb_datagen_spark](https://github.com/ldbc/ldbc_snb_datagen_spark)
  and the audited implementations under
  [github.com/ldbc](https://github.com/ldbc) — the driver's
  dependency-tracking is the part worth reading for M22
