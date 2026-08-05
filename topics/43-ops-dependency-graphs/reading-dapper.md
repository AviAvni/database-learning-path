# Dapper: 204 nanoseconds, and one trace in a thousand

Dapper is where your service dependency graph comes from. Every tracing system you have used —
OpenTelemetry, Jaeger, Zipkin, X-Ray — inherits its vocabulary almost word for word: traces,
spans, annotations, context propagation. What makes the paper worth reading rather than skimming
is that it is mostly about *not* being expensive. Google's engineers would not have enabled a
tracing system that cost them measurable throughput, so the design is a sequence of decisions
each justified by a nanosecond count or a percentage, and the biggest of them — sample one
request in a thousand — comes with an argument about what you are actually looking for that is
still the clearest thing written on the subject.

This is a paper, not a codebase, so every number below is anchored to the section, table or figure
of the **Dapper technical report** (Sigelman et al., *dapper-2010-1*, Google, April 2010) that
states it; each was re-checked against the PDF while writing this chapter. Where a figure comes
from this repo's own crate instead, it is marked as a lane of `ops_bench` and traced to
[FINDINGS.md](../../FINDINGS.md) or the topic's `notes.md`.

## The problem in one sentence

**Reconstruct the causal structure of every request across thousands of services, with an overhead
small enough that nobody notices and nobody turns it off.**

## The concepts, step by step

### Step 1 — Two requirements, and they fight

> **In:** nothing yet — this step is the motivation.
> **Out:** the single constraint (overhead negligible *always*, not just *sometimes*) that every
> later step is optimising, plus the three design goals it decomposes into.

Dapper names two requirements up front (§1): **ubiquitous deployment** — the tracing must cover
essentially every service, because "the usefulness of a tracing infrastructure can be severely
impacted if even small parts of the system are not being monitored" — and **continuous
monitoring**, because "unusual or otherwise noteworthy system behavior is difficult or impossible
to reproduce", so it must already be on when the incident starts.

Both requirements push toward the same constraint: the overhead must be negligible *always*, not
just acceptable *sometimes*. A tracing system that covers 90% of your services will fail to explain
the incident that involves the other 10%, and one you turn on during incidents will not be on when
the incident starts. That is what the rest of the design is optimising.

§1 decomposes the constraint into three **design goals**:

- **Low overhead** — "the tracing system should have negligible performance impact on running
  services… even small monitoring overheads are easily noticeable, and might compel the deployment
  teams to turn the tracing system off." The overhead section later reinforces the framing: "one
  can argue that a valuable tracing infrastructure could be worth a performance penalty, [but] we
  believed that initial adoption would be greatly facilitated if the baseline overheads could be
  demonstrably negligible" (§4).
- **Application-level transparency** — programmers should not need to be aware of the tracing
  system, because "a tracing infrastructure that relies on active collaboration from
  application-level developers… becomes extremely fragile."
- **Scalability** — it must handle Google's size "for at least the next few years."

Why it matters: transparency and ubiquity are the same requirement seen from two sides, and low
overhead is the price of admission for both.

### Step 2 — The trace tree, and where instrumentation actually goes

> **In:** the transparency goal from Step 1.
> **Out:** the data model — a **trace** made of **spans** carrying **annotations** — and the three
> instrumentation points that emit spans without the application's help. Step 3 collects what they
> emit.

A **trace** is the record of one request's path across services, structured as a tree. A **span**
is one node of that tree: it has a span name, a span id, a parent span id, and belongs to a single
trace id; spans usually correspond to RPCs (§2.1). An **annotation** is application-attached data
on a span — a timestamp or a key/value pair the developer chose to record. **Context propagation**
is the mechanism that carries the trace id and current span id along the request so that every
service attaches its spans to the same tree.

Each span usually carries client-send / server-receive / server-send / client-receive timestamps.
Those live on different machines with unsynchronised clocks, so Dapper avoids trusting NTP: "an RPC
client always sends a request before a server receives one, and vice versa for the server response.
In this way we have a lower and upper bound for the span timestamps on the server side of RPCs"
(§2.1). Clock skew handled by **causality** — the happened-before ordering of send and receive —
rather than by synchronised clocks. (Topic 43's other papers, Pivot Tracing and Lamport's ordering,
are this same idea generalised.)

Transparency comes from instrumenting three things and nothing else (§2.2):

1. **Thread-local trace context** — when a thread handles a traced control path, the trace context
   is attached to thread-local storage.
2. **The common control-flow library**, so deferred and asynchronous work carries the trace context
   of its creator into the callback.
3. **The RPC framework** — "nearly all of Google's inter-process communication is built around a
   single RPC framework."

That is the whole trick, and it is why it worked: Google had one RPC framework and one threading
library. The core instrumentation is **under 1000 lines of C++ and under 800 of Java** (§3.1).
Where the single-framework assumption fails, so does the tracing — "40 C++ applications and 33 Java
applications required some manual trace propagation" (§3.2), and programs using raw TCP sockets or
SOAP RPCs get nothing.

Why it matters: the data model is inherited unchanged by every modern tracer, but the transparency
that made it deployable is a property of a monoculture few organisations have.

### Step 3 — Out-of-band collection, for two non-obvious reasons

> **In:** the spans emitted by Step 2's instrumentation.
> **Out:** a Bigtable of traces (**one row per trace, one column per span**), plus the price paid
> for collecting them off the request path — a collection latency that is usually seconds and
> occasionally hours.

**Out-of-band collection** means the trace data travels through a side channel — written to local
logs, pulled by a daemon, written to a store — instead of riding back inside the RPC responses. The
pipeline is a **three-stage process** (§2.5): span data is (1) written to local log files, (2)
pulled from every production host by per-machine Dapper daemons, and (3) written to a regional
Bigtable, where **a trace is one row and each span is a column**. Sparse Bigtable rows are exactly
right for traces with an arbitrary number of spans.

Why not just return trace data in the RPC response? §2.5.1 gives two reasons and the second is the
one people miss:

- **It would dwarf the application data.** "RPC responses — even near the root of such large
  distributed traces — can still be comparatively small: often less than ten kilobytes… the in-band
  Dapper trace data would dwarf the application data and bias the results of subsequent analyses."
- **It assumes perfect nesting.** "in-band collection schemes assume that all RPCs are perfectly
  nested. We find that there are many middleware systems which return a result to their caller
  before all of their own backends have returned a final result." An in-band scheme cannot account
  for that non-nested execution.

Cost of the choice: collection is not instantaneous. Median latency from log to repository is
**under 15 seconds**, but the 98th percentile is bimodal — "approximately 75% of the time, 98th
percentile collection latency is less than two minutes, but the other approximately 25% of the time
it can grow to be many hours" (§2.5). If you build on Dapper-style traces, freshness is a
distribution, not a number.

Why it matters: the design choice that keeps tracing off the hot path is also the one that means a
trace you need during an incident may not have landed yet.

### Step 4 — The overhead budget, itemised

> **In:** the runtime library and daemon of Steps 2–3.
> **Out:** a table of per-operation costs — the numbers that got Dapper deployed, and the bar for
> anything you build.

This is the section that got Dapper deployed (§4.1–4.2), and the numbers are worth memorising
because they set the bar for anything you build:

```
   root span creation + destruction ....... 204 ns   (§4.1; extra cost: allocating a global trace id)
   non-root span .......................... 176 ns   (§4.1)
   annotation, span NOT sampled ............. 9 ns   (§4.1; a thread-local lookup)
   annotation, span sampled ................ 40 ns   (§4.1)
   collection daemon .................... < 0.3% of one core, negligible memory   (§4.2, Table 1)
   per span on the wire ................... 426 bytes   (§4.2)
   share of production network traffic ... < 0.01%   (§4.2)
```

(Measured on a 2.2 GHz x86 server, §4.1.) The difference between the 204 ns root span and the
176 ns non-root span **is** the cost of allocating a globally unique trace id, and nothing else. The
daemon is also "restricted to the lowest possible priority in the kernel scheduler in case CPU
contention arises" (§4.2).

Note the 9-vs-40 ns split for annotations. The unsampled path is a **thread-local read** — no lock,
no allocation — which is what lets Dapper tell developers to annotate freely; and they did: **70% of
all spans and 90% of all traces have at least one application-specific annotation** (§3.3). The
cheap path is what bought the coverage.

Why it matters: a per-request nanosecond budget, published operation by operation, is how you argue
a monitoring system into a latency-sensitive fleet.

### Step 5 — Sampling, and the two questions it answers differently

> **In:** the overhead numbers from Step 4 — in particular the 16.3% latency cost of tracing every
> request.
> **Out:** the decision to sample, the rate, and the distinction between an **aggregate** question
> and a **rare-event** question that decides whether a rate is safe.

**Sampling** means recording only a fraction of requests. Table 2 (§4.3), measured on a web-search
cluster with experimental error 2.5% for latency and 0.15% for throughput, is the reason it is not
optional:

```
   sampling      avg latency     avg throughput      (Dapper Table 2, §4.3)
   1/1              +16.3%           −1.48%
   1/2               +9.40%          −0.73%
   1/4               +6.38%          −0.30%
   1/8               +4.12%          −0.23%
   1/16              +2.12%          −0.08%
   1/1024            −0.20%          −0.06%     ← inside experimental error
```

Tracing everything costs 16.3% of your latency. So the **first production version of Dapper**
sampled **one trace in 1024**, a uniform probability applied to Google's high-throughput services
(§4.4) — this is where the famous "1 in 1024" belongs: a fleet-wide default for services doing tens
of thousands of requests per second, not a universal law. It justified the rate with an argument
about the *kind* of question being asked (§4.5):

> for high-throughput services, aggressive sampling does not hinder most important analyses. If a
> notable execution pattern surfaces once in such systems, it will surface thousands of times.

with the caveat in the very next sentence:

> Services with lower volume — perhaps dozens rather than tens of thousands of requests per second
> — can afford to trace every request; this is what motivated our decision to move towards adaptive
> sampling rates.

Lane 3 of this topic's crate measures exactly the gap between those two sentences (reference values
in `notes.md`; full table in the topic README):

```
   rate      traces   edge recall   rare-path recall   mean-latency err   p99 err   (ops_bench lane 3)
   1/1        40000         1.000              1.000               0.0%     0.0%
   1/16        2470         1.000              0.062               0.0%     1.5%
   1/1024        39         1.000              0.001               5.8%    25.6%
```

Two terms name the two questions. **Edge recall** is the fraction of the true dependency edges that
appear at all in the sampled set — an *aggregate* question, since every edge is exercised constantly.
**Rare-path recall** is the fraction of rarely-taken execution paths that survive — a *rare-event*
question. Work the arithmetic: 40,000 requests at 1/1024 keep `40000 / 1024 = 39.06`, so **39
traces**. Every dependency edge still shows up somewhere in those 39 traces, so edge recall is
1.000. But a path taken by one request in 40,000 appears in the sample with probability only
`39 / 40000 ≈ 0.001` — which is exactly the measured rare-path recall. Same 39 traces, and the
answer to "what is the dependency graph?" is *complete* while the answer to "what happened on that
one weird path?" is *gone*.

And note which *metrics* survive (last two columns). The **mean** latency is unbiased under uniform
sampling — averaging a tenth of a percent of the requests still estimates the average — so it stays
within 5.8% even at 39 traces. The **p99** (the latency 99% of requests beat) is made of the tail,
and at 39 traces the tail is a handful of samples, so it is 25.6% off. Aggregate, rare-event, tail:
three different verdicts from one sample.

Why it matters: "is this sampling rate safe?" has no answer until you say which of the three kinds
of question you are asking of the data.

### Step 6 — Two more sampling layers, and one crucial detail

> **In:** the uniform 1/1024 rate from Step 5, and its two failure modes (low-traffic blind spots;
> a repository write limit).
> **Out:** adaptive sampling and collection-time sampling — and the invariant that makes any of it
> safe: sample **whole traces, never individual spans**.

**Adaptive sampling** (§4.4) replaces the uniform probability with "a desired rate of sampled traces
per unit time", so low-traffic workloads sample themselves up automatically while very high-traffic
ones lower their rate to keep overhead bounded. One detail decides whether the resulting data is
usable: "the actual sampling probability used is recorded along with the trace itself; this
facilitates accurate accounting of trace frequencies in analytical tools" — without it, you cannot
weight a low-volume trace against a high-volume one, and every aggregate is wrong.

**Collection-time sampling** (§4.6) is a second, independent round, needed because the repository
has its own write-throughput limit: Google's clusters "presently generate more than 1 terabyte of
sampled trace data per day", which users want retained "for at least two weeks." And here is the
detail that matters most:

> We leverage the fact that all spans for a given trace — though they may be spread across thousands
> of distinct host machines — share a common trace id. For each span seen in the collection system,
> we hash the associated trace id as a scalar `z`, where `0 ≤ z ≤ 1`. If `z` is less than our
> collection sampling coefficient, we keep the span… By depending on the trace id for our sampling
> decision, we either sample or discard entire traces rather than individual spans within traces.

**Whole traces, not spans.** Hashing the *trace* id means every span of a kept trace is kept and
every span of a dropped trace is dropped. Sampling spans independently would leave you with
disconnected fragments — you would keep the data and destroy the causality, which is the only thing
you were collecting it for. The crate's `sampling_keeps_whole_traces` test exists to make you
implement this correctly.

Why it matters: the causal structure is fragile in exactly one way, and this one hashing decision is
what protects it across two independent sampling stages.

### Step 7 — What it was actually used for

> **In:** the trace store filled by Steps 3–6.
> **Out:** the three access patterns a trace store must support, and the storage fact that stops you
> indexing everything.

Worth knowing, because it shapes what a trace store must support. The Depot API (DAPI, §5.1) offers
three access patterns: **by trace id**, **bulk** (a MapReduce over billions of traces), and
**indexed**. The indexing note is a nice storage-engineering aside: "the compressed storage required
for an index into the trace data is only 26% less than for the actual trace data itself" (§5.1) — an
index that is only a quarter smaller than the data is an index you cannot afford to build over
everything.

Section 6 (worth skimming for the war stories) covers inferring service dependencies, tracking
network usage, tracing shared storage, and — the surprise — using trace data to verify that security
policies hold: such measurements "provide greater assurance than source code audits" (§2.6). Section
6.2, inferring service dependencies, is this topic's lane 1 in production form.

Why it matters: a trace store is a database, and the access patterns above are its query workload;
design the storage for them or you will index yourself out of your disk budget.

## How to read the paper (with the concepts in hand)

- **§1.** The two requirements (ubiquitous deployment, continuous monitoring) and three design
  goals. Note the framing that adoption depends on demonstrable negligibility.
- **§2 + Figures 2–3.** Trace trees and spans. The clock-skew-by-causality remark is in §2.1.
- **§2.2.** The three instrumentation points. Ask yourself which of the three your own stack has.
- **§2.3 + Figure 4.** Annotations, and the configurable upper bound on annotation volume — a
  guardrail against your own users.
- **§2.5 + Figure 5.** The three-stage collection pipeline and the bimodal 98th-percentile latency.
- **§2.5.1.** Out-of-band collection. Both reasons; the perfect-nesting one is the subtle one.
- **§4.1–4.3 + Tables 1–2.** The overhead numbers. Memorise the 204 / 176 / 9 / 40 ns figures and
  Table 2's sampling costs.
- **§4.4–4.6.** Adaptive sampling, the "if it surfaces once it will surface thousands of times"
  argument (§4.5), and trace-id-hash collection sampling (§4.6).
- **§5.1.** The Depot API and the 26% index remark.
- **§6.** Skim the experience reports; §6.2 (inferring service dependencies) is this topic's lane 1
  in production form.
- **After the paper.** Implement `sampling.rs` and reproduce lane 3, then do exercise 4 — localize
  the fault using only sampled traces, and find the rate at which top-1 accuracy breaks.

## Questions to answer in notes.md

1. Dapper's transparency rests on Google having one RPC framework and one control-flow library.
   List what your own stack would need instrumented, and estimate how many of your services would
   fall into the "manual propagation required" bucket (§3.2 puts Google's count at 40 C++ and 33
   Java out of thousands).
2. Explain both reasons for out-of-band collection (§2.5.1). Then say what out-of-band costs, using
   the bimodal 98th-percentile collection latency (§2.5).
3. Annotations cost 9 ns unsampled and 40 ns sampled (§4.1). Work out why that gap is the design
   decision that made 70%-of-spans annotation coverage (§3.3) possible.
4. Lane 3 shows edge recall at 1.000 with 39 traces and rare-path recall at 0.001. State the
   property of a question that determines which curve it follows, and classify five questions you
   have actually asked of a tracing system.
5. Sampling per trace rather than per span (§4.6) is presented almost in passing. Construct the
   failure: what does a per-span-sampled data set let you compute, and what does it silently get
   wrong?

## Done when

Answer each before unfolding it.

- [ ] You can draw a trace tree and say what a span carries.

  <details><summary>Answer</summary>

  A trace is a tree of spans representing one request's path across services (§2.1). Each span
  carries a span name, a span id, a parent span id, and the trace id it belongs to — the parent id
  and trace id are what make the collection of spans a tree rather than a bag. A span usually
  corresponds to one RPC and holds four timestamps: client-send, server-receive, server-send,
  client-receive.

  Because those timestamps come from unsynchronised machine clocks, Dapper does not trust their
  absolute values across the client/server boundary. It uses the causal fact that a client sends
  before a server receives, and a server sends its response before the client receives it, to bound
  the server-side timestamps (§2.1). Developers may also attach annotations — timestamps or
  key/value pairs — and 70% of spans carry at least one (§3.3).

  </details>

- [ ] You can name the three instrumentation points and the assumption each depends on.

  <details><summary>Answer</summary>

  Thread-local trace context (§2.2) assumes a request is handled by threads that read a thread-local
  store; the common control-flow library assumes deferred and async work goes through that one
  library, so the callback inherits its creator's context; and the RPC framework assumes "nearly all
  of Google's inter-process communication is built around a single RPC framework" (§2.2).

  All three are the same bet: a monoculture. When it fails, tracing fails — §3.2 records 40 C++ and
  33 Java applications that needed manual propagation, and programs on raw TCP sockets or SOAP get
  nothing. The whole instrumentation is under 1000 lines of C++ and 800 of Java (§3.1) *because* it
  only has to touch three shared libraries.

  </details>

- [ ] You can give both reasons for out-of-band collection.

  <details><summary>Answer</summary>

  First, size and bias: traces can have thousands of spans, but RPC responses "even near the root…
  can still be comparatively small: often less than ten kilobytes", so in-band trace data "would
  dwarf the application data and bias the results of subsequent analyses" (§2.5.1). Returning the
  trace inside the response would change the network behaviour you are trying to measure.

  Second, and the one people miss: "in-band collection schemes assume that all RPCs are perfectly
  nested" (§2.5.1), and many middleware systems return to their caller before their own backends
  finish. An in-band scheme cannot represent that non-nested execution at all. The cost of going
  out-of-band is collection latency: median under 15 s, but a bimodal 98th percentile that is under
  two minutes 75% of the time and up to many hours the rest (§2.5).

  </details>

- [ ] You can quote the overhead numbers and Table 2's sampling costs.

  <details><summary>Answer</summary>

  From §4.1, on a 2.2 GHz x86 server: root span create+destroy 204 ns, non-root 176 ns (the 28 ns
  gap is allocating a globally unique trace id), an unsampled annotation 9 ns (a thread-local
  lookup), a sampled one 40 ns. From §4.2: the collection daemon never exceeds 0.3% of one core,
  each span is 426 bytes on the wire, and trace collection is under 0.01% of production network
  traffic.

  Table 2 (§4.3), on a web-search cluster with 2.5%/0.15% experimental error: tracing every request
  (1/1) costs +16.3% latency and −1.48% throughput; 1/16 costs +2.12% / −0.08%; 1/1024 is −0.20% /
  −0.06%, inside the error bars. The 16.3% latency hit at 1/1 is the number that forced sampling.

  </details>

- [ ] You can explain why sampling must be per trace, not per span.

  <details><summary>Answer</summary>

  Because the thing you are collecting is the *causal structure* — the tree that links a request's
  spans — and that structure only exists if you keep all of a trace's spans or none. Dapper hashes
  the **trace id** to a scalar `z ∈ [0,1]` and keeps the span iff `z` is below the coefficient
  (§4.6); since every span of a trace shares that id, the decision is identical for all of them, so
  entire traces are kept or dropped.

  Sample spans independently and you keep, say, a thousandth of the spans of every trace — a pile of
  disconnected fragments with no parent whose span survived. You would pay the full collection cost
  and be left unable to reconstruct a single request. The `sampling_keeps_whole_traces` test in the
  crate encodes exactly this invariant.

  </details>

- [ ] Your `sampling.rs` reproduces lane 3's two curves.

  <details><summary>Answer</summary>

  Two curves, because there are two questions. Edge recall stays at 1.000 all the way down to
  1/1024 (39 traces from 40,000): every dependency edge is exercised on nearly every request in this
  topology, so a thousandth of the traffic still touches every edge. Rare-path recall falls
  roughly linearly with the rate — 1.000, 0.062 at 1/16, 0.001 at 1/1024 — because a path taken by
  one request in 40,000 appears with probability `39/40000 ≈ 0.001`.

  The metric columns behave differently again: mean-latency error stays ≤ 5.8% because the mean is
  unbiased under uniform sampling, while p99 error reaches 25.6% at 39 traces because the tail is
  built from a handful of samples. Reproducing all three columns is the point of the lane — it makes
  concrete that "aggregate", "rare-event" and "tail" are not the same question.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five questions push on the parts of Dapper that transfer to your own stack: what instrumenting
  three shared libraries would cost you (§2.2, §3.2), why out-of-band collection is worth its latency
  (§2.5, §2.5.1), why the 9-ns unsampled annotation path bought 70% coverage (§3.3, §4.1), how to
  classify a question as aggregate/rare-event/tail before choosing a rate (lane 3), and how per-span
  sampling silently destroys causality (§4.6).

  Write the answers against the anchors above, not from memory — the point of the exercise is that
  every claim traces to a section, a table, or a lane of `ops_bench`.

  </details>

## References

- Sigelman, Barroso, Burrows, Stephenson, Plakal, Beaver, Jaspan, Shanbhag. *Dapper, a Large-Scale
  Distributed Systems Tracing Infrastructure.* Google Technical Report dapper-2010-1, April 2010 —
  [PDF](https://static.googleusercontent.com/media/research.google.com/en//archive/papers/dapper-2010-1.pdf).
  Section, table and figure citations in this chapter refer to that report.
- Fonseca, Porter, Katz, Shenker, Stoica. *X-Trace: A Pervasive Network Tracing Framework.*
  NSDI 2007 — the metadata-propagation ancestor.
- The OpenTelemetry specification, if you want to see this paper's vocabulary standardised.
- Local exercise stub: `topics/43-ops-dependency-graphs/experiments/src/sampling.rs`.
- Topic 34 (debugging & production diagnosis) — sampling bias and coordinated omission, the
  single-machine version of this problem.
