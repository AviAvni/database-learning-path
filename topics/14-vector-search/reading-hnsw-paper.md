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

Every paper claim below carries a section, algorithm-line, or figure
number from Malkov & Yashunin, *"Efficient and robust approximate
nearest neighbor search using Hierarchical Navigable Small World
graphs"*, IEEE TPAMI 42(4), 2018 — read here as
[arXiv:1603.09320v4](https://arxiv.org/abs/1603.09320). Code anchors
are `qdrant/qdrant@44ad62f` and `unum-cloud/usearch@9fd6b01`, the
revisions pinned in `resources/codebases.md`. Where the paper and an
implementation disagree, this guide says so rather than smoothing it
over.

## The problem in one sentence

Return the k nearest of n high-dimensional vectors without computing
n distances per query — because the exhaustive alternative, measured
on this topic's own bench, runs at **117 QPS at recall 1.000**, and
that single point is what every ANN index is betting against.

That number is not borrowed. `./verify.sh 14` builds 100 000
random 128-dimensional f32 vectors (51 MB), issues 500 queries for
k=10, and the brute-force lane takes **4.28 s** on an Apple M3 Pro
(`topics/14-vector-search/notes.md`, baseline measured 2026-07-28).
Work out what the machine was doing:

```
  distances per query   = 100 000 vectors
  multiply-adds each    = 128 dimensions
  queries               = 500
  ------------------------------------------------
  total multiply-adds   = 500 × 100 000 × 128 = 6.4 × 10⁹
  wall clock            = 4.28 s
  throughput            = 6.4e9 / 4.28 = 1.50 × 10⁹ MAC/s
  query rate            = 500 / 4.28 = 117 QPS
```

1.5 G multiply-adds per second is a *healthy* number for one core —
the scan is not slow, there is simply too much of it. You cannot fix
117 QPS by making the loop faster; a 4× SIMD win buys 468 QPS, still
three orders of magnitude short of a production vector store. The
only lever with the right exponent is touching less data, and that is
what a proximity graph sells. HNSW's claim is a few hundred distance
computations per query instead of 100 000 — a ~300× reduction in
work, paid for with recall@10 slightly below 1.0.

**recall@10** here means the fraction of the true 10 nearest
neighbours that the approximate answer actually contains, averaged
over queries; **QPS** is completed queries per second, single
threaded unless said otherwise. Both are defined this way in
`topics/14-vector-search/README.md` and measured that way by the
bench.

## The concepts, step by step

### Step 1 — k-NN search, and why "approximate" is the product

> **In:** a query vector `q`, a dataset of `n` vectors, an integer
> `k`. **Out:** the vocabulary for the rest of the guide — *exact
> k-NN*, *ANN*, *recall*, and the reason the approximate answer is
> the product rather than a concession.

**k-nearest-neighbour (k-NN) search** takes a query vector and
returns the k database vectors with the smallest distance to it (l2,
dot, or cosine — the algorithm will not care; see Step 7). Exact k-NN
has exactly one implementation: compute all n distances, keep the k
best. That is a memory-bound streaming scan — topic 12's lesson, now
paid once per query, and it is the 117 QPS above.

**Approximate nearest neighbour (ANN)** search is the entire field
built on one trade: accept recall < 1.0 in exchange for touching a
*tiny, query-dependent subset* of the data. Every algorithm is a
point on the recall-vs-QPS curve in the topic README.

The word "approximate" is doing something specific. It is not that
the index is a lossy cache of a correct answer you could get later;
it is that *the recall you want is an input*. HNSW's distinguishing
property is that this input is supplied **per query**, after the
index is built (Step 4), so one index serves a 0.90-recall
autocomplete and a 0.999-recall reranker at different latencies.

### Step 2 — the proximity graph: navigate instead of scan

> **In:** the n vectors and a distance function. **Out:** a graph
> where each node links to a handful of near neighbours, and a search
> procedure — greedy routing — whose cost is *degree × hops* instead
> of n.

A **proximity graph** connects each vector to a handful of its near
neighbours, and search becomes navigation: start anywhere,
repeatedly hop to whichever neighbour is closest to the query, stop
when no neighbour improves. Each hop computes distances for one
node's neighbours only — with degree 16 and, say, 40 hops that is
640 distance computations instead of 100 000.

```
        q ×
             ●───●          greedy routing: from entry ●,
            /     \         always hop to the neighbor
      ●───●        ●        nearest to q; stop at a local
       \   \      /         minimum — hopefully the true
        ●───●───●  ← entry  nearest neighbor
```

That is **NSW**, the paper's own predecessor (§3 recaps it). It
worked, but §3 names two flaws: node degree grew polylogarithmically
with n, because early-inserted nodes kept accumulating links from
later ones, and the graph's quality depended on insertion order. Long
routes across the dataset and short local links were tangled into one
structure, so you could not tune them separately.

### Step 3 — the skip-list fix: layers with geometrically fewer nodes

> **In:** NSW's single tangled graph. **Out:** L layers, each a
> proximity graph over a geometrically smaller sample, plus the level
> assignment rule and the constant `mL` that sets the ratio.

A skip list (topic 2) fixes slow linked-list search by adding
express lanes: each element gets a random level, higher levels hold
geometrically fewer elements, and search descends from sparse to
dense. HNSW applies exactly this fix to the proximity graph — the
"Hierarchical" in the name:

```
 L2:  ●────────────────●              sparse "highways"
       \                \
 L1:  ●──●─────●────────●──●          each node: level ⌊-ln(U)·mL⌋
       \  \     \        \  \
 L0:  ●─●─●─●─●─●─●─●─●─●─●─●─●      dense base layer, Mmax0 = 2M links
```

Algorithm 1 line 4 of the paper is the rule, and the floor matters:

```
 skip list:  express lanes over a linked list, level ~ Geometric(p)
 HNSW:       l ← ⌊-ln(unif(0..1)) · mL⌋          (Alg. 1, line 4)
```

**mL** is the level normalisation constant. §4.1: *"a simple choice
for the optimal mL is `1/ln(M)`, this corresponds to the skip list
parameter p = 1/M with an average single element overlap between the
layers."* Work out why that ratio falls out — this is question 1, and
it is four lines:

```
  level l = ⌊ -ln(U) · mL ⌋,  U ~ Uniform(0,1),  mL = 1/ln M

  P(l ≥ j) = P( -ln(U)/ln M ≥ j )        substitute mL
           = P( -ln(U)   ≥ j·ln M )
           = P(  U       ≤ e^(-j ln M) )  exponentiate, flip
           = M^(-j)

  so layer j holds n·M^(-j) nodes in expectation: each layer up
  is M× thinner, exactly p = 1/M.

  top layer = the j where n·M^(-j) ≈ 1  ⇒  j = ln n / ln M

  n = 1 000 000, M = 16:
      ln(1e6)/ln(16) = 13.8155 / 2.7726 = 4.98  ⇒  ~5 layers
```

Five layers to descend, then one bounded search at the base. Unlike
NSW, none of this depends on insertion order: the level is drawn
from a distribution, not earned by arriving early.

One implementation note to carry into
[reading-qdrant-hnsw.md](reading-qdrant-hnsw.md): qdrant does not
floor. `graph_layers_builder.rs:392` calls `.round()` on the same
`-ln(U)·level_factor` expression, which raises the fraction of nodes
promoted above layer 0 from `1/(M−1)` to roughly `M^(−1/2)` — for
M=16, from 6.7% to about 25%. usearch floors, via a C++ cast to an
integer type (`index.hpp:4339`). Same paper, two different graphs.

### Step 4 — search: greedy descent, then a bounded best-first beam

> **In:** the layered graph, a query `q`, and two integers `k` and
> `ef`. **Out:** the k nearest found, plus the reason `ef` is the
> only knob you turn at query time.

The query path (Algorithm 5, K-NN-SEARCH) has two phases. Phase one:
from the top layer's entry point, greedily descend — `for lc ← L …
1`, each layer calling SEARCH-LAYER with **ef = 1**, keeping just the
single closest node found, then dropping a layer. Phase two, on layer
0: one SEARCH-LAYER call with the user's `ef`, then return the K
nearest elements from the result set W.

**ef** ("size of the dynamic candidate list" in the paper's own
words) is the number of candidate results kept while searching — the
recall/latency knob. Algorithm 2 tracks it with two structures: a
min-heap `C` of candidates to expand and a bounded set `W` of the
best ef found so far. The stop test is Algorithm 2 lines 7–8:
extract the nearest candidate, and if it is farther than the worst
element of W, break — no unexpanded candidate can improve the answer.

```rust
// ILLUSTRATION — not quoted from any file; this is Algorithms 2 and 5
// condensed into one function. The real ones are the paper's
// pseudocode, and in code at usearch include/usearch/index.hpp:4629
// (search_to_find_in_base_) and qdrant
// lib/segment/src/index/hnsw_index/graph_layers.rs:109 (search_on_level).
fn search(idx: &Hnsw, q: &[f32], k: usize, ef: usize) -> Vec<Id> {
    let mut ep = idx.entry_point;
    for level in (1..=idx.max_level).rev() {
        ep = greedy_closest(idx, level, ep, q);   // Alg 5: ef=1 descent
    }
    let mut cands = MinHeap::from([(dist(q, ep), ep)]);  // Alg 2's C
    let mut best = BoundedMaxHeap::new(ef);              // Alg 2's W
    let mut visited = VisitedSet::from([ep]);            // Alg 2's v
    while let Some((d, c)) = cands.pop() {
        if d > best.worst() { break; }         // Alg 2, lines 7-8
        for n in idx.neighbors(0, c) {
            if !visited.insert(n) { continue; }
            let dn = dist(q, idx.vec(n));
            if dn < best.worst() || !best.full() {   // Alg 2, line 13
                cands.push((dn, n));
                best.push_evicting((dn, n));   // ef bounds BOTH
            }
        }
    }
    best.take_top(k)                           // Alg 5: K nearest of W
}
```

Two costs to notice. First, `ef` is per-*query*: the recall/latency
trade is decided at search time, nothing in the index changes. Second,
`visited` is the hot structure — it is touched once per neighbour
examined and must be cleared once per query, which is why both qdrant
and usearch pool it rather than allocating (topic 13's stamp trick;
qdrant's is `lib/segment/src/index/visited_pool.rs:78`, a `u8`
generation counter that only really zeroes the array every 255
queries).

Note the asymmetry Algorithm 5 creates: `W` holds at most `ef`
elements, and the function returns `K` of them. If `ef < K` there are
not enough elements to return, which is why every implementation
clamps `ef` up to at least `k`. At exactly `ef = k`, `W` is full from
the first k neighbours examined and the line-13 admission test
degenerates to "strictly better than the current worst" — the beam
can never hold a candidate that is temporarily bad but leads
somewhere good, and recall falls off sharply.

### Step 5 — insert: draw a level, search down, connect

> **In:** a new vector and the existing graph. **Out:** the graph
> with the new point linked in, and the cost model that makes build
> time one of the three currencies.

Insert (Algorithm 1) reuses search. Draw the new point's level
`l = ⌊-ln(U)·mL⌋` (Step 3). From the top entry point, greedily
descend with ef=1 down to layer `l+1` — just locating the
neighbourhood. Then from layer `min(L, l)` down to 0, run the Step 4
beam with **efConstruction** (a build-time ef), pick M neighbours
from the beam's results with SELECT-NEIGHBORS (Step 6), add
bidirectional links, and shrink any neighbour that now exceeds its
budget — `Mmax` on upper layers, `Mmax0` on layer 0.

Cost: an insert is roughly one search plus O(M) link edits, so
building is ~n searches. §4.1 is explicit that efConstruction has no
canonical default — the guidance is to *"select an efConstruction
value that is large enough to produce K-ANNS recall close to unity
during the construction process (0.95 is enough for most
use-cases)."* The paper's own experiments use whatever that turned
out to be: §5's 200M SIFT run uses efConstruction=500 (5.6 hours) and
a cheaper efConstruction=40 run (42 minutes) on the same hardware.
The frequently quoted "100" comes from Fig. 10's 10M SIFT example
(3 minutes on four 10-core Xeon E5-4650 v2), not from a stated
default.

### Step 6 — the neighbour-selection heuristic: directions, not distances

> **In:** the efConstruction candidates found in Step 5 and a budget
> M. **Out:** which M of them become edges — and why "the M nearest"
> is the wrong answer.

This is the load-bearing detail. The paper gives two selectors:

- **Algorithm 3, SELECT-NEIGHBORS-SIMPLE** — return the M nearest.
  The strawman.
- **Algorithm 4, SELECT-NEIGHBORS-HEURISTIC** — walk candidates
  nearest-first and keep candidate `e` only if, per line 11, *"e is
  closer to q compared to any element from R"*, the set already kept.
  In other words: `d(e, q) < d(e, r)` for every kept `r`.

Effect: neighbours cover **directions**, not just distances. A dense
nearby cluster gets one representative edge — every other member of
that cluster is closer to the representative than to the new point,
so the test rejects it — and the remaining budget buys long links
outward:

```
   M-nearest:  new ●══▶ ○○○ (all 3 links into one cluster;
                          the other cluster is unreachable)
   heuristic:  new ●──▶ ○   (one link per direction:
                    └────────▶ ●  far cluster stays connected)
```

Without it, inter-cluster navigability dies: greedy routing from one
cluster can never reach another, and recall collapses no matter how
large `ef` gets — the beam explores a component that does not contain
the answer.

Algorithm 4 takes two further flags, and both are usually off.
`extendCandidates` widens the candidate set to the candidates'
neighbours; the paper says it is *"set to false by default"* and is
useful only for extremely clustered data.
`keepPrunedConnections` back-fills the budget with rejected
candidates so every node reaches exactly M links. Neither appears in
qdrant's implementation of the heuristic
(`lib/segment/src/index/hnsw_index/links_container.rs:47`); qdrant
implements the plain Algorithm 4 and makes the whole heuristic
optional behind a `use_heuristic` flag
(`graph_layers_builder.rs:41-42`).

### Step 7 — parameters, memory, and the warts

> **In:** everything above. **Out:** the numbers you will actually
> type into a config, where the paper's advice ends and the
> ecosystem's convention begins, and the two things HNSW does badly.

The paper gives ranges and one derived constant; the ecosystem froze
particular values into defaults. Keep the columns separate:

| param | what it is | paper (§4.1) | qdrant default | usearch default |
|---|---|---|---|---|
| M | links/node, upper layers | *"a reasonable range of M is from 5 to 48"* | 16 (`types.rs:1412`) | 16 (`index.hpp:1563`, `connectivity`) |
| Mmax0 | links at layer 0 | *"simulations suggest 2·M is a good choice"* | `m * 2` (`config.rs:46`) | `connectivity * 2` (`index.hpp:1591`) |
| mL | level constant | `1/ln(M)` | `1/ln(max(m,2))` (`graph_layers_builder.rs:317`) | `1/log(connectivity)` (`index.hpp:4149`) |
| efConstruction | build-time beam | no default; *"large enough to produce recall ≈ 0.95 during construction"* | 100 (`types.rs:1409`) | 128 (`index.hpp:1568`) |
| ef | query-time beam — THE knob | ≥ k, chosen per query | defaults to `ef_construct` (`config.rs:48`) | 64 (`index.hpp:1573`) |

Three properties round out the picture.

**Metric-agnostic.** Distance enters only through comparisons, so
one codebase serves cosine, dot and l2. usearch takes this furthest:
ten metric kinds in one enum (`index_plugins.hpp:114-133`).

**Memory hunger.** §4.2.3 gives the formula directly: average memory
per element is `(Mmax0 + mL·Mmax) · bytes_per_link`, which for
4-byte ids and M in 6..48 the paper reports as *"about 60-450 bytes
per object"*. Work the standard configuration:

```
  n = 1 000 000,  d = 128,  f32 vectors,  M = 16
  Mmax = M = 16,  Mmax0 = 2M = 32,  mL = 1/ln 16 = 0.36067
  bytes_per_link = 4 (u32 id)

  vectors :  1e6 × 128 × 4 B                    = 512.0 MB
  links   :  1e6 × (32 + 0.36067×16) × 4 B
          =  1e6 × (32 + 5.771) × 4 B
          =  1e6 × 151.08 B                     = 151.1 MB
                                                  ---------
  total                                           663.1 MB
  vectors / links                               = 3.39×
```

Two honest caveats on that 151 MB. The paper's formula uses `mL`
where the *expected number of layers above 0* is actually
`Σ_{j≥1} M^(-j) = 1/(M−1) = 1/15 = 0.0667`, because Algorithm 1
floors the draw. Using the flooring value gives
`(32 + 0.0667×16) × 4 = 132.3 B`, so the paper's figure is ~14% high
— it is the reserved capacity, not the occupied one, and every
implementation that preallocates per-level link arrays actually pays
the higher number. And it counts only ids: qdrant's real level-0
container also stores a length, and usearch's tape adds a 10-byte
node header per point (`index.hpp:2341` — an 8-byte key plus a 2-byte
level) and a 4-byte count per level
(`index.hpp:4150-4151`). Whatever the accounting, the conclusion
holds: **vectors dominate links by roughly 3–4×**, which is why
quantizing the vectors (topic 14's other half,
[reading-pq.md](reading-pq.md)) is the memory lever and shrinking the
graph is not.

**Deletes are the unsolved wart.** The paper has no delete algorithm
at all — Algorithms 1–5 cover insert and search only. Real systems
tombstone and rebuild; qdrant carries a whole
`graph_layers_healer.rs` to repair the links a removed point leaves
dangling. It is the CSR-update-pain story from topic 13 again: a
structure optimised for a static read layout is expensive to mutate.

## How to read the paper (with the concepts in hand)

The paper numbers its pseudocode; the steps above are the reading
lens.

| paper | step | what to extract |
|---|---|---|
| §1–3 (intro, related work, NSW) | 1–2 | *why* NSW's degree grew with n, and how layers fix it |
| Alg. 1 (INSERT) | 5 | the two phases, and the floor in line 4 |
| Alg. 2 (SEARCH-LAYER) | 4 | lines 7–8 (the stop test) and line 13 (the admission test) |
| Alg. 3 vs Alg. 4 | 6 | Alg 3 is the strawman; Alg 4 line 11 is the product |
| Alg. 5 (K-NN-SEARCH) | 4 | ef=1 descent, then one Alg 2 call at layer 0 |
| §4.1 | 3, 7 | mL = 1/ln(M), Mmax0 = 2M, M ∈ 5..48, the efConstruction guidance |
| §4.2.1 | 3 | the O(log N) argument — and its assumption |
| §4.2.3 | 7 | the memory formula and the 60–450 bytes/object range |
| §5 (evaluation) | — | skimmable; the recall/QPS curves are the topic README's curve, measured |

One thing to read carefully rather than accept. §4.2.1's O(log N)
scaling is derived *under the assumption of exact Delaunay graphs* —
which HNSW does not build, precisely because constructing a Delaunay
graph in high dimensions is intractable. The paper's own text says
the argument is confirmed by simulations on low-dimensional data and
that further analytic evidence is required for the high-dimensional
case. So "HNSW is O(log N)" is a claim about an idealised relative,
supported empirically for the real thing. The mechanism is worth
holding onto even so: with `p = exp(−mL)` the probability that a
greedy step stays in the same layer, the expected number of steps per
layer is bounded by `S = 1/(1 − exp(−mL))`, and the number of layers
scales as `log N` — a constant amount of work per layer, logarithmically
many layers.

## Questions (answer in notes.md)

1. Derive why mL = 1/ln(M) gives expected max level ln(n)/ln(M).
2. What breaks if you connect to the M NEAREST instead of Alg 4's
   heuristic on two well-separated clusters? Draw it.
3. Why must ef ≥ k? What happens at ef = k exactly?
4. Where does HNSW's memory go for n=1M, d=128, M=16 (f32)? Vectors
   vs links — which dominates and by how much?
5. The paper claims robustness to dimensionality vs NSW. What's the
   skip-list analogue of "the entry point is always the same node"?

## Done when

Answer each before unfolding it.

- [ ] You can explain what makes "approximate" the product rather than a compromise, using this topic's measured 117 QPS brute-force floor.
  <details><summary>Answer</summary>

  The brute-force lane is not badly written — 6.4 × 10⁹ multiply-adds
  in 4.28 s is 1.5 G MAC/s on one core. It is 117 QPS because it does
  500 × 100 000 × 128 units of work, and no constant-factor
  optimisation changes that exponent: perfect 4-wide SIMD gives 468
  QPS, still far from a production store. Approximation is the only
  lever that changes the amount of data touched — a few hundred
  distance computations instead of 100 000. And because `ef` is a
  per-query argument (Alg. 5), the amount of approximation is chosen
  by the caller after the index exists, so it is a product feature
  rather than a defect: one index serves a cheap 0.90-recall path and
  an expensive 0.999-recall path.
  </details>

- [ ] You can derive why `mL = 1/ln(M)` gives an expected max level of `ln(n)/ln(M)`.
  <details><summary>Answer</summary>

  Level is `l = ⌊-ln(U)·mL⌋` with `U ~ Uniform(0,1)` (Alg. 1, line
  4). Then
  `P(l ≥ j) = P(-ln U ≥ j/mL) = P(U ≤ e^(-j/mL)) = e^(-j/mL)`.
  Substituting `mL = 1/ln M` gives `e^(-j·ln M) = M^(-j)`. So layer
  j holds `n·M^(-j)` nodes in expectation — each layer is M× thinner,
  which is the skip list's `p = 1/M`, and §4.1 says exactly that. The
  top non-empty layer is where the expectation reaches 1:
  `n·M^(-j) = 1 ⇒ j = ln n / ln M`. For n = 10⁶ and M = 16 that is
  `13.8155 / 2.7726 = 4.98`, about five layers.
  </details>

- [ ] You can state what Algorithm 4's neighbour selection does differently from taking the M nearest, and what breaks if you take the nearest.
  <details><summary>Answer</summary>

  Alg. 3 returns the M nearest. Alg. 4 walks candidates
  nearest-first and keeps `e` only if, per its line 11, `e` is closer
  to the new point than to any already-kept neighbour. On two
  well-separated clusters, every member of the near cluster is closer
  to the first-kept member than to the new point, so all but one are
  rejected — one edge represents the whole cluster and the remaining
  budget goes to the far cluster. Take the M nearest instead and all
  M links land inside the near cluster; the far cluster becomes
  unreachable by greedy routing, so recall collapses and no increase
  in `ef` fixes it, because the beam is exploring a component that
  does not contain the answer. The paper's two extra flags,
  `extendCandidates` and `keepPrunedConnections`, are off by default
  (§4.1) and absent from qdrant's implementation
  (`links_container.rs:47-71`).
  </details>

- [ ] You can say why `ef >= k` is required and what happens at exactly `ef = k`.
  <details><summary>Answer</summary>

  Algorithm 5's last step returns the K nearest elements *of W*, and
  Algorithm 2 bounds `|W| ≤ ef`. With `ef < k` there are simply not k
  elements to return. At `ef = k` exactly, W fills from the first k
  neighbours examined and Alg. 2's line-13 admission test
  (`distance(e,q) < distance(f,q) or |W| < ef`, where f is W's
  furthest) reduces to "strictly better than the current worst". The
  beam can no longer hold a candidate that is temporarily worse but
  routes toward a better region, so the search behaves like plain
  greedy descent and recall drops sharply. Implementations clamp: for
  example usearch defaults `expansion_search` to 64
  (`index.hpp:1573`), comfortably above a typical k=10.
  </details>

- [ ] You can account for HNSW's memory at n=1M, d=128, M=16, splitting vectors from links.
  <details><summary>Answer</summary>

  Vectors: `1e6 × 128 × 4 B = 512 MB`. Links, by §4.2.3's formula
  `(Mmax0 + mL·Mmax) · bytes_per_link` with Mmax0=32, Mmax=16,
  mL=0.36067, 4-byte ids: `(32 + 5.771) × 4 = 151.08 B` per element,
  so 151.1 MB — inside the paper's stated 60–450 bytes/object range
  for M ∈ 6..48. Total ≈ 663 MB, vectors dominating links by 3.39×.
  Caveat worth stating: the formula uses `mL`, whereas the expected
  number of layers above 0 under the floored draw is `1/(M−1) =
  0.0667`, which would give 132.3 B — the paper's figure is the
  reserved capacity, ~14% above the occupied one. Either way the
  ratio is what matters: quantizing vectors is the memory lever
  ([reading-pq.md](reading-pq.md)), shrinking the graph is not.
  </details>

- [ ] You can name one place where a production implementation deviates from the paper, and say what the deviation changes.
  <details><summary>Answer</summary>

  Several are verifiable at the pinned revisions. (a) Alg. 1 line 4
  *floors* the level draw; qdrant *rounds*
  (`graph_layers_builder.rs:392`), which promotes roughly 25% of
  points above layer 0 at M=16 instead of 6.7% — a taller, wider
  hierarchy and more link memory. (b) §4.1 gives no efConstruction
  default; qdrant picks 100 (`types.rs:1409`) and usearch picks 128
  (`index.hpp:1568`). (c) The paper's `extendCandidates` and
  `keepPrunedConnections` do not exist in qdrant's heuristic
  (`links_container.rs:47-71`). (d) qdrant's serve-time `ef` defaults
  to `ef_construct` rather than to anything derived from k
  (`config.rs:48`).
  </details>

- [ ] You wrote answers to all five questions in notes.md.
  <details><summary>Answer</summary>

  The five questions are mirrored in
  `topics/14-vector-search/notes.md`. Questions 1, 3 and 4 have
  worked arithmetic above and should be re-derived rather than
  copied; question 2 wants the two-cluster picture drawn by hand;
  question 5's skip-list analogue is that a skip list also has a
  single fixed head — the entry point is the top-level sentinel, and
  descending from it is exactly Alg. 5's ef=1 phase.
  </details>

## References

**Papers**
- Malkov, Yashunin — "Efficient and robust approximate nearest
  neighbor search using Hierarchical Navigable Small World graphs"
  (IEEE TPAMI 42(4), 2018,
  [arXiv:1603.09320](https://arxiv.org/abs/1603.09320))

| where | what it says |
|---|---|
| Alg. 1, line 4 | `l ← ⌊-ln(unif(0..1))·mL⌋` — the floored level draw |
| Alg. 2, lines 7–8 | the stop test: nearest candidate worse than W's furthest |
| Alg. 2, line 13 | the admission test: `d(e,q) < d(f,q) or |W| < ef` |
| Alg. 3 / Alg. 4 | M-nearest vs the heuristic; Alg. 4 line 11 is the rule |
| Alg. 4 params | `extendCandidates` *"set to false by default"* |
| Alg. 5 | ef=1 descent `for lc ← L … 1`, one ef search at layer 0 |
| §4.1 | mL = 1/ln(M); Mmax0 = 2M; M ∈ 5..48; efConstruction guidance |
| §4.2.1 | O(log N) — under the exact-Delaunay assumption, low-d simulations |
| §4.2.3 | memory = `(Mmax0 + mL·Mmax)·bytes_per_link`, 60–450 B/object |
| Fig. 10 / §5 | efConstruction=100 example; 500 and 40 in the 200M SIFT runs |

**Code** (pins in `resources/codebases.md`)

| file:line | repo | what |
|---|---|---|
| `include/usearch/index.hpp:1563,1568,1573,1591` | usearch@9fd6b01 | M=16, efConstruction=128, ef=64, Mmax0=2M |
| `include/usearch/index.hpp:4149` | usearch@9fd6b01 | `inverse_log_connectivity` — mL |
| `include/usearch/index.hpp:4336-4340` | usearch@9fd6b01 | `choose_random_level_`, the floored draw |
| `lib/segment/src/types.rs:1409-1422` | qdrant@44ad62f | m=16, ef_construct=100 |
| `lib/segment/src/index/hnsw_index/config.rs:46,48` | qdrant@44ad62f | `m0 = m*2`, `ef = ef_construct` |
| `lib/segment/src/index/hnsw_index/graph_layers_builder.rs:317,392` | qdrant@44ad62f | `level_factor`, and `.round()` where the paper floors |
| `lib/segment/src/index/hnsw_index/links_container.rs:47-71` | qdrant@44ad62f | Algorithm 4, without the two flags |

**Companion guides**
- [reading-usearch.md](reading-usearch.md) — the algorithms as C++,
  almost line-for-line
- [reading-qdrant-hnsw.md](reading-qdrant-hnsw.md) — the production
  version, with filtering
- [reading-diskann.md](reading-diskann.md) — what to do when the
  512 MB of vectors will not fit
