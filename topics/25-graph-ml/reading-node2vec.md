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

A **random walk** — start at a vertex, repeatedly hop to a random
neighbor, record the sequence — turns a graph into a corpus of
"sentences" whose "words" are vertex ids. That is DeepWalk's entire
insight: word2vec (the standard word-embedding trainer) only needs a
stream of tokens where co-occurrence implies relatedness, and
vertices that co-occur on short walks are exactly the related ones.
Generate, say, 10 walks of length 80 per vertex, and the learning
half of the problem is *finished* — solved by an NLP tool that never
knows it's looking at a graph. Cheap, too: our scalar Rust walker
does 42.8 million steps/second on an M3 Pro.

### Step 3 — skip-gram with negative sampling: the training objective

Skip-gram trains two vectors per vertex (an embedding z and a context
vector c) so that pairs that co-occur within a window on some walk
get high dot products, and random pairs get low ones. Maximize
`log sigma(z_u . c_v)` for co-visited pairs (sigma = the sigmoid
squashing a dot product into a probability), and `log sigma(-z_u . c_n)`
for k random "negative" vertices n — the negatives are what stop the
trivial solution where every vector is identical. PyG's
`Node2Vec.loss` (node2vec.py:135-160) is a direct transcription —
read it as the reference: two embedding lookups, inner product,
`-log(sigmoid)`, positive + negative terms summed. Walk generation
there is `torch.ops.pyg.random_walk` (node2vec.py:64) — a custom
C++/CUDA op, because Python-level walking would dominate runtime.
The lesson in that anchor: in this whole pipeline, the *walker* is
the systems bottleneck, not the SGD.

### Step 4 — the p/q bias: a second-order walk

node2vec's contribution is to bias Step 2's uniform walk with two
knobs, evaluated against the *previous* vertex t — making it a
**second-order walk** (the next-hop distribution depends on the edge
you arrived by, not just where you stand):

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
from t: t itself (weight 1/p — the backtrack knob), mutual neighbors
of t and v (weight 1 — sideways), everything else (weight 1/q — the
outward knob). This figure is §3.2, and §3.2 is the whole paper. The
second-order property is what costs: any preprocessing must be
per-EDGE (t, v), not per-node — which is where Step 6's trap comes
from.

### Step 5 — what the knobs buy: roles vs communities

The q knob selects which *kind* of similarity the embedding encodes,
by shaping what a walk's co-occurrence window contains:

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

Sampling from Step 4's weighted distribution in O(1) is a solved
problem — an **alias table** (a precomputed pair of arrays that turns
a biased die roll into one uniform draw plus one comparison) — but
because the walk is second-order, the original implementation builds
one alias table per directed edge over the destination's neighbors:
O(1) sampling but **O(m · avg_deg) memory** — on our 16K-vertex SBM
that's 566K x 34.6 ≈ 20M table entries for a toy graph. This is the
documented reason node2vec "doesn't scale"; it's the sampling that
doesn't. Fixes:

- rejection sampling (KnightKing, our stub's prescription): draw
  uniform from N(v), accept with w/w_max, w_max = max(1, 1/p, 1/q).
  O(1) memory; expected draws worsen as p, q leave 1.
- or accept first-order walks (DeepWalk) — on many benchmarks the
  p/q gain is small; know what you're buying.

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

### Step 7 — what this looks like from inside a database

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
