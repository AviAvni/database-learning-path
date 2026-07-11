# Topic 23 notes — full-text search & inverted indexes

## Baseline (provided code, Apple M3 Pro, measured 2026-07-10)

Corpus: 100K docs, vocab 50K zipf θ=1.0, ~10M tokens, 7.87M postings.
Gen 236 ms, index build 335 ms (single thread, HashMap tf-counting).
df(t0)=99888 (in 99.9% of docs — "the"), df(t100)=8259, df(t10000)=83.

### BM25 top-10, exhaustive TAAT oracle

| query | ms | postings walked | top1 score |
|---|---|---|---|
| common∧common [t0 t1 t5] | 8.75 | 272,310 | 0.612 |
| mid∧mid [t100 t1000] | 0.507 | 9,098 | 9.142 |
| common∧rare [t0 t12000] | 6.34 | 99,964 | 8.975 |
| rare∧rare [t9000 t15000] | 0.008 | 159 | 9.208 |

The common∧rare row is the WAND poster child: 99,964 postings walked
but the rare term (df=83, idf≈9.0) contributes ~93% of the top-1
score — nearly all of t0's 99,888 postings are provably hopeless
once the heap holds 10 docs that contain t12000. ~32 ns/posting for
the oracle (hash accumulate dominates — topic 22's Q1 story again).

### Posting-list AND/OR, sorted-vec two-pointer

| pair | AND µs | OR µs | result size |
|---|---|---|---|
| t0∧t1 (99888 ∩ 97580) | 97 | 116 | 97,474 |
| t0∧t5000 (99888 ∩ 172) | 52 | 81 | 172 |

Dense∧sparse costs HALF of dense∧dense despite a 567× smaller
output — two-pointer is O(|dense|); the walk of t0 is the price.
That asymmetry is the roaring (probe 172 u16s into bitmaps) and
galloping motivation.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| wand docs_scored on common∧rare [t0 t12000] (oracle walks 99,964) | | |
| wand on common∧common [t0 t1 t5] — does block-max help when all idfs are low? | | |
| wand speedup over oracle, common∧rare, wall clock ×? | | |
| roaring t0∧t1 AND (both ~bitmap containers) vs 97 µs vec ×? | | |
| roaring t0∧t5000 AND vs 52 µs vec ×? | | |
| roaring t0 memory vs 400 KB `Vec<u32>` | | |

## Implementation log

- [ ] wand.rs `wand_topk` — 3 oracle-match tests + work bound green
- [ ] postings.rs `Roaring` — sparse/dense/mixed oracle tests green
- [ ] prediction table reconciled
- [ ] stretch: quantize block max_score to u8 (Lucene-style), verify
      top-k unchanged (bounds may only round UP)
- [ ] stretch: galloping vec_and, three-way race vec/gallop/roaring
- [ ] stretch: RRF fusion demo — BM25 top-k + a fake vector top-k,
      `1/(60+rank)` sum, the M23 hybrid in 20 lines

Surprises / dead ends:

- df(t0) = 99,888 of 100K docs: zipf θ=1.0 over a 50K vocab puts
  rank-0 in essentially every 100-token doc. Realistic ("the") but
  it means common-term posting lists here are ~dense bitsets — good
  for the roaring dense lane, remember it when reading AND numbers.
- catch_unwind lanes again saved the bench binary: stub panics print
  `[stub — implement …]` and the provided lanes still report.

## Questions from the reading guides

### Zobel & Moffat (reading-zobel-moffat.md)

1. Expected gap at df=n/2; why 128-block bitpack beats varint there:
2. Capped accumulators vs WAND — which is exact, what does the other buy:
3. Runs/merges ↔ memtable/flush/compaction; tiered-vs-leveled for text:
4. When M23 needs positions vs cheaper substitutes:
5. Which cost models survive the BM25+vector hybrid:

### BM25 (reading-bm25.md)

1. tf for 90% of the K1+1 ceiling; keyword-stuffing implication:
2. idf smoothing at df=0 and df=N:
3. b=0.75→0 on uniform-length corpus vs tweets+books:
4. 1-byte fieldnorm worst-case score error; why ranking survives:
5. Where M23 gets relevance feedback for full RSJ weights:

### Block-max WAND (reading-blockmax-wand.md)

1. θ after heap fills on [t0 t12000]; can t0 alone cross it:
2. Why block-max helps most on common terms (block-max variance):
3. Metadata overhead/posting at 128; u8 quantization direction rule:
4. Hybrid with unbounded vector scores — WAND candidates + RRF vs fused:
5. Deleted docs holding block maxima — still exact? merge-time fix:

### Roaring (reading-roaring.md)

1. Derive 4096 crossover; when run containers win; doc-id locality:
2. Predicted bitmap∧bitmap cost for t0∧t1 vs 97 µs measured vec:
3. Galloping vs container probe — where roaring wins on memory traffic:
4. Roaring containers ↔ GraphBLAS sparse/bitmap format lattice:
5. RediSearch→GraphBLAS conversion cost FalkorDB pays; native saving:

### tantivy (reading-tantivy.md)

1. FST vs hash dict — three query types, the sorted-insert cost:
2. df-in-dictionary makes which WAND input free:
3. Tail <128 postings — vint fallback vs RediSearch always-varint:
4. Tiered segments fine for text, bad for LSM point reads — why:
5. Quickwit on S3: which files fetched, what order:

### RediSearch (reading-redisearch.md)

1. Effective block-size policy; why variable blocks resist block-max:
2. gc_marker/unique_id ↔ delta-matrix wait/version:
3. Codec ladder ↔ Lucene positions/doc-values taxonomy:
4. Varint vs bitpack bytes/posting at delta≈1; where varint wins:
5. What to lift verbatim into falkordb-rs-next-gen; what the graph changes:

## Cross-topic threads

- Segments + merge policies = topic 4's LSM; tiered-not-leveled works
  because text queries fan out to all segments anyway (no key-range
  pruning). Deletes-as-bitmap = tombstones; RediSearch GC = in-place
  compaction.
- BM25 scoring loop = topic 22's YCSB lesson: the accumulator hash
  map dominates (32 ns/posting TAAT) — WAND is to search what the
  flat-array group-by was to Q1: remove the hash from the hot loop.
- Block-max metadata = topic 3's B-tree fence keys but for SCORES;
  skip-without-decode = topic 12's zone maps, exactly (min/max per
  block, prune before touching data).
- Varint-vs-bitpack = topic 17: byte-at-a-time branchy decode caps
  throughput; 128-wide unpack is the SIMD lane.
- FST term dictionary = topic 2/3's ordered-dictionary trade: hash
  gives O(1) lookup, FST gives prefix/range/regex + compression —
  same reason B-trees beat hashes for range scans.
- Roaring containers = topic 20's GraphBLAS sparse↔bitmap format
  switch at 64K-chunk granularity; M23's hit-set → masked mxv makes
  the connection literal.
- Hybrid search RRF = topic 14's HNSW top-k fused with BM25 top-k —
  rank-based fusion sidesteps incomparable score scales.

## M23 log (capstone)

- [ ] analyzer + per-property inverted index over node/edge string
      props (codec: doc_ids_only for filters, freqs for ranked)
- [ ] BM25 top-k procedure with block-max WAND; node ids = doc ids
- [ ] hit-set as roaring bitmap → mask input to M20 matrix ops
- [ ] RRF hybrid with M14's HNSW lane
- [ ] decide mutable-chained-blocks (RediSearch) vs immutable-segment
      (tantivy) — leaning segments + merge, matching the LSM the
      capstone already has

## Done when

- Both stubs green with lanes filled; prediction table reconciled;
  guide questions answered; M23 design decision (segments vs mutable
  blocks) argued in writing.
