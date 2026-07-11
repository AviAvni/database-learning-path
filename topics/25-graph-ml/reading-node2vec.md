# node2vec: the neighborhood is a query, p and q are its knobs

Read node2vec as a *sampling-strategy* paper: the contribution is not
the learning (that's word2vec, untouched) but a parameterized family
of neighborhood definitions. A database person should recognize the
move: "what is a node's context?" is a query, and p/q are its knobs.

## The walk bias (§3.2 — the whole paper is this figure)

```
        came from t, now at v — where next?
                     x1  (dist 1 from t: mutual neighbor)   weight 1
                    /
          t ────  v ── x2 (dist 2 from t: away)             weight 1/q
           \       \
            \       x3 (dist 2)                             weight 1/q
             └───── t  (return)                             weight 1/p

  SECOND-order: the distribution depends on the edge (t, v) you arrived
  by, not just on v. That's why preprocessing is per-EDGE, not per-node.
```

- q > 1: stay near t — BFS-flavored samples → embeddings encode
  *structural roles* (hubs look like hubs).
- q < 1: push outward — DFS-flavored → embeddings encode *communities*
  (homophily). Our test pins this: on a ring of cliques, q=0.25 must
  visit >1.15x more distinct vertices per walk than q=4.
- p large: don't backtrack. p small: stay glued to the previous vertex.

## Skip-gram with negative sampling (§3.1, inherited)

Maximize `log sigma(z_u . c_v)` for co-visited pairs, `log sigma(-z_u . c_n)`
for k random negatives. PyG's `Node2Vec.loss` (node2vec.py:135-160) is a
direct transcription — read it as the reference: two embedding lookups,
inner product, `-log(sigmoid)`, positive + negative terms summed.
Walk generation there is `torch.ops.pyg.random_walk` (node2vec.py:64) — a
custom C++/CUDA op, because Python-level walking would dominate runtime.
Our measured yardstick: 42.8 Msteps/s scalar Rust on M3 Pro.

## The systems trap: alias tables (§3.2.1)

Original impl precomputes an alias table per directed edge over the
destination's neighbors: O(1) sampling but **O(m · avg_deg) memory** —
on our 16K-vertex SBM that's 566K x 34.6 ≈ 20M table entries for a toy
graph. This is the documented reason node2vec "doesn't scale"; it's the
sampling that doesn't. Fixes:
- rejection sampling (KnightKing, our stub's prescription): draw uniform
  from N(v), accept with w/w_max, w_max = max(1, 1/p, 1/q). O(1) memory;
  expected draws worsen as p, q leave 1.
- or accept first-order walks (DeepWalk) — on many benchmarks the p/q
  gain is small; know what you're buying.

One biased step via rejection, the whole mechanism:

```rust
fn step(g: &Csr, t: u32, v: u32, p: f64, q: f64, rng: &mut Rng) -> u32 {
    let w_max = 1f64.max(1.0 / p).max(1.0 / q);
    loop {
        let x = g.neighbors(v).choose(rng);        // uniform proposal, O(1)
        let w = if x == t { 1.0 / p }              // return to t
                else if g.has_edge(t, x) { 1.0 }   // mutual neighbor: dist 1
                else { 1.0 / q };                  // away: dist 2 from t
        if rng.f64() < w / w_max { return x; }     // accept ∝ true bias —
    }                                              //   no per-edge alias table
}
```

## Engine-side notes

- Walks are embarrassingly parallel and CSR-native — a database can
  generate them without materializing anything (cursor per walker).
- has_edge(t, x) for the distance-1 check = binary search in the sorted
  CSR row — O(log deg). Bloom-style edge sketches would trade accuracy
  for speed; the walk is already stochastic, so approximate membership is
  admissible (nice essay question, see notes.md).
- Determinism (topic 16 bar): seeded walks + seeded SGD = reproducible
  embeddings; document that parallel SGD (Hogwild) breaks this.

## Questions (answer in notes.md)

1. Why must the walk bias be second-order to distinguish BFS-ish from
   DFS-ish? What can a first-order bias (weight by degree, say) not
   express?
2. Rejection sampling's expected draw count at p=1, q=0.25 on our ring
   of cliques — derive it from the weight distribution at a bridge
   vertex.
3. The paper evaluates with logistic regression on frozen embeddings.
   What does that measurement HIDE that an end-to-end GNN shows?
4. Embeddings as a materialized view: an edge insert invalidates which
   walks? Why is the answer "unboundedly many" (and what does that say
   about incremental maintenance — topic 27)?
5. For `CALL algo.node2vec()` in M25: which of (p, q, walk_len,
   walks_per_node, dim, window, negs, epochs, lr, seed) belong in the
   API, and which should be fixed opinions? Compare FalkorDB's
   proc_pagerank arg surface (topic 24).

## References

**Papers**
- Grover & Leskovec — "node2vec: Scalable Feature Learning for
  Networks" (KDD 2016,
  [arXiv:1607.00653](https://arxiv.org/abs/1607.00653)) — §3.2 (the
  walk bias) is the whole paper; §3.1 is inherited word2vec

**Code**
- [pytorch_geometric](https://github.com/pyg-team/pytorch_geometric)
  `torch_geometric/nn/models/node2vec.py` — `loss` (:135-160) is a
  direct SGNS transcription; walks are a custom op (:64)
