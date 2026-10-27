# Topic 14 notes — vector search

## Baseline (provided lane, Apple M3 Pro, measured 2026-07-28)

`cargo run --release --bin ann_bench` — 100 000 × 128-dim f32 (51 MB) in 200
clusters, 500 queries, k=10.

| method | recall@10 | QPS | total |
|---|---|---|---|
| brute force | 1.000 (by definition) | **117** | 4.28 s |
| HNSW (yours) | | | stub |
| u8 quantized + rescore (yours) | | | stub |

**117 QPS is the number the whole field exists to beat, and it comes with
perfect recall for free.** Every approximate index in this topic is a bet that
you can trade a measurable amount of that 1.000 for orders of magnitude of
throughput — so the honest way to read any ANN benchmark is as a *curve*
against this point, never as a single QPS figure.

Worth being precise about what makes it slow: 51 MB of vectors is small enough
to be a pure compute problem, not a memory-bound one — 500 queries × 100 000
candidates × 128 dimensions is 6.4 G multiply-adds. So brute force here is a
SIMD/bandwidth exercise (topic 17), and a scalar-quantized scan that shrinks
each vector 4× is attacking the same wall from the data side rather than the
algorithm side. Predict where those two lanes land relative to each other
before you write either.

### Lane 1b — the over-fetch factor of a filtered query (`filter_bench`, provided)

`cargo run --release --bin filter_bench` — same 100 000 × 128-dim data, 500
queries, K = 10; exact rankings, no index, so every count below is seeded and
reproduces exactly. α is the rank of the K-th *matching* vector in the exact
ordering — how many candidates a post-filter must fetch.

```
filter          s       |Vf|  | mean α   median     p99  α/(K/s) |   K/s  Eq.5 mean
independent    50.0%   50000  |   19.9      19       33     1.00  |    20      20.4
independent    10.0%   10000  |   99.1      97      170     0.99  |   100     107.4
independent     1.0%    1000  |  958.6     922     1652     0.96  |  1000    3798.1
independent     0.1%     100  | 8527.0    8153    14641     0.85  | 10000   41247.9
aligned        50.0%   50000  |   10.2      10       14     0.51  |    20      20.0
aligned        10.0%   10000  |   34.3      22      293     0.34  |   100     103.6
aligned         1.0%    1000  |  317.5     211     1918     0.32  |  1000    1524.4
aligned         0.1%     100  | 6779.4    6160    17120     0.68  | 10000   38651.5
anti-aligned   50.0%   50000  |  337.8     484      747    16.89  |    20      49.3
anti-aligned   10.0%   10000  |  852.5     687     2665     8.53  |   100     382.3
anti-aligned    1.0%    1000  | 2332.7    1734    13220     2.33  |  1000    3945.4
anti-aligned    0.1%     100  |11563.0   10532    32584     1.16  | 10000   41572.1
```

Reading it: with a filter independent of the vectors, α = K/s to within 15 %
(1 % → ~1,000 fetched for 10 results). At identical 50 % selectivity the
aligned, independent and anti-aligned filters need 10.2, 19.9 and 337.8 —
33× — which is the Lim et al. point that selectivity does not order filtered
queries by difficulty. The last column is the Eq. 5 estimator of
[arXiv:2606.14193](https://arxiv.org/abs/2606.14193) evaluated as printed
(L2 distances, not squared): it has K/s as a floor, so it overshoots the
measured α by 4.0× at 1 % independent and 4.8× at 0.1 %; the paper validates
it only by rank correlation, which this lane does not test.

## Predictions (fill BEFORE implementing hnsw.rs / quant.rs)

Baseline (provided, measured): brute force 117 QPS at recall 1.0
(100K × 128-d f32 = 51 MB per scan, 500 queries in 4.28 s).

| config | predicted recall@10 | predicted QPS | actual recall | actual QPS |
|---|---|---|---|---|
| hnsw ef=16 | | | | |
| hnsw ef=64 | | | | |
| hnsw ef=256 | | | | |
| u8 scan+rescore ×1 | | | | |
| u8 scan+rescore ×4 | | | | |

| question | prediction | actual |
|---|---|---|
| hnsw build time for 100K (vs 4.28 s for one brute sweep) | | |
| ef=16→256: how many × QPS lost for how much recall gained? | | |
| u8 scan ×4: above or below the hnsw curve? (it's O(n) but 4× fewer bytes) | | |
| max_level with m=16 on 100K points (ln n / ln m ≈ ?) | | |

## Reference numbers for the exercise lanes

Read these after filling the worksheet. What a reference solution printed
(Apple M3 Pro; HNSW m = 16, m0 = 32, ef_construction = 128, seed 42; the
heuristic neighbour selection of Alg 4 at every level, selecting m
neighbours and capping at m0 on layer 0 / m above).

### Lane 2 and lane 3 — the recall/QPS curve (`ann_bench`, one locked run)

`cargo run --release --bin ann_bench` with the reference `hnsw.rs` and
`quant.rs` in place; the same run measured the brute-force row, so the three
lanes share one machine state (brute force ran at 149 QPS in this run against
117 QPS in the baseline above, a 27 % swing between runs of the identical
code — compare lanes within a run, not across runs).

| config | recall@10 | QPS | × brute force |
|---|---|---|---|
| brute force (provided) | 1.000 | 149 | 1× |
| HNSW ef = 16 | 0.962 | 22,336 | 150× |
| HNSW ef = 32 | 0.988 | 15,490 | 104× |
| HNSW ef = 64 | 0.999 | 11,450 | 77× |
| HNSW ef = 128 | 1.000 | 8,884 | 60× |
| HNSW ef = 256 | 1.000 | 6,933 | 47× |
| u8 scan + rescore ×1 | 0.961 | 698 | 4.7× |
| u8 scan + rescore ×2 | 1.000 | 602 | 4.0× |
| u8 scan + rescore ×4 | 1.000 | 453 | 3.0× |

Build: 13.3 s for 100,000 points, about four of brute force's 3.35 s
500-query sweeps, paid once; `max_level` = 4 (ln 100,000 / ln 16 = 4.15).

What it says:

- **The numerator.** The curve the README promises: at the lowest ef the graph
  already returns 0.962 of the true top 10 at 150× the brute-force QPS, and
  recall 0.999 costs 77×. Going from ef = 16 to 256 loses 3.2× the QPS
  (22,336 → 6,933) for 0.038 of recall — nearly all of it paid before ef = 64.
  This generator is kind to HNSW: clustered Gaussian blobs, queries are
  perturbed dataset rows, so the true neighbours sit in one tight cluster;
  expect a worse curve on sift-1m-like data (the stretch goal).
- **Quantization attacks the other wall.** The u8 scan is O(n) like brute
  force and only 4× fewer bytes per vector; it buys 4.7× at ×1 — consistent
  with the compute-bound reading in the baseline (the scan stays a full
  100,000-vector pass) — and recall 0.961 at ×1 becomes 1.000 at ×2, where the
  rescore of 20 shortlist vectors costs almost nothing (698 → 602 QPS). The
  graph beats the best u8 point by 32× (22,336 / 698) at the same recall.
- **Prediction check for the worksheet**: "u8 scan ×4: above or below the HNSW
  curve?" — below, by 15× against HNSW ef = 256 at the same recall of 1.000
  (6,933 / 453).


### Filtered traversal on the reference HNSW (lane 2 extended, seeded counts)

Same index, 200 queries, K = 10, three filters × four selectivities (the
filters of lane 1b). Recall is against the exact top-10 *among matching
rows*. "post ×16 / ×64" runs an unfiltered search of ef = 16·K / 64·K and
discards non-matches; "restricted" refuses to enqueue non-matching nodes;
"score-through" enqueues them as stepping stones but only returns matches
(both ef = 128). Evals are distance evaluations per query; the matching-row
scan costs |Vf|.

```
filter        s       |Vf|  post×16  post×64 | restricted        | score-through       | scan
                                              |  recall   evals   |  recall    evals    | evals
independent  50.0%   50000    1.000    1.000 |   0.999     278   |   1.000      592    | 50000
independent  10.0%   10000    0.993    1.000 |   0.566      87   |   1.000     3438    | 10000
independent   1.0%    1000    0.159    0.650 |   0.018       5   |   1.000    26058    |  1000
independent   0.1%     100    0.014    0.073 |   0.002       5   |   0.996    99880    |   100
aligned      50.0%   50000    1.000    1.000 |   0.995     509   |   1.000      544    | 50000
aligned      10.0%   10000    0.987    0.997 |   0.909     252   |   1.000     1145    | 10000
aligned       1.0%    1000    0.708    0.948 |   0.285      24   |   1.000    21814    |  1000
aligned       0.1%     100    0.213    0.383 |   0.057       6   |   0.999    99880    |   100
anti-aligned 50.0%   50000    0.399    0.903 |   0.373     196   |   0.979     1814    | 50000
anti-aligned 10.0%   10000    0.001    0.489 |   0.016      20   |   0.986     5940    | 10000
anti-aligned  1.0%    1000    0.000    0.123 |   0.000       5   |   0.999    29206    |  1000
anti-aligned  0.1%     100    0.000    0.015 |   0.001       6   |   1.000    99880    |   100
```

What it says:

- **The over-fetch factor predicts the post-filter cliff.** Independent 1 %:
  mean α = 958.6 (lane 1b), and a ×16 fetch (160 candidates) recovers 0.159 of
  the matches, ×64 (640) recovers 0.650 — still short of α. At 10 %
  (α ≈ 99) a ×16 fetch (160) already reaches 0.993. Anti-aligned 10 %
  (α = 852.5): ×16 recovers 0.001.
- **Restricted traversal has a connectivity cliff** (the percolation the
  qdrant guide describes): independent recall 0.999 → 0.566 → 0.018 → 0.002
  as s falls 50 % → 0.1 %. With M0 = 32 a node has ~32 neighbours, of which
  s·32 match: 16, 3.2, 0.32, 0.032 on average — recall collapses once that
  expected count approaches 1, between the 10 % and 1 % rows. Aligned filters
  keep more of the graph connected (0.909 at 10 %, 0.285 at 1 %) because the
  matching rows form one geometric neighbourhood.
- **Score-through never loses recall, but it pays in evaluations**:
  3,438 at 10 % (still below the 10,000 of a scan), 26,058 at 1 % (26× the
  1,000 of a scan) and 99,880 at 0.1 % — essentially every node in the graph —
  against 100. The traversal-to-scan crossover is between 10 % and 1 %, the
  zone where the 2026 survey's winners switch from graph methods to partitioned
  scans (reading-filtered-ann-survey.md, Step 3). The survey's failing
  HNSW-plus-post-filtering at 0.1 % is the ×16 / ×64 columns here (0.014 /
  0.073).

Every count above is seeded and was identical across two runs; recall and
evaluation counts do not depend on the machine.

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
6. Same mean error, different flip rate — the margin decomposition:

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
6. RaBitQ-SSD vs DiskANN at recall 0.98, D_m = D — which resource is saturated:
7. CatapultDB at equal beam width vs equal recall — what to hold fixed:

### Filtered ANN survey (reading-filtered-ann-survey.md)

1. 1 % independent filter, K = 10: candidates a post-filter fetches, implied ef:
2. Monotonic HNSW search vs IVF scan at 0.1 %:
3. Aligned vs anti-aligned at 50 % — which is harder, by how much:
4. Why segmented-edge methods fail at 0.1 % (construction-time search):
5. M14's traversal→scan switch point, and which measurement sets it:

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
