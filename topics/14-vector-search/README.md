# Topic 14 — Vector Search

qdrant territory, and every DB is adding it. The ANN problem: return
the k nearest vectors WITHOUT scanning everything, trading exactness
for speed. The whole field is one curve — **recall@k vs QPS** — and
every algorithm is a point-generator on it.

## The problem, measured (bench lane 1, provided — runs today)

`cargo run --release --bin ann_bench` — 100 000 × 128-dim f32 (51 MB) in 200
clusters, 500 queries, k=10:

```
brute force: 4.28 s total, 117 QPS — recall 1.000 by definition
```

**117 queries per second, with perfect recall for free.** That single point is
what the entire approximate-nearest-neighbour field exists to beat, and stating
it first changes how you read every ANN result afterwards: a QPS figure quoted
without its recall is not a measurement, because the trivial way to get more QPS
is to return worse answers.

It is worth being precise about *why* it is slow, because it decides which lane
should win. 51 MB of vectors is small — this is a compute problem, not a memory
one: 500 queries × 100 000 candidates × 128 dimensions is 6.4 G multiply-adds.
So brute force here is really a SIMD exercise (topic 17), which means the two
exercise lanes attack different walls. HNSW cuts the *number* of candidates;
scalar quantization shrinks each candidate 4× and rescopes the same scan. Predict
where the two land relative to each other, and to this row, before writing either.

## 1. The problem shape

Exact k-NN over n vectors of dimension d = n·d multiply-adds per
query: memory-bound streaming (topic 12's lesson: 100K × 128-d f32 =
51 MB per scan). Indexes buy sublinear queries with three currencies:
RAM, build time, and recall.

```
             recall@10
   1.0 ┤ brute force ●
       │        HNSW ef=256 ●
       │      HNSW ef=64 ●          ← the curve every ANN
       │    HNSW ef=16 ●              paper/bench plots
       │  IVF nprobe=1 ●
   0.5 ┤
       └──────────────────────────► QPS (log)
```

## 2. HNSW anatomy

A skip list generalized to proximity graphs (topic 2's ladder, in
metric space):

```
 L2:  ●────────────────●              sparse "highways"
       \                \
 L1:  ●──●─────●────────●──●          each node: level ~ -ln(U)·1/ln(M)
       \  \     \        \  \
 L0:  ●─●─●─●─●─●─●─●─●─●─●─●─●      dense base layer, M0 = 2M links
```

- **search**: greedy-descend upper layers (ef=1), then best-first
  search on L0 with a candidate heap of size **ef** — ef IS the
  recall/latency knob, per query
- **insert**: draw a level, search down to it, at each level connect
  to M nearest found — but with the **heuristic**: keep a candidate
  only if it's closer to the new point than to any already-kept
  neighbor (prunes clustered edges, keeps "spread" — this is what
  makes HNSW navigable, not just M-NN)
- memory hunger: links = n·(M0 + M·E[levels]) ids + the raw vectors —
  RAM-resident by design

## 3. The quantization ladder

Compression IS performance again (topic 12), now with a recall knob:

| scheme | bytes/dim (f32=4) | distance on encoded | recall cost |
|---|---|---|---|
| scalar u8 | 1 | integer dot + affine postprocess | tiny |
| PQ (m chunks × 256 centroids) | ~0.06–0.5 | LUT sums — d/m table lookups | real |
| binary | 1 bit | XOR + popcount | big, needs rescore |

The standard trick: search quantized with **oversampling** (fetch
3–4× top), then **rescore** the shortlist with full-precision vectors
(qdrant `get_oversampled_top`, search.rs:57). Late materialization,
vector edition.

## 4. IVF and DiskANN — the other two families

- **IVF**: k-means the space into nlist cells; query probes nprobe
  nearest cells. An index on the DATA distribution, not a graph;
  pairs naturally with PQ (IVF-PQ = Faiss's workhorse). Cheap build,
  worse curve at high recall.
- **DiskANN/Vamana**: one flat graph (no levels), robust-pruned with
  slack α > 1 so greedy search converges in few hops; graph +
  full vectors on SSD, PQ codes in RAM steer the walk — one SSD read
  per hop visits a node's vectors+links together. The B-tree/LSM
  disk-layout lesson (topics 3-4) applied to ANN: layout = access
  pattern.

## 5. Filtered search — the actually hard part

`WHERE category = X AND vec NEAR q` breaks graph indexes: filtering
DURING traversal cuts edges → the graph disconnects below a
selectivity threshold (percolation: a graph with avg degree K falls
apart when ~1/K of nodes survive — qdrant estimates this literally,
build.rs:378-386). The menu qdrant implements (search.rs:59-84):

```
 selectivity ~1.0  → HNSW, filter as you score
 selectivity  low  → plain scan of the filtered ids (index useless)
 in between        → ACORN (traverse 2-hop through blocked nodes)
                     or extra category-aware links (payload_m)
```

The planner-shaped decision (topic 10!): estimate filter cardinality
→ pick the algorithm. M14 inherits this: graph query + vector
similarity = the anchor-selection problem again.

## Newer work (2025–2026)

**Practical and Asymptotically Optimal Quantization of
High-Dimensional Vectors in Euclidean Space for Approximate Nearest
Neighbor Search** (extended RaBitQ) — Jianyang Gao, Yutong Gou,
Yuexuan Xu, Yongyi Yang, Cheng Long, Raymond Chi-Wing Wong. Proc. ACM
Manag. Data 3(3), Article 202 (SIGMOD 2025).
[doi](https://doi.org/10.1145/3725413) ·
[arXiv](https://arxiv.org/abs/2409.09913)

RaBitQ (Gao & Long, SIGMOD 2024, [doi](https://doi.org/10.1145/3654970))
is the rung §3's ladder is missing: apply a random rotation, snap each
normalized vector to the nearest rotated hypercube vertex — one bit per
dimension, 32× smaller than f32 — and estimate distances with an
estimator that is unbiased and comes with an error bound (§2.2 of this
paper). One bit is too coarse to reach high recall without rescoring
against the raw vectors, so the extension quantizes to B bits per
dimension with a codebook built by shifting, normalizing and rotating
a grid of B-bit integers, and splits each code so its most significant
bits are exactly the 1-bit RaBitQ code: search estimates from those
bits first and reads the remaining bits only for candidates it cannot
prune (§1, §3, §4.2). At more than 6 bits per dimension, LVQ's average
relative distance error is 1.3–3.1× larger than extended RaBitQ's at
the same bit budget (§5.2.1, Fig. 3; arXiv v1 numbering). It slots
between the scalar-u8 and binary rows of §3's table, and the
TurboQuant callout in
[reading-qdrant-quantization.md](reading-qdrant-quantization.md)
describes Qdrant's own rotate-then-quantize family.

**Fast Vector Search in PostgreSQL: A Decoupled Approach** — Jiayi Liu,
Yunan Zhang, Chenzhe Jin, Aditya Gupta, Shige Liu, Jianguo Wang. CIDR
2026. [pdf](https://www.cidrdb.org/cidr2026/papers/p2-liu.pdf)

pgvector stores its HNSW and IVF indexes in PostgreSQL pages, so every
graph hop goes through the buffer manager and every update is an
in-place page write — a poor fit for indexes built to live in memory.
PostgreSQL-V keeps PostgreSQL's index-access-method interface but
implements the index outside the page structure, using native
libraries (Faiss, hnswlib) inside an LSM-style framework of a mutable
memtable plus immutable indexed segments; crash consistency comes from
keeping the index's existence metadata in PostgreSQL's transactional
storage and recovering the memtable and per-segment delete bitmaps on
restart (§3). On SIFT10M and DEEP10M with pgvector's buffer pool sized
to hold the whole index, search is 6.5–8.9× faster than pgvector with
IVF_FLAT and 2.9–3.5× faster with HNSW (§4.2, Figs. 3–4). For the
capstone's M14 this is the integration question in miniature: whether
the vector index lives in the engine's own storage or beside it with a
consistency protocol.

**Revisiting Filtered ANN Benchmarks: A Hardness-Controlled Benchmark
Generator for Realistic Evaluation** — Mintaek Lim, Dogeun Kim, Minwoo
Kim, Jaeyoung Do. PVLDB 19(10), VLDB 2026.
[arXiv](https://arxiv.org/abs/2606.14193)

§5 says filtered search is the hard part; this paper asks how to
measure how hard a given filtered query is. Selectivity alone does not
order queries: a more restrictive filter makes a vector-first
(post-filtering) search harder, because more candidates must be
fetched, but a filter-first search easier, because less is left to
traverse (§3.4.3). The authors
define the over-fetch factor — how many vector-ranked candidates must
be examined before K of them satisfy the filter — and build
α-Hardness on it; across datasets and strategies it tracks measured
performance with Spearman correlation typically ρ ≤ −0.7, while
selectivity and attribute–vector correlation are often near zero or
change sign (§3.2, §3.4.3, Fig. 3). Widely used synthetic filtered-ANN
benchmarks turn out to sample a narrow, easy band of that hardness
range. Read it with §5's planner menu and
[reading-qdrant-hnsw.md](reading-qdrant-hnsw.md): a planner that
estimates only filter cardinality is estimating the proxy this paper
shows is unreliable.

## Experiments (`experiments/`)

1. `brute.rs` + `data.rs` + `distance.rs` — PROVIDED: exact top-k
   oracle over seeded clustered vectors; the recall referee.
2. `hnsw.rs` — YOU implement: insert (level draw, greedy descent,
   heuristic neighbor selection, M/M0/ef_construction) + search (ef
   knob). Tests pin recall@10 ≥ 0.9 at ef=128 vs the oracle.
3. `quant.rs` — YOU implement: global-min/max scalar u8 quantization +
   integer-dot distance + rescoring pipeline. Tests pin the error
   bound and rescored recall.
4. `ann_bench` — PROVIDED: 100K × 128-d, 1K queries; brute-force
   baseline, then your HNSW recall/QPS across ef ∈ {16..256}, then
   quantized+rescore. Plot the curve, compare qdrant on same data
   (optional, via docker) in notes.md.

## Reading guides

| guide | chapter |
|---|---|
| [reading-hnsw-paper.md](reading-hnsw-paper.md) | HNSW: a skip list in metric space |
| [reading-qdrant-hnsw.md](reading-qdrant-hnsw.md) | Qdrant's HNSW: filtered search is a planner problem |
| [reading-qdrant-quantization.md](reading-qdrant-quantization.md) | The quantization ladder: shrink, search, rescore |
| [reading-usearch.md](reading-usearch.md) | usearch: HNSW with the fat trimmed |
| [reading-pq.md](reading-pq.md) | Product quantization: 2^128 centroids in 16 bytes |
| [reading-diskann.md](reading-diskann.md) | DiskANN: one SSD read per hop |

(helix-db was on the menu but its public repo now ships only
CLI/SDKs — engine source no longer readable; qdrant + usearch cover
the territory.)

## Capstone M14

Vector index on node properties + distance kernels:

- [ ] `vector` property type on nodes; distance kernels (l2, dot,
      cosine) — scalar now, SIMD in M17
- [ ] HNSW index built from experiments/hnsw.rs, wired as an index
      type next to M3's range indexes
- [ ] Cypher surface: `CALL db.idx.vector.query(label, prop, vec, k)`
      (FalkorDB-compatible shape)
- [ ] the filtered-search decision: label+property filters over the
      vector index — start with post-filter + oversampling, record
      the percolation cliff for M22
- [ ] bench: recall/QPS curve inside the engine vs raw index (the
      M11 interpretation-overhead measurement again)
