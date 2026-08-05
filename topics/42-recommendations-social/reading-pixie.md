# Pixie: three billion items in sixty milliseconds

Pinterest's recommender ranks 3 billion items for 200 million users, in real time, from a graph
held entirely in the RAM of one machine. The reason it can is a single property that the paper
states almost in passing: **a random walk's cost depends on the number of steps, not on the size
of the graph**. Everything else in Pixie — the biased edge selection, the weighted query set, the
multi-hit booster, the early stopping — is engineering on top of that one fact. It is worth
reading as a case study in choosing an algorithm whose cost model matches your latency budget,
and then spending all your cleverness inside it. It is *also* the topic's worked example of a
published trick that does not reproduce: the multi-hit booster buys nothing on this repo's
generator, and the honest half of the guide is explaining exactly which premise is missing.

Every section, figure and equation number below is from the published Pixie paper — Eksombatchai
et al., *Pixie*, WWW 2018 ([arXiv:1711.07601](https://arxiv.org/abs/1711.07601)). The measured
numbers labelled "lane 1" and "lane 2" are this repo's own, from
[`../../FINDINGS.md`](../../FINDINGS.md) row 42, this topic's [`README.md`](README.md) and
[`notes.md`](notes.md); they were produced by the crate in `experiments/`, not by the paper.

## The problem in one sentence

**Recommend from a 17-billion-edge graph in under 60 milliseconds, per request, at 100,000
requests per second — without precomputing, because precomputed recommendations are a day stale
and reacting to intent in real time is worth 30–50% more engagement.**

## The concepts, step by step

### Step 1 — Why real-time, and why not precompute

> **In:** nothing yet — this step fixes the budget and the reason the whole system exists.
> **Out:** two numbers every later step obeys — a **60 ms** per-request latency budget and the
> **30–50%** engagement lift that pays for hitting it in real time rather than from a cache.

A **recommender** here answers one query: given a *query pin* — an item the user just engaged with
— return a ranked list of other items they are likely to want. The industry default was **batch
precomputation**: run a pipeline nightly, write each user's recommendations to a key–value store,
and serve them as static lookups. Pixie's argument against batch is economic, not aesthetic (§1):

> if providing recommendations would take just 1 second, then the user would have to wait too
> long ... In such cases recommendations would have to be precomputed (say once a day) and then
> served out of a key-value store. However, old recommendations are stale and not engaging.

And the measured consequence, also §1: "reacting to user's intent in real-time leads to 30-50%
higher engagement than needing to wait days or hours for recommendations to refresh."

That 30–50% is the entire justification for the system: it is what buys back the cost of computing
a fresh answer on every request. Hold on to it — everything after this is what you must build once
you have decided 60 ms is the budget and that a day-old cache will not do.

Why it matters: a recommender's design is downstream of its latency budget. Pixie picks an
algorithm whose cost it can bound *before* choosing any of the four refinements below.

### Step 2 — The graph, and Algorithm 1

> **In:** the 60 ms budget from Step 1.
> **Out:** `BasicRandomWalk` (Algorithm 1) and its one load-bearing property — cost is `O(N)`
> steps, independent of graph size — plus the measured way it fails (lane 1). Steps 3–6 each fix
> one of its failures; Step 8 makes its inner loop free.

Pinterest is a **bipartite graph** `G = (P, B, E)` — a graph whose vertices fall into two sets with
edges only *between* the sets, never within one. Here the two sets are pins `P` and boards `B`,
and an edge `(p, b) ∈ E` means a user saved pin `p` to board `b`. A **random walk** from a query
pin `q` is a process that starts at `q` and repeatedly steps to a randomly chosen neighbour;
because the graph is bipartite each pair of steps goes pin → board → pin. A **visit count** `V[p]`
is how many times the walk landed on pin `p`; the pins with the highest visit counts are the
recommendation.

Algorithm 1 is eleven lines, transcribed here exactly as the paper prints it:

```
Algorithm 1 — Basic Random Walk (Pixie §3), transcribed verbatim
BasicRandomWalk(q: Query pin, E: Set of edges, α: Real, N: Int)
 1: totSteps = 0, V = 0
 2: repeat
 3:   currPin = q
 4:   currSteps = SampleWalkLength(α)          # α sets the (random) length of one walk
 5:   for i = [1 : currSteps] do
 6:     currBoard = E(currPin)[rand()]         # pin  -> board, uniform neighbour
 7:     currPin   = E(currBoard)[randNeighbor()] # board -> pin,  uniform neighbour
 8:     V[currPin]++
 9:   totSteps += currSteps
10: until totSteps >= N
11: return V
```

`α` is the **restart probability's cousin**: `SampleWalkLength(α)` draws how many steps one walk
runs before it snaps back to `q`, so a smaller expected length keeps visits close to the query.
There is no matrix, no factorization, no learned model. And the only thing that determines runtime
is `N`, the total step budget on line 10 — "the time taken by this procedure is constant and
independent of graph size (determined by parameter N)" (§3). That sentence is why 60 ms is
reachable from 17 billion edges: you pay for steps, not for the graph.

Now the failure, measured. Lane 1 of this topic's crate runs exactly this walk
(`graphs::basic_random_walk`) alongside a plain popularity recommender, and reports them side by
side:

```
lane 1 (provided) — 3000 users x 6000 items, 30 communities, 60000 training edges
   recommender     hit-rate@50   personalization   overlap w/ bestsellers
   popularity         0.340          0.155               0.923
   basic walk         0.403          0.820               0.451
```

The `popularity` row is this topic's headline: recommending the global bestseller list to everyone
scores **0.340 hit-rate@50** with **0.923 overlap** with the bestseller list. That is the result
[`FINDINGS.md`](../../FINDINGS.md) row 42 headlines as "35.3% hit-rate@50 with 92.2% overlap" — the
crate lane here reports 0.340 / 0.923, agreeing to within about a percentage point. Popularity is
not a weak baseline; the walk has to *beat* it.

Two definitions are needed to read that block, and both are computed by `experiments/src/graphs.rs`:

- **hit-rate@k** (`hit_rate`, k = 50): the fraction of users for whom *at least one* held-out item
  appears in the top-k list. For each user you hold out some of their real future engagements,
  build their top-50, and score 1 if any held-out item is in it, 0 otherwise; the metric is the
  mean of those over all users. Worked micro-example, k = 3, five users with one held-out item each:
  if the held-out item lands in the top-3 for two of the five, hit-rate@3 = 2/5 = 0.40.
- **overlap w/ bestsellers** (`popularity_overlap`): the mean over users of
  `|list ∩ bestsellers| / |list|`, where `bestsellers` is the global top-k by degree. 1.0 means
  "you rebuilt the bestseller list"; 0.0 means "nothing you returned was globally popular". Worked
  micro-example, k = 5, bestsellers `{A,B,C,D,E}`: a user who owns none of them gets the list
  `{A,B,C,D,E}` → overlap 5/5 = 1.0; a user who already owns `E` has it filtered out and gets
  `{A,B,C,D,F}` (F the 6th bestseller) → overlap 4/5 = 0.80. Average a population where most users
  own zero or one top item and you land near 0.92 — which is why popularity's overlap is 0.923, not
  a flat 1.0.

So the basic walk gets 40.3% of users right — but **45.1% of every list it returns is the global
bestseller list**, because an unbiased walk's long-run visit distribution drifts toward high
degree (a hub is reachable from everywhere). It personalizes (0.820) and still leaks popularity.
Steps 3–6 are the four fixes; Step 3 attacks exactly this drift.

Why it matters: a walk this simple is already a personalized recommender, and its cost is a knob
you set. Everything else is buying back quality the plain walk gives away.

### Step 3 — Biasing the walk (innovation 1)

> **In:** Algorithm 1's uniform neighbour choice from Step 2 (line 6–7's `rand()`), which drifts
> toward degree.
> **Out:** `PersonalizedNeighbor(E, U)` — the same walk with a *user-biased* edge choice — and the
> paper's sharpest measurement, Table 3. This is the one innovation the crate leaves to you
> (exercise 2).

The fix for "the walk returns the same popular pins to everyone" is to make edge selection depend
on the *user*, not just the graph. **`PersonalizedNeighbor(E, U)`** takes the walking pin's edges
`E` and the user's feature vector `U` (language, topic) and prefers edges that match `U`, so the
same query gives different results to different people. The paper is careful about why this is
cheap (§3.1, innovation 1):

> one could think of this method as using a different graph for each user where edge weights are
> tailored to that user (but without the need to store a different graph for each of the 200+
> million users).

The implementation trick that keeps it O(1): "we currently limit the weights to only take values
from a discrete set of possible values ... by storing edges for similar languages and topics
consecutively in memory ... `PersonalizedNeighbor(E, U)` is a **subrange operator**" (§3.1). A
subrange operator is a slice, not a filter: matching edges are already adjacent in the array, so
you sample from a contiguous window `[lo, hi)` of the adjacency list instead of scanning it.

Table 3 is the payoff — the percentage of returned pins actually in the user's target language,
basic walk versus Pixie:

```
Table 3 (Pixie §4.2) — % of results in the target language
                     En->Japanese   Japanese->Japanese
   BasicRandomWalk        16.35%          52.95%
   PixieRandomWalk        80.33%         100.00%

                     En->Slovak     Slovak->Slovak
   BasicRandomWalk         2.13%          16.06%
   PixieRandomWalk        42.55%         100.00%
```

Read the Slovak row: an English-speaking user querying with a Slovak interest went from **2.13% to
42.55%** target-language content — the basic walk was returning essentially nothing usable for a
small-language interest, because the walk drowned in the majority language's high-degree pins. The
crate does not implement biasing; exercise 2 asks you to add a language attribute, make neighbour
selection prefer matching edges, reproduce the *shape* of Table 3, and measure the per-step cost.

Why it matters: this is degree-drift's direct cure, and the paper's largest single measured
effect. If you build only one of the four innovations, build this one.

### Step 4 — Multiple query pins, and the step-allocation problem (innovation 2)

> **In:** the single-query walk of Steps 2–3.
> **Out:** a weighted query *set* `Q = {(q, w_q)}`, and Equations 1–2 — a sub-linear rule that
> hands every query pin a step budget `N_q`. Each pin's walk produces its own counter `V_q`, which
> Step 5 combines.

A user is not one pin. Pixie takes a **weighted query set** `Q = {(q, w_q)}` — recent interactions
`q`, each with a weight `w_q` set by recency and interaction type — and runs a separate walk per
query pin. That raises a budgeting question with a non-obvious answer. A high-degree query pin
needs *more* steps, because its walk diffuses across many neighbours before its top stabilises. But
allocate steps *linearly* in degree and a low-degree pin gets **less than one step** — its whole
interest is silently dropped (§3.1):

> the challenge remains that if we assign the number of steps in linear proportion to the degree
> then we can end up allocating not even a single step to pins with low degrees.

Equation 1 builds a scaling factor that grows **sub-linearly** in degree, and Equation 2 turns the
scaling factors into a normalized step budget:

```
Eq. 1:  s_q = |E(q)| · (C - log|E(q)|)
Eq. 2:  N_q = w_q · s_q / Σ_{r∈Q} s_r · N
```

Naming the symbols: `|E(q)|` is the degree of query pin `q`; `w_q` is its weight in the set;
`Σ_{r∈Q} s_r` normalizes across the query set; `N` is the total step budget; and `C` is a
graph-wide maximum. **A subtlety the paper states loosely and the crate pins exactly.** Pixie's §3.1
prints "`C = max_{p∈P} |E(p)|` ... the maximum pin degree", but taken literally that makes
`C − log|E(q)| ≈ C` for every pin, so `s_q ≈ |E(q)|·C` is *linear* in degree — the exact behaviour
Equation 1 exists to avoid, and it contradicts the paper's own next sentence ("does not give
disproportionately high weights to popular pins"). For the sub-linearity to exist, `C` must be on
the log scale, and that is what this repo implements: `C = max_{p∈P} log|E(p)| = log(max pin
degree)`, taken over **all** pins in the graph, not the query set (`experiments/src/pixie.rs`,
`allocate_steps`: "`C = ln(max item degree in the WHOLE graph)`").

Why "the whole graph" matters: if you computed `C` over only the query set `Q`, then the
highest-degree *query* pin has `log|E(q)| = C`, so `s_q = |E(q)|·(C − C) = 0` — it gets zero steps.
The crate's `every_query_pin_gets_at_least_one_step` test catches exactly that, and also asserts
the step ratio is strictly below the degree ratio (the sub-linearity).

Worked example — two query pins, degrees `|E(q₁)| = 1` and `|E(q₂)| = 10,000`, equal weights
`w = 1`, budget `N = 10,000`, and a graph whose maximum pin degree is `100,000` so
`C = ln(100,000) = 11.5129`:

```
linear allocation  N_q ∝ |E(q)|:
   N_1 = 10,000 · 1/(1+10,000)      = 0.9999  ->  floor to 0 steps   (interest dropped)

Equation 1 (sub-linear):
   s_1 = 1     · (11.5129 - ln 1)     = 1 · 11.5129      = 11.5129
   s_2 = 10,000· (11.5129 - ln 10,000)= 10,000 · 2.30259 = 23,025.9
   Σ s = 23,037.4
   N_1 = 10,000 · 11.5129 / 23,037.4  = 5.00  steps        (interest kept)
   N_2 = 10,000 · 23,025.9 / 23,037.4 = 9,995 steps

   step ratio N_2/N_1 = 1,999   vs   degree ratio = 10,000   ->  strictly sub-linear
```

The low-degree pin gets ~5 steps under Equation 1 and 0 under linear allocation; the high-degree
pin still gets the lion's share, but the ratio (1,999) is a fifth of the degree ratio (10,000).

Why it matters: this is the least-glamorous innovation and the biggest measured win. Lane 2 moves
hit-rate@50 from the basic walk's 0.403 to **0.823** on eight query pins with this allocation —
the largest single jump in the topic ([notes.md](notes.md)).

### Step 5 — The multi-hit booster (innovation 3)

> **In:** the per-query visit counters `V_q[p]` produced by Step 4's walks.
> **Out:** one combined score `V[p]` per pin, via Equation 3 (implemented as line 5 of Algorithm
> 3) — and the topic's headline negative result, which you must not overstate.

With one counter `V_q` per query pin, you still have to combine them into a single ranking.
Summing them is the obvious choice. Pixie instead uses Equation 3, which the paper implements as
line 5 of **Algorithm 3** (`PixieRandomWalkMultiple`):

```
Algorithm 3 line 5 = Eq. 3 (Pixie §3.1), transcribed verbatim
   V[p] = ( Σ_{q ∈ Q} sqrt( V_q[p] ) )²
```

Naming the symbols: `V_q[p]` is how many times the walk *from query pin q* visited pin `p`; the
inner sum is over the query set; and `V[p]` is the combined score. Worked on the paper's own
intuition:

```
single source:  p visited 4 times from one query pin        -> ( sqrt(4) )²        = 4
multi source:   p visited twice from each of two query pins  -> ( sqrt(2)+sqrt(2) )² = 8
```

Same **four total visits**, twice the score, when they came from two interests instead of one. And
a single-source pin is unchanged — `(√4)² = 4` — so Equation 3 is strictly a *bonus* for
cross-interest pins, never a re-weighting. The paper's justification (§3.1, innovation 3):

> candidates with high visit counts from multiple query pins are more relevant to the query than
> for example candidates having equally high total visit count but all coming from a single query
> pin.

**And now the honest part — this is the topic's worked example of "report the negative result".**
Lane 2 measures the booster against plain summed visit counts on the crate's synthetic graph, at
one interest per user *and* at three, and finds **no gain either way** ([README.md](README.md),
[notes.md](notes.md)):

```
lane 2 — multi-hit booster ablation
   1 interest/user :  0.823 unboosted   vs   0.803 boosted
   3 interests/user:  0.563 unboosted   vs   0.547 boosted
```

Summing raw visit counts scores *slightly better* than Equation 3 in both regimes. This is **not**
an implementation bug: the crate's unit test pins the arithmetic exactly — `(√2+√2)² = 8` against a
single-source `4` — so the formula is right. What is missing is the *premise*. Equation 3 is a bet
that a pin sitting at the intersection of several of your interests is more engaging than one deep
inside a single interest — a claim about **people**, not about graphs. This generator draws each
user's held-out item from the same distribution as its training items, so being reachable from
several query pins carries no extra information about which item the user actually takes next; the
boost is a no-op at best.

That is the transferable lesson, and the reason this topic exists: **a published trick encodes a
domain assumption, and you owe it a measurement on your own data before you ship it.** Exercise 4
asks you to add a `cross_interest_bias` that makes held-out items favour the overlap of a user's
interests, sweep it, and find the crossover where the boost finally pays.

Why it matters: the elegant idea is the one that fails here, and diagnosing *why* — a missing
premise, not a bug — is a skill worth more than the booster itself.

### Step 6 — Early stopping (innovation 4)

> **In:** the fixed per-query budgets `N_q` from Step 4 — walks that run to completion.
> **Out:** Algorithm 2's `n_p`/`n_v` termination condition, which stops each walk once its top is
> stable, and the measured speedup (lane 2 and §4.2).

The walks run for a fixed `N_q` steps. But you do not need the walk to *converge*; you need its
**top** — the highest-visited pins — to stop moving. Pixie terminates a walk once at least `n_p`
candidate pins have each been visited at least `n_v` times. Two integers name the rule:

- **`n_v`** — the visit threshold a pin must reach to be "high-visited".
- **`n_p`** — how many pins must cross `n_v` before the walk stops.

Monitoring this could cost more than the walk, so Pixie keeps a single counter, incremented the one
step a pin's count crosses `n_v` *exactly* — Algorithm 2, transcribed:

```
Algorithm 2 — Pixie Random Walk with early stopping (Pixie §3.1), transcribed verbatim
 9:   V[currPin]++
10:   if V[currPin] == n_v then
11:     nHighVisited++
12:   totSteps += currSteps
13: until totSteps >= N or nHighVisited > n_p
14: return V
```

Line 10's `== n_v` (not `>= n_v`) is what makes the counter O(1): a pin bumps `nHighVisited` on the
single step it *reaches* the threshold, never again, so no per-step scan of the candidate set is
needed. The counter is **per walk** — Algorithm 2 is invoked once per query pin (Algorithm 3, line
3), and each pin decides for itself when to stop. Sharing one counter across the query set would
let early, high-degree pins trip it and starve the later ones; the crate's implementation note
(`experiments/src/pixie.rs`) says so, because it is an easy mistake.

The paper's measurement (§4.2): at `n_p = 2000, n_v = 4`, results overlap the gold-standard long
walk by **84%** at **one third** of the runtime; raising to `n_v = 6` halves the runtime again.
Lane 2 reproduces the shape almost exactly at `n_p = 100, n_v = 3` on the smaller crate graph:

```
lane 2 — early stopping
   full walk:  9,000,004 steps, 2.12 ms/query
   early stop: 3,170,675 steps (35% of full), 0.97 ms/query  (2.2x faster)
   early-stopped top-50 overlaps the full walk by 0.793,  hit rate unchanged
```

35% of the steps, 2.2× faster, top-50 overlap 0.79, hit-rate@50 unchanged — the same trade the
paper reports at a third of the runtime for 84% overlap.

Why it matters: unlike Step 5's booster, early stopping *does* reproduce here, and it is free
quality-for-speed — the one refinement you can adopt without checking a domain premise first.

### Step 7 — Pruning improves quality *and* shrinks the graph

> **In:** the raw Pinterest graph — 7 billion nodes, over 100 billion edges (§3.2).
> **Out:** a pruned graph of 1 B boards, 2 B pins, 17 B edges in ~120 GB that recommends *better*,
> and the single most instinct-changing number in the paper.

Pixie prunes the graph two ways before serving from it (§3.2). **Board entropy**: a board's topic
mix is scored by the entropy of its **LDA** topic distribution — *Latent Dirichlet Allocation*, a
model that assigns each board a distribution over latent topics — and boards with high entropy
("diverse boards diffuse the walk in too many directions") are removed entirely. **Edge cosine
similarity**: for high-degree pins, edges to boards whose topic vector has low **cosine
similarity** (the cosine of the angle between two vectors — 1.0 when identical, 0 when orthogonal)
to the pin's are discarded, controlled by a **pruning factor** `δ`.

The result should change your instincts about data cleaning (§4.3):

> when δ = 0.91, the F1 score peaks at 58% above the unpruned graph F1 and the graph contains only
> 20% the original number of edges.

**F1** is the harmonic mean of precision and recall — the standard single-number quality score, so
"58% above" is a real quality gain, not a size trade. A graph a fifth the size that recommends 58%
*better* — and, because it is now 1 B boards, 2 B pins, 17 B edges in about **120 GB**, one that
fits on a single AWS r3.8xlarge (244 GB RAM) and never has to be distributed at all (§1).

Why it matters: the reflex is that throwing data away costs quality. Here it *buys* quality and a
cheaper machine at once, because the discarded edges were noise the walk would otherwise diffuse
into. Exercise 3 in [README.md](README.md) has you find the analogous `δ` for a workload you know.

### Step 8 — The two data structures that make the inner loop free

> **In:** Algorithm 2's inner loop (Step 6) — a neighbour sample and a visit-count increment, run
> billions of times.
> **Out:** two hand-built structures that make each of those O(1), plus the OS trick that removes
> the page-table tax. This is the section a systems engineer reads twice (§3.3).

Lines 6–11 of Algorithm 2 are the entire runtime, so two structures are hand-built.

**`edgeVec`** — every adjacency list concatenated into one contiguous array, with an `offset` per
node, allocated once from an object pool (so no per-node allocation and no fragmentation). Sampling
a neighbour of node `i` is Equation 4:

```
Eq. 4 (Pixie §3.3):  F[ offset_i + ( rand() % (offset_{i+1} - offset_i) ) ]
```

`offset_{i+1} − offset_i` is node `i`'s degree; one multiply-free modulo picks a slot; one load
returns the neighbour. "The accesses on lines 5, 8, and 10 of Algorithm 2 can be performed
efficiently in constant time" (§3.3).

**The visit counter** — an open-addressing hash table. **Open addressing** stores entries directly
in one array and, on a collision, probes nearby slots rather than chasing a linked list; Pixie uses
**linear probing** (try the next slot, then the next) "to maintain good cache locality", and a
**multiplicative hash** (multiply the key by a fixed prime, modulo the array size) because it must
be fast (§3.3). The table is sized to `N` up front and never resizes, because "the number of steps
N provides an upper bound on the number of keys" — a walk of `N` steps can visit at most `N`
distinct pins.

And an operational detail worth stealing (§3.3): Pixie uses **Linux HugePages** to raise the page
size from 4 KB to 2 MB, "thus decreasing the number of page table entries needed by a factor 512.
Too many page table entries is especially problematic on virtual machines; the HugePages option
enabled Pixie on virtual machines to serve twice as many requests at half the runtime." A page
table is the map from virtual to physical addresses; 512× fewer entries means 512× fewer TLB
misses walking a graph that is almost entirely random access.

Why it matters: the O(N) cost model from Step 2 is only *achievable* if each of the N steps is
genuinely O(1). These three structures are what make the constant factor small enough for 60 ms.

## How to read the paper (with the concepts in hand)

- **§1.** The scale claims (17 B edges, 120 GB, r3.8xlarge, p99 < 60 ms, ~1,200 req/s per server,
  ~100,000 cluster-wide) and the 30–50% engagement number. This is the budget the rest obeys.
- **§2 Related work.** One pass. The paragraph on Twitter's WTF is the connection to the GraphJet
  guide; the paragraph on collaborative filtering explains why factorization was rejected
  (complexity linear in nodes, and Pinterest has billions).
- **§3 + Algorithm 1.** Read the eleven lines and convince yourself the cost is `N`, full stop.
- **§3.1 innovation (1).** Biasing, `PersonalizedNeighbor`, and the "different graph per user
  without storing one" subrange trick.
- **§3.1 innovation (2) + Equations 1–2.** The step-allocation problem. Derive for yourself why
  linear allocation starves low-degree pins (Step 4's worked example), then confirm `C` must be the
  graph-wide maximum on the *log* scale.
- **§3.1 innovation (3) + Equation 3 / Algorithm 3 line 5.** The booster. Then read Step 5 above and
  hold the claim loosely — it does not reproduce on this generator.
- **§3.1 innovation (4) + Algorithm 2 lines 9–13.** Early stopping; note the counter is per walk and
  the `== n_v` test is what keeps it O(1).
- **§3.2 Graph pruning.** The 58%-better-at-20%-of-the-edges result.
- **§3.3 Implementation.** `edgeVec` (Eq. 4), the open-addressing visit counter, HugePages, the
  once-a-day graph build. Read twice.
- **§4.1 + Tables 1–2.** Hit rate against content-based baselines (6.3% / 23.1% / 52.2% at top
  10/100/1000, against content-combined 2.1 / 4.6 / 10.5%) and the A/B lifts (homefeed +48%,
  related pins +13%, localization +48–75%).
- **§4.2 + Figures 1–3 + Table 3.** Runtime linear in steps; stability against step count; the
  language-biasing table; the early-stopping sweeps.
- **After the paper.** Implement `pixie.rs` and reproduce lane 2. Then do exercise 2 — biasing —
  the one innovation the crate leaves to you, and the one with the biggest measured effect.

## Questions to answer in notes.md

1. Pixie's cost is `O(N)` steps, independent of graph size. Name the two things that *are* affected
   by graph size, and say what each costs at Pinterest's scale (hint: §3.3's `edgeVec` build and
   HugePages, and §4.2's cache-miss remark).
2. Derive the failure of linear step allocation: with query pins of degree 1 and 10,000 and a
   budget of 10,000 steps, how many steps does the low-degree pin get under `N_q ∝ |E(q)|`, and
   under Equation 1? Then explain why `C` must be the graph-wide maximum on the log scale.
3. Lane 2 finds the multi-hit booster gives no gain. Write down the precise property a data set
   must have for Equation 3 to pay, as a statement about the joint distribution of (query pins,
   held-out item). Then say how you would test for it in one query, before implementing anything.
4. Pruning improves F1 by 58% while removing 80% of edges. That is a statement about the *graph*,
   not the algorithm. What is the equivalent move for a workload you know, and what would you
   measure to find your δ?
5. Early stopping monitors `n_p` pins reaching `n_v` visits. Why is that a good proxy for "the top
   of the ranking has stopped moving", and construct a graph where it is a bad one.

## Takeaway

Pixie is one algorithm — a random walk whose cost is `O(N)` steps, not `O(graph)` — with four
refinements bolted on: bias the edge choice, split the budget sub-linearly across a weighted query
set, boost cross-interest pins, and stop each walk when its top is stable. Three of the four
reproduce on this repo's generator; the fourth, the multi-hit booster, does not, because the
generator lacks the behavioural premise Equation 3 bets on. That gap is the lesson: **measure a
borrowed trick's assumption on your own data before shipping it.**

## Done when

Answer each before unfolding it.

- [ ] You can state the cost model in one sentence and explain why it makes 60 ms possible.

  <details><summary>Answer</summary>

  A random walk of `N` steps from the query pins costs `O(N)`, and `N` is a parameter set on line
  10 of Algorithm 1 — "the time taken by this procedure is constant and independent of graph size"
  (§3). It makes 60 ms possible because you pay for steps, not for the 17 billion edges: the graph
  can grow arbitrarily and the per-request cost does not move, as long as each step stays O(1).
  Step 8's `edgeVec` (Eq. 4) and the open-addressing visit counter are what keep each step O(1), so
  the constant factor is small enough that ~3 M steps fit in 2.12 ms (lane 2), and the paper's
  full-scale walk fits in a p99 under 60 ms (§1).

  </details>

- [ ] You can write Algorithm 1 from memory.

  <details><summary>Answer</summary>

  `totSteps = 0, V = 0`; then `repeat`: set `currPin = q`, draw `currSteps = SampleWalkLength(α)`,
  and for that many iterations step `currBoard = E(currPin)[rand()]` then
  `currPin = E(currBoard)[randNeighbor()]` and `V[currPin]++`; add `currSteps` to `totSteps`;
  `until totSteps >= N`; `return V`. Eleven lines (Pixie §3). The only runtime knob is `N`; `α`
  controls how long each sub-walk runs before restarting at `q`, which keeps visits local. The
  recommendation is the pins with the highest `V`.

  </details>

- [ ] You can explain all four innovations and what each one fixes.

  <details><summary>Answer</summary>

  (1) **Biasing** (`PersonalizedNeighbor`, §3.1) fixes the basic walk returning the same popular
  pins to everyone: it slices edges matching the user's language/topic, a subrange operator, and
  moves Slovak-target content from 2.13% to 42.55% (Table 3). (2) **Weighted query set +
  sub-linear allocation** (Eqs. 1–2) fixes both "a user is not one pin" and "linear allocation
  starves low-degree pins" — it is lane 2's largest win, 0.403 → 0.823. (3) **Multi-hit booster**
  (Eq. 3 / Algorithm 3 line 5) is meant to reward cross-interest pins, `(√2+√2)² = 8` vs a
  single-source `4`, but reproduces no gain here because the generator lacks the premise. (4)
  **Early stopping** (Algorithm 2, `n_p`/`n_v`) fixes over-walking: stop when the top is stable,
  measured at 35% of the steps and 2.2× faster with unchanged hit rate.

  </details>

- [ ] You can give the language-biasing numbers and the pruning result.

  <details><summary>Answer</summary>

  Language biasing (Table 3, §4.2): for an English user with a Japanese interest, target-language
  content rises from 16.35% (basic walk) to 80.33% (Pixie); for a Slovak interest, 2.13% → 42.55%.
  Same-language queries go to ~100%. Pruning (§4.3): at `δ = 0.91` the F1 score peaks 58% above the
  unpruned graph while keeping only 20% of the edges — a smaller graph that recommends better,
  because the discarded edges were high-entropy boards and low-cosine-similarity edges the walk
  would have diffused into. The pruned graph is 1 B boards, 2 B pins, 17 B edges in ~120 GB (§1).

  </details>

- [ ] Your `pixie.rs` reproduces lane 2's early stopping (~35% of steps, ~0.79 overlap) and you have
      measured the multi-hit booster yourself rather than assuming it helps.

  <details><summary>Answer</summary>

  Early stopping should land near lane 2's reference: ~3.17 M of 9.0 M steps (35%), ~0.97 ms/query
  against 2.12 ms (2.2× faster), top-50 overlap ~0.793, hit-rate@50 unchanged
  ([notes.md](notes.md)). The booster is the test of the lesson: run it against plain summed visit
  counts at one interest per user and at three, and you should see **no gain** (0.823 vs 0.803, and
  0.563 vs 0.547) — not because the arithmetic is wrong (the unit test pins `(√2+√2)² = 8`) but
  because the generator's held-out item is drawn independently of interest overlap. If your run
  shows a gain, check whether your generator accidentally introduced cross-interest correlation.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five questions are in `notes.md`'s guide-question checklist. The load-bearing ones: Q1 (graph
  size affects the `edgeVec` build and cache-miss rate, not the per-request step count); Q2 (linear
  gives the degree-1 pin `< 1` step → 0, Equation 1 gives it ~5, and `C` must be the graph-wide
  log-max or the top query pin gets 0); Q3 (Equation 3 pays only when reachability from multiple
  query pins is correlated with the held-out item — test by measuring that correlation in the
  training data first). Q4 and Q5 are open-ended; answer them against a workload you actually know.

  </details>

## References

- Eksombatchai, Jindal, Liu, Liu, Sharma, Sugnet, Ulrich, Leskovec. *Pixie: A System for
  Recommending 3+ Billion Items to 200+ Million Users in Real-Time.* WWW 2018 —
  [arXiv:1711.07601](https://arxiv.org/abs/1711.07601). Every section, equation, algorithm and
  table number in this chapter is from that paper.

| Where | What |
|-------|------|
| §1 | scale (17 B edges, 120 GB, r3.8xlarge, p99 < 60 ms, ~1,200 req/s per server); 30–50% engagement lift |
| §3, Algorithm 1 | `BasicRandomWalk`, cost `O(N)` steps |
| §3.1 innovation (1) | biasing, `PersonalizedNeighbor` as a subrange operator |
| §3.1 innovation (2), Eqs. 1–2 | sub-linear step allocation; `C = max log-degree` over all pins |
| §3.1 innovation (3), Eq. 3 / Algorithm 3 line 5 | multi-hit booster |
| §3.1 innovation (4), Algorithm 2 lines 9–13 | early stopping, `n_p`/`n_v` |
| §3.2 / §4.3 | pruning; `δ = 0.91`, F1 +58% at 20% of edges |
| §3.3, Eq. 4 | `edgeVec`, open-addressing visit counter, HugePages (factor 512) |
| §4.1, Tables 1–2 | hit rate 6.3 / 23.1 / 52.2%; A/B lifts |
| §4.2, Table 3 | early-stopping sweep (84% at 1/3 runtime); language biasing |

- Leskovec & Sosič. *SNAP: A General-Purpose Network Analysis and Graph-Mining Library* — the
  library Pixie is built on.
- Repo sources: [`../../FINDINGS.md`](../../FINDINGS.md) row 42; this topic's [`README.md`](README.md),
  [`notes.md`](notes.md), and the exercise stub
  `topics/42-recommendations-social/experiments/pixie.rs` (`allocate_steps`, `walk_per_query`,
  `multi_hit_boost`, `pixie_walk`).
- Topic 38 (GraphRAG) — HippoRAG's personalized PageRank is the same primitive with different
  seeds; topic 18 (GPU) — `edgeVec` is a CSR by another name.
