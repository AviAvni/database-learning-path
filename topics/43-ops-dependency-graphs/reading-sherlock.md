# Sherlock: probabilistic blame on a graph you had to infer

In 2007, before "microservices" and before distributed tracing was ordinary, Microsoft Research
built a system that watched packets on an enterprise network, inferred which services depended on
which, assembled the result into a probabilistic graph, and used it to name the component
responsible when users complained. Everything the modern observability industry sells is in this
paper, and two of its ideas have not been improved on since: modelling a component's health as
*three* states rather than two, and pruning an exponential search with a single empirical
observation about how incidents actually happen.

This is a paper, not a codebase, so every claim below is anchored to the section, figure or table of
*Towards Highly Reliable Enterprise Network Services via Inference of Multi-level Dependencies*
(Bahl et al., SIGCOMM 2007) that states it; each was re-checked against the PDF while writing this
chapter. Where a figure comes from this repo's own crate instead, it is marked as a lane of
`ops_bench` and traced to `notes.md`.

## The problem in one sentence

**Users report that a service is slow; hundreds of components are involved and any of them could be
responsible; find the one that is, using only client-side response times and a dependency graph you
had to infer from network traffic.**

## The concepts, step by step

### Step 1 — Three states, not two

> **In:** the binary up/down health model everything else assumes.
> **Out:** Sherlock's three-state node (§3.1) and the reason the third state exists — it is the one
> that defeats a health check, and the one this topic's lane 1 plants.

Every node in Sherlock's model carries a three-tuple (§3.1):

```
   (P_up, P_troubled, P_down)      summing to 1
```

`P_down` is a fail-stop failure — a server is off, a link is cut. `P_troubled` is the state the whole
paper exists for: **"servers or links continue to function but users perceive poor performance"**
(§3.1).

This is *gray failure*, ten years before the HotOS paper named it, and it is the state that defeats
binary health checks. This topic's lane 1 plants exactly it: a shared dependency that is slow on 55%
of calls, whose own error rate never leaves the baseline (0.0040) while 34 of 55 services alert. A
model with only up and down cannot represent the thing that is wrong.

Why it matters: the whole localization machinery below only earns its keep because it can distinguish
*troubled* from *down* and from *up* — collapse those to two states and Sherlock degenerates into the
per-node ranking that lane 1 shows failing.

### Step 2 — Three kinds of node

> **In:** the three-state node from Step 1.
> **Out:** the three node types of the **Inference Graph** (§3.1) — what is a hidden cause, what is
> measurable, and the layer of "glue" nodes where all the modelling happens.

```
   root-cause nodes    physical components whose failure can cause an end-user
                       experience failure: a computer (an IP address), a service
                       (an IP,port), a router, an IP link
   observation nodes   what Sherlock can actually measure — one per client, per
                       service the client accesses
   meta-nodes          the glue between the two, and where all the modelling is
```

The state of root-cause nodes is independent; the state of an observation node is "uniquely
determined from the state of its ancestors" (§3.1). Edges are labelled with a **dependency
probability**: a client may not need DNS on every file fetch, because the name may already be in its
local cache, so the edge is real but weaker than 1.0.

Why it matters: the split between what you can *measure* (observation nodes) and what you want to
*blame* (root-cause nodes) is the entire problem statement — localization is inference from the first
layer to the third across the meta-node glue.

### Step 3 — Meta-nodes: three ways for a parent to affect a child

> **In:** the meta-node layer from Step 2.
> **Out:** the three propagation semantics (§3.1.1, Figures 3–5) — noisy-max, selector, failover —
> and the worked argument for why one rule cannot cover all three.

The whole art is here, and the paper is explicit that no single rule works.

**Noisy-max** (§3.1.1, Figure 3). *Max*: if any parent is down, the child is down; if none is down
and any is troubled, the child is troubled. *Noisy*: "unless a parent's dependency probability is
1.0, there is some chance that the child will be up even if the parent is down. Formally, if the
weight of a parent's edge is `d`, then with probability `(1−d)` the child is not affected by that
parent." Figure 3's truth table works this out for two parents — e.g.
`P(Child=Troubled | Parent1=Down, Parent2=Troubled) = (1 − d₁) · d₂`, because the child escapes
parent1's down state with probability `1−d₁` and then inherits parent2's troubled state with
probability `d₂`.

**Selector** (§3.1.1, Figure 4). Load balancing. A network load balancer in front of two servers
hashes requests and sends each client to one of them. "An NLB cannot be modeled as a noisy-max
meta-node because the client cannot depend on each server with a probability of 0.5. Using a
noisy-max meta-node will assign the client a 25% chance of being up even when both the servers are
down, which is obviously incorrect." The selector's truth table forces `P(up) = 0` when both parents
are down. Exercise 3 of this topic asks you to build this and show the noisy-max version getting it
wrong.

**Failover** (§3.1.1, Figure 5). Primary/secondary — DNS, WINS, authentication, DHCP. "As long as
the primary server is up or troubled, the child is not affected by the state of the secondary server.
When the primary server is in the down state, the child is still up if the secondary server is up."

Why it matters: get the meta-node wrong and every probability downstream of it is wrong — the NLB/25%
example is the paper showing you that the "obvious" noisy-max default silently mismodels one of the
most common topologies in a data centre.

### Step 4 — The escape hatch, priced

> **In:** the Inference Graph from Steps 2–3, which is necessarily incomplete.
> **Out:** the two pseudo-root-causes (§4.2) that absorb everything the model left out, and the exact
> probabilities that make the choice defensible rather than a fudge.

Every Inference Graph gets two extra root causes (§3.1 / §4.2): **always troubled** at `(0,1,0)` and
**always down** at `(0,0,1)`, wired to *every* observation node. They model "external factors not
part of our model that might cause a user-perceived failure."

The probabilities are stated, not hand-waved (§4.2): edges from AT/AD to observation nodes get
**0.001**, "which implies that 1 in 1000 failures are caused by a component not in our model", and
router or path meta-node edges get **0.9999**, "a 1-in-10,000 chance that our network topology or
traceroutes are incorrect or the router is not actually on the path."

Two things worth taking from this. Every model is incomplete, and the honest response is a term that
absorbs the incompleteness rather than pretending it away. And the paper immediately adds that
"Sherlock's results are not sensitive to the precise setting of these parameters (Section 6.2)" —
which is the sentence that makes the choice defensible.

Why it matters: an escape-hatch term with a tiny, sensitivity-tested weight is how a probabilistic
model stays honest about what it does not know without letting that ignorance dominate the ranking.

### Step 5 — The cost of propagation, and the way out

> **In:** the noisy-max semantics of Step 3, applied to a node with `n` parents.
> **Out:** why the naive computation is `O(3ⁿ)` and how noisy-max collapses it to `O(n)` (§3.1.2) —
> the three closed-form products, read as English.

Computing a child's state distribution from `n` parents is `O(3ⁿ)` in general for a three-state
model — you sum over the full truth table. For noisy-max nodes, which are the majority, that collapses
to **`O(n)`** (§3.1.2):

```
   P(child up)       = Π_j ( (1 − d_j) · (p_j^troubled + p_j^down) + p_j^up )
   1 − P(child down) = Π_j ( 1 − p_j^down + (1 − d_j) · p_j^down )
   P(child troubled) = 1 − ( P(child up) + P(child down) )
```

Read the first line as: the child is up only if, for every parent, either it does not depend on that
parent (probability `1−d_j`) or that parent is up. Selector and failover stay exponential, but "these
two types of meta-nodes have no more than 6 parents, and hence do not add a significant computation
burden" (§3.1.2).

Why it matters: the `O(3ⁿ)→O(n)` collapse is what lets Sherlock evaluate a single candidate quickly;
the *number* of candidates is a separate explosion, handled next.

### Step 6 — Ferret, and Observation 3.1

> **In:** a fast way to score one candidate (Step 5), and `3^r` candidates to score.
> **Out:** the empirical observation (§3.2) that prunes `3^r` to `(2r)^k`, the worked size of that
> reduction, and the second observation that cuts the constant by two orders of magnitude.

An **assignment-vector** assigns a state to every root-cause node — "link₁ is down and server₂ is down
and all the other root-cause nodes are up". Fault localization is finding the assignment vector that
best explains the observations. With `r` root causes there are `3^r` of them, and "existing solutions
to this problem in machine learning literature, such as loopy belief propagation, do not scale to the
Inference Graph sizes encountered in enterprise networks" (§3.2).

The way out is not a better algorithm. It is a fact about incidents:

> **Observation 3.1.** It is very likely that at any point in time only a few root-cause nodes are
> troubled or down.
>
> In large enterprises, there are problems all the time, but they are usually not ubiquitous.

So Ferret evaluates only the assignment vectors with at most `k` abnormal nodes: `2r` vectors with one
abnormal, `2²·C(r,2)` with two, and so on — **at most `(2r)^k`** (§3.2). Work the reduction for a
realistic graph, `r = 200` root causes and `k = 2`: the brute-force space is `3^200 ≈ 10^95`, while
Ferret evaluates the one-abnormal vectors (`2·200 = 400`) plus the two-abnormal vectors
(`2²·C(200,2) = 4·19,900 = 79,600`) — about **80,000** vectors, under the bound `(2·200)² = 160,000`.
That is `10^95` down to `10^5`. And the error is bounded: "the probability that Ferret does not arrive
at the correct solution ... decreases exponentially with `k` and becomes vanishingly small for
`k = 4` onwards" (§3.2). The one caveat is in a footnote: the observation can fail "in important cases
such as rapid malware infection and propagation" — the regime where many components go bad at once.

A second observation halves the constant:

> **Observation 3.2.** Since a root-cause is assigned to be *up* in most assignment vectors, the
> evaluation of an assignment vector only requires evaluation of states at the descendants of
> root-cause nodes that are not *up*.

Ferret preprocesses by setting everything up and propagating once; each candidate then only recomputes
the descendants of its abnormal nodes and rolls back afterwards. "As there are never more than `k`
nodes that change state out of the hundreds of root-cause nodes in our Inference Graphs, this reduces
Ferret's time to localize by roughly two orders of magnitude" (§3.2).

Why it matters: both observations are the same technique — when a search space is exponential, look
for a fact about the *distribution of real inputs* before you look for a cleverer algorithm.

### Step 7 — Scoring against real measurements

> **In:** a candidate assignment vector and the actual client measurements.
> **Out:** the two-Gaussian response-time score (§4.3) and the significance test that decides whether
> the top-ranked candidate deserves attention at all.

For each observation node, Ferret needs a score in `[0,1]` for how well the predicted state
distribution matches what was actually measured.

When the observation is an error or a timeout, the score is just the predicted probability of being
down. When it is a **response time**, Sherlock fits two Gaussians to the historical data —
`Gaussian_up` and `Gaussian_troubled` (the paper's example, from Figure 1: mean 200 ms and mean 2 s)
— and scores a measured time `t` as (§4.3):

```
   p_up · Prob(t | Gaussian_up) + p_troubled · Prob(t | Gaussian_troubled)
```

The score for an assignment vector is the product over observations. And then a significance test,
because a ranked list is worthless without one (§4.3). Ferret computes the score of the *null
hypothesis* (all root causes up), and over time obtains the distribution of
`Score(best prediction) − Score(null hypothesis)`. For a new set of observations the prediction is
declared significant only if that score difference **exceeds the median of that distribution by more
than one standard deviation** — not merely "beats the null by one standard deviation." The bar is the
median of the historical best-minus-null gap, plus one standard deviation.

This topic's crate implements the `k = 1` case with a simpler scoring function — least-squares
residual on predicted front-end failure rates — and the detail that makes it work is worth noticing:
**clamping the fitted severity to `[0,1]`**. A severity is a probability, so a candidate that is
simply not on enough requests would need one above 1 to explain the observed rates, and the clamp is
what makes it pay for that. Without the clamp, all five infrastructure leaves score alike; with it,
the right one wins 5/5 (lane 2).

Why it matters: the significance test is what separates "here is the most likely cause" from "there
is a cause worth paging someone about" — and getting its definition right (median + one std dev of
the *difference* distribution) is the difference between a calibrated alert and a noise generator.

### Step 8 — Discovering the graph in the first place

> **In:** everything above assumed an Inference Graph existed.
> **Out:** how Sherlock infers the dependency edges from packet timing (§4.1), the 10 ms interval
> trade-off, the chance-co-occurrence correction, and the deployment numbers that show it scales.

Sherlock has no service registry, so it infers dependencies from timing (§4.1): "if accessing service
B depends on service A, then packets exchanged with A and B are likely to co-occur." The dependency
probability of A when accessing B is approximated as the conditional probability of accessing A within
a **dependency interval** — fixed at **10 ms** — before accessing B.

The trade is stated plainly (§4.1): "Too large an interval will introduce false dependencies on
services that are accessed with a high frequency, while too small an interval will miss some true
dependencies." And there is a chance-co-occurrence correction: with average interval `I` between
accesses to a service, the likelihood of accidental co-occurrence is estimated as `(10ms)/I`, and only
dependencies far above that are kept.

Deployment (§5–6): 40 servers, 34 routers, 54 IP links, 2 LANs, three weeks, ~1,500 clients with
agents on 23 of them. Agents report every 300 s; a per-host dependency graph is under 40 KB, so
**10⁵ agents would need about 10 Mbps** in aggregate. Localization complexity is "proportional to the
number of root causes in the inference graph × the graph depth", and depth is "less than 10 for all
the applications we have studied."

Why it matters: the graph is the input to everything else, and Sherlock's willingness to *infer* it
from traffic — rather than demand a hand-maintained registry — is what made it deployable, and is
exactly the move this topic's lane 1 generator reverses to test the localizers.

## How to read the paper (with the concepts in hand)

- **§1 + Figure 1.** The motivating incident and the *troubled* state. Read the definition twice.
- **§3.1 + Figure 2.** The three node types on a worked example (a client fetching a file from a
  network share, via Kerberos, via DNS, via routers). Trace one path from observation to root cause
  yourself.
- **§3.1.1 + Figures 3–5.** The three meta-nodes and their truth tables. Derive one entry of Figure 3
  by hand; then read the NLB/25% argument for why selector must exist.
- **§3.1.2.** The `O(3ⁿ) → O(n)` reduction. Read the first product formula as a sentence in English.
- **§3.2 + Algorithm 1.** Ferret. Observations 3.1 and 3.2 are the paper's real contribution;
  everything else is bookkeeping.
- **§4.1.** Dependency discovery, the 10 ms interval, chance co-occurrence, and aggregating across
  similar clients.
- **§4.2–4.3.** Graph construction, the AT/AD escape hatch and its 0.001, and the two-Gaussian
  response-time scoring plus the significance test.
- **§5–6.** Implementation and the production deployment (Figure 8's topology).
- **After the paper.** Implement `sherlock_single_fault` in `rca.rs` and reproduce lane 2, then do
  exercises 2 and 3 — `k = 2` for simultaneous faults, and the selector meta-node.

## Questions to answer in notes.md

1. Sherlock's *troubled* state predates the Gray Failure paper by a decade. State what a binary
   up/down model cannot express, using lane 1's numbers as the example.
2. Derive `P(Child = Troubled | Parent1 = Down, Parent2 = Troubled) = (1 − d₁) · d₂` from the
   noisy-max definition, in words.
3. Show concretely that a noisy-max node models a load balancer incorrectly: two servers, both down,
   dependency probability 0.5 each. What does noisy-max give, and what should it be?
4. Observation 3.1 turns `3^r` into `(2r)^k`. Compute both for `r = 200` and `k = 2`, and say what
   assumption about incidents you are buying with that reduction — then name a failure mode where the
   assumption is false (the paper names one).
5. The AT/AD pseudo-causes absorb model error at probability 0.001. Argue for and against making that
   a tunable, given the paper's claim that results are insensitive to it.

## Done when

Answer each before unfolding it.

- [ ] You can name the three node types and the three meta-nodes, and say what each meta-node is for.

  <details><summary>Answer</summary>

  Node types (§3.1): root-cause nodes (physical components that can fail — a computer, a service, a
  router, an IP link), observation nodes (one per client-per-service measurement, the only thing
  Sherlock actually sees), and meta-nodes (the glue that propagates state from causes to
  observations).

  Meta-nodes (§3.1.1): noisy-max is the default AND-of-dependencies with a per-edge escape probability
  `1−d`; selector models a load balancer, forcing `P(up)=0` when all backends are down (which
  noisy-max gets wrong, assigning 25% up for two 0.5-weight down parents); failover models
  primary/secondary, where the secondary only matters once the primary is fully down.

  </details>

- [ ] You can define *troubled* and explain why two states are not enough.

  <details><summary>Answer</summary>

  *Troubled* is when "servers or links continue to function but users perceive poor performance"
  (§3.1) — degraded, not dead. A binary up/down model has nowhere to put it: the component answers
  health checks (so it is "up") while users suffer, so a two-state model records it as healthy.

  Lane 1 is the numeric proof: the broken infra leaf is slow on 55% of calls but its own error rate
  stays at the 0.0040 baseline, so a per-node up/down view ranks it 41st of 55 by error rate while
  34 of its callers alert. The third state is exactly what a model needs to represent "working but
  hurting."

  </details>

- [ ] You can state Observations 3.1 and 3.2 and the complexity each one buys.

  <details><summary>Answer</summary>

  Observation 3.1 (§3.2): at any moment only a few root causes are abnormal, so Ferret evaluates only
  assignment vectors with at most `k` abnormal nodes — `2r` with one, `2²·C(r,2)` with two, at most
  `(2r)^k` overall. For `r=200, k=2` that is ~80,000 vectors (bound 160,000) instead of `3^200 ≈
  10^95`, with error "vanishingly small for `k=4` onwards." The assumption fails under mass events
  like rapid malware propagation (the paper's footnote).

  Observation 3.2 (§3.2): since most root causes are *up* in most vectors, only the descendants of the
  abnormal nodes need re-evaluating. Ferret propagates the all-up state once, then recomputes only the
  affected subtree per candidate — "roughly two orders of magnitude" faster.

  </details>

- [ ] You can explain the two-Gaussian response-time score and the significance test.

  <details><summary>Answer</summary>

  For a response-time observation `t`, Sherlock fits `Gaussian_up` and `Gaussian_troubled` to history
  (Figure 1's example: means 200 ms and 2 s) and scores a candidate predicting `(p_up, p_troubled,
  p_down)` as `p_up·Prob(t|Gaussian_up) + p_troubled·Prob(t|Gaussian_troubled)` (§4.3); the vector's
  score is the product over all observations.

  The significance test: Ferret computes the null-hypothesis score (all root causes up) and, over
  time, the distribution of `Score(best) − Score(null)`. A new prediction counts as significant only
  if its best-minus-null difference **exceeds the median of that distribution by more than one
  standard deviation** (§4.3) — the bar is median + 1 std dev of the historical gap, not simply "one
  std dev above the null."

  </details>

- [ ] You can describe how the dependency graph is discovered, and the 10 ms trade-off.

  <details><summary>Answer</summary>

  With no registry, Sherlock infers edges from packet timing: if B depends on A, packets to A and B
  co-occur, so the dependency probability of A given B is the conditional probability of an A access
  within a fixed **10 ms** dependency interval before a B access (§4.1). Too large an interval invents
  false dependencies on high-frequency services; too small a one misses real ones — hence a fixed,
  tuned middle value.

  A chance-co-occurrence correction guards against coincidence: with mean inter-access interval `I`,
  accidental co-occurrence is ~`(10ms)/I`, and only dependencies well above that survive (§4.1). At
  deployment scale (§5–6) this stayed cheap: per-host graphs under 40 KB, ~10 Mbps for 10⁵ agents,
  graph depth under 10.

  </details>

- [ ] Your `rca.rs` reproduces lane 2: mean rank 1.0 against the baselines' 36.4 and 44.0.

  <details><summary>Answer</summary>

  Lane 2 runs the graph-aware localizer against the two per-node baselines. The baselines put the true
  cause at mean rank 36.4 (rank-by-failure-count) and 44.0 (rank-by-error-rate) — bottom half, exactly
  the differential-observability failure. The Sherlock-style single-fault localizer recovers it at
  mean rank 1.0 across the seeds.

  The load-bearing detail is clamping the fitted severity to `[0,1]`: a candidate that is not on
  enough requests would need a severity above 1 to explain the observed failure rates, and the clamp
  forces it to pay for that mismatch. Without the clamp all five infra leaves score alike; with it the
  true cause wins 5/5.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five questions cover the transferable core: why the *troubled* state is irreducible (lane 1),
  the noisy-max conditional derivation, the load-balancer counter-example that forces the selector
  meta-node, the `3^r → (2r)^k` pruning and the malware-propagation regime where it fails, and whether
  the AT/AD escape-hatch weight should be tunable given the paper's insensitivity claim.

  Answer them against the anchors above — §3.1 for the model, §3.1.1 for the meta-nodes, §3.2 for
  Ferret's observations, §4.1–4.3 for discovery and scoring — not from memory. The recurring lesson is
  Observation 3.1's: beat an exponential search with a fact about real inputs before reaching for a
  cleverer algorithm.

  </details>

## References

- Bahl, Chandra, Greenberg, Kandula, Maltz, Zhang. *Towards Highly Reliable Enterprise Network
  Services via Inference of Multi-level Dependencies.* SIGCOMM 2007 —
  [PDF](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/sherlock_sigcomm_07.pdf).
  Section, figure and table citations in this chapter refer to this paper.
- Kandula, Katabi, Vasseur. *Shrink: A Tool for Failure Diagnosis in IP Networks.* SIGCOMM MineNet
  2005 — the two-level, two-state predecessor Ferret's approximation builds on.
- Kim, Sumbaly, Shah. *Root Cause Detection in a Service-Oriented Architecture.* SIGMETRICS 2013 —
  MonitorRank, the random-walk alternative the crate's other stub implements.
- Local exercise stub: `topics/43-ops-dependency-graphs/experiments/src/rca.rs`.
- Topic 40 (attack graphs) — the same reasoning with the arrows reversed; topic 21 (formal methods) —
  what it would take to verify a model like this rather than tune it.
