# Topic 14 notes — vector search

## Predictions (fill BEFORE implementing hnsw.rs / quant.rs)

Baseline (provided, measured): brute force 185 QPS at recall 1.0
(100K × 128-d f32 = 51 MB per scan, 500 queries in 2.70 s).

| config | predicted recall@10 | predicted QPS | actual recall | actual QPS |
|---|---|---|---|---|
| hnsw ef=16 | | | | |
| hnsw ef=64 | | | | |
| hnsw ef=256 | | | | |
| u8 scan+rescore ×1 | | | | |
| u8 scan+rescore ×4 | | | | |

| question | prediction | actual |
|---|---|---|
| hnsw build time for 100K (vs 2.7 s for one brute sweep) | | |
| ef=16→256: how many × QPS lost for how much recall gained? | | |
| u8 scan ×4: above or below the hnsw curve? (it's O(n) but 4× fewer bytes) | | |
| max_level with m=16 on 100K points (ln n / ln m ≈ ?) | | |

## Implementation log

- [ ] hnsw.rs: level draw + insert (Alg 1/4) + search (Alg 2); all
      5 tests green
- [ ] quant.rs: affine u8 + symmetric distance + rescore pipeline;
      all 5 tests green
- [ ] ann_bench curve recorded above
- [ ] optional: qdrant docker on the same data — its ef curve vs
      mine:
- [ ] stretch: sift-1m from ann-benchmarks; recall/QPS there:

Surprises / dead ends:

## Questions from the reading guides

### HNSW paper (reading-hnsw-paper.md)

1. Why mL = 1/ln(M) ⇒ E[max level] = ln(n)/ln(M):
2. M-nearest vs Alg-4 heuristic on two clusters (draw it):
3. Why ef ≥ k; what happens at ef = k:
4. 1M × 128-d, M=16: vectors vs links bytes:
5. Skip-list analogue of the fixed entry point:

### qdrant HNSW + filtering (reading-qdrant-hnsw.md)

1. Why the visited pool matters more than in hop_bench:
2. ACORN 2-hop vs payload_m extra links — cost each, when each:
3. M14's estimate_cardinality equivalent (label bitmaps):
4. Why full_scan_threshold is in kB not points:
5. Build/serve split → kuzu transient/persistent + Delta_Matrix map:

### qdrant quantization (reading-qdrant-quantization.md)

1. u8 dot expansion derivation + what's stored per vector:
2. Why PQ hurts HNSW traversal more than IVF scans:
3. binary 1536-d vs u8 128-d: bytes/distance/recall/oversampling:
4. ADC LUT [m×256] in L1 — d=128 m=16 vs m=64:
5. M14 quantization rung — commit + reason:

### usearch (reading-usearch.md)

1. Bytes/node: tape vs Vec-of-Vecs (headers, slack, allocator):
2. Why preallocate max link slots:
3. 1% filter on usearch traversal — what happens, qdrant's fix:
4. Template metric vs enum scorer → compiled-vs-vectorized map:
5. My hnsw.rs layout decision + M17 SIMD needs:

### PQ (reading-pq.md)

1. m=16 vs m=64 at fixed bytes — which knob trades what:
2. Chunk independence + OPQ ↔ BYTE_STREAM_SPLIT:
3. When ADC table build dominates:
4. Residual encoding in FOR terms:
5. Why nobody ships SDC:

### DiskANN (reading-diskann.md)

1. SSD reads per hop: naive HNSW-on-disk vs DiskANN:
2. α > 1 shortens walks — geometric argument:
3. Beam width W ↔ topic 0's MLP:
4. PQ steers / f32 ranks — the remaining failure mode:
5. M28: DiskANN over S3 — what breaks, which knob:

## Cross-topic threads

- Recall/QPS curve = the RUM triangle with a new axis: you can now
  buy speed with CORRECTNESS, not just space.
- HNSW = topic 2's skip list in metric space; level ~ -ln(U)/ln(M)
  is Geometric(1/M) in disguise.
- Quantization = topic 12's compression-IS-performance, lossy
  edition; oversample+rescore = late materialization.
- Filtered search = topic 10's planner inside an index: estimate
  cardinality → pick HNSW / ACORN / plain scan; percolation is the
  measured failure mode.
- DiskANN block layout = topic 3's pages + topic 0's MLP for SSDs;
  visited-set pooling = hop_bench's stamp trick, concurrent.

## M14 log (vector index + distance kernels)

- [ ] vector property type + l2/dot/cosine kernels (scalar; M17
      SIMD)
- [ ] HNSW index type alongside M3 range indexes
- [ ] `CALL db.idx.vector.query(...)` surface
- [ ] filtered search: post-filter + oversampling first; record the
      percolation cliff for M22
- [ ] engine-vs-raw recall/QPS bench (M11 overhead measurement)

## Done when

- All hnsw + quant tests green; ann_bench table filled; HNSW beats
  brute-force QPS by >10× at recall ≥ 0.9.
- Reading-guide questions answered; M14 quantization decision
  committed.
