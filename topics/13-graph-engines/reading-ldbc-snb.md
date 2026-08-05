# LDBC SNB: the graph benchmark referee

A benchmark only referees if it forces the hard parts: updates flowing
during reads, power-law data with real correlations, audited full
disclosure. LDBC SNB is that referee for graph engines. Before you
skim the spec, this chapter builds what makes it one, step by step —
the workloads, why the correlated data generator is the whole point,
the update requirement that closes the biggest cheat, the audit
rules, and what M22's shootout should steal.

Every claim below is cited to a numbered section of **The LDBC Social
Network Benchmark, version 0.3.6**
([arXiv:2001.02299](https://arxiv.org/abs/2001.02299), 144 pp), which
was read to check it. Three things the previous version of this
chapter asserted — how many workloads SNB has, what SF1 weighs, and
what distribution the friend degrees follow — did not survive that
check and are corrected in place.

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

> **In:** the three ways a vendor can make its own engine look good.
> **Out:** the list of rules a benchmark must impose to close them —
> which is also the reading order for the rest of the spec.

A **benchmark** is only as honest as the shortcuts it forbids. The
three standard graph-benchmark cheats: run read-only over a frozen,
pre-built structure (no update machinery to pay for); generate uniform
synthetic data (no supernodes, no correlations — every plan looks
fine); self-report unaudited numbers with undisclosed warmup, drivers,
and scale. LDBC (the Linked Data Benchmark Council — an industry
consortium, engines' vendors included) exists to close all three, the
way TPC did for relational systems. The spec's own table of contents
is the checklist:

> "This document contains: • A detailed specification of the data
> used in the whole LDBC SNB benchmark. • A detailed specification of
> the workloads. • A detailed specification of the execution rules of
> the benchmark. • A detailed specification of the auditing rules and
> the full disclosure report's required contents."
> — Executive Summary, p.3

Four documents, four loopholes. Why it matters: each following step is
one closed loophole — read the spec as a list of cheats it outlaws.

### Step 2 — two workloads, two different questions

> **In:** the question "is this graph engine fast?"
> **Out:** the two workloads SNB actually defines, the different
> primary metric each one reports, and the boundary where SNB stops.

**Correction.** The previous version of this chapter listed *three*
SNB workloads, with Graphalytics as one of them. The spec's abstract
is explicit that there are two:

> "LDBC SNB consists of **two workloads** that focus on different
> functionalities: the Interactive workload (interactive transactional
> queries) and the Business Intelligence workload (analytical
> queries)."
> — Abstract, p.2

Graphalytics is a *sibling* LDBC benchmark, not an SNB workload:
"Initially, a graph analytics workload was also included in the
roadmap of LDBC SNB, but this was finally delegated to the
Graphalytics benchmark project [34, 35], which was adopted as an
official LDBC graph analytics benchmark" (§1.1, p.10), and §1.4
*Related Projects* lists it alongside the Semantic Publishing
Benchmark as a separate thing.

The distinction is not pedantry, because the two workloads report
**incomparable primary metrics**:

```
 SNB Interactive  three query classes (§5, p.45):
                    complex read-only  (IC 1 … IC 14)
                    short read-only    (IS 1 … IS 7)
                    transactional inserts
                  primary metric: "Operations per second for a given
                  SF (throughput)"                        — §5, p.45

 SNB BI           reads (BI 1 … BI 20, §6.4) + refreshes
                  (inserts and deletes, §6.5)
                  metric: "the system is characterized by TWO metrics:
                  the geometric mean of the read query execution times
                  and the geometric mean of the time required to load
                  daily batches"                          — §6.3, p.69
```

One number versus two; throughput versus geometric-mean latency plus
geometric-mean load time. There is no arithmetic that converts
between them. Interactive is the one FalkorDB-shaped engines care
about, and the spec describes its complex reads exactly as this
topic's benchmark does:

> "This workload consists of a set of relatively complex read-only
> queries, that touch a significant amount of data – often the
> **two-step friendship neighbourhood** and associated messages –, but
> typically in close proximity to a single node. Hence, the query
> complexity is **sublinear to the dataset size**."
> — §5, p.45

That is `hop_bench` with a social-network schema bolted on: a single
anchor node, two expands, aggregate. The "sublinear to the dataset
size" claim is the one this topic's headline stress-tests — sublinear
in *n*, yes, but linear in the anchor's two-hop neighbourhood size,
which is why the same query costs 4.9 µs from a random node and
495 µs from a supernode. Why it matters: an engine's rank can flip
between workloads — quoting "the LDBC number" without naming the
workload is itself a benchmarketing move.

### Step 3 — correlated data is the point, and the degrees are not a power law

> **In:** a target number of Persons and three simulated years.
> **Out:** a graph whose degree distribution, attribute correlations
> and temporal bursts all break independence assumptions — plus the
> exact mechanism the generator uses to produce each.

The datagen produces a graph that is skewed AND correlated, because
both properties break engines in ways uniform data can't. But be
precise about which distribution does what.

**Correction.** The previous version said the degree distribution is a
power law. The spec does not say that. It says the knows-degree
follows a Facebook-shaped empirical distribution, and the power law
it *does* specify is for something else entirely — comment timing:

> "…the number of knows relationships of every person, which is
> guided by a degree distribution function **similar to that found in
> Facebook** [68]."
> — §3.3.2 *Graph Generation*, p.22

> "Comment always occur within γ days of their parent message
> following a **power-law distribution**…"
> — §3.6.5, and Figure 3.3 "The power-law used to generate comments"

The distinction is worth keeping straight: the spec anchors the
knows-degree to a measured empirical distribution from a real social
network (reference [68]) rather than to a closed-form power law, and
does not state its tail exponent or maximum anywhere in the document.
So do not assume the generator will reproduce this topic's
6 565-degree preferential-attachment tail — if you need that shape,
measure the generated graph rather than inferring it from the spec.

The correlations are the deeper point, and the spec names the
mechanism. Edges are drawn by **homophily**:

> "…similar persons (with similar interests and behaviors) tend to be
> connected. This is known as the **Homophily principle** [46, 14],
> and implies the presence of a larger amount of **triangles** than
> that expected in a random network."
> — §3.3.2, p.22

implemented by sorting persons under a similarity function M(p) and
picking connections from the K nearest positions with a geometric
distribution over ranked distance — and split across exactly three
axes:

> "In Datagen, **three correlated dimensions** are chosen: the first
> one depends on where the person studied and when, and the second
> correlation dimension depends on the interests of the person, and
> the third one is random (to reproduce the random noise present in
> real data)."
> — §3.3.2, p.23

Plus temporal bursts — "**flash mob** events" assigned a random tag,
around which activity volume spikes (§3.3.2, p.23).

Every one of those is an independence assumption a cost model would
otherwise make. Attribute-value filters are not independent of graph
position; two-hop expansion is not degree² because triangles close;
timestamps are not uniform. Cardinality errors compound through
multi-hop patterns even faster than in JOB (topic 10's Leis lesson —
uniform synthetic data hides planner sins — applied to graphs). Why
it matters: an engine tuned on uniform data meets reality's
supernodes and correlations in production, at p99.

### Step 4 — updates run during reads, on a schedule, with curated parameters

> **In:** a stream of timestamped update operations and a set of
> substitution parameters per query template.
> **Out:** a query mix whose issue times are fixed by the spec rather
> than chosen by the vendor — the rule that makes this a database
> benchmark rather than a data-structure benchmark.

Interactive's driver interleaves inserts with the read queries, and
the inserts are not fired as fast as possible:

> "Update queries' issue times are taken from the update streams
> generated by the data generator. **These are the times where the
> actual event happened during the simulation of the social
> network.** Complex reads' times are expressed in terms of update
> operations."
> — §4.4 *Load Definition*, p.43

So the engine must serve reads over a structure that is being mutated,
on someone else's clock. This single rule is why every architecture in
this topic grew a delta mechanism (kuzu's transient buffers,
FalkorDB's Delta_Matrix, memgraph's MVCC): a read-only CSR would win
every frozen-graph benchmark and be disqualified here.

There is a second mechanism the previous version of this chapter
missed entirely, and it is the cleverest thing in the spec. Because
query cost varies wildly with the parameter you plug in — the very
effect this topic measures at 101× — LDBC does not sample parameters
at random. It **curates** them, to three stated properties:

> "**P1:** the query runtime has a bounded variance … **P2:** the
> runtime distribution is stable … **P3:** the optimal logical plan
> (optimal operator order) of the queries is the same … As a result,
> the amount of data that the query touches is roughly the same for
> every parameter binding … Such effects could arise due to the
> **data skew and correlations** between values in the generated
> dataset."
> — §4.3 *Substitution Parameters*, p.42

Parameter Curation runs in two stages: compute intermediate-result
sizes for every candidate binding as a side effect of generation, then
greedily select bindings with similar counts. Read that against this
topic's headline and the trade is stark: LDBC deliberately *removes*
the supernode-versus-random-node spread so that a single mean is
meaningful. The 101× gap is real and LDBC hides it on purpose — which
is exactly why your own `hop_bench` reports both lanes separately.

The other scheduling knob is the frequency table (Table 4.1, p.43),
where "a frequency value is assigned which specifies the relation
between the number of updates performed per complex read" — i.e. the
number of updates between two instances of that query, so a *larger*
number means a *rarer* query. It is scale-dependent, and in opposite
directions:

```
 Table 4.1 (updates per complex read):
              SF1     SF1000
   IC 8        45          1     → 45× MORE frequent at SF1000
   IC 9       157        967     →  6.2× RARER at SF1000
   IC 1        26         26     →  unchanged

 the IC8 : IC9 ratio in the mix
   at SF1:     157/45  =   3.5 IC8s per IC9
   at SF1000:  967/1   = 967   IC8s per IC9
   the mix shifts by 967 / 3.5 = 277×
```

The mix at SF1000 is a different workload from the mix at SF1, by
design — expensive queries are throttled so "faster query types" are
not made "purposeless" (§4.4, p.43). Why it matters: this is the
requirement that makes the benchmark measure a *database* rather than
a data structure, and the reason two SFs are two experiments.

### Step 5 — audit, disclosure, and pinned scale factors

> **In:** a claimed result.
> **Out:** the specific checklist that makes it citable — dataset
> size, run length, warm-up, on-time percentage, and who verified it.

An official LDBC result requires an **audit** by a trained, certified
auditor (§7.2.1) plus a Full Disclosure Report (§7.4.8) carrying the
system description and pricing, data generation and loading, driver
details, performance metrics, validation results, ACID compliance, and
a supplementary package with a README and the database configuration
files — "to ensure reproducibility of the audited results".

**Scale factors** pin the dataset, and the definition is not a node
count:

> "For both workloads, **the SF1 data set is 1 GiB**, the SF100 is
> 100 GiB, and the SF10000 data set is 10000 GiB (not 10 TiB)."
> — §3.4.1, p.25

**Correction.** The previous version wrote "an SF1 (~3 GB) number".
SF1 is 1 GiB of serialized CSV, not 3 GB. And the *composition* of
that gibibyte differs by workload — Interactive counts 90% initial
data plus the 10% update streams with the `csv-singular-merged-fk`
serializer; BI counts a 97% initial snapshot plus refresh operations
with `csv-composite-merged-fk` (§3.4.1). Same SF number, different
bytes on disk. The proposed SFs are 1, 3, 10, 30, 100, 300, 1000,
3000, 10000, 30000, plus 0.003, 0.1 and 0.3 for validation; all SFs
cover three years starting in 2010, and scaling the SF scales the
number of Persons.

The run rules are equally specific, and they interlock:

```
 §7.4.1.1  validation run          on SF10
 §7.4.1.1  audited benchmark runs  on SF30 or larger
 §7.4.7.1  valid run               ≥ 2 hours wall clock
 §7.4.7.1  95% on-time requirement
             actual_start_time − scheduled_start_time < 1 second
             for 95% of issued queries
 §7.4.7.2  warm-up ≥ 30 min, then a 2-hour measurement window
```

The SF30 floor is not arbitrary; the spec derives it, and the
derivation is worth reproducing because it is the whole benchmark in
one calculation:

```
 §7.4.7.2: "The SNB Datagen produces 3 years worth data of which 10%
 is used for updates, i.e. approximately 3×365×0.1 = 109.5 days
 = 2628 hours."

 Time Compression Ratio (TCR) replays those updates faster:
   playback wall clock = 2628 h × TCR
   spec floor          = TCR ≥ 0.001
   → shortest possible run = 2628 × 0.001 = 2.628 hours

 required: 30 min warm-up + 2 h measurement = 2.5 hours
 2.628 ≥ 2.5  ✓  — with 7.7 minutes to spare
```

"System that can achieve a better compression (i.e. lower TCR value)
on a given scale factor should use larger SFs for their benchmark
runs – otherwise their total runs will be less than 2.5 hours, making
them unsuitable for auditing" (§7.4.7.2). A fast engine is *forced*
onto a bigger dataset. Why it matters: this is the machinery that
separates a referee from a blog post — and the checklist to apply to
any vendor claim you read.

### Step 6 — what to steal for M22

> **In:** the five preceding steps' rules.
> **Out:** the two or three of them that are worth the implementation
> cost for a single-developer shootout, and the ones to skip.

M22 shouldn't implement all of SNB — it should steal the load-bearing
ideas (record decisions in notes.md):

- the operation mix idea: complex reads + short reads + inserts at a
  spec'd ratio, driven by a workload generator with dependency
  tracking (an insert must be visible to later reads)
- 2-3 representative queries rather than all 14: one anchored 2-hop
  with filters (IC-style), one path query, one aggregation
- report: throughput at bounded p99, not just mean — the supernode
  tail is the honest number

The one place to deliberately *depart* from LDBC is Parameter
Curation. SNB curates the skew out (Step 4, P1–P3) so a mean is
meaningful; this topic's whole finding is what lives in the skew.
Keep the two lanes — random sources and highest-degree sources —
reported separately, and you get both the referee's comparability and
the number LDBC's design suppresses.

Scale calibration, from the spec's own entity counts (Appendix B.1,
Table B.1 — real numbers, unlike the still-TODO Table 3.12):

```
 SF     persons      person_knows_person rows    rows / person
 1       11 000                 452 622             41.1
 10      73 000               4 654 416             63.8
 30     184 000              14 212 356             77.2
 100    499 000              46 598 276             93.4
 1000 3 600 000             447 163 916            124.2

 this topic's graph:  1 000 000 nodes,  16.0e6 directed edges  →  16.0

 → in edge count the topic graph sits just above SF30; in NODE count
   it is 5.4× SF30 and 0.28× SF1000. It is a sparser, wider graph
   than any SNB scale factor — worth stating explicitly before
   claiming any result transfers.
```

Note the drift in the last column: SNB's density *rises* with scale
(41 → 124 rows per person, a 3.0× increase from SF1 to SF1000)
because the simulated period is fixed at three years while the
population grows. Why it matters: the shootout's credibility comes
from adopting the referee's *constraints* (updates flowing, skewed
data, tail reporting), not its full query set — and from saying
plainly where your graph is not theirs.

## How to read the spec (with the concepts in hand)

| Step | Section | Pages |
|---|---|---|
| 1 | Executive Summary; §1.1 Scope; §1.4 Related Projects | 3, 10 |
| 2 | Abstract; §5 opening; §5.1–5.3; §6.3 Target metric | 2, 45, 69 |
| 3 | §3.3.2 Graph Generation (homophily, three correlation dimensions, flash mobs) | 22-23 |
| 4 | §4.3 Substitution Parameters (P1/P2/P3, Parameter Curation); §4.4 Load Definition + Table 4.1 | 42-44 |
| 5 | §3.4.1 scale factors; §7.2.1 auditors; §7.4.1.1 SF10/SF30; §7.4.7 timing; §7.4.8 FDR | 25, 94, 101, 106 |
| 6 | Appendix B.1 Table B.1 (per-SF entity counts) | 128+ |

1. **Data generation section (§3.3)** — read properly; it's Step 3
   operationalized (which correlations exist, how degrees are drawn).
   This is the part most readers skip and the part that matters most.
2. **Interactive workload definition (§5)** — skim all 14 complex
   reads, then read 2–3 closely (IC5-ish friends-of-friends is
   question 2 below); note the anchor + expand + filter shape.
3. **Driver / load definition (§4.3–4.4)** — read enough to answer why
   inserts are scheduled with timed dependencies (Step 4; question 1),
   and read Parameter Curation properly; it is the subtlest idea in
   the document.
4. **Audit rules and SF definitions (§3.4.1, §7.4)** — skim, but
   internalize the checklist for reading vendor claims (Step 5).
5. The SIGMOD 2015 paper is the narrative version (the spec cites it
   at §5, p.45 as reference [24]): read its correlated-generation and
   choke-point sections; skim the rest.

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

## Done when

Answer each before unfolding it.

- [ ] You can name the workloads SNB actually defines and the different question each one asks — including which metric each reports.

  <details><summary>Answer</summary>

  **Two**: Interactive and Business Intelligence (Abstract, p.2).
  Graphalytics is a separate LDBC benchmark, delegated out of the SNB
  roadmap (§1.1, p.10; §1.4).

  Interactive: complex reads (IC 1–14), short reads (IS 1–7) and
  transactional inserts; primary metric "Operations per second for a
  given SF (throughput)" (§5, p.45).

  BI: 20 read queries plus refresh operations (inserts and deletes);
  metric is a *pair* — the geometric mean of read query execution
  times and the geometric mean of daily-batch load time (§6.3, p.69).
  Nothing converts one metric into the other.
  </details>

- [ ] You can explain why correlated data is the point rather than a realism garnish — and connect it to the 101x supernode gap this topic measures.

  <details><summary>Answer</summary>

  §3.3.2 gives the mechanism: homophily (similar persons connect,
  producing more triangles than a random graph) implemented over
  three correlation dimensions — where and when the person studied,
  their interests, and random noise — plus flash-mob temporal bursts.
  Each one breaks an independence assumption that a cost model makes
  for free, and multi-hop patterns compound the error.

  The degrees are *Facebook-like*, not a power law (§3.3.2, p.22);
  the spec's power law is for comment delay (Figure 3.3). So SNB has
  supernodes but a bounded tail, where this topic's preferential
  attachment generator produces a 6 565-degree node on 1 M nodes and
  the 101× two-hop gap that follows from it.
  </details>

- [ ] You can say what running updates during reads prevents a vendor from doing, and what Parameter Curation deliberately removes.

  <details><summary>Answer</summary>

  Updates prevent shipping a frozen read-only CSR: §4.4 fixes the
  insert issue times to the simulated event times, and complex read
  times are expressed in updates, so the engine must serve reads over
  a mutating structure on the spec's clock rather than batching at
  its convenience. Every engine in this topic answers with a delta
  mechanism.

  Parameter Curation (§4.3) removes the parameter-dependent variance:
  P1 bounded runtime variance, P2 stable runtime distribution across
  streams, P3 same optimal logical plan for every binding — chosen by
  matching intermediate-result sizes. It is the 101× effect,
  deliberately engineered out so that a mean is a meaningful summary.
  Your own bench keeps the two lanes apart instead.
  </details>

- [ ] You can state what a pinned scale factor and an audit rule are for, and reproduce the spec's own derivation of the 2.5-hour floor.

  <details><summary>Answer</summary>

  SF pins the dataset by *serialized size*: SF1 = 1 GiB, SF100 =
  100 GiB, SF10000 = 10000 GiB (§3.4.1, p.25) — with different
  composition per workload (Interactive 90% initial + 10% streams,
  `csv-singular-merged-fk`; BI 97% snapshot + refreshes,
  `csv-composite-merged-fk`). Comparisons must name the SF and the
  workload.

  Audit rules: validation on SF10, audited runs on SF30 or larger
  (§7.4.1.1); ≥ 2 h wall clock with a 95% on-time requirement
  (`actual_start_time − scheduled_start_time < 1 s`) (§7.4.7.1);
  ≥ 30 min warm-up then a 2 h measurement window (§7.4.7.2).

  The derivation: 3 years × 365 × 10% = 109.5 days = 2628 hours of
  updates; TCR ≥ 0.001, so the shortest legal replay is 2.628 hours,
  which just covers the 0.5 + 2 = 2.5 hours required. A faster engine
  must move to a bigger SF or run out of updates.
  </details>

- [ ] You wrote answers to all questions in notes.md, including what you intend to steal for M22.

  <details><summary>Answer</summary>

  Question 5 needs real counts, and Appendix B.1 Table B.1 has them:
  SF10 is 73 000 persons and 4 654 416 `person_knows_person` rows;
  SF30 is 184 000 and 14 212 356; SF100 is 499 000 and 46 598 276.
  Multiply by your per-edge estimates — memgraph's are computable
  from `sizeof(Vertex) == 80` plus a 24-byte `EdgeTriple` per
  direction, a CSR's are 8 bytes per edge plus 8 per node, a
  Delta_Matrix's are GraphBLAS hypersparse (index + value per entry,
  times the number of live matrices).

  For M22, steal: the operation mix with dependency tracking, two or
  three representative queries, and p99 reporting. Skip: Parameter
  Curation — deliberately, because the skew it removes is this
  topic's actual finding.
  </details>

## References

**Papers**
- **The LDBC Social Network Benchmark, version 0.3.6**
  ([arXiv:2001.02299](https://arxiv.org/abs/2001.02299)) — the
  authority for everything above. §3.3.2 data generation; §3.4.1
  scale factors; §4.3 Parameter Curation; §4.4 load definition and
  Table 4.1; §5 Interactive; §6.3 BI target metric; §7.4 auditing;
  Appendix B.1 Table B.1 per-SF entity counts
- Erling et al. — "The LDBC Social Network Benchmark: Interactive
  Workload" (SIGMOD 2015) — the narrative version; the spec cites it
  at §5, p.45 as its detailed description of the Interactive workload
- Iosup et al. — "LDBC Graphalytics" (VLDB 2016) — a *separate* LDBC
  benchmark (§1.4), topic 24's referee; noted here for the boundary

**Code**
- [ldbc_snb_datagen_spark](https://github.com/ldbc/ldbc_snb_datagen_spark)
  and the audited implementations under
  [github.com/ldbc](https://github.com/ldbc) — the driver's
  scheduling of update streams against `scheduled_start_time` (§7.4.7.1)
  is the part worth reading for M22

**Cross-references in this topic**
- [reading-kuzu.md](reading-kuzu.md), [reading-memgraph-storage.md](reading-memgraph-storage.md),
  [reading-graphblas-internals.md](reading-graphblas-internals.md) —
  the three delta mechanisms Step 4's update rule forces
- [notes.md](notes.md) — the 4.9 µs /
  495 µs baseline that Step 2's "sublinear to the dataset size" claim
  should be read against
