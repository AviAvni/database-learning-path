# Topic 14 — Vector Search

qdrant territory, and every DB is adding it. The ANN problem: return
the k nearest vectors WITHOUT scanning everything, trading exactness
for speed. The whole field is one curve — **recall@k vs QPS** — and
every algorithm is a point-generator on it.

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
