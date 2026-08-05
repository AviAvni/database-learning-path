# node2vec: the neighborhood is a query, p and q are its knobs

Read node2vec as a *sampling-strategy* paper: the contribution is not
the learning (that's word2vec, untouched) but a parameterized family
of neighborhood definitions. A database person should recognize the
move: "what is a node's context?" is a query, and p/q are its knobs.
This chapter builds the whole pipeline step by step — what an
embedding is, walks as sentences, the skip-gram objective, the p/q
bias, and the memory trap that made "node2vec doesn't scale" true for
a decade — before pointing you at the paper's one essential section.

## The problem in one sentence

To feed graph structure to any vanilla ML model you must first turn
each vertex into a fixed-length vector — and the original node2vec
implementation's per-edge sampling tables cost O(m · avg_degree)
memory, which on our toy 16K-vertex SBM is already **~20 million
table entries**, so the interesting engineering is in the sampler,
not the learner.

## The concepts, step by step

### Step 1 — node embeddings: geometry as a stand-in for structure

> **In:** a graph — vertices and edges, no coordinates.
> **Out:** one dense vector per vertex, where distance in vector space stands
> in for structural closeness. Step 2 is how those vectors get trained.

A **node embedding** assigns each vertex a dense vector (say 128
floats) such that geometric closeness in vector space stands in for
structural closeness in the graph. Once vertices are points, the
entire off-the-shelf ML toolbox applies — logistic regression for
node classification, dot products for link prediction, a vector index
(topic 14) for "find similar nodes" — none of which can consume an
adjacency list directly. The cost of the move: the embedding is a
lossy snapshot. Whatever notion of "closeness" the training procedure
encoded is the only question the vectors can answer, and the graph
can change after the snapshot is taken.

### Step 2 — walks as sentences: borrow word2vec wholesale

> **In:** the graph's adjacency (a CSR neighbour list).
> **Out:** a corpus of walk "sentences" — vertex-id sequences. Step 3 feeds
> them to word2vec; Step 4 biases how they are generated.

A **random walk** — start at a vertex, repeatedly hop to a random
neighbor, record the sequence — turns a graph into a corpus of
"sentences" whose "words" are vertex ids. That is DeepWalk's entire
insight: word2vec (the standard word-embedding trainer) only needs a
stream of tokens where co-occurrence implies relatedness, and
vertices that co-occur on short walks are exactly the related ones.
Generate, say, 10 walks of length 80 per vertex (the paper's settings:
`r = 10` walks per node, `l = 80` per walk, §4.1), and the learning
half of the problem is *finished* — solved by an NLP tool that never
knows it's looking at a graph. Cheap, too: this topic's scalar Rust
walker does **42.8 million steps/second** on an M3 Pro (notes.md,
`uniform walks 65,536 × 40`).

### Step 3 — skip-gram with negative sampling: the training objective

> **In:** the walk corpus from Step 2.
> **Out:** trained embedding vectors — the Step 1 output. Step 4 changes how
> the corpus is sampled, not this objective.

**Skip-gram with negative sampling (SGNS)** trains vectors so that pairs
that co-occur within a window on some walk get high dot products, and
random pairs get low ones. Maximize `log sigma(z_u . c_v)` for co-visited
pairs (sigma = the sigmoid squashing a dot product into a probability),
and `log sigma(-z_u . c_n)` for k random "negative" vertices n — the
negatives are what stop the trivial solution where every vector is
identical. Classic SGNS keeps *two* tables — an embedding `z` and a
separate context `c` per vertex (this repo's `embed.rs:27` stub does
exactly that). PyG's `Node2Vec.loss` (node2vec.py:135) takes a shortcut
worth noting (rule 6): it looks the start node and the context node up in
**the same `self.embedding` table** (node2vec.py:140 and :142), so in that
implementation there is one vector per vertex, not two. Read it as the
reference for the *shape* — two lookups, inner product, `-log(sigmoid)`
for the positive term (:146), `-log(1 - sigmoid)` for the negatives
(:157), summed (:159). Walk generation there is `torch.ops.pyg.random_walk`
(node2vec.py:64) — a custom C++/CUDA op, because Python-level walking
would dominate runtime. The lesson in that anchor: in this whole
pipeline, the *walker* is the systems bottleneck, not the SGD.

### Step 4 — the p/q bias: a second-order walk

> **In:** Step 2's uniform walk, plus the vertex `t` you arrived from.
> **Out:** a biased next-hop distribution over `v`'s neighbours — three
> weight classes set by `p` and `q`. Step 5 reads off what they buy.

node2vec's contribution is to bias Step 2's uniform walk with two
knobs, evaluated against the *previous* vertex t — making it a
**second-order walk** (the next-hop distribution depends on the edge
you arrived by, not just where you stand). The paper's unnormalized
transition weight is `π_vx = α_pq(t,x) · w_vx`, where the search bias
`α_pq(t,x)` is `1/p` if `d_tx = 0`, `1` if `d_tx = 1`, and `1/q` if
`d_tx = 2` (§3.2, eq. for α), and `d_tx` is the shortest-path distance
from the previous vertex t to the candidate x:

```
        came from t, now at v — where next?
                     x1  (dist 1 from t: mutual neighbor)   weight 1
                    /
          t ────  v ── x2 (dist 2 from t: away)             weight 1/q
           \       \
            \       x3 (dist 2)                             weight 1/q
             └───── t  (return)                             weight 1/p
```

Every neighbor of v falls into exactly three classes by its distance
from t: t itself (`d_tx = 0`, weight `1/p` — **p is the return
parameter**, the backtrack knob), mutual neighbors of t and v
(`d_tx = 1`, weight 1 — sideways), everything else (`d_tx = 2`, weight
`1/q` — **q is the in-out parameter**, the outward knob). This figure is
§3.2, and §3.2 is the whole paper. The second-order property is what
costs: any preprocessing must be per-EDGE (t, v), not per-node — which is
where Step 6's trap comes from.

### Step 5 — what the knobs buy: roles vs communities

> **In:** the `p`/`q` bias from Step 4.
> **Out:** which notion of similarity the embedding encodes — structural
> roles or communities. Step 6 is what this costs to sample.

The q knob selects which *kind* of similarity the embedding encodes,
by shaping what a walk's co-occurrence window contains (§3.1):

- q > 1: stay near t — BFS-flavored samples → embeddings encode
  *structural roles* (hubs look like hubs, bridges like bridges,
  even across the graph from each other).
- q < 1: push outward — DFS-flavored → embeddings encode
  *communities* (homophily: my neighbors' neighbors are my people).
  Our test pins this: on a ring of cliques, q=0.25 must visit >1.15x
  more distinct vertices per walk than q=4.
- p large: don't backtrack. p small: stay glued to the previous
  vertex.

This is the "neighborhood is a query" claim made concrete: p and q
are query parameters over the same graph, producing different answer
semantics from identical storage. Choose them per workload, like any
query knob.

### Step 6 — the systems trap: alias tables vs rejection sampling

> **In:** Step 4's per-edge weighted distribution.
> **Out:** an O(1) sampler and its memory bill — the reason node2vec earned
> its "doesn't scale" reputation. Step 7 maps the fix onto engine machinery.

Sampling from Step 4's weighted distribution in O(1) is a solved
problem — an **alias table** (a precomputed pair of arrays that turns
a biased die roll into one uniform draw plus one comparison) — but
because the walk is second-order, the original implementation builds
one alias table per directed edge over the destination's neighbors:
O(1) sampling but **O(m · avg_deg) memory** — on our 16K-vertex SBM
that's `m = 566,564` directed edges × `avg_deg 34.6 ≈ 19.6M ≈ 20M` table
entries for a toy graph (notes.md graph stats). This is the documented
reason node2vec "doesn't scale"; it's the sampling that doesn't. Fixes:

- rejection sampling (KnightKing, our stub's prescription): draw
  uniform from N(v), accept with w/w_max, w_max = max(1, 1/p, 1/q).
  O(1) memory; expected draws worsen as p, q leave 1.
- or accept first-order walks (DeepWalk) — on many benchmarks the
  p/q gain is small; know what you're buying.

One biased step via rejection, the whole mechanism:

```rust
// ILLUSTRATION — not quoted; the measured node2vec step is walks.rs:56,
// with the 1/p, 1, 1/q weights at walks.rs:37-40.
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

### Step 7 — what this looks like from inside a database

> **In:** the walk + sampler machinery from Steps 2–6.
> **Out:** the mapping onto structures an engine already owns — CSR rows,
> binary search, seeded RNG.

Everything above maps onto machinery an engine already owns:

- Walks are embarrassingly parallel and CSR-native (CSR = compressed
  sparse row: each vertex's neighbor list stored as one contiguous
  sorted slice) — a database can generate them without materializing
  anything (cursor per walker).
- has_edge(t, x) for the distance-1 check = binary search in the
  sorted CSR row — O(log deg). Bloom-style edge sketches would trade
  accuracy for speed; the walk is already stochastic, so approximate
  membership is admissible (nice essay question, see notes.md).
- Determinism (topic 16 bar): seeded walks + seeded SGD =
  reproducible embeddings; document that parallel SGD (Hogwild)
  breaks this.

## How to read the paper (with the concepts in hand)

- §3.1 is Step 3 — inherited word2vec, skimmable if you've seen SGNS
  before; use PyG's `Node2Vec.loss` as the executable version.
- §3.2 is Steps 4–5 and is the whole paper: the figure, the three
  weight classes, and the BFS/DFS interpolation argument. Read it
  until you can reproduce the figure from memory.
- §3.2.1 (the alias-table preprocessing) is Step 6's trap — read it
  *as* a bug report: compute the table memory for a graph you care
  about before believing any scalability claim.
- The experiments (logistic regression on frozen embeddings) hide as
  much as they show — question 3 below.

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

## Done when

Answer each before unfolding it.

- [ ] You can explain why the walk bias must be second-order to interpolate between BFS-ish and DFS-ish neighbourhoods.

  <details><summary>Answer</summary>

  The bias `α_pq(t,x)` is a function of the *previous* vertex t as well as
  the current vertex v — it classifies each candidate x by `d_tx ∈ {0,1,2}`.
  A first-order rule (one that only sees v) cannot tell "back toward t" from
  "onward, away from t", so it cannot dial between staying local (BFS-ish,
  `q>1`) and exploring outward (DFS-ish, `q<1`). The memory of where you came
  from is the whole mechanism, and it is what forces per-edge preprocessing.

  </details>

- [ ] You can say what p and q buy — roles against communities — and predict the effect before running the lane.

  <details><summary>Answer</summary>

  `q>1` keeps walks near the origin (BFS-flavoured), so co-occurrence
  captures *structural roles* — hubs resemble hubs even far apart (structural
  equivalence, §3.1). `q<1` pushes outward (DFS-flavoured), so co-occurrence
  captures *communities* (homophily). `p` (the return parameter) tunes
  backtracking: large `p` discourages returning to t, small `p` keeps the
  walk glued locally. On the ring-of-cliques lane, `q=0.25` should visit
  >1.15× more distinct vertices per walk than `q=4`.

  </details>

- [ ] You can state the skip-gram-with-negative-sampling objective.

  <details><summary>Answer</summary>

  Maximize `log σ(z_u · c_v)` over pairs (u,v) that co-occur within a window
  on a walk, and `Σ_n log σ(-z_u · c_n)` over k random negatives n. The
  positive term pulls co-visited vectors together; the negatives push random
  pairs apart and prevent the degenerate all-equal solution. In PyG's
  `Node2Vec.loss` the two roles read from one shared table (node2vec.py:140,
  :142); classic SGNS and this repo's `embed.rs` use separate embedding and
  context tables.

  </details>

- [ ] You can explain the alias-table against rejection-sampling trade and compute the expected draw count at p=1, q=0.25.

  <details><summary>Answer</summary>

  Alias tables give O(1) draws but need one table per directed edge for a
  second-order walk — O(m·avg_deg) ≈ 20M entries on the SBM. Rejection
  sampling needs O(1) memory: propose uniformly from N(v), accept with
  probability `w/w_max`, `w_max = max(1, 1/p, 1/q)`. Expected draws = the
  reciprocal of the mean acceptance probability. At `p=1, q=0.25`,
  `w_max = max(1,1,4) = 4`; a candidate's weight is 1 for return/mutual and
  `1/q = 4` for outward, so acceptance depends on the local mix — at a bridge
  vertex where most neighbours are "away", mean acceptance ≈ (weighted mean
  of w)/4, giving ≈ 4/(fraction near 4) draws. Derive the exact figure from
  the bridge's degree split in notes.md.

  </details>

- [ ] You can explain embeddings as a materialized view and say which ones an edge insert invalidates.

  <details><summary>Answer</summary>

  Frozen embeddings are a materialized view over the walk corpus. One edge
  insert changes the transition distribution at both endpoints and therefore
  any walk that *could* have passed through them — unboundedly many, since a
  walk reaching either endpoint later is affected too. That is why the view
  is effectively non-incremental (topic 27): you cannot cheaply patch a
  bounded set of walks, so re-embedding is periodic, not per-write.

  </details>

- [ ] You wrote answers to all five questions in notes.md, and compared your walk rate against the measured uniform-walk baseline of 42.8 Msteps/s.

  <details><summary>Answer</summary>

  The baseline is `42.8 Msteps/s` (notes.md, `uniform walks 65,536 × 40` =
  2.62M steps in 61.2 ms ≈ 23 ns/step). Record your own biased-walk rate
  beside it: rejection sampling adds a `has_edge` binary search per candidate
  and repeats on rejection, so expect it below the uniform figure, more so as
  p, q move away from 1. All five `## Questions` answered in notes.md.

  </details>

## References

**Papers**
- Grover & Leskovec — "node2vec: Scalable Feature Learning for
  Networks" (KDD 2016,
  [arXiv:1607.00653](https://arxiv.org/abs/1607.00653)) — §3.2 (the
  walk bias) is the whole paper; §3.1 is inherited word2vec

**Code**
- [pytorch_geometric](https://github.com/pyg-team/pytorch_geometric)
  `torch_geometric/nn/models/node2vec.py` — `loss` (:135, positive term
  :146, negative :157, sum :159) reads start and context from one shared
  `self.embedding` (:140, :142); walks are a custom op (:64)
