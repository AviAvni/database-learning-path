# Louvain to Leiden: communities that stay connected

Community detection's most-used algorithm (Louvain) has a bug in its
GUARANTEES, not its code: it can output communities that are
internally DISCONNECTED. Traag, Waltman & van Eck demonstrate it,
explain why, and fix it with one extra phase. Read it as a
correctness paper wearing a clustering costume — very topic-16.

## Modularity + Louvain in five lines

```
  Q = (1/2m) Σ_ij [ A_ij − k_i·k_j/2m ] · δ(c_i, c_j)
      "edges inside communities, minus what a degree-preserving
       random graph would put there"   (γ = resolution knob)

  Louvain:  repeat until stable:
    1. local moves: greedily move single vertices to the neighbor
       community with max ΔQ           (fast: ΔQ is O(deg) to eval)
    2. aggregate: contract each community to a super-vertex,
       recurse on the smaller graph
```

The local-move kernel — the part both algorithms share and Leiden
speeds up with a queue:

```rust
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

## The bug (paper §2, Fig. 1 — internalize this figure)

A vertex v can be the BRIDGE holding community C together. Local
moves later relocate v (its ΔQ is evaluated against current
neighbors, not C's connectivity) — C is left in two pieces that the
aggregation phase then FREEZES into one super-vertex forever. Up to
25% of Louvain communities on real graphs end up disconnected
(§Results); iterating Louvain makes it WORSE, not better.

The root cause generalizes: greedy local search + irreversible
aggregation = errors that can't be undone. (Compare topic 21's
rule-ordering trap: greedy destructive rewriting parks in a local
optimum; egg's fix was also "don't destroy — keep options open".)

## Leiden's fix

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
that are guaranteed γ-connected (Theorem: Leiden communities are
connected; iterated Leiden converges to subset-optimal partitions).
Empirically it's also FASTER than Louvain (the queue) — the fix
costs nothing.

## Engine-side notes (for M24)

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

## Questions (answer in notes.md)

1. Reproduce Fig. 1's failure in your head (or on paper) with a
   5-vertex example: which move disconnects the community and why
   was its ΔQ positive?
2. The resolution limit: modularity at γ=1 can't see communities
   smaller than ~√(2m). Where does that bite a fraud-ring query on
   a payments graph, and which knob (γ, or CPM as the paper hints)
   fixes it?
3. Leiden's refinement merges randomly ∝ exp(ΔQ/θ). What breaks if
   you make it greedy-deterministic (the paper tells you — §Methods)?
4. Map one Leiden iteration onto the M20 sparse core: which steps
   are SpGEMM, which are the SPA-style local kernel, and where do
   delta matrices interact with aggregation?
5. Louvain communities can be disconnected — write the topic-16
   style property test for a community-detection procedure
   (connectivity check per community = one BFS each, or one
   FastSV on the induced subgraph).

## References

**Papers**
- Traag, Waltman, van Eck — "From Louvain to Leiden: guaranteeing
  well-connected communities" (Scientific Reports 2019,
  [arXiv:1810.08473](https://arxiv.org/abs/1810.08473)) — §2 and
  Fig. 1 are the bug; §Methods has the randomized refinement and why
  greedy breaks it
