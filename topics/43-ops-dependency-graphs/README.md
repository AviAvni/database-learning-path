# Topic 43 — Network & IT-Ops Dependency Graphs

Last of six graph use-case deep dives, and the one where the graph is
*inferred* rather than given. Nobody writes down a microservice
dependency graph; you reconstruct it from traces, and then you use it to
answer the question every incident starts with — **which of these fifty
alerts is the cause?** **Dapper** (2010) is where the graph comes from,
and where the sampling trade lives. **Sherlock** (SIGCOMM'07) is how you
reason over it probabilistically. **Pivot Tracing** (SOSP'15) is the
database paper hiding inside an operations topic: a relational operator,
the happened-before join, with predicate pushdown. And **Gray Failure**
(HotOS'17) is why the whole exercise is necessary — the broken component
is frequently the one whose own dashboard is green.

## The problem, measured (bench lane 1, provided — runs today)

```
   55 services (4 frontends, 5 infra), 40000 requests, 152 configured edges
   the broken service is infra-0 — SLOW on 55% of calls, not failing

   services alerting above a 5% error rate: 34 of 55
   is the broken service among them? NO
   its own error rate: 0.0040  (baseline is 0.0040)

   ranking                top 3                                    rank of the cause
   by failure count       svc2-9(21618) svc1-4(16654) svc3-8(16151)   35 of 55
   by error rate          frontend-3(0.92) frontend-1(0.90) ...       41 of 55

   and every infra leaf looks identical from per-node statistics:
     infra-0   error rate 0.0040, 20 callers   <-- the broken one
     infra-1   error rate 0.0040, 11 callers
     infra-2   error rate 0.0041, 13 callers
     infra-3   error rate 0.0041, 13 callers
     infra-4   error rate 0.0041, 14 callers
```

One slow shared dependency, thirty-four alerts, and the cause is in the
bottom half of every ranking a dashboard can offer. This is a **gray
failure**: `infra-0` is *slow*, not wrong, so its own error rate never
leaves the baseline, and the errors are manufactured one hop above it by
callers that time out. Huang et al. call the general condition
**differential observability** — the system's own failure detector and
its users disagree about whether it is healthy.

Sherlock encoded the same insight ten years earlier by refusing a binary
health model. Every node in its Inference Graph carries a three-tuple
`(P_up, P_troubled, P_down)`, and *troubled* is defined as exactly this
case: "servers or links continue to function but users perceive poor
performance."

The five infrastructure leaves are statistically indistinguishable —
0.0040 to 0.0041, and the one with the most callers is the broken one
only by coincidence of this seed. **No per-node statistic can separate
them. Only the graph can.**

## Localization: two ways, both work

```
   RANDOM WALK (MonitorRank shape)      INFERENCE (Sherlock / Ferret)
   ───────────────────────────────      ─────────────────────────────
   needs: topology + a correlation      needs: topology + a propagation
          signal, no model of how              model
          failure spreads
   walk backward from the symptom,      score every ASSIGNMENT VECTOR
   weighting edges by correlation       (a state per root cause) by how
                                        well it explains the observations
   three edge types:                    3^r vectors, so evaluate only
     backward  → toward the causes      those with ≤ k abnormal nodes:
     forward   → escape a dead end      at most (2r)^k
     self      → stay if nothing
                 correlates better
```

Measured lane 2, five topologies:

| method | mean rank of cause | top-1 | top-3 |
|---|---|---|---|
| by failure count | 36.4 | 0/5 | 0/5 |
| by error rate | 44.0 | 0/5 | 0/5 |
| **random walk** | **1.0** | **5/5** | **5/5** |
| **Sherlock k=1** | **1.0** | **5/5** | **5/5** |

```
   200k-step walk 22.8 ms (rank 1), backward-only variant rank 3
   Sherlock k=1 over 55 candidates x 4 frontends: 21.4 ms (rank 1)
```

The backward-only ablation is the instructive one. A walk that can only
move toward callees drains into the leaves and cannot climb back out of
a dead end, so its mass spreads over all five infra nodes instead of
concentrating. The forward and self edges are what let the correlation
weights bite.

Ferret's real contribution is not the scoring function but the search
pruning. There are `3^r` assignment vectors over `r` root causes, which
is hopeless; Observation 3.1 — *"it is very likely that at any point in
time only a few root-cause nodes are troubled or down"* — cuts it to at
most `(2r)^k` by considering only vectors with at most `k` abnormal
nodes, with approximation error that "decreases exponentially with k and
becomes vanishingly small for k = 4 onwards". Observation 3.2 (a root
cause assigned *up* needs no re-propagation) buys another two orders of
magnitude. And in this crate's k = 1 implementation, the detail that
makes it work is clamping the fitted severity to `[0, 1]`: a service
that simply is not on enough requests would need a severity above 1 to
explain the observed rates, and the clamp makes it pay for that.

## Sherlock's Inference Graph: three node types and three meta-nodes

```
   root-cause nodes    physical components: a host, a service (IP,port),
                       a router, an IP link
   observation nodes   what you can actually measure — a client's
                       response time for a service
   meta-nodes          the glue, and where the modelling happens:

     noisy-max   any parent down ⟹ child down; any parent troubled ⟹
                 child troubled. "Noisy" because unless the dependency
                 probability d is 1.0, with probability (1−d) the child
                 escapes its parent's state entirely.
     selector    load balancing — child picks parent1 with probability d.
                 A noisy-max node cannot express this: it would give a
                 client a 25% chance of being up when both servers are
                 down, which is obviously wrong.
     failover    primary/secondary. The child is unaffected while the
                 primary is up or troubled; only when the primary is
                 down does the secondary's state matter.
```

Two details worth stealing. Every Inference Graph gets two pseudo
root-causes, **always troubled** and **always down**, wired to every
observation node at probability **0.001** — "1 in 1000 failures are
caused by a component not in our model". An escape hatch for model
error, priced explicitly. And computing a child's state from `n` parents
is `O(3ⁿ)` in general, reduced to **`O(n)`** for noisy-max nodes by a
product formula; selector and failover stay exponential but never have
more than six parents.

The dependency graph itself is *discovered*, not configured: Sherlock's
agents watch packets and infer that accessing B depends on A when A's
traffic precedes B's within a **10 ms dependency interval**, discounting
chance co-occurrence at `(10ms)/I` where `I` is the average interval
between accesses.

## Dapper: where the graph comes from, and what sampling costs

Dapper's model is the one you already run under a different name —
OpenTelemetry's trace/span/annotation/context-propagation vocabulary is
Dapper's, near-verbatim. Two design decisions are worth the read.

**Out-of-band collection**, for two reasons that are easy to get wrong.
In-band trace data returned in RPC responses "would dwarf the
application data and bias the results of subsequent analyses" — spans
are small but traces have thousands of them and responses are often
under ten kilobytes. And in-band schemes "assume that all RPCs are
perfectly nested", which middleware that returns before its backends
finish simply violates.

**The overhead budget**, which is why anyone allowed it in production:

```
   root span create + destroy ....... 204 ns
   non-root span .................... 176 ns
   annotation, span not sampled ....... 9 ns   (a thread-local lookup)
   annotation, span sampled .......... 40 ns
   collection daemon ................ <0.3% of one core
   per span on the wire ............. 426 bytes, <0.01% of network traffic
```

And the sampling table that decided the design, on a web-search cluster:

| sampling | avg latency | avg throughput |
|---|---|---|
| 1/1 | **+16.3%** | −1.48% |
| 1/2 | +9.40% | −0.73% |
| 1/16 | +2.12% | −0.08% |
| 1/1024 | −0.20% | −0.06% |

(The last row is inside the 2.5% / 0.15% experimental error — it is
"free".)

Lane 3 measures what that rate actually buys and costs:

```
   rate      traces   edge recall   rare-path recall   mean-latency err   p99 err
   1/1        40000         1.000              1.000               0.0%     0.0%
   1/4         9926         1.000              0.249               0.0%     0.9%
   1/16        2470         1.000              0.062               0.0%     1.5%
   1/64         638         1.000              0.016               0.7%     0.6%
   1/256        149         1.000              0.004               3.8%     3.9%
   1/1024        39         1.000              0.001               5.8%    25.6%
```

Two questions, two completely different answers. *"What is the
dependency graph?"* is an **aggregate** question: every edge is
exercised constantly, so 39 traces recover all 113 of them. *"What
happened on that one weird path?"* is a **rare-event** question, and
recall falls roughly linearly with the rate — a thousandth of the
traffic finds a thousandth of the rare paths. Dapper says both things in
consecutive sentences: "if a notable execution pattern surfaces once in
such systems, it will surface thousands of times", and "services with
lower volume — perhaps dozens rather than tens of thousands of requests
per second — can afford to trace every request".

Note also which *metrics* survive. The mean is unbiased under sampling
and stays within 5.8% even at 1/1024; the p99 is made of the tail, and
at 39 traces the tail is a handful of samples — 25.6% off.

One caveat stated rather than glossed: edge recall hits 1.000 at 39
traces because in *this* topology almost every request touches almost
every edge. A real service graph has far more path diversity, so the
curve bends sooner. Exercise 5 adds skew and finds where. The shape of
the result survives; the exact rate does not.

And the detail that makes sampling work at all: the decision is made
**per trace, not per span**. Dapper hashes the trace id to a scalar
`z ∈ [0,1]` and keeps the whole trace if `z` is below the coefficient.
Sampling spans independently would shred every trace into disconnected
fragments and destroy exactly the causal structure you are paying to
collect.

## Pivot Tracing: the happened-before join

The database paper in the set. Pivot Tracing lets you write a query
across tracepoints in *different processes*, joined on causality:

```
   Q1 ⋈ Q2   produces t1t2 for all t1 ∈ Q1, t2 ∈ Q2 with t1 → t2
             (Lamport's happened-before, within one request)

   From, Union, σ (selection), Π (projection), A (aggregation),
   GroupBy, GroupByAggregation, and ⋈ — the happened-before join
```

The naive evaluation is what everyone builds first: ship every tuple to
a central node and join there. Pivot Tracing instead carries tuples
along the request in **baggage** — a per-request container propagated
across thread, process and machine boundaries — so `PACK` at one
tracepoint and `UNPACK` at another evaluates the join **in situ**,
during execution, with no central collection at all. (Baggage is the
W3C `baggage` header, if you have met it in OpenTelemetry.)

And then the part that will look familiar from topic 10: Table 3 is a
set of **query rewrite rules** that push projection, selection and
aggregation as close as possible to the source tracepoints —
`Π_{p,q}(P ⋈ Q) → Π_p(P) ⋈ Π_q(Q)`, `σ_q(P ⋈ Q) → P ⋈ σ_q(Q)`, and so
on. Table 3's own target is the number of tuples **packed into the
baggage** — happened-before joins carried in-band. The separately
measured ~600 tuples/s → **6 tuples/s per DataNode** headline is §4's
*process-level (intermediate) aggregation*, which aggregates emitted
tuples within each process and reports globally once per second. Two
optimizations, two metrics: keep them apart. Predicate pushdown and
join placement, in a tracing system, plus in-process pre-aggregation
for a hundredfold reduction in emitted-tuple traffic.

## Reading guides

1. [reading-dapper.md](reading-dapper.md) — Dapper: trace trees, out-of-band collection, and the sampling economics.
2. [reading-sherlock.md](reading-sherlock.md) — Sherlock SIGCOMM'07: the Inference Graph, meta-nodes, and Ferret's pruning.
3. [reading-pivot-tracing.md](reading-pivot-tracing.md) — Pivot Tracing SOSP'15: the happened-before join, baggage, and pushdown.
4. [reading-gray-failure.md](reading-gray-failure.md) — Gray Failure HotOS'17: differential observability, and why lane 1 is hard.

## Experiments

```
cd experiments
cargo test              # 4 provided tests pass; 9 fix the contract for your stubs
cargo run --release --bin ops_bench
```

- `services.rs` (PROVIDED) — the microservice topology generator, the
  gray-failure workload (slow dependency + caller timeouts), traces with
  paths/edges/latency, the two per-node baselines
  (`rank_by_failures`, `rank_by_error_rate`), the symptom correlation,
  and `participation` (P(service on path | entry frontend)) — the weak,
  realistic observable Ferret has to work from.
- `rca.rs` (stub) — `random_walk_rca` (three edge types, correlation
  weights, restart) and `sherlock_single_fault` (Ferret at k = 1, with
  the severity clamp).
- `sampling.rs` (stub) — `sample` (whole traces), `edge_recall`,
  `rare_path_recall`. `mean_latency_us` and `p99_latency_us` provided.

Bench lanes: 1 = the alert storm (provided, above). 2 = localization
(reference: per-node baselines mean rank 36.4 and 44.0 with 0/5 top-3;
walk and Ferret both mean rank 1.0 with 5/5 top-1; backward-only walk
ranks 3rd). 3 = sampling (reference: edge recall 1.000 throughout,
rare-path recall 1.000 → 0.001, p99 error 0% → 25.6% as the rate goes
1/1 → 1/1024).

## Exercises

1. Implement the stubs until all 13 tests pass and lanes 2–3 print.
2. **Two faults at once.** Ferret's k = 1 assumes a single abnormal root
   cause. Break two services simultaneously and watch it fail, then
   implement k = 2 — `2²·C(r,2)` assignment vectors — and measure both
   the accuracy recovery and the cost. Where does k stop paying?
3. **Meta-nodes.** Add a load balancer in front of two replicas and
   implement Sherlock's **selector** truth table. Show concretely that a
   noisy-max node gets it wrong: with both replicas down it gives the
   client a 25% chance of being up.
4. **Localize under sampling.** The question the whole topic converges
   on: re-run lane 2 using only the sampled traces from lane 3, sweeping
   the rate. At what sampling rate does top-1 accuracy break, and is it
   nearer the edge-recall curve or the rare-path curve?
5. **Path skew.** Lane 3's edge recall saturates unrealistically early
   because every request touches nearly every edge. Add per-request
   feature flags so that only some requests exercise some subgraphs,
   then re-measure. Where does the curve bend, and what does that imply
   for a service with a rarely-used code path?
6. **Adaptive sampling.** Dapper's later design targets a *rate of
   sampled traces per unit time* rather than a uniform probability, so
   low-traffic services sample themselves up. Implement it per front end
   and re-measure lane 3's rare-path recall at the same total trace
   budget.
7. **The happened-before join.** Implement `Q1 ⋈ Q2` over the trace set:
   given two tracepoint predicates, return the pairs where one causally
   precedes the other within a request. Then implement Pivot Tracing's
   Table 3 pushdown rules and measure the reduction in tuples that have
   to cross the join — that is Table 3's metric (packed tuples). The
   paper's ~600/s → 6/s figure is a different one: emitted tuples under
   process-level aggregation (§4). Measure both if you can.

## Cross-topic threads

- **Topic 38 / 42 ↔ 43**: personalized PageRank, a third time.
  HippoRAG seeds it with query entities, Pixie with recent engagements,
  MonitorRank-style RCA with alerting front ends. Same primitive, three
  domains — and in all three the reason it is chosen is that a walk's
  cost is set by the step budget, not the graph.
- **Topic 34 (debugging & production diagnosis) ↔ 43**: this topic is
  topic 34's material at cluster scale. Coordinated omission there,
  sampling bias here; flame graphs there, trace trees here.
- **Topic 37 (distributed query execution) ↔ 43**: the tail-at-scale
  fan-out arithmetic explains *why* one slow dependency turns into 34
  alerts, and hedged requests are one of the few mitigations that work
  against a gray failure.
- **Topic 10 (query planning) ↔ 43**: Pivot Tracing's Table 3 is
  predicate pushdown and join placement; the 600 → 6 tuples/s result is
  §4's process-level pre-aggregation. Both are wins an optimizer exists
  to produce — pushdown and partial aggregation.
- **Topic 26 (probabilistic structures) ↔ 43**: a real trace pipeline
  cannot store per-edge latency raw. Histograms, t-digests and
  count-min sketches are what M43's edge weights have to be.
- **Topic 40 (attack graphs) ↔ 43**: the same graph question with the
  arrows reversed — there, *what can reach tier zero*; here, *what
  reached this symptom*. Dominators would price a single-node
  remediation here too.
- **Topic 27 (streaming & IVM) ↔ 43**: trace ingest is a stream and the
  dependency graph is a materialized view over it. Whether edge weights
  are recomputed or maintained is the same question, again.

## Capstone M43 — the observability path on the Rust graph engine

- A **trace ingest pipeline** writing spans into M31's storage as a
  dependency graph, with edge weights (call counts, error counts,
  latency distributions) maintained incrementally and sketched with
  topic 26's structures rather than stored raw.
- A **localization procedure** running both the correlation-weighted
  walk over the topic-18 CSR and Ferret's k ≤ 2 assignment-vector
  search, returning a ranked candidate list.
- A **happened-before join operator** in the query engine: a Cypher
  procedure joining two tracepoint streams on causal precedence within a
  request, with Pivot Tracing's Table 3 rewrites.
- Deliverable numbers: span ingest rate and bytes-per-span against
  Dapper's 426 bytes; localization latency on a 10,000-service graph vs
  `ops_bench` lane 2; **top-1 accuracy under sampling** — the number the
  whole topic converges on; and happened-before join cost with and
  without pushdown, against Pivot Tracing's 600 → 6 tuples/s (which is
  its emitted-tuple, aggregation-side figure).
