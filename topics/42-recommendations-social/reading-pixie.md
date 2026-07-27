# Pixie: three billion items in sixty milliseconds

Pinterest's recommender ranks 3 billion items for 200 million users, in real time, from a graph
held entirely in the RAM of one machine. The reason it can is a single property that the paper
states almost in passing: **a random walk's cost depends on the number of steps, not on the size
of the graph**. Everything else in Pixie — the biased edge selection, the weighted query set, the
multi-hit booster, the early stopping — is engineering on top of that one fact. It is worth
reading as a case study in choosing an algorithm whose cost model matches your latency budget,
and then spending all your cleverness inside it.

## The problem in one sentence

**Recommend from a 17-billion-edge graph in under 60 milliseconds, per request, at 100,000
requests per second — without precomputing, because precomputed recommendations are a day stale
and reacting to intent in real time is worth 30–50% more engagement.**

## The concepts, step by step

### Step 1 — Why real-time, and why not precompute

The industry default was batch: run a pipeline nightly, write recommendations to a key-value
store, serve them. Pixie's argument against it is economic, not aesthetic: "if providing
recommendations would take just 1 second, then the user would have to wait too long ... In such
cases recommendations would have to be precomputed (say once a day) and then served out of a
key-value store. However, old recommendations are stale and not engaging." And the measured
consequence: "reacting to user's intent in real-time leads to 30-50% higher engagement than
needing to wait days or hours for recommendations to refresh."

That number is the entire justification for the system. Hold on to it — the rest of the paper is
what you have to build once you have decided 60 ms is the budget.

### Step 2 — The graph, and Algorithm 1

Pinterest is a bipartite graph `G = (P, B, E)`: pins `P`, boards `B`, an edge when a user saved
a pin to a board. Recommending from a query pin `q` is a random walk that alternates sides:

```
   BasicRandomWalk(q, E, α, N):
     totSteps = 0; V = 0
     repeat
       currPin = q
       currSteps = SampleWalkLength(α)
       for i in 1..currSteps:
         currBoard = E(currPin)[rand()]        # pin -> board
         currPin   = E(currBoard)[rand()]      # board -> pin
         V[currPin]++
       totSteps += currSteps
     until totSteps ≥ N
     return V
```

Twenty lines, and it is already a personalized recommender: the top visit counts are the pins
most reachable from `q`. Note there is no matrix, no factorization, no model. Note also that `N`
— the step budget — is the only thing that determines runtime.

Lane 1 of this topic's crate runs exactly this and measures what is wrong with it: a hit rate of
0.403, but **45% of every returned list is the global bestseller list**, because an unbiased
walk's visit distribution drifts toward degree.

### Step 3 — Biasing the walk (innovation 1)

The fix for personalization-beyond-the-query is to make edge selection depend on the *user*, not
just the graph. `PersonalizedNeighbor(E, U)` prefers edges matching the user's features —
language, topic — so the same query set gives different results to different people. The paper
is careful about why this is cheap: "one could think of this method as using a different graph
for each user where edge weights are tailored to that user (but without the need to store a
different graph for each of the 200+ million users)." In practice the weights take values from a
small discrete set and edges for similar languages are stored consecutively in memory, so
`PersonalizedNeighbor` is a *subrange operator* — a slice, not a filter.

Table 3 is the sharpest measurement in the paper. Percentage of results in the target language,
basic walk vs Pixie:

```
                     En→Japanese   Japanese→Japanese
   BasicRandomWalk        16.35%          52.95%
   PixieRandomWalk        80.33%         100.00%

                     En→Slovak     Slovak→Slovak
   BasicRandomWalk         2.13%          16.06%
   PixieRandomWalk        42.55%         100.00%
```

Slovak goes from 2.13% to 42.55%. For a small-language user the basic walk was returning
essentially nothing usable.

### Step 4 — Multiple query pins, and the step-allocation problem (innovation 2)

A user is not one pin. Pixie takes a weighted query set `Q = {(q, w_q)}` built from recent
interactions, weighted by recency and interaction type, and runs a separate walk per query pin.

Which raises a budgeting question with a non-obvious answer. High-degree query pins need *more*
steps, because their walks diffuse across many neighbours. But allocate steps linearly in degree
and a low-degree pin gets **less than one step** — its interest is silently dropped. The paper:
"the challenge remains that if we assign the number of steps in linear proportion to the degree
then we can end up allocating not even a single step to pins with low degrees."

Equation 1's scaling factor grows sub-linearly:

```
   s_q = |E(q)| · (C − log|E(q)|)          C = max_p log|E(p)|      (over ALL pins)
   N_q = w_q · s_q / Σ_{r∈Q} s_r
```

Note `C` is the maximum over the *whole graph*, not over the query set. Get that wrong and the
highest-degree query pin gets `s_q = 0`; the crate's `every_query_pin_gets_at_least_one_step`
test catches it, and the test also asserts the step ratio is strictly below the degree ratio,
which is the sub-linearity.

### Step 5 — The multi-hit booster (innovation 3)

Equation 3:

```
   V[p] = ( Σ_{q ∈ Q} sqrt( V_q[p] ) )²
```

A pin visited 4 times from one query pin scores 4. A pin visited twice from each of two query
pins scores `(√2+√2)² = 8`. Same total visits, twice the score. The intuition: "candidates with
high visit counts from multiple query pins are more relevant to the query than for example
candidates having equally high total visit count but all coming from a single query pin."

Note the boost leaves single-source scores unchanged — `(√4)² = 4` — so it is strictly a bonus,
not a re-weighting.

**And now the honest part.** Lane 2 of this crate measures the booster against plain summed visit
counts on a synthetic graph, at one interest per user and at three, and finds **no gain either
time** — 0.823 unboosted against 0.803 boosted, 0.563 against 0.547. The arithmetic is right (the
unit test pins it); the *premise* is missing. Equation 3 is a bet that a pin sitting at the
intersection of several of your interests is more engaging than one deep inside a single
interest. That is a claim about people, and a generator that draws its held-out item from the
same distribution as its training items does not contain it.

This is the most useful thing in the topic. A published trick encodes a domain assumption. Before
you ship it, measure whether your data has the assumption in it — exercise 4 asks you to build a
graph where it does, and to find how strong the effect must be before the boost pays.

### Step 6 — Early stopping (innovation 4)

The walks run for a fixed `N_q`. But you do not need convergence, you need a *stable top*. Pixie
terminates once at least `n_p` candidate pins have each been visited at least `n_v` times —
monitored with a single counter, incremented when a pin's count crosses `n_v` exactly, so the
check is O(1) per step:

```
   Algorithm 2, lines 10–13:
     V[currPin]++
     if V[currPin] == n_v: nHighVisited++
     until totSteps ≥ N or nHighVisited > n_p
```

The counter is *per walk* — Algorithm 2 is invoked once per query pin, and each pin decides for
itself. (Sharing one counter across the query set starves the later pins; the crate's
implementation note says so, because it is an easy mistake.)

The paper's measurement: at `n_p = 2000, n_v = 4`, results overlap the gold-standard long walk by
**84%** at **one third** of the runtime; at `n_v = 6` the runtime halves. Lane 2 reproduces the
shape almost exactly at `n_p = 100, n_v = 3` on a smaller graph: **35% of the steps, 2.2× faster,
0.793 top-50 overlap, hit rate unchanged**.

### Step 7 — Pruning improves quality *and* shrinks the graph

The original Pinterest graph is 7 billion nodes and over 100 billion edges. Pixie prunes it two
ways: boards whose LDA topic distribution has high entropy (diverse boards "diffuse the walk in
too many directions") are removed entirely, and for high-degree pins, edges to boards whose topic
vector has low cosine similarity to the pin's are discarded, controlled by a pruning factor δ.

The result is the one that should change your instincts about data cleaning:

> when δ = 0.91, the F1 score peaks at 58% above the unpruned graph F1 and the graph contains
> only 20% the original number of edges.

A graph a fifth the size that recommends 58% better — and, because it now fits, one that does not
have to be distributed at all. The pruned graph is 1B boards, 2B pins, 17B edges, about **120 GB**
on an r3.8xlarge with 244 GB of RAM.

### Step 8 — The two data structures that make the inner loop free

Lines 6–13 of Algorithm 2 are the whole runtime, so two structures get hand-built:

**`edgeVec`** — every adjacency list concatenated into one contiguous array, with an offset per
node, allocated from an object pool. Sampling a neighbour of node `i` is then:

```
   F[ offset_i + (rand() % (offset_{i+1} − offset_i)) ]
```

One multiply, one modulo, one load. No per-node allocation, no fragmentation, no pointer chasing.

**The visit counter** — an open-addressing hash table with linear probing and a multiplicative
hash, sized to `N` up front because "the number of pins with non-zero visit counts can never
exceed the number of steps", so it never resizes. Linear probing is chosen explicitly for cache
locality.

And an operational detail worth stealing: Pixie uses **Linux HugePages** to raise the page size
from 4 KB to 2 MB, "thus decreasing the number of page table entries needed by a factor 512. Too
many page table entries is especially problematic on virtual machines; the HugePages option
enabled Pixie on virtual machines to serve twice as many requests at half the runtime."

## How to read the paper (with the concepts in hand)

- **§1.** The scale claims and the 30–50% engagement number. This is the budget the rest obeys.
- **§2 Related work.** One pass. The paragraph on Twitter's WTF is the connection to the GraphJet
  guide; the paragraph on collaborative filtering explains why factorization was rejected
  (complexity linear in nodes, and Pinterest has billions).
- **§3 + Algorithm 1.** Read the twenty lines and convince yourself the cost is `N`, full stop.
- **§3.1 innovation (1).** Biasing, and the "different graph per user without storing one" trick.
- **§3.1 innovation (2) + Equations 1–2.** The step-allocation problem. Derive for yourself why
  linear allocation starves low-degree pins, then check `C` is the graph-wide maximum.
- **§3.1 innovation (3) + Equation 3.** The booster. Then read Step 5 above and hold the claim
  loosely.
- **§3.1 innovation (4) + Algorithm 2 lines 10–13.** Early stopping, and note the counter is per
  walk.
- **§3.2 Graph pruning.** The 58%-better-at-20%-of-the-edges result.
- **§3.3 Implementation.** `edgeVec`, the visit counter, HugePages, the once-a-day graph build.
  This is the section a systems engineer should read twice.
- **§4.1 + Tables 1–2.** Hit rate against content-based baselines (6.3% / 23.1% / 52.2% at top
  10/100/1000) and the A/B lifts (homefeed +48%, localization +48–75%).
- **§4.2 + Figures 1–3 + Table 3.** Runtime linear in steps; stability against step count; the
  language-biasing table; the early-stopping parameter sweeps.
- **After the paper.** Implement `pixie.rs` and reproduce lane 2. Then do exercise 2 — biasing —
  which is the one innovation the crate leaves to you, and the one with the biggest measured
  effect in the paper.

## Questions to answer in notes.md

1. Pixie's cost is `O(N)` steps, independent of graph size. Name the two things that *are*
   affected by graph size, and say what each costs at Pinterest's scale (hint: §3.3 and §4.2's
   cache-miss remark).
2. Derive the failure of linear step allocation: with query pins of degree 1 and 10,000 and a
   budget of 10,000 steps, how many steps does the low-degree pin get under `N_q ∝ |E(q)|`, and
   under Equation 1? Then explain why `C` must be the graph-wide maximum.
3. Lane 2 finds the multi-hit booster gives no gain. Write down the precise property a data set
   must have for Equation 3 to pay, as a statement about the joint distribution of (query pins,
   held-out item). Then say how you would test for it in one query, before implementing anything.
4. Pruning improves F1 by 58% while removing 80% of edges. That is a statement about the *graph*,
   not the algorithm. What is the equivalent move for a workload you know, and what would you
   measure to find your δ?
5. Early stopping monitors `n_p` pins reaching `n_v` visits. Why is that a good proxy for "the
   top of the ranking has stopped moving", and construct a graph where it is a bad one.

## Done when

- [ ] You can state the cost model in one sentence and explain why it makes 60 ms possible.
- [ ] You can write Algorithm 1 from memory.
- [ ] You can explain all four innovations and what each one fixes.
- [ ] You can give the language-biasing numbers and the pruning result.
- [ ] Your `pixie.rs` reproduces lane 2's early stopping (~35% of steps, ~0.79 overlap) and you
      have measured the multi-hit booster yourself rather than assuming it helps.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Eksombatchai, Jindal, Liu, Liu, Sharma, Sugnet, Ulrich, Leskovec. *Pixie: A System for
  Recommending 3+ Billion Items to 200+ Million Users in Real-Time.* WWW 2018 —
  [arXiv:1711.07601](https://arxiv.org/abs/1711.07601).
- Leskovec & Sosič. *SNAP: A General-Purpose Network Analysis and Graph-Mining Library* — the
  library Pixie is built on.
- Local exercise stub: `topics/42-recommendations-social/experiments/pixie.rs`.
- Topic 38 (GraphRAG) — HippoRAG's personalized PageRank is the same primitive with different
  seeds; topic 18 (GPU) — `edgeVec` is a CSR by another name.
