# Louvain to Leiden: communities that stay connected

Community detection's most-used algorithm (Louvain) has a bug in its
GUARANTEES, not its code: it can output communities that are
internally DISCONNECTED. Traag, Waltman & van Eck demonstrate it,
explain why, and fix it with one extra phase. Read it as a
correctness paper wearing a clustering costume — very topic-16. This
chapter builds up to the bug step by step: what modularity measures,
how Louvain climbs it, exactly where the greedy climb breaks
connectivity, and how Leiden's refinement phase repairs the guarantee
for free.

*Paper: Traag, Waltman & van Eck, "From Louvain to Leiden"
(Scientific Reports 2019, arXiv:1810.08473). The disconnection
frequencies, guarantees, and refinement rule below are quoted from it;
this topic has no pinned Louvain/Leiden code, so the snippets are
illustrative pseudocode.*

## The problem in one sentence

Louvain greedily optimizes a global score with moves that never check
connectivity, and on real graphs **up to 16% of the communities it
outputs are internally disconnected** (and up to 25% are *badly*
connected) — two islands wearing one label — and iterating the
algorithm makes it worse, not better (Traag et al. 2019, abstract).

## The concepts, step by step

### Step 1 — communities, and a score for them: modularity

> **In:** the goal "find groups with many internal, few crossing
> edges".
> **Out:** modularity Q — internal edges minus a degree-preserving
> random baseline — worked on a concrete graph, and the fact that Q
> never mentions connectivity.

A **community** is a set of vertices with many edges inside the set
and few crossing its boundary — and to optimize for that, you need a
number. **Modularity** (Q) compares each community's internal edge
count against what a random graph with the same vertex degrees would
put there:

```
  Q = (1/2m) Σ_ij [ A_ij − γ·k_i·k_j/2m ] · δ(c_i, c_j)
      "edges inside communities, minus what a degree-preserving
       random graph would put there"

  The paper writes it per community (Eq. 1):
      H = (1/2m) Σ_c ( e_c − γ·K_c²/2m )
  with e_c the internal edge weight of community c (both directions)
  and K_c the sum of its vertices' degrees; γ is the resolution knob.
```

Here A_ij is the (weighted) adjacency entry, k_i is vertex i's
degree, m the total edge weight, and δ(c_i, c_j) is 1 when i and j
share a community. Work it on two triangles {a,b,c} and {d,e,f} joined
by one bridge edge c–d (m = 7 edges, 2m = 14; degrees a,b,e,f = 2 and
c,d = 3):

```
  partition {{a,b,c},{d,e,f}}, γ = 1:
    community {a,b,c}: internal edge weight e_c = 2×3 = 6  (3 edges,
                       both directions); K_c = 2+2+3 = 7
                       term = 6 − 7²/14 = 6 − 3.5 = 2.5
    community {d,e,f}: identical by symmetry              = 2.5
    Q = (1/14)(2.5 + 2.5) = 0.357

  everything in ONE community:
    e = 2×7 = 14 (all edges internal); K = 14
    term = 14 − 14²/14 = 0        →   Q = 0
```

Splitting into the two triangles scores Q = 0.357 against 0 for one
blob — modularity rewards the real structure. The subtraction is the
insight: two hubs sharing an edge is unremarkable (random graphs do
that too — k_i·k_j/2m is high), two leaves sharing an edge is signal.
γ (the resolution parameter) scales the null-model term to tune
community size. Why it matters: modularity turns "find communities"
into "maximize Q" — an optimization a greedy algorithm can attack.
Note what Q does *not* mention: connectivity. That omission is the
whole paper.

### Step 2 — Louvain: greedy local moves plus aggregation

> **In:** the objective "maximize Q".
> **Out:** Louvain's two alternating phases (greedy single-vertex
> moves, then aggregate-and-recurse) and the O(deg) ΔQ that makes a
> move cheap — plus the fact that aggregation is irreversible.

Louvain climbs Q with two alternating phases — move single vertices
greedily, then shrink the graph and repeat:

```
  Louvain:  repeat until stable:
    1. local moves: greedily move single vertices to the neighbor
       community with max ΔQ           (fast: ΔQ is O(deg) to eval)
    2. aggregate: contract each community to a super-vertex,
       recurse on the smaller graph
```

The local-move kernel — the part both algorithms share and Leiden
speeds up with a queue — is cheap because moving one vertex changes Q
by an amount (ΔQ) computable from just that vertex's edges:

```
// ILLUSTRATION — pseudocode; this topic pins no Louvain/Leiden source.
fn local_move(v: u32, g: &Csr, comm: &mut [u32], tot: &mut [f64]) -> bool {
    let mut w_to = HashMap::new();                  // topic 20's SPA, again
    for (u, w) in g.edges(v) { *w_to.entry(comm[u]).or_insert(0.0) += w; }
    let (kv, m2) = (g.wdeg(v), g.total_weight_x2());
    let (mut best, mut best_gain) = (comm[v], 0.0);
    for (&c, &w_vc) in &w_to {                      // ΔQ is O(deg) to evaluate:
        let gain = w_vc / m2                        //   edges gained inside c
                 - kv * tot[c] / (m2 * m2);         //   minus null-model expectation
        if gain > best_gain { best = c; best_gain = gain; }
    }
    // NOTE: ΔQ never asks "does removing v disconnect my old community?"
    if best != comm[v] { move_vertex(v, best, comm, tot); true } else { false }
}
```

Aggregation is what makes Louvain fast in practice: after one round
of moves on a million-vertex graph, the contracted graph might have
tens of thousands of super-vertices, and the next round runs on that.
The cost: contraction is *irreversible* — whatever the moves decided
is frozen into the super-vertices.

### Step 3 — the bug: a bridge vertex walks away

> **In:** Louvain's local move plus irreversible aggregation.
> **Out:** the concrete failure — a bridge vertex leaves, its old
> community splits into two islands that keep one label, and
> aggregation freezes them — traced on Fig. 1's graph.

A vertex v can be the BRIDGE holding community C together — remove v
and C falls into two pieces. Louvain's local move relocates v anyway:
ΔQ is evaluated against v's current neighbors' communities (look at
the code above — the comment marks the missing question), never
against C's connectivity. After v leaves, C is two disconnected
islands still sharing one community label — and the aggregation phase
then FREEZES them into a single super-vertex forever:

```
   community C, held together by bridge v:

     a ─ b            a ─ b
      \                \
       v      ──►       ·        v moved to a neighbor community
      /                /         (its ΔQ there was positive);
     c ─ d            c ─ d      {a,b} and {c,d} keep C's label,
                                 then aggregation fuses them for good
```

This is the paper's §2 and Fig. 1 — internalize this figure. On real
graphs, up to 16% of Louvain communities end up *disconnected* and up
to 25% are *badly connected* (Traag et al. 2019, abstract & Fig. 2);
iterating Louvain makes it WORSE, not better, because each iteration
adds more frozen mistakes.

### Step 4 — the root cause generalizes: greedy + irreversible = unfixable

> **In:** the bridge-vertex failure from Step 3.
> **Out:** why it's structural, not a slip — greedy local scoring plus
> irreversible aggregation accumulate errors monotonically — and the
> two possible fixes (per-move connectivity check vs. an undo path).

The failure is not a coding slip; it is a structural property of the
algorithm class: greedy local search makes locally-scored decisions,
and irreversible aggregation removes the ability to undo them — so
errors accumulate monotonically. Compare topic 21's rule-ordering
trap: greedy destructive rewriting parks a query plan in a local
optimum; egg's fix was also "don't destroy — keep options open". Any
fix for Louvain must therefore either check connectivity per move
(expensive: a connectivity query per candidate move) or restore an
undo path before aggregation freezes things. Leiden picks the second.

### Step 5 — Leiden's fix: refine before you freeze

> **In:** the "keep an undo path" fix direction.
> **Out:** Leiden's refinement phase — re-cluster within each
> community from singletons, randomized ∝ ΔH, before aggregating — so
> aggregation only ever fuses connected pieces.

Leiden inserts a refinement phase between moving and aggregating —
re-cluster each community from scratch, *within* the community, so
aggregation only ever fuses pieces that are actually connected:

```
  1. local moves (as Louvain, but with a QUEUE — only revisit
     vertices whose neighborhood changed: faster)
  2. REFINEMENT: inside each community, re-cluster from singletons,
     merging only within the community, RANDOMIZED proportional to
     ΔQ — communities split into their well-connected parts
  3. aggregate on the REFINED partition (but keep phase-1 communities
     as the initial coarse assignment)
```

Refinement is the undo mechanism: aggregation now operates on pieces
that are guaranteed γ-connected (Traag et al. 2019 prove Leiden
communities are γ-connected and, iterated, converge to subset-optimal
partitions — their Table I of guarantees). The randomization in step 2
(merge with probability ∝ exp((1/θ)·ΔH) when ΔH ≥ 0, else 0 — the
paper's Eq. 38, with H the modularity from Step 1), not greedy-max, is
load-bearing — it lets refinement explore partitions the greedy climb
would never visit; §Methods explains what breaks if you make it
deterministic (question 3). And empirically Leiden is also FASTER than
Louvain (Traag et al. 2019, abstract & Fig. 3) — the queue in phase 1
(only revisit vertices whose neighborhood changed) more than pays for
refinement. The fix costs nothing.

### Step 6 — what this costs an engine: SPA, SpGEMM, and seeds

> **In:** the Louvain/Leiden algorithm as described.
> **Out:** where each phase lands on the M20 sparse core — ΔQ on a SPA
> accumulator, aggregation as S·A·Sᵀ SpGEMM, and why randomized
> refinement makes seeding mandatory for a reproducible engine.

Mapping the algorithm onto the M20 sparse core, each phase lands on
machinery that already exists:

- ΔQ evaluation needs, per vertex: weights to each neighbor
  community + community total degrees — a hash-or-array accumulator
  keyed by community id. That's topic 20's SPA again; skew (hub
  vertices touch many communities) decides array vs hash.
- Aggregation = building the quotient graph = SpGEMM: S·A·Sᵀ with S
  the n×k assignment matrix. Louvain/Leiden over the M20 core is
  two masked SpGEMMs + a local-move kernel.
- Determinism: local-move ORDER changes the output. For a database
  procedure (`CALL algo.community()`), fix the seed and document
  that reruns on the same snapshot match (topic 16's reproducibility
  bar) — Leiden's randomized refinement makes seeding mandatory.

## How to read the paper (with the concepts in hand)

- §2 and Fig. 1 are Step 3 — read them first and reconstruct the
  bridge-vertex failure on paper (question 1) before continuing.
- The results on disconnected-community frequency (the 16%
  disconnected / 25% badly-connected numbers, and the
  it-gets-worse-with-iteration result) justify Step 4's
  framing: this is accumulation, not bad luck.
- §Methods carries Step 5: the queue-based fast local move, the
  randomized refinement (find the exp((1/θ)·ΔH) rule, Eq. 38, and the
  argument for why greedy refinement fails), and the connectivity /
  subset-optimality theorems.
- Read the guarantees the way topic 16 reads invariants: "communities
  are γ-connected" is a property you can test — question 5 turns it
  into one.

## Questions (answer in notes.md)

1. Reproduce Fig. 1's failure in your head (or on paper) with a
   5-vertex example: which move disconnects the community and why
   was its ΔQ positive?
2. The resolution limit: modularity at γ=1 can't see communities
   smaller than ~√(2m) (Fortunato & Barthélemy, "Resolution limit in
   community detection", PNAS 2007). Where does that bite a fraud-ring
   query on a payments graph, and which knob (γ, or CPM as the paper
   hints) fixes it?
3. Leiden's refinement merges randomly ∝ exp((1/θ)·ΔH) (Eq. 38). What
   breaks if you make it greedy-deterministic (the paper tells you —
   §Methods)?
4. Map one Leiden iteration onto the M20 sparse core: which steps
   are SpGEMM, which are the SPA-style local kernel, and where do
   delta matrices interact with aggregation?
5. Louvain communities can be disconnected — write the topic-16
   style property test for a community-detection procedure
   (connectivity check per community = one BFS each, or one
   FastSV on the induced subgraph).

## Done when

Answer each before unfolding it.

- [ ] You can define modularity and say what it scores.
  <details><summary>Answer</summary>

  Q = (1/2m) Σ_ij [A_ij − γ·k_i·k_j/2m]·δ(c_i,c_j): the fraction of
  edges inside communities minus what a degree-preserving random graph
  would put there. It scores "are there more internal edges than chance
  predicts?" — and mentions connectivity nowhere.

  </details>
- [ ] You can reproduce Figure 1's failure: how a bridge vertex ends up disconnected from its own community.
  <details><summary>Answer</summary>

  A vertex v holds community C connected. Louvain evaluates v's ΔQ
  against its neighbours' communities and moves it out (positive gain
  there), never checking that removing v splits C. C's two halves keep
  C's label as disconnected islands, then aggregation fuses them into
  one super-vertex permanently (paper §2, Fig. 1).

  </details>
- [ ] You can state the general root cause — greedy plus irreversible is unfixable — and what Leiden's refinement changes.
  <details><summary>Answer</summary>

  Greedy local moves score decisions locally; irreversible aggregation
  removes the undo path, so errors accumulate monotonically. Leiden adds
  a refinement phase that re-clusters within each community from
  singletons before aggregating, so aggregation only fuses genuinely
  connected pieces — restoring the undo path.

  </details>
- [ ] You can explain the resolution limit and what γ does about it.
  <details><summary>Answer</summary>

  At γ=1 modularity can't resolve communities smaller than ~√(2m)
  (Fortunato & Barthélemy 2007): small real groups get merged. Raising
  γ scales up the null-model penalty so smaller communities survive;
  the paper's CPM objective removes the size-dependence entirely.

  </details>
- [ ] You can map one Leiden iteration onto SPA and SpGEMM steps.
  <details><summary>Answer</summary>

  ΔQ evaluation is a SPA-style accumulator keyed by community id (topic
  20's SPA). Aggregation is the quotient graph S·A·Sᵀ — two masked
  SpGEMMs. So one iteration ≈ two SpGEMMs plus the local-move kernel,
  with a seed fixed for reproducibility.

  </details>
- [ ] You wrote answers to all five questions in notes.md, including the topic-16 property test for community connectivity.
  <details><summary>Answer</summary>

  Done when notes.md answers Q1–Q5, and Q5 is a concrete property test:
  for each output community, run one BFS (or FastSV on the induced
  subgraph) and assert it reaches every member — the test Louvain fails
  and Leiden passes.

  </details>

## References

**Papers**
- Traag, Waltman, van Eck — "From Louvain to Leiden: guaranteeing
  well-connected communities" (Scientific Reports 2019,
  [arXiv:1810.08473](https://arxiv.org/abs/1810.08473)) — §2 and
  Fig. 1 are the bug; §Methods has the randomized refinement and why
  greedy breaks it
