# Gray failure: when the system and its users disagree

Six pages, one idea, and it explains why lane 1 of this topic is hard. Cloud systems are built on
the assumption that failures are detectable: a component is up or it is down, a health check says
which, and the redundancy machinery does the rest. Huang et al.'s observation is that the failures
that actually cause long outages are not like that. The component is *degraded* — slow,
intermittently wrong, dropping a fraction of packets — and, critically, **its own failure detector
does not notice while its users are suffering**. They name the general condition **differential
observability** (§3.2), and once you have the term you start seeing it in every postmortem you read.

This is a paper, not a codebase, so every claim below is anchored to the section, table or figure of
*Gray Failure: The Achilles' Heel of Cloud-Scale Systems* (Huang et al., HotOS 2017) that states it;
each was re-checked against the PDF while writing this chapter. Where a figure comes from this
repo's own crate instead, it is marked as a lane of `ops_bench` and traced to `notes.md`.

## The problem in one sentence

**The failure detector says the component is healthy, the applications using it say it is not, and
because the detector is what triggers recovery, nothing happens.**

## The concepts, step by step

### Step 1 — The model: a system, an observer, a reactor, and an app that disagrees

> **In:** the informal intuition that "degraded but up" breaks health checks.
> **Out:** the paper's four-entity model (§3.1) and the precise definition of gray failure as one
> quadrant of a two-by-two table (§3.2). Every later step is commentary on this table.

The framing is deliberately minimal (§3.1, Figure 2). There is a **system** that provides a service
(a storage service, a data-center network, an IaaS platform) and an **app** that uses it (a web
application, a user, an operator — and "one system may be an app for another system"). Inside the
system live two more entities, and the paper is careful to keep them distinct:

```
   the OBSERVER  — "actively or passively gathers information about
                    whether the system is failing or not": the failure
                    detector, the health check, the monitoring probe.
   the REACTOR   — "based on the observations, takes actions to recover
                    the system": the failover logic, the load-balancer
                    eviction, the quorum reconfiguration.
   the APP       — makes its OWN observations, "typically based on
                    application-specific, end-to-end metrics such as
                    query latency and remote I/O status".
```

The observer observes; the reactor acts on what the observer reported. Keeping them separate is the
whole point: the reactor is only ever as good as the observer's view, so if the observer is blind,
the reactor is inert no matter how well it is built. Both observer and reactor "are considered part
of the system" (§3.1).

Now the two-by-two (Table 1, §3.2). Rows are the observer's verdict (`Sgood` / `Sbad`); columns are
the app's (`Agood` / `Abad`):

```
                    app good (Agood)     app bad (Abad)
   observer good      ➊ no failure         ➋ GRAY FAILURE
   observer bad       ➌ "good kind"        ➍ fail-stop / crash
```

- **➊** neither sees a problem — no failure.
- **➋** the app observes a failure but the observer does not. **This is gray failure**, "since users
  are suffering but the reactor will not be invoked to help fix the problem" (§3.2).
- **➌** the observer sees a problem the app has not felt yet. This is *also* differential
  observability, "but of the good kind": the observer "will take proactive steps to repair it"
  before the app is affected. The paper flags it as problematic *only if it is a false positive* —
  "but that is a different kind of problem than gray failure" (§3.2). It is not simply "annoying but
  safe"; used well it is the system fixing itself early.
- **➍** both agree the system is failing — "crash and fail-stop failures fall under this case", and
  the recovery machinery works.

The exact definition, worth quoting because people paraphrase it wrongly: a system experiences gray
failure "when at least one app makes the observation that system is unhealthy, but observer observes
that system is healthy" (§3.2). Cell ➋, and only cell ➋.

### Step 2 — Why redundancy does not save you, with the fan-out arithmetic

> **In:** the model from Step 1, plus the industry reflex that redundancy buys availability.
> **Out:** the §2.1 result that redundancy can *lower* availability under gray failure, and the
> one formula that makes it quantitative.

Every fault-tolerance mechanism you own is keyed on the *observer's* view, because the reactor acts
only on what the observer reports. Failover triggers when the detector says the primary is down. A
load balancer ejects a backend when a probe fails. A quorum excludes a replica when it stops
responding. Under gray failure (cell ➋) none of that fires — and worse, the degraded component keeps
*accepting* work, because it is up, so a round-robin or least-connections balancer may send it
*more*.

§2.1 ("High redundancy hurts") turns this into arithmetic. Consider a front-end that must fan out a
request to many back-ends and wait for almost all to respond. With `n` core switches and fan-out
factor `m`, the probability that a *given* core switch is traversed by a request is:

```
   P(switch on path) = 1 − ((n − 1) / n)^m        (Gray Failure §2.1)
```

Read the limit: as `m` grows, `((n−1)/n)^m → 0`, so `P → 100%` — "each such request has a high
probability of involving every core switch." So a gray failure at *any one* switch delays *nearly
every* front-end request. And now the counter-intuitive part: "the more core switches there are, the
more likely at least one of them will experience a gray failure." Adding redundancy adds surfaces
that can silently degrade, and the fan-out guarantees each one touches almost every request. The
mechanism that was supposed to mask failures is the mechanism that spreads this one.

This is the connective tissue with topic 37: a fan-out to N backends takes the maximum of N
latencies, so one degraded backend in a hundred contaminates a large fraction of requests. Hedged
requests are one of the few mitigations that work against a gray failure precisely because they do
not require anybody to declare the slow component dead.

### Step 3 — Why detection is genuinely hard, not merely neglected

> **In:** the fact from Step 2 that recovery depends on the observer noticing.
> **Out:** three structural reasons the observer misses the problem — drawn from the §2.2 "under the
> radar" incident — so you stop treating gray failure as a monitoring-team oversight.

The paper is careful not to make this a story about lazy monitoring; its §2.2 example is a driver
bug where "the failure detector, a remote compute manager, does not observe any problems because it
does not exercise the VM's external network" — it reads heartbeats over a local RPC path the bug
does not touch. Generalise that into three structural reasons:

- **The observer is usually cheap and shallow**, by necessity: a health check that exercised every
  code path would cost as much as the workload. So it checks liveness, not correctness, and
  certainly not latency under contention.
- **The observer's workload differs from the app's** (this is the §2.2 incident exactly). A probe on
  a path the fault does not touch cannot see the fault; app observations are "based on
  application-specific, end-to-end metrics" (§3.2) that exercise different paths.
- **Degradation is often partial and intermittent.** A disk slow on 1% of writes, a NIC dropping a
  small fraction of packets, a memory leak that only matters after eight hours. Any single probe is
  likely to miss it.

The paper's word for the underlying gap is *observational differences* (§2.2): the app and the
observer are looking at different things, so of course they can disagree.

### Step 4 — Gray failures escalate, which is why they end up in postmortems

> **In:** the persistent, undetected degradation of Step 3.
> **Out:** the §3.3 temporal model — latent → gray → complete failure, a ➊→➋→➍ walk across Table 1
> — and why the postmortem always arrives after the trail has gone cold.

§3.3 ("Temporal evolution") gives the lifecycle explicitly: "initially, the system experiences minor
faults (latent failure) that it tends to suppress. Gradually, the system transits into a degraded
mode (gray failure) that is externally visible but which the observer does not see. Eventually, the
degradation may reach a point that takes the system down (complete failure), at which point the
observer also realizes the problem." In Table 1's coordinates this "manifests as a transition from
➊ to ➋ to ➍." The canonical example the paper gives is a memory leak.

Operationally that ordering is the trap: by the time the failure is detectable (cell ➍), you are
diagnosing the crash rather than the degradation that caused it, and the trail is cold. §2.3
("Recovery that kills") is the worked horror story — a storage manager keeps routing writes to a
capacity-degraded server it cannot see is degraded, crashing and rebooting it in a loop until a
cascading failure takes down the cluster.

If you have read topic 35, this is a metastable failure with a gray failure as its trigger: the
sustaining feedback loop (retries against a slow dependency) outlives whatever started it. And in
this topic's lane 1 you can see the mechanism in miniature — the broken service is slow, its callers
time out, and the *timeouts* are what generate the error storm.

### Step 5 — What to do about it, and the ranking arithmetic that proves you must

> **In:** the escalation from Step 4 and the definition from Step 1.
> **Out:** the paper's four solution directions (§4), the way the two localization methods in this
> topic instantiate them, and the lane-1 numbers — worked out — that show a per-node health check
> ranking the broken service *below* the median.

The paper's §4 outlines four directions, and it is worth being precise about them because the naive
summary ("just watch the app side") is not quite what the paper says:

- **§4.1 Multi-dimensional health monitoring.** Move "from singular failure detection (e.g., with
  heartbeats) to multi-dimensional health monitoring" — the vital-signs analogy: not just a
  heartbeat but temperature and blood pressure too.
- **§4.2 Approximating application views.** Eliminating differential observability entirely is
  "practically infeasible", so instead the system should "measure metrics that approximate the
  observations of its apps." Note carefully: the paper's own example *is* a probe — "send probes to
  measure server-to-server latency and reachability to emulate observations of the network… as in
  Pingmesh." So the fix is not "stop using probes"; it is "use probes/metrics that approximate the
  app's end-to-end experience," with the caveat that "overly active probing may further burden an
  already degraded system."
- **§4.3 Leveraging the power of scale.** Because gray failure "is often due to isolated
  observations of an observer," aggregate observations "from a large number of different components
  that are complementary to each other" and apply statistical inference. This is the direction the
  two localization methods in this topic live in.
- **§4.4 Harnessing temporal patterns.** Find the temporal precursors (the latent-failure prelude)
  to warn before apps are affected.

The two localization methods in this topic are answers to §4.3 — cross-component inference from
app-side signals:

- **Sherlock** (2007) refuses a binary health model outright. Its *troubled* state — "servers or
  links continue to function but users perceive poor performance" — is differential observability
  encoded in the data model, and its **observation nodes are client-side measurements**, never
  server-side health checks.
- **The random walk** never asks any component whether it is healthy. It only uses the topology and
  the correlation between a component being on a path and that request failing — a purely app-side
  signal.

Now lane 1 measures what a per-node health check does *without* any of this — and the ranking
arithmetic is the point, so work it through rather than just reading the numbers:

```
   the broken service is infra-0 — SLOW on 55% of calls, not failing
   services alerting above a 5% error rate: 34 of 55
   is the broken service among them? NO
   its own error rate: 0.0040  (baseline is 0.0040)

   ranked 35 of 55 by failure count, 41 of 55 by error rate
   and all five infra leaves sit at 0.0040-0.0041 — indistinguishable
```

Why 41st by error rate? Because being slow never sets infra-0's *own* error flag. In the generator
its own failures come only from the `baseline_error` = 0.004 coin, exactly like every other healthy
leaf, so its error rate is 0.0040 — pinned to the baseline. Fifty-four services, and 40 of them
happen to have a slightly higher error rate by chance or by manufacturing timeouts, so the *actually
broken* one lands 41st. A per-error-rate ranking does worse than random on it.

Why then is it 35th by failure *count*, a little higher? Because infra-0 is the infra leaf with the
most callers (20 of them), so it receives the most calls; the same baseline 0.4% rate applied to a
larger call volume produces more failures in *absolute* count, nudging it up from 41st to 35th. But
still bottom-half: the 34 services above it are its *callers*, which time out on its slowness
(`timeout_prob` = 0.7) and "report an error of its own — so the errors appear one hop above the thing
that is actually broken" (services.rs). Those 34 callers are exactly the "34 of 55 alerting", and
the broken service sits at rank 35, just underneath the storm it caused. That gap — cause below the
median, symptoms above the alert line — is differential observability made numeric. Lane 2 shows
both graph methods recovering it at mean rank 1.0.

### Step 6 — The transferable habit

> **In:** everything above — the model, the escalation, the numbers.
> **Out:** two questions to ask of any system you operate, and one design principle that generalises
> past infrastructure.

Two questions to ask of any system you operate:

1. **Whose view triggers my recovery?** If the answer is a health endpoint the component serves
   about itself, you have a differential-observability gap by construction: the observer and the
   thing it observes are the same component (§3.1's observer-inside-the-system).
2. **What would a degraded-but-up component look like in my telemetry?** If the honest answer is
   "like a healthy one", you will find out about it from a user — cell ➋, every time.

And a design note that generalises: any time a system's self-assessment drives its own remediation,
ask what happens when the self-assessment is the thing that is broken. That is §2.3's storage
manager, and it is the reactor acting on a blind observer.

## How to read the paper (with the concepts in hand)

It is six pages; read all of it. But read it in this order:

- **§3 (the model) first** — §3.1 Terminology (the system / observer / reactor / app quartet and
  Figure 2) and §3.2 Differential observability (the four-cell Table 1). The term is *defined* in
  §3.2 and the rest of the paper is commentary; §3.3 adds the temporal ➊→➋→➍ walk.
- **§2 (the examples) second**, now that you have the frame: §2.1 High redundancy hurts (the fan-out
  formula), §2.2 Under the radar (the driver-bug detector gap), §2.3 Recovery that kills (the
  cascading storage loop), §2.4 The blame game. The value of the examples is recognising the shape,
  not memorising the incidents.
- **§4 (directions) last.** Read the four directions and note which of this topic's methods each one
  predicts. Read the escalation argument against topic 35's metastable-failure paper and note that
  they describe the same lifecycle from two ends.
- **After the paper.** Re-read lane 1's output and identify, for each row, which of the two views it
  represents. Then do exercise 4 of this topic — localize under sampling — because "how much
  observability do I actually need to close the gap?" is the practical form of §4.2.

## Questions to answer in notes.md

1. Draw the four-cell observer × app table (§3.2) and put a real incident you have seen in each cell.
   Which cell was hardest to diagnose, and did the model predict that?
2. Lane 1's broken service has an error rate exactly at baseline. Write the health check that would
   have caught it, then estimate what that health check costs to run continuously against every
   component. Is it affordable? Tie your answer to §4.2's warning that "overly active probing may
   further burden an already degraded system."
3. The paper argues gray failures escalate into fail-stop ones (§3.3). Connect that to topic 35's
   metastable failures: which is the trigger and which is the sustaining loop, and where would you
   cut?
4. Sherlock's *troubled* state predates this paper by ten years. Why do you think the industry still
   ships binary health checks — and what would have to change in a load balancer's interface to
   express three states?
5. Both localization methods in lane 2 use only app-side signals — §4.3's "leveraging scale." 
   Construct a gray failure that defeats them both, and say what additional observation would be
   needed.

## Done when

Answer each before unfolding it.

- [ ] You can define differential observability and draw the four-cell table.

  <details><summary>Answer</summary>

  Gray failure is differential observability: "at least one app makes the observation that system is
  unhealthy, but observer observes that system is healthy" (§3.2). The four-cell table has the
  observer's verdict on the rows (`Sgood`/`Sbad`) and the app's on the columns (`Agood`/`Abad`): ➊
  both good = no failure; ➋ `Sgood`/`Abad` = **gray failure** (users suffer, reactor never invoked);
  ➌ `Sbad`/`Agood` = differential observability "of the good kind", where the observer repairs
  proactively before the app feels it (bad only if it is a false positive); ➍ both bad = fail-stop,
  where recovery works.

  The trap is ➋ and only ➋. ➌ is the *same* asymmetry pointed the other way and is usually benign —
  do not lump it in with gray failure.

  </details>

- [ ] You can explain why redundancy mechanisms are inert under gray failure.

  <details><summary>Answer</summary>

  Because every fault-tolerance mechanism is driven by the *reactor*, and the reactor acts only on
  the *observer's* view (§3.1). Under gray failure the observer sees health (cell ➋), so nothing
  fails over, nothing is evicted, no quorum reconfigures. Worse, the degraded component keeps
  accepting work because it is "up", so a round-robin or least-connections balancer may route it
  *more* traffic.

  §2.1 makes it quantitative: with `n` core switches and fan-out `m`, a given switch is on a request
  with probability `1 − ((n−1)/n)^m`, which tends to 100% as `m` grows — so one gray-failing switch
  delays nearly every request, and adding switches only adds more surfaces that can silently
  degrade. Redundancy raises the chance that *at least one* component is gray-failing.

  </details>

- [ ] You can give three structural reasons detection is hard.

  <details><summary>Answer</summary>

  (1) The observer is cheap and shallow by necessity — a check that exercised every path would cost
  as much as the workload, so it checks liveness, not latency-under-contention. (2) The observer's
  workload differs from the app's: the §2.2 incident is a driver bug the detector never sees because
  its heartbeat travels a local RPC path the bug does not touch, while the app's traffic takes the
  broken external path. (3) Degradation is partial and intermittent — 1% of writes slow, a fraction
  of packets dropped — so any single probe likely misses it.

  The paper's umbrella term is *observational differences* (§2.2): the observer and the app measure
  different things, so disagreement is structural, not negligent.

  </details>

- [ ] You can connect gray failure to metastable failure as trigger and sustaining loop.

  <details><summary>Answer</summary>

  §3.3 gives gray failure a lifecycle: latent → gray → complete, a ➊→➋→➍ walk. The gray phase is a
  persistent, undetected degradation. In topic 35's terms that degradation is the *trigger*, and the
  *sustaining loop* is the retry/timeout traffic it induces — callers giving up on the slow
  dependency and retrying, which keeps the pressure on even after the original fault would have
  cleared. §2.3's storage manager rebooting a degraded server in a loop is the worked example.

  Where to cut: break the sustaining loop (bound retries, shed load, hedge instead of retry) rather
  than only chasing the trigger, because by the time you reach cell ➍ the trail to the trigger is
  cold.

  </details>

- [ ] You can point at lane 1's output and say which numbers are the observer's view and which are
      the app's.

  <details><summary>Answer</summary>

  The observer's view is infra-0's own error rate, 0.0040 — pinned to the baseline because being
  slow never sets its own error flag (its failures come only from the `baseline_error` = 0.004 coin,
  like every healthy leaf). So a per-node health check ranks it 41st of 55 by error rate and 35th by
  failure count; the failure-count rank is a little higher only because infra-0 has the most callers
  (20), so the same baseline rate over more calls yields more absolute failures.

  The app's view is the "34 of 55 alerting" — those 34 are infra-0's *callers*, which time out on
  its slowness (`timeout_prob` = 0.7) and manufacture errors "one hop above the thing that is
  actually broken." Cause at rank 35, symptoms at ranks 1–34: differential observability made
  numeric. Lane 2's graph methods put it back at mean rank 1.0.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five questions push the model onto systems you operate: placing real incidents in the four
  cells (§3.2), pricing the health check that would catch lane 1's fault against §4.2's
  probing-burden caveat, mapping the gray→fail-stop escalation onto topic 35's trigger/loop
  distinction, asking why binary health checks persist despite Sherlock's three-state model, and
  constructing a gray failure that defeats app-side localization.

  Write the answers against the anchors above — §3.1's reactor-vs-observer split, §2.1's fan-out
  formula, §3.3's lifecycle, and lane 1's ranking arithmetic — not from the summary.

  </details>

## References

- Huang, Guo, Lou, Liu, Bragstad, Bhatti, Chandra, Kumar, Maltz, Zhang. *Gray Failure: The
  Achilles' Heel of Cloud-Scale Systems.* HotOS 2017 —
  [PDF](https://www.microsoft.com/en-us/research/wp-content/uploads/2017/06/paper-1.pdf). Section and
  table citations in this chapter refer to this paper.
- Bahl et al. *Towards Highly Reliable Enterprise Network Services via Inference of Multi-level
  Dependencies.* SIGCOMM 2007 — the *troubled* state, ten years earlier.
- Dean & Barroso. *The Tail at Scale.* CACM 2013 (topic 37) — why one degraded backend contaminates
  a fan-out, and why hedging works when failure detection does not.
- Bronson, Aghayev, Charapko, Zhu. *Metastable Failures in Distributed Systems.* HotOS 2021
  (topic 35) — the lifecycle a gray failure often triggers.
- Local experiment: `topics/43-ops-dependency-graphs/experiments/src/services.rs` — the gray failure,
  planted.
