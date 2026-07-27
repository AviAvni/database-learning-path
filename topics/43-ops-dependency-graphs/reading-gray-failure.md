# Gray failure: when the system and its users disagree

Six pages, one idea, and it explains why lane 1 of this topic is hard. Cloud systems are built on
the assumption that failures are detectable: a component is up or it is down, a health check says
which, and the redundancy machinery does the rest. Huang et al.'s observation is that the failures
that actually cause long outages are not like that. The component is *degraded* — slow,
intermittently wrong, dropping a fraction of packets — and, critically, **its own failure detector
does not notice while its users are suffering**. They name the general condition *differential
observability*, and once you have the term you start seeing it in every postmortem you read.

## The problem in one sentence

**The failure detector says the component is healthy, the applications using it say it is not, and
because the detector is what triggers recovery, nothing happens.**

## The concepts, step by step

### Step 1 — The model: an observer, an app, and a disagreement

The framing is deliberately minimal. A system has some *ground truth* about its own health. Two
parties form a view of it:

```
   the OBSERVER  — the failure detector, the health check, the monitoring
                   probe. Its view triggers recovery.
   the APP       — everything that actually uses the component. Its view
                   is what users experience.
```

Four combinations. Three are unremarkable: both healthy (fine), both unhealthy (a *fail-stop*
failure, and the recovery machinery works), observer unhealthy but app fine (a false positive,
annoying but safe).

The fourth is the one with a name. **Differential observability**: the app sees a problem, the
observer does not. That is a gray failure, and it is dangerous precisely *because* the recovery
machinery is inert — the system believes it is fine, so nothing fails over, nothing sheds, nothing
pages, and the degradation persists until a human works it out.

### Step 2 — Why redundancy does not save you

Every fault-tolerance mechanism you have is keyed on the observer's view. Failover triggers when
the detector says the primary is down. Load balancers eject a backend when a probe fails. A quorum
excludes a replica when it stops responding.

Under differential observability none of that fires. Worse, the degraded component keeps
*accepting* work — it is up, after all — so it keeps absorbing traffic it will serve slowly, and a
load balancer using round-robin or least-connections may even send it *more*.

This is the connective tissue with topic 37: a fan-out to N backends takes the maximum of N
latencies, so one degraded backend in a hundred contaminates a large fraction of requests. Hedged
requests are one of the few mitigations that work against a gray failure precisely because they do
not require anybody to declare the slow component dead.

### Step 3 — Why detection is genuinely hard, not merely neglected

The paper is careful not to make this a story about lazy monitoring. Three structural reasons:

- **The observer is usually cheap and shallow**, by necessity: a health check that exercised every
  code path would cost as much as the workload. So it checks liveness, not correctness, and
  certainly not latency under contention.
- **The observer's workload differs from the app's.** A probe that reads a fixed small key will not
  see a problem that appears only under a particular access pattern, at a particular size, on a
  particular device.
- **Degradation is often partial and intermittent.** A disk that is slow on 1% of writes, a NIC
  dropping a small fraction of packets, a memory leak that only matters after eight hours. Any
  single probe is likely to miss it.

### Step 4 — Gray failures escalate, which is why they end up in postmortems

The observation that makes this operationally urgent: gray failure is frequently not the end state
but the *prologue*. The degradation persists, work backs up behind it, queues grow, retries pile
on, and eventually something crosses a threshold and fails hard — at which point the failure is
detectable, but you are now diagnosing the crash rather than the degradation that caused it, and
the trail is cold.

If you have read topic 35, this is a metastable failure with a gray failure as its trigger: the
sustaining feedback loop (retries against a slow dependency) outlives whatever started it. And in
topic 43's lane 1 you can see the mechanism in miniature — the broken service is slow, its callers
time out, and the *timeouts* are what generate the error storm.

### Step 5 — What to do about it, and what this topic does about it

The paper's direction is to close the gap between the two views: make the observer's view
approximate the app's, by deriving health signals from what applications actually experience
rather than from synthetic probes. Aggregate client-side latency, error rates observed *by callers*,
and cross-check components against their peers, since a degraded component usually looks different
from its replicas even when it looks fine on its own.

That is exactly what the two localization methods in this topic do, and it is worth seeing them as
answers to this paper:

- **Sherlock** (2007) refuses a binary health model outright. Its *troubled* state — "servers or
  links continue to function but users perceive poor performance" — is differential observability
  encoded in the data model, and its **observation nodes are client-side measurements**, never
  server-side health checks.
- **The random walk** never asks any component whether it is healthy. It only uses the topology and
  the correlation between a component being on a path and that request failing — a purely
  app-side signal.

Lane 1 measures what happens when you do not do this:

```
   the broken service is infra-0 — SLOW on 55% of calls, not failing
   services alerting above a 5% error rate: 34 of 55
   is the broken service among them? NO
   its own error rate: 0.0040  (baseline is 0.0040)

   ranked 35 of 55 by failure count, 41 of 55 by error rate
   and all five infra leaves sit at 0.0040-0.0041 — indistinguishable
```

Thirty-four alerts, and the cause is in the bottom half of both rankings with a health check that
is green. Lane 2 shows both graph methods finding it at mean rank 1.0.

### Step 6 — The transferable habit

Two questions to ask of any system you operate:

1. **Whose view triggers my recovery?** If the answer is a health endpoint the component serves
   about itself, you have a differential-observability gap by construction.
2. **What would a degraded-but-up component look like in my telemetry?** If the honest answer is
   "like a healthy one", you will find out about it from a user.

And a design note that generalises past infrastructure: any time a system's self-assessment drives
its own remediation, ask what happens when the self-assessment is the thing that is broken.

## How to read the paper (with the concepts in hand)

It is six pages; read all of it. But read it in this order:

- **§2 (the model)** first — the observer/app/ground-truth triangle and the four-cell table. The
  term *differential observability* is defined here and the rest of the paper is commentary.
- **§1 and §3 (the examples)** second, now that you have the frame. The value of the examples is
  recognising the shape, not memorising the incidents.
- **§4–5 (implications and directions)** last. Read the escalation argument against topic 35's
  metastable-failure paper and note that they are describing the same lifecycle from two ends.
- **After the paper.** Re-read lane 1's output and identify, for each row, which of the two views
  it represents. Then do exercise 4 of this topic — localize under sampling — because the question
  "how much observability do I actually need to close the gap?" is the practical form of this
  paper's argument.

## Questions to answer in notes.md

1. Draw the four-cell observer × app table and put a real incident you have seen in each cell.
   Which cell was hardest to diagnose, and did the model predict that?
2. Lane 1's broken service has an error rate exactly at baseline. Write the health check that would
   have caught it, then estimate what that health check costs to run continuously against every
   component. Is it affordable?
3. The paper argues gray failures escalate into fail-stop ones. Connect that to topic 35's
   metastable failures: which is the trigger and which is the sustaining loop, and where would you
   cut?
4. Sherlock's *troubled* state predates this paper by ten years. Why do you think the industry
   still ships binary health checks — and what would have to change in a load balancer's interface
   to express three states?
5. Both localization methods in lane 2 use only app-side signals. Construct a gray failure that
   defeats them both, and say what additional observation would be needed.

## Done when

- [ ] You can define differential observability and draw the four-cell table.
- [ ] You can explain why redundancy mechanisms are inert under gray failure.
- [ ] You can give three structural reasons detection is hard.
- [ ] You can connect gray failure to metastable failure as trigger and sustaining loop.
- [ ] You can point at lane 1's output and say which numbers are the observer's view and which are
      the app's.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Huang, Guo, Lou, Liu, Bragstad, Bhatti, Chandra, Kumar, Maltz, Zhang. *Gray Failure: The
  Achilles' Heel of Cloud-Scale Systems.* HotOS 2017 —
  [PDF](https://www.microsoft.com/en-us/research/wp-content/uploads/2017/06/paper-1.pdf).
- Bahl et al. *Towards Highly Reliable Enterprise Network Services via Inference of Multi-level
  Dependencies.* SIGCOMM 2007 — the *troubled* state, ten years earlier.
- Dean & Barroso. *The Tail at Scale.* CACM 2013 (topic 37) — why one degraded backend contaminates
  a fan-out, and why hedging works when failure detection does not.
- Bronson, Aghayev, Charapko, Zhu. *Metastable Failures in Distributed Systems.* HotOS 2021
  (topic 35) — the lifecycle a gray failure often triggers.
- Local experiment: `topics/43-ops-dependency-graphs/experiments/services.rs` — the gray failure,
  planted.
