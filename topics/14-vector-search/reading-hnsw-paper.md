# HNSW: a skip list in metric space

The index behind nearly every production vector store is topic 2's
skip list generalized to proximity graphs: express layers over a
navigable base graph, greedy descent, and one query-time knob (ef)
that buys recall with latency. Before you open the paper, this
chapter builds the machine one concept at a time — the search
problem, greedy routing on a graph, the layer trick, the two beams,
and the one heuristic that makes the whole thing navigable — then
maps the paper's five algorithms onto those concepts. They map
almost line-for-line onto usearch's implementation
([reading-usearch.md](reading-usearch.md)), so read the two together.

## The problem in one sentence

Return the k nearest of 1M 128-dimensional vectors without computing
1M distances per query — exact search streams 512 MB and does 128M
multiply-adds every single time, while HNSW answers in a few hundred
distance computations at recall@10 (the fraction of the true 10
nearest neighbors the approximate answer actually contains) above
0.95.

## The concepts, step by step

### Step 1 — k-NN search, and why "approximate" is the product

k-nearest-neighbor (k-NN) search takes a query vector and returns
the k database vectors with the smallest distance to it (l2, dot, or
cosine — the algorithm won't care, see Step 7). Exact k-NN has
exactly one implementation: compute all n distances, keep the k
best. That's a memory-bound streaming scan — topic 12's lesson, now
per query. The entire ANN (approximate nearest neighbor) field is
one trade: accept recall < 1.0 in exchange for touching a *tiny,
query-dependent subset* of the data. Every algorithm is a point on
the recall-vs-QPS curve in the topic README; HNSW's claim to fame is
generating the best points on it while keeping the trade adjustable
per query.

### Step 2 — the proximity graph: navigate instead of scan

A proximity graph connects each vector to a handful of its near
neighbors, and search becomes navigation: start anywhere, repeatedly
hop to whichever neighbor is closest to the query, stop when no
neighbor improves. Each hop only computes distances for one node's
~16–32 neighbors — that's the sublinearity.

```
        q ×
             ●───●          greedy routing: from entry ●,
            /     \         always hop to the neighbor
      ●───●        ●        nearest to q; stop at a local
       \   \      /         minimum — hopefully the true
        ●───●───●  ← entry  nearest neighbor
```

This is NSW, the paper's predecessor: one navigable graph, greedy
routing from a random entry. It worked, but with two flaws: node
degree grew polylogarithmically with n (early-inserted nodes
accumulated links), and quality depended on insertion order. Long
routes across the dataset and short local links were tangled in one
graph.

### Step 3 — the skip-list fix: layers with geometrically fewer nodes

A skip list (topic 2) fixes slow linked-list search by adding
express lanes: each element gets a random level, higher levels have
geometrically fewer elements, and search descends from sparse to
dense. HNSW applies exactly this fix to the proximity graph — that's
the "Hierarchical" in the name:

```
 L2:  ●────────────────●              sparse "highways"
       \                \
 L1:  ●──●─────●────────●──●          each node: level ~ -ln(U)·mL
       \  \     \        \  \
 L0:  ●─●─●─●─●─●─●─●─●─●─●─●─●      dense base layer, M0 = 2M links
```

```
 skip list:  express lanes over a linked list, level ~ Geometric(p)
 HNSW:       express graphs over a proximity graph, level ~ ⌊-ln(U)·mL⌋
```

with `mL = 1/ln(M)` — chosen so level occupancy drops by factor M
(the per-node link budget, Step 6's table), exactly a skip list's
p = 1/M. Upper layers hold long links between far-apart points;
layer 0 holds everyone with short links. Search cost becomes
O(log n) descent plus a constant-quality local search at L0 —
and, unlike NSW, it no longer depends on insertion order.

### Step 4 — search: greedy descent, then a bounded best-first beam

The query path (paper's Alg 5) has two phases. Phase one: from the
top layer's entry point, greedily descend — on each upper layer keep
just the single closest node found (a beam of width 1), then drop a
layer. Phase two, on layer 0: best-first search with **ef** (the
"expansion factor", the number of candidate results kept while
searching — THE recall/latency knob), tracked by two heaps: a
min-heap of candidates to expand (nearest on top) and a bounded
max-heap of the best ef results seen (worst on top). Stop when the
nearest unexpanded candidate is farther than the worst kept result —
no expansion can improve the answer.

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

The costs to notice: ef is per-QUERY — the recall/latency trade is
decided at search time, not build time; nothing in the index
changes. And the visited set is the hot structure — allocated and
cleared once per query, which is why qdrant and usearch both pool it
(topic 13's stamp trick).

### Step 5 — insert: draw a level, search down, connect

Insert (paper's Alg 1) reuses search. Draw the new point's level
ℓ = ⌊-ln(U)·mL⌋ (Step 3). From the top entry point, greedily descend
(ef=1) to layer ℓ+1 — just finding the neighborhood. Then from layer
ℓ down to 0, run the Step 4 beam with `ef_construction` (a
build-time ef, typically ~100–128), pick M neighbors from the beam's
results (how to pick is Step 6), add bidirectional links, and shrink
any neighbor that now exceeds its budget (M on upper layers, M0 = 2M
on layer 0). Cost: an insert is roughly one search plus O(M) link
edits — building the index is ~n searches, which is why build time
is one of the three currencies (RAM, build time, recall).

### Step 6 — the neighbor-selection heuristic: directions, not distances

The load-bearing detail (paper's Alg 4): when connecting a new point
to M neighbors, do NOT take the M nearest. Take candidates
nearest-first, and keep candidate c only if
`d(c, new) < d(c, kept)` for every already-kept neighbor — c must be
closer to the new point than to anything already chosen. Effect:
neighbors cover DIRECTIONS, not just distances — a dense nearby
cluster gets one representative edge, and the remaining budget buys
long links outward:

```
   M-nearest:  new ●══▶ ○○○ (all 3 links into one cluster;
                          the other cluster is unreachable)
   heuristic:  new ●──▶ ○   (one link per direction:
                    └────────▶ ●  far cluster stays connected)
```

Without it, inter-cluster navigability dies — greedy routing from
one cluster can never reach another, and recall collapses no matter
how big ef gets. `extendCandidates` and `keepPrunedConnections` are
the paper's own knobs over the heuristic. This is also where
implementations differ or cheat: qdrant's `use_heuristic` flag
(graph_layers_builder.rs:41-42) makes it optional; usearch always
applies it.

### Step 7 — parameters, memory, and the warts

The ecosystem froze the paper's advice into defaults:

| param | paper | usearch default | meaning |
|---|---|---|---|
| M | 5-48 | 16 (`connectivity`) | links/node upper layers |
| M0 | 2M | 32 (`connectivity_base`) | links at layer 0 |
| ef_construction | ~100 | 128 (`expansion_add`) | build-time beam |
| ef | ≥ k | 64 (`expansion_search`) | query-time beam — THE knob |

Three properties round out the picture:

- **Metric-agnostic**: distance only enters via comparisons, so HNSW
  works for any metric-ish function — cosine/dot/l2 are one
  codebase.
- **Memory hunger**: links cost n·(M0 + M·E[levels]) ids on top of
  the raw vectors — for n=1M, d=128, M=16 that's ~512 MB of vectors
  plus ~140 MB of u32 links, RAM-resident by design (DiskANN exists
  because of this — [reading-diskann.md](reading-diskann.md)).
- **Deletes are the unsolved wart**: the paper has none; real
  systems tombstone + rebuild (qdrant has a
  graph_layers_healer.rs) — the CSR-update-pain story (topic 13)
  again.

## How to read the paper (with the concepts in hand)

The paper numbers its pseudocode; the steps above are its reading
lens:

- **§1–3 (intro, related work, NSW recap)** — skim; Steps 1–2. The
  one thing to extract is *why* NSW's degree grew and how the layers
  fix it (Step 3).
- **Alg 1 (INSERT)** — Step 5. Note the two phases: ef=1 descent to
  layer ℓ+1, then ef_construction beams from ℓ down to 0.
- **Alg 2 (SEARCH-LAYER)** — Step 4's two heaps, in the authors'
  words. Match each line against the Rust condensation above.
- **Alg 3 vs Alg 4 (SELECT-NEIGHBORS simple vs heuristic)** — Step 6.
  Alg 3 is the strawman (M nearest); Alg 4 is the product. Work the
  two-cluster picture by hand.
- **Alg 5 (K-NN-SEARCH)** — Step 4's phase structure: descent + one
  Alg 2 call at layer 0 with ef.
- **§4 (complexity)** — the mL = 1/ln(M) derivation; question 1
  below.
- **§5 (evaluation)** — skimmable; the recall/QPS curves are the
  topic README's curve, measured.

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
