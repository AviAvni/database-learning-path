# Topic 25 notes — GNNs & graph ML

## Baseline (provided code, Apple M3 Pro, measured 2026-07-10)

SBM: 64 blocks x 256 = 16,384 vertices, m=566,564 directed
(avg_deg 34.6), p_in=0.12, p_out=0.00025, build 34.4 ms.

| lane | result |
|---|---|
| uniform walks 65,536 x 40 | 61.2 ms, 42.8 Msteps/s |
| SpMM (D^-1 A) x X[16384x64] | 3.42 ms/iter, 21.2 GFLOP/s |
| dense matmul [16384x64]x[64x64] | 5.12 ms/iter, 26.2 GFLOP/s |

- SpMM at **81% of dense matmul throughput** — the 64-float rows the
  gather drags along amortize the irregular access. Fat RHS forgives
  sparsity; topic 20's SpMV (RHS width 1) never gets this mercy.
- Walk generation is rng-bound, not memory-bound: 42.8 Msteps/s ≈ 23 ns
  per step (rng + one CSR row index) — the corpus for skip-gram costs
  less than one training epoch will.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| node2vec p=1,q=0.5 Msteps/s vs uniform's 42.8 (rejection + has_edge binary search per candidate) | | |
| ring-of-cliques distinct-per-walk: q=0.25 vs q=4.0 ratio | | |
| skipgram 1 epoch over 2.6M-step corpus, d=64, 5 negs — seconds | | |
| SBM intra-cos − inter-cos margin after 1 epoch | | |
| gcn_norm (CSR, n=16K) ms vs one spmm iter (3.42 ms) | | |
| gcn 2-layer forward 64→64→16: predicted from kernel lanes (2 spmm-ish + 2 dense) | | |

## Implementation log

- [ ] walks.rs node2vec_walks — 4 tests (stationary dist, uniform match,
      q exploration order, p backtrack order)
- [ ] embed.rs train_skipgram — SBM block separation > 0.2 margin
- [ ] gcn.rs gcn_norm + gcn_forward — dense oracle to 1e-4, sorted rows
- [ ] prediction table reconciled
- [ ] stretch: TransE on a typed toy KG (score + margin loss), test:
      true triples outrank corrupted after training
- [ ] stretch: neighbor-sampled SAGE mean-aggregation forward; compare
      full-2-hop vs S=(25,10) nodes-touched on the SBM
- [ ] stretch: aggregate-first vs transform-first FLOP crossover sweep
      (vary d_in at fixed hidden) — plot against measured times

Surprises / dead ends:

- (from building the infra) an SBM inter-block edge count of
  p_out x inter_pairs = 0.00025 x ~134M pairs ≈ 33.5K sampled edges was
  the cheap O(m) route — the naive O(n²) Bernoulli sweep over inter
  pairs would have been 134M rng calls for the same result.

## Questions from the reading guides

### node2vec (reading-node2vec.md)

1. Second-order necessity (what first-order bias can't express):
2. Rejection-sampling draw count at q=0.25 on a bridge vertex:
3. What frozen-embedding + logistic-regression evaluation hides:
4. Edge insert invalidates unboundedly many walks — IVM implications:
5. `CALL algo.node2vec` arg surface vs proc_pagerank:

### GCN (reading-gcn.md)

1. A_hat eigenvalues in [-1,1] — renormalization's job:
2. 2-hop receptive field on RMAT (low diameter → oversmoothing):
3. FLOP crossover for (AX)W vs A(XW), Cora vs our SBM:
4. Graph baked into A_hat: what W survives 10% new edges:
5. Pending deltas in A_hat = topic 24's algo.wcc three options:

### GraphSAGE (reading-graphsage.md)

1. mean + lin_r ≈ concat — lost expressiveness:
2. Nodes-touched B=512 S=(25,10) vs full 2-hop (SBM, RMAT hub):
3. Sampling for variance vs sampling for work (Afforest):
4. Lazy embedding refresh semantics on insert (topic 8 words):
5. Transductive vs inductive as the stored artifact — ship which:

### GAT (reading-gat.md)

1. Softmax over in-edges forces which storage direction:
2. Edge passes per GAT vs GCN layer; forward-time ratio estimate:
3. a_src/a_dst split = factor-out-of-join:
4. Attention as explanation — Cypher surface:
5. Is GAT worth engine support (kernel inventory argument):

### PyG (reading-pyg-message-passing.md)

1. GCNConv.forward trace; _cached_adj_t's database name:
2. COO m x d temp vs CSR spmm memory, bench config + RMAT d=128:
3. reduce='max' breaks backward — GraphBLAS-GNN constraint:
4. NeighborLoader subgraph ↔ buffer-pool working set:
5. The one kernel to expose to Cypher:

### TransE (reading-transe.md)

1. Symmetric-relation collapse proof:
2. False-negative corruption vs cardinality stats:
3. Filtered ranking = filtered ANN (topic 14):
4. TransE degeneracy on untyped SBM:
5. Per-relation vectors: where they live, DDL transactionality:

### GraphRAG-SDK (reading-graphrag-sdk.md)

1. The one-query replacement for expand_relationships:
2. Engine-side 3-way score fusion — WAND-able?:
3. Transactional embedding writes — cost to ingest:
4. Costing the router (stats for graph-vs-ANN choice):
5. M25 acceptance test sketch:

## Cross-topic threads

- Aggregation = M20 SpMM with dense RHS; today's number says the sparse
  kernel is NOT the bottleneck at d=64 — the transform is comparable.
  Direction switching never fires (frontier always dense — Ligra's
  PageRank row, topic 24).
- Associativity (AX)W vs A(XW) = topic 10 join ordering; GAT's
  SDDMM = topic 24's masked SpGEMM; PyG's COO-vs-CSR modes = materialize
  the join vs pipeline the aggregate.
- GraphSAGE neighbor sampling = Afforest's neighbor_rounds (topic 24) =
  block-max skipping (topic 23): pay for a sample/bound, not the input.
- Embeddings as materialized views (topic 27 preview): walks are
  non-incremental (one edge → unboundedly many stale walks); GCN's
  A_hat·H·W is algebra — delta-able in principle; SAGE's stored
  aggregator makes staleness LOCAL (recompute = one sampled forward).
- GraphRAG hybrid = topic 14 (ANN) + topic 23 (score fusion / top-k) +
  topic 10 (planning the join between indexes).
- Reproducibility bar (topic 16): seeded walks + seeded SGD; Hogwild
  parallelism trades it away — same determinism-vs-speed line as
  Leiden's seeded refinement (topic 24).

## M25 log (capstone)

- [ ] embeddings pipeline: `CALL algo.node2vec(...)` /
      `CALL algo.gcn_embed(...)` computing with M20's SpMM, writing
      vecf32 properties into the M14 vector index in one transaction
- [ ] hybrid query: pattern match + `db.idx.vector.queryNodes` in one
      Cypher plan (kill GraphRAG-SDK's k+1 round trips — pushdown join)
- [ ] snapshot semantics for embedding procedures (same decision matrix
      as topic 24's algo.wcc: flush / main-only / masked)
- [ ] staleness metadata: embedding rows carry the snapshot id they were
      computed at; queries can demand max-staleness
- [ ] stretch: SDDMM kernel in the M20 core (unlocks GAT + attention-as-
      explanation queries)

## Done when

- Three stubs green with lanes filled; prediction table reconciled;
  guide questions answered; a one-page "which embeddings can the engine
  own" memo (node2vec vs GCN vs SAGE vs external text embeddings) with
  our own numbers behind it.
