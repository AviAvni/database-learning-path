# HNSW: a skip list in metric space

The index behind nearly every production vector store is topic 2's
skip list generalized to proximity graphs: express layers over a
navigable base graph, greedy descent, and one query-time knob (ef)
that buys recall with latency. This chapter reads the paper's five
algorithms; they map almost line-for-line onto usearch's
implementation ([reading-usearch.md](reading-usearch.md)), so read
the two together.

## The skip-list lens (topic 2 cashed in)

NSW (the predecessor) was one navigable graph: greedy routing from a
random entry, O(log n)-ish hops but polylog degree growth and a
dependence on insertion order. HNSW's fix IS the skip-list fix:

```
 skip list:  express lanes over a linked list, level ~ Geometric(p)
 HNSW:       express graphs over a proximity graph, level ~ ⌊-ln(U)·mL⌋
```

with `mL = 1/ln(M)` — chosen so level occupancy drops by factor M,
exactly a skip list's p = 1/M. Search cost: O(log n) descent + a
constant-quality local search at L0.

## The algorithms (paper numbering)

- **Alg 1 INSERT**: draw level ℓ; from the top entry point greedily
  descend (ef=1) to layer ℓ+1; from layer ℓ down to 0 run
  SEARCH-LAYER with ef_construction, connect to M selected neighbors,
  shrink any neighbor that now exceeds M_max (M0 = 2M at layer 0).
- **Alg 2 SEARCH-LAYER**: best-first over a min-heap of candidates
  and a max-heap of results, both bounded by **ef**; stop when the
  nearest candidate is farther than the worst result. The visited set
  is the hot structure — qdrant/usearch both pool it (topic 13's
  stamp trick).
- **Alg 4 SELECT-NEIGHBORS-HEURISTIC**: the load-bearing detail.
  Take candidates nearest-first; keep c only if
  `d(c, new) < d(c, kept)` for all already-kept. Effect: neighbors
  cover DIRECTIONS, not just distances — clusters get one
  representative edge plus a long link outward. Without it (simple
  M-nearest), inter-cluster navigability dies. `extendCandidates` and
  `keepPrunedConnections` are the paper's own knobs over it.

The whole query path (Alg 5 = descent + Alg 2), condensed:

```rust
fn search(idx: &Hnsw, q: &[f32], k: usize, ef: usize) -> Vec<Id> {
    let mut ep = idx.entry_point;
    for level in (1..=idx.max_level).rev() {
        ep = greedy_closest(idx, level, ep, q);   // upper layers: ef=1, just descend
    }
    let mut cands = MinHeap::from([(dist(q, ep), ep)]);  // nearest candidate on top
    let mut best = BoundedMaxHeap::new(ef);              // worst-of-ef on top
    let mut visited = VisitedSet::from([ep]);            // THE hot structure
    while let Some((d, c)) = cands.pop() {
        if d > best.worst() { break; }         // nearest cand can't improve: stop
        for n in idx.neighbors(0, c) {
            if !visited.insert(n) { continue; }
            let dn = dist(q, idx.vec(n));
            if dn < best.worst() || !best.full() {
                cands.push((dn, n));
                best.push_evicting((dn, n));   // ef bounds BOTH heaps
            }
        }
    }
    best.take_top(k)                           // hence ef ≥ k
}
```

## Parameters, with defaults the ecosystem agreed on

| param | paper | usearch default | meaning |
|---|---|---|---|
| M | 5-48 | 16 (`connectivity`) | links/node upper layers |
| M0 | 2M | 32 | links at layer 0 |
| ef_construction | ~100 | 128 (`expansion_add`) | build-time beam |
| ef | ≥ k | 64 (`expansion_search`) | query-time beam — THE knob |

## What to notice

1. ef is per-QUERY: the recall/latency trade is decided at search
   time, not build time — nothing in the index changes.
2. The heuristic (Alg 4) is where implementations differ or cheat;
   qdrant's `use_heuristic` flag (graph_layers_builder.rs:41-42)
   makes it optional, usearch always applies it.
3. Distance metric only enters via comparisons — HNSW works for any
   metric-ish function, which is why cosine/dot/l2 are one codebase.
4. Deletes are the unsolved wart: the paper has none; real systems
   tombstone + rebuild (qdrant has a graph_layers_healer.rs) — the
   CSR-update-pain story (topic 13) again.

## Questions (answer in notes.md)

1. Derive why mL = 1/ln(M) gives expected max level ln(n)/ln(M).
2. What breaks if you connect to the M NEAREST instead of Alg 4's
   heuristic on two well-separated clusters? Draw it.
3. Why must ef ≥ k? What happens at ef = k exactly?
4. Where does HNSW's memory go for n=1M, d=128, M=16 (f32)? Vectors
   vs links — which dominates and by how much?
5. The paper claims robustness to dimensionality vs NSW. What's the
   skip-list analogue of "the entry point is always the same node"?

## References

**Papers**
- Malkov, Yashunin — "Efficient and robust approximate nearest
  neighbor search using Hierarchical Navigable Small World graphs"
  (IEEE TPAMI 2018,
  [arXiv:1603.09320](https://arxiv.org/abs/1603.09320)) — Algorithms
  1-5 are the chapter; the eval is skimmable

**Code**
- [usearch](https://github.com/unum-cloud/usearch) — the paper's
  algorithms map to functions almost line-for-line; walked in
  [reading-usearch.md](reading-usearch.md)
- [qdrant](https://github.com/qdrant/qdrant) — the production version,
  walked in [reading-qdrant-hnsw.md](reading-qdrant-hnsw.md)
