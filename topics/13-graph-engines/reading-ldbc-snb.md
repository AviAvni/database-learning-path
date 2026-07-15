# LDBC SNB: the graph benchmark referee

A benchmark only referees if it forces the hard parts: updates flowing
during reads, power-law data with real correlations, audited full
disclosure. LDBC SNB is that referee for graph engines. Before you
skim the spec, this chapter builds what makes it one, step by step —
the three workloads, why the correlated data generator is the whole
point, the update requirement that closes the biggest cheat, the audit
rules, and what M22's shootout should steal.

## Why this matters

M22 runs an LDBC-style shootout against FalkorDB. Read this now so
M13's baseline engine grows toward queries a referee will actually
ask — and so you recognize which benchmark claims in vendor blogs are
apples-to-oranges (topic 0's Fair Benchmarking lesson, graph edition).

## The problem in one sentence

Any engine can win a benchmark it designs itself — freeze the graph,
generate uniform data, pick friendly queries — so a referee benchmark
must force concurrent updates, realistic skew and correlation, and
audited disclosure, or the numbers mean nothing.

## The concepts, step by step

### Step 1 — a referee benchmark forces the parts vendors skip

A **benchmark** is only as honest as the shortcuts it forbids. The
three standard graph-benchmark cheats: run read-only over a frozen,
pre-built structure (no update machinery to pay for); generate uniform
synthetic data (no supernodes, no correlations — every plan looks
fine); self-report unaudited numbers with undisclosed warmup, drivers,
and scale. LDBC (the Linked Data Benchmark Council — an industry
consortium, engines' vendors included) exists to close all three, the
way TPC did for relational systems. Why it matters: each following
step is one closed loophole — read the spec as a list of cheats it
outlaws.

### Step 2 — three workloads, three different questions

SNB (the Social Network Benchmark) splits into workloads because "is
it fast?" is three questions with three different answers:

```
 SNB Interactive   OLTP-ish: 2-hop neighborhoods, short paths,
                   + concurrent inserts (people, posts, likes)
 SNB BI            analytics: global scans/aggregations over the graph
 Graphalytics      pure algorithms: BFS, PageRank, WCC, CDLP, SSSP
```

Interactive is the one FalkorDB-shaped engines care about: latency per
query with updates flowing. Its complex reads (IC1–IC14) are mostly
anchored multi-hop patterns with property filters and aggregation —
i.e. exactly scan-anchor-then-expand plus M12's property columns.
(Graphalytics is topic 24's referee.) Why it matters: an engine's rank
can flip between workloads — quoting "the LDBC number" without naming
the workload is itself a benchmarketing move.

### Step 3 — correlated power-law data is the point

The datagen produces a graph that is skewed AND correlated, because
both properties break engines in ways uniform data can't. **Power-law
degree distribution** (a few nodes have enormous degree — supernodes —
while most have little): your tail latency becomes a graph-shape
property, which is why hop_bench deliberately includes the 100
highest-degree sources. **Correlation** (attribute values predict
structure): people named "Wang" cluster in China, friendships
correlate with universities, activity spikes around events — so
cardinality estimates that assume independence are wrong, and the
errors *compound* through multi-hop patterns even faster than in JOB
(topic 10's Leis lesson — uniform synthetic data hides planner sins —
applied to graphs). Why it matters: an engine tuned on uniform data
meets reality's supernodes and correlations in production, at p99.

### Step 4 — updates run during reads: no frozen-CSR cheating

Interactive's driver interleaves inserts (people, posts, likes) with
the read queries, with **dependency tracking** — an insert must be
visible to reads scheduled after it — so the engine must serve reads
over a structure that is being mutated, with correctness constraints
on visibility. This single rule is why every architecture in this
topic grew a delta mechanism (kuzu's transient buffers, FalkorDB's
Delta_Matrix, memgraph's MVCC): a read-only CSR would win every
frozen-graph benchmark and be disqualified here. Inserts are scheduled
at spec'd timestamps, not fired as fast as possible — throughput comes
from meeting a schedule, not from batching liberties. Why it matters:
this is the requirement that makes the benchmark measure a *database*
rather than a data structure.

### Step 5 — audit, disclosure, and pinned scale factors

An official LDBC result requires an **audit** — an independent
reviewer reruns the benchmark under the published rules — plus full
disclosure of drivers, warmup, configuration; results are reproducible
or they're not results. **Scale factors** (SF1 … SF30K — dataset sizes
with a defined generator seed) pin the dataset exactly, so comparisons
must name their SF: an SF1 (~3 GB) number and an SF1000 number are
different experiments, not two scores on one leaderboard. Why it
matters: this is the machinery that separates a referee from a blog
post — and the checklist to apply to any vendor claim you read.

### Step 6 — what to steal for M22

M22 shouldn't implement all of SNB — it should steal the load-bearing
ideas (record decisions in notes.md):

- the operation mix idea: complex reads + short reads + inserts at a
  spec'd ratio, driven by a workload generator with dependency
  tracking (an insert must be visible to later reads)
- 2-3 representative queries rather than all 14: one anchored 2-hop
  with filters (IC-style), one path query, one aggregation
- report: throughput at bounded p99, not just mean — the supernode
  tail is the honest number

Why it matters: the shootout's credibility comes from adopting the
referee's *constraints* (updates flowing, skewed data, tail
reporting), not its full query set.

## How to read the spec (with the concepts in hand)

1. **Data generation section** — read properly; it's Step 3
   operationalized (which correlations exist, how degrees are drawn).
   This is the part most readers skip and the part that matters most.
2. **Interactive workload definition** — skim all 14 complex reads,
   then read 2–3 closely (IC5-ish friends-of-friends is question 2
   below); note the anchor + expand + filter shape.
3. **Driver / dependency tracking** — read enough to answer why
   inserts are scheduled with timed dependencies (Step 4; question 1).
4. **Audit rules and SF definitions** — skim, but internalize the
   checklist for reading vendor claims (Step 5).
5. The SIGMOD 2015 paper is the narrative version: read its
   correlated-generation and choke-point sections; skim the rest.

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
