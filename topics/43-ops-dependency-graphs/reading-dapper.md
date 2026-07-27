# Dapper: 204 nanoseconds, and one trace in a thousand

Dapper is where your service dependency graph comes from. Every tracing system you have used —
OpenTelemetry, Jaeger, Zipkin, X-Ray — inherits its vocabulary almost word for word: traces,
spans, annotations, context propagation. What makes the paper worth reading rather than skimming
is that it is mostly about *not* being expensive. Google's engineers would not have enabled a
tracing system that cost them measurable throughput, so the design is a sequence of decisions
each justified by a nanosecond count or a percentage, and the biggest of them — sample one
request in a thousand — comes with an argument about what you are actually looking for that is
still the clearest thing written on the subject.

## The problem in one sentence

**Reconstruct the causal structure of every request across thousands of services, with an overhead
small enough that nobody notices and nobody turns it off.**

## The concepts, step by step

### Step 1 — Two requirements, and they fight

Dapper names them up front: **ubiquitous deployment** and **continuous monitoring**. A tracing
system that covers 90% of your services will fail to explain the incident that involves the other
10%, and one you turn on during incidents will not be on when the incident starts.

Both requirements push toward the same constraint: the overhead must be negligible *always*, not
just acceptable *sometimes*. That is what the rest of the design is optimising.

Three derived design goals:

- **Low overhead** — "a valuable tracing infrastructure could be worth a performance penalty, [but]
  we believe that initial adoption would be greatly facilitated if the baseline overheads could be
  demonstrably negligible."
- **Application-level transparency** — programmers should not have to be aware of it.
- **Scalability**.

### Step 2 — The trace tree, and where instrumentation actually goes

A trace is a tree of **spans**; each span has a name, a span id, a parent span id, and belongs to a
trace id. Spans usually correspond to RPCs, with client-send/server-receive/server-send/client-recv
timestamps — and since those live on different machines, "in our analysis tools, we take advantage
of the fact that an RPC client always sends a request before a server receives one, and vice versa
for the server response. In this way we have a lower and upper bound for the span timestamps on
the server side of RPCs." Clock skew handled by causality rather than by NTP.

Transparency comes from instrumenting three things and nothing else:

1. **Thread-local trace context** when a thread handles a traced control path.
2. **The common control-flow library**, so deferred and asynchronous work carries the trace context
   of its creator into the callback.
3. **The RPC framework** — "nearly all of Google's inter-process communication is built around a
   single RPC framework".

That is the whole trick, and it is why it worked: Google had one RPC framework and one threading
library. The core instrumentation is **under 1000 lines of C++ and under 800 of Java**. Where the
assumption fails, so does the tracing — "40 C++ applications and 33 Java applications required
some manual trace propagation", and programs using raw TCP sockets or SOAP get nothing.

### Step 3 — Out-of-band collection, for two non-obvious reasons

Spans are written to local log files, pulled by a per-machine daemon, and written to a Bigtable
where **a trace is one row and each span is a column** — sparse rows being exactly right for
traces with an arbitrary number of spans.

Why not just return trace data in the RPC response? §2.5.1 gives two reasons and the second is the
one people miss:

- **It would dwarf the application data.** "RPC responses — even near the root of such large
  distributed traces — can still be comparatively small: often less than ten kilobytes... the
  in-band Dapper trace data would dwarf the application data and bias the results of subsequent
  analyses."
- **It assumes perfect nesting.** "in-band collection schemes assume that all RPCs are perfectly
  nested. We find that there are many middleware systems which return a result to their caller
  before all of their own backends have returned a final result."

Cost of the choice: collection is not instantaneous. Median latency from log to repository is
**under 15 seconds**, but the 98th percentile is bimodal — "approximately 75% of the time, 98th
percentile collection latency is less than two minutes, but the other approximately 25% of the
time it can grow to be many hours."

### Step 4 — The overhead budget, itemised

This is the section that got Dapper deployed, and the numbers are worth memorising because they
set the bar for anything you build:

```
   root span creation + destruction ....... 204 ns   (extra cost: allocating a global trace id)
   non-root span .......................... 176 ns
   annotation, span NOT sampled ............. 9 ns   (a thread-local lookup)
   annotation, span sampled ................ 40 ns
   collection daemon .................... < 0.3% of one core, negligible memory
   per span on the wire ................... 426 bytes
   share of production network traffic ... < 0.01%
```

(Measured on a 2.2 GHz x86 server.) The daemon is also "restricted to the lowest possible priority
in the kernel scheduler in case CPU contention arises."

Note the 9-vs-40 ns split for annotations. Making the unsampled path a thread-local read is what
lets Dapper tell developers to annotate freely — and they did: **70% of all spans and 90% of all
traces have at least one application-specific annotation.**

### Step 5 — Sampling, and the two questions it answers differently

Table 2, on a web-search cluster (experimental error 2.5% for latency, 0.15% for throughput):

```
   sampling      avg latency     avg throughput
   1/1              +16.3%           −1.48%
   1/2               +9.40%          −0.73%
   1/4               +6.38%          −0.30%
   1/8               +4.12%          −0.23%
   1/16              +2.12%          −0.08%
   1/1024            −0.20%          −0.06%     ← inside experimental error
```

Tracing everything costs 16% of your latency. So Dapper sampled **one trace in 1024**, uniformly,
and justified it with an argument about the *kind* of question being asked:

> for high-throughput services, aggressive sampling does not hinder most important analyses. If a
> notable execution pattern surfaces once in such systems, it will surface thousands of times.

with the caveat in the very next sentence:

> Services with lower volume — perhaps dozens rather than tens of thousands of requests per second
> — can afford to trace every request; this is what motivated our decision to move towards
> adaptive sampling rates.

Lane 3 of this topic's crate measures exactly the gap between those two sentences:

```
   rate      traces   edge recall   rare-path recall   mean-latency err   p99 err
   1/1        40000         1.000              1.000               0.0%     0.0%
   1/16        2470         1.000              0.062               0.0%     1.5%
   1/1024        39         1.000              0.001               5.8%    25.6%
```

Thirty-nine traces recover **all** of the dependency edges, and **one thousandth** of the rare
paths. The mean latency is unbiased; the p99 is 25.6% off. Same sample, three completely different
verdicts, depending on whether your question is aggregate, rare-event, or tail.

### Step 6 — Two more sampling layers, and one crucial detail

**Adaptive sampling** (§4.4) replaces the uniform probability with "a desired rate of sampled
traces per unit time", so low-traffic workloads sample themselves up automatically. The actual
probability used is recorded *with the trace*, so analytical tools can weight correctly — do not
skip that detail if you build this.

**Collection-time sampling** (§4.6) is a second, independent round, needed because the repository
has its own write-throughput limit (Google generated **over 1 TB of sampled trace data per day**,
retained at least two weeks). And here is the detail that matters most:

> We leverage the fact that all spans for a given trace — though they may be spread across
> thousands of distinct host machines — share a common trace id. For each span seen in the
> collection system, we hash the associated trace id as a scalar `z`, where `0 ≤ z ≤ 1`. If `z` is
> less than our collection sampling coefficient, we keep the span and write it to the Bigtable.
> Otherwise, we discard it. By depending on the trace id for our sampling decision, we either
> sample or discard entire traces rather than individual spans within traces.

**Whole traces, not spans.** Sampling spans independently would leave you with disconnected
fragments — you would keep the data and destroy the causality, which is the only thing you were
collecting it for. The crate's `sampling_keeps_whole_traces` test exists to make you implement
this correctly.

### Step 7 — What it was actually used for

Worth knowing, because it shapes what a trace store must support. The Depot API (DAPI) offers three
access patterns: **by trace id**, **bulk** (MapReduce over billions of traces), and **indexed** —
and the indexing note is a nice storage-engineering aside: "the compressed storage required for an
index into the trace data is only 26% less than for the actual trace data itself", so you cannot
index everything.

Section 6 (worth skimming for the war stories) covers inferring service dependencies, tracking
network usage, tracing shared storage, and — the surprise — using trace data to verify that
security policies hold, "which provide greater assurance than source code audits".

## How to read the paper (with the concepts in hand)

- **§1.** The two requirements and three design goals. Note the framing that adoption depends on
  demonstrable negligibility.
- **§2 + Figures 2–3.** Trace trees and spans. The clock-skew-by-causality remark is in §2.1.
- **§2.2.** The three instrumentation points. Ask yourself which of the three your own stack has.
- **§2.3 + Figure 4.** Annotations, and the configurable upper bound on annotation volume — a
  guardrail against your own users.
- **§2.5.1.** Out-of-band collection. Both reasons; the perfect-nesting one is the subtle one.
- **§4.1–4.3 + Tables 1–2.** The overhead numbers. Memorise the 204 / 176 / 9 / 40 ns figures and
  Table 2's sampling costs.
- **§4.4–4.6.** Adaptive sampling, the "if it surfaces once it will surface thousands of times"
  argument, and trace-id-hash collection sampling.
- **§5.1.** The Depot API and the 26% index remark.
- **§6.** Skim the experience reports; §6.2 (inferring service dependencies) is this topic's lane 1
  in production form.
- **After the paper.** Implement `sampling.rs` and reproduce lane 3, then do exercise 4 — localize
  the fault using only sampled traces, and find the rate at which top-1 accuracy breaks.

## Questions to answer in notes.md

1. Dapper's transparency rests on Google having one RPC framework and one control-flow library.
   List what your own stack would need instrumented, and estimate how many of your services would
   fall into the "manual propagation required" bucket.
2. Explain both reasons for out-of-band collection. Then say what out-of-band costs, using the
   bimodal 98th-percentile collection latency.
3. Annotations cost 9 ns unsampled and 40 ns sampled. Work out why that gap is the design decision
   that made 70%-of-spans annotation coverage possible.
4. Lane 3 shows edge recall at 1.000 with 39 traces and rare-path recall at 0.001. State the
   property of a question that determines which curve it follows, and classify five questions you
   have actually asked of a tracing system.
5. Sampling per trace rather than per span is presented almost in passing. Construct the failure:
   what does a per-span-sampled data set let you compute, and what does it silently get wrong?

## Done when

- [ ] You can draw a trace tree and say what a span carries.
- [ ] You can name the three instrumentation points and the assumption each depends on.
- [ ] You can give both reasons for out-of-band collection.
- [ ] You can quote the overhead numbers and Table 2's sampling costs.
- [ ] You can explain why sampling must be per trace, not per span.
- [ ] Your `sampling.rs` reproduces lane 3's two curves.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Sigelman, Barroso, Burrows, Stephenson, Plakal, Beaver, Jaspan, Shanbhag. *Dapper, a Large-Scale
  Distributed Systems Tracing Infrastructure.* Google Technical Report dapper-2010-1, April 2010 —
  [PDF](https://static.googleusercontent.com/media/research.google.com/en//archive/papers/dapper-2010-1.pdf).
- Fonseca, Porter, Katz, Shenker, Stoica. *X-Trace: A Pervasive Network Tracing Framework.*
  NSDI 2007 — the metadata-propagation ancestor.
- The OpenTelemetry specification, if you want to see this paper's vocabulary standardised.
- Local exercise stub: `topics/43-ops-dependency-graphs/experiments/sampling.rs`.
- Topic 34 (debugging & production diagnosis) — sampling bias and coordinated omission, the
  single-machine version of this problem.
