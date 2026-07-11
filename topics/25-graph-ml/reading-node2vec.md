# Reading guide — "node2vec: Scalable Feature Learning for Networks" (Grover & Leskovec, KDD 2016)

Read it as a *sampling-strategy* paper: the contribution is not the
learning (that's word2vec, untouched) but a parameterized family of
neighborhood definitions. A database person should recognize the move:
"what is a node's context?" is a query, and p/q are its knobs.

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
