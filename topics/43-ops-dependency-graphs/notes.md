# Topic 43 notes — Network & IT-ops dependency graphs

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin ops_bench`:

```
== lane 1: one broken service, thirty-four alerts ==
   55 services (4 frontends, 5 infra), 40000 requests, 152 configured edges
   the broken service is infra-0 — SLOW on 55% of calls, not failing

   services alerting above a 5% error rate: 34 of 55
   is the broken service among them? NO
   its own error rate: 0.0040  (baseline is 0.0040)

   ranking                top 3                                  rank of the cause
   by failure count       svc2-9(21618) svc1-4(16654) svc3-8(16151) 35 of 55
   by error rate          frontend-3(0.92) frontend-1(0.90) frontend-0(0.90) 41 of 55

   and every infra leaf looks identical from per-node statistics:
     infra-0   error rate 0.0040, 20 callers   <-- the broken one
     infra-1   error rate 0.0040, 11 callers
     infra-2   error rate 0.0041, 13 callers
     infra-3   error rate 0.0041, 13 callers
     infra-4   error rate 0.0041, 14 callers

   This is a gray failure: the component's own health check is
   green because it is slow rather than wrong, and the errors are
   generated one hop ABOVE it by callers that time out. No sorting
   of a per-service dashboard can find it — the five infra leaves
   are statistically indistinguishable. Only the graph separates
   them, which is what lane 2 is for.

== lane 2: root-cause localization ==
   seed   method                     rank of cause   top-1   top-3
[stub — implement the todo!()s to unlock lane 2]

== lane 3: Dapper sampling — the aggregate question vs the rare one ==
   40000 traces, 113 reachable edges (152 configured), 37907 distinct paths of which 37738 are rare
   full-trace mean latency 68564 us, p99 177420 us

   rate      traces   edge recall   rare-path recall   mean-latency err   p99 err
[stub — implement the todo!()s to unlock lane 3]
```

Lane 1 is seeded, so every figure — 34 of 55
services alerting, the cause not among them at error rate 0.0040; rank 35 by
failure count, 41 by error rate; infra leaves 0.0040–0.0041 — is identical on
every run.

Three mechanics worth memorizing.

**A gray failure never trips its own alert.** The broken service is
*slow*, not wrong, so its error rate stays at baseline while its callers
time out — which means the errors in the storm are manufactured one hop
*above* the cause. Both per-node rankings therefore point away from it,
and error rate points the furthest away of all, straight at the front
ends. Huang et al. call the general condition *differential
observability*; Sherlock encoded it a decade earlier as the *troubled*
state, and that is exactly why its model has three states instead of two.

**The five infra leaves are statistically identical.** 0.0040 to 0.0041.
No sorting of any per-node column separates them. The only thing that
differs is their position in the graph and their correlation with the
symptom — which is the whole argument for this topic existing.

**One sample, three different verdicts.** At 1/1024 (39 traces from
40,000) the dependency graph is recovered *completely*, the rare paths
are 99.9% gone, the mean latency is within 5.8%, and the p99 is 25.6%
off. Before choosing a sampling rate, decide whether your question is
aggregate, rare-event, or tail — they do not have the same answer.

## Predictions (fill BEFORE implementing the stubs)

| question | predicted | measured |
|---|---|---|
| lane 1: services alerting from one fault | | **34 of 55** |
| lane 1: is the broken service alerting? | | **NO** — a gray failure never trips its own alert |
| lane 1: its own error rate | | **0.0040** — exactly the baseline |
| lane 1: rank of the cause by failure count | | **35 of 55** |
| lane 1: rank of the cause by error rate | | **41 of 55** — worse than random |
| lane 1: spread across the 5 infra leaves | | **0.0040–0.0041** — indistinguishable |
| lane 2: failure-count baseline, mean rank | | |
| lane 2: error-rate baseline, mean rank | | |
| lane 2: random walk, mean rank | | |
| lane 2: Sherlock k=1, mean rank | | |
| lane 2: backward-only walk | | |
| lane 2: cost | | |
| lane 3: edge recall at 1/1024 | | |
| lane 3: rare-path recall at 1/16 | | |
| lane 3: rare-path recall at 1/1024 | | |
| lane 3: mean-latency error at 1/1024 | | |
| lane 3: p99 error at 1/1024 | | |

<details>
<summary>Author's prediction</summary>

| question | author's prediction |
|---|---|
| lane 1: services alerting from one fault | ~15 |
| lane 1: is the broken service alerting? | yes |
| lane 1: its own error rate | elevated |
| lane 1: rank of the cause by failure count | mid-table |
| lane 1: rank of the cause by error rate | top-10 |
| lane 1: spread across the 5 infra leaves | visible |
| lane 2: failure-count baseline, mean rank | ~20 |
| lane 2: error-rate baseline, mean rank | ~10 |
| lane 2: random walk, mean rank | top-3 |
| lane 2: Sherlock k=1, mean rank | top-3 |
| lane 2: backward-only walk | slightly worse |
| lane 2: cost | ~100 ms |
| lane 3: edge recall at 1/1024 | ~0.5 |
| lane 3: rare-path recall at 1/16 | ~0.5 |
| lane 3: rare-path recall at 1/1024 | ~0.01 |
| lane 3: mean-latency error at 1/1024 | large |
| lane 3: p99 error at 1/1024 | ~same as mean |

</details>

## Reference numbers for the exercise lanes

Read these after filling the worksheet. What the reference solution printed
(measured 2026-07-27, Apple M3 Pro):

| lane | reference |
|---|---|
| lane 2: failure-count baseline, mean rank | **36.4**, 0/5 top-3 |
| lane 2: error-rate baseline, mean rank | **44.0**, 0/5 top-3 |
| lane 2: random walk, mean rank | **1.0**, **5/5 top-1** |
| lane 2: Sherlock k=1, mean rank | **1.0**, **5/5 top-1** |
| lane 2: backward-only walk | rank **3** vs full walk's 1 |
| lane 2: cost | walk **22.8 ms**, Ferret k=1 **21.4 ms** |
| lane 3: edge recall at 1/1024 | **1.000**, from 39 traces |
| lane 3: rare-path recall at 1/16 | **0.062** |
| lane 3: rare-path recall at 1/1024 | **0.001** |
| lane 3: mean-latency error at 1/1024 | **5.8%** — the mean is unbiased |
| lane 3: p99 error at 1/1024 | **25.6%** — the tail is not |

## Guide-question checklist

- [ ] reading-dapper.md Q1–Q5
- [ ] reading-sherlock.md Q1–Q5
- [ ] reading-pivot-tracing.md Q1–Q5
- [ ] reading-gray-failure.md Q1–Q5

## Paper numbers worth keeping

| fact | source |
|---|---|
| root span create+destroy **204 ns**, non-root **176 ns** | Dapper §4.1 |
| annotation **9 ns** unsampled / **40 ns** sampled (thread-local lookup either way) | Dapper §4.1 |
| daemon **<0.3%** of one core; **426 bytes**/span; **<0.01%** of network traffic | Dapper §4.2 |
| sampling cost: **+16.3%** latency at 1/1, +2.12% at 1/16, **−0.20%** at 1/1024 (inside error) | Dapper Table 2 |
| **>1 TB** of sampled trace data per day, retained ≥2 weeks | Dapper §4.6 |
| collection latency median **<15 s**; p98 bimodal — 75% under 2 min, 25% up to many hours | Dapper §2.5 |
| core instrumentation **<1000 lines C++**, <800 Java; 40 C++ / 33 Java apps needed manual propagation | Dapper §2.2, §3.1–3.2 |
| **70% of spans / 90% of traces** carry an app-specific annotation | Dapper §3.3 |
| sampling is **per trace, not per span** — hash the trace id to z ∈ [0,1] | Dapper §4.6 |
| a compressed index into trace data is only **26% smaller** than the data itself | Dapper §5.1 |
| Sherlock node state is **(P_up, P_troubled, P_down)**; *troubled* = "function but users perceive poor performance" | Sherlock §3.1 |
| noisy-max: with probability **(1−d)** the child escapes a parent's state entirely | Sherlock §3.1.1 |
| a noisy-max node models a load balancer wrong: **25% chance of up with both servers down** | Sherlock §3.1.1 |
| always-troubled / always-down pseudo-causes at **0.001**; router-path edges at **0.9999** | Sherlock §4.2 |
| state propagation **O(3ⁿ) → O(n)** for noisy-max; selector/failover stay exponential but have ≤6 parents | Sherlock §3.1.2 |
| Ferret: **3^r assignment vectors → at most (2r)^k**, error "vanishingly small for k = 4 onwards" | Sherlock §3.2 |
| Observation 3.2 (up root causes need no re-propagation) buys **two orders of magnitude** | Sherlock §3.2 |
| dependency discovery: **10 ms** dependency interval, chance co-occurrence discounted at (10ms)/I | Sherlock §4.1.1 |
| deployment: 40 servers / 34 routers / 54 links / 3 weeks; agent report <40 KB, 10⁵ agents ≈ **10 Mbps** | Sherlock §5–6 |
| Pivot Tracing: `Q1 ⋈ Q2` on Lamport's →, evaluated **in-band via baggage** | Pivot §3–4 |
| advice primitives: **OBSERVE, UNPACK, FILTER, PACK, EMIT**; no jumps, no recursion, guaranteed to terminate | Pivot §3 |
| **process-level (intermediate) aggregation** reduces one query's *emitted* tuples from **~600/s to 6/s** per DataNode; Table 3's rewrites target *packed* (baggage) tuples instead | Pivot §4 |
| "all users of HBase pay the **10% performance overhead**" of SchemaMetrics | Pivot §2.3 |
| gray failure = **differential observability**: the app sees a problem, the observer does not | Gray Failure §2 |

## Cross-topic threads (worked)

- **Topic 38 / 42 ↔ 43**: personalized PageRank for the third time.
  HippoRAG seeds it with query entities, Pixie with recent engagements,
  MonitorRank-style RCA with alerting front ends. Same primitive, three
  domains, and the same justification every time — a walk's cost is set
  by the step budget, not by the graph.
- **Topic 34 ↔ 43**: this is topic 34 at cluster scale. Coordinated
  omission there, sampling bias here; flame graphs there, trace trees
  here; and in both cases the measurement apparatus is the thing most
  likely to be lying to you.
- **Topic 37 ↔ 43**: the tail-at-scale arithmetic explains *why* one slow
  dependency becomes 34 alerts, and hedged requests are among the few
  mitigations that work against a gray failure — precisely because they
  do not require anybody to declare the slow component dead.
- **Topic 35 ↔ 43**: a gray failure is frequently the *trigger* and a
  retry storm the *sustaining loop* of a metastable failure. Lane 1's
  timeout-generated errors are that loop in miniature.
- **Topic 10 ↔ 43**: Pivot Tracing's Table 3 is predicate and aggregate
  pushdown; the 600 → 6 tuples/s result is §4's process-level
  pre-aggregation. Both are wins an optimizer exists to produce — the
  novelty is only *where* they are applied.
- **Topic 26 ↔ 43**: a real trace pipeline cannot store per-edge latency
  raw. Histograms, t-digests and count-min sketches are what M43's edge
  weights have to be, and the p99 row of lane 3 is the reason.
- **Topic 40 ↔ 43**: the same graph question with the arrows reversed —
  *what can reach tier zero* versus *what reached this symptom*. A
  dominator pass would price a single-node remediation here too, and the
  flat-directory result (no single cut helps) has an operational twin.
- **Topic 27 ↔ 43**: trace ingest is a stream and the dependency graph is
  a materialized view over it. Recompute or maintain — the same question
  as topic 40's derived BloodHound edges and topic 41's Leopard closure.
- **Topic 21 ↔ 43**: Sherlock's model is tuned rather than verified. What
  would it take to state and check its invariants?

## Open questions

- Exercise 4 is the one that matters and is not answered by any of the
  four papers: **at what sampling rate does localization break?** Edge
  recall survives 1/1024 and rare-path recall does not, and the answer
  presumably sits between them — but nobody seems to have measured it.
- Ferret's k=1 works here because there is one fault. Real incidents
  frequently have two (a deploy plus a capacity limit). Exercise 2 adds
  the second; the question is whether the (2r)^k cost is affordable at
  the scale where you would need it.
- Both localization methods score *services*. Operators act on
  *deployments* and *config changes*. What does the graph look like when
  the nodes are change events rather than components, and does anything
  here transfer?
- Pivot Tracing evaluates the happened-before join in-band because it can
  instrument everything. A database given only stored traces must do it
  post-hoc. Are the Table 3 rewrites still the right ones when the join
  is a stored-graph pattern match rather than a baggage lookup?
