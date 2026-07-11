# Reading guide — "Better bitmap performance with Roaring bitmaps" (Chambi, Lemire, Kaser, Godin — arXiv:1603.06549)

The set-representation that ate the world: Lucene doc-id sets, Spark,
ClickHouse, Druid, Pilosa — and the `postings::Roaring` stub. The
insight is that NO single representation wins: sorted arrays win
sparse, bitmaps win dense, so partition the space and choose per
chunk.

## The layout

```
  u32 value = [ high 16 bits | low 16 bits ]
                    │              │
                    ▼              ▼
     sorted Vec of (key, container); container holds the low bits:

     Array container: sorted Vec<u16>       when |chunk| ≤ 4096
     Bitmap container: [u64; 1024] = 8 KiB  when |chunk| > 4096
     Run container: (start,len) pairs       (the '16 paper's addition)

  4096 = the crossover where 2 bytes/value (array) meets
         8 KiB/65536 possible values (bitmap) — a container is
         NEVER worse than 2 bytes per value, and never bigger
         than 8 KiB.
```

## The kernel matrix (§3 — what the stub implements)

| A ∩/∪ B | array | bitmap |
|---|---|---|
| **array** | two-pointer merge (galloping when sizes differ ≥64×) | probe each u16 into the bitmap: O(|array|) word tests |
| **bitmap** | ← same, swapped | 1024 word-wise AND/OR + popcount to pick the OUTPUT container type |

Two details that carry the performance:
- **output container choice**: bitmap∩bitmap may produce a sparse
  result — popcount during the AND, convert to array if ≤4096. Union
  of bitmaps stays bitmap (never shrinks).
- **cardinality is tracked**, not recomputed — every kernel returns
  it as a byproduct (the popcount is fused into the AND loop; on
  M-series that's `cnt` on each of 1024 words, memory-bound anyway).

## Why postings lists care (vs our two-pointer vec)

Measured in fts_bench: `t0 ∧ t5000` (99888 ∩ 172 docs) costs 52 µs
with two-pointer — it walks all 99888. Roaring: t0 at df≈100K over
100K docs is ~1.5 dense chunks → bitmap containers; the 172-element
side probes 172 times → ~1 µs. Same asymmetry galloping fixes for
arrays, but roaring ALSO compresses t0 to 8 KiB·2 instead of 400 KB.

Lucene's `RoaringDocIdSet` and RediSearch's doc tables use exactly
this for filters (the `docs_ids_only` codec in
`redisearch_rs/inverted_index/src/codec/doc_ids_only.rs` is the
varint cousin). Note what roaring does NOT store: tf, positions,
scores — it's the FILTER lane (Cypher `WHERE n.name CONTAINS ...`
feeding a graph traversal), not the RANKING lane.

## Questions (answer in notes.md)

1. Derive the 4096 crossover from bytes/value. Where does the
   run-container (RLE) change the math, and what posting-list shape
   produces runs (hint: doc ids assigned by insertion order +
   crawler locality)?
2. Our t0 has df 99888 over doc space 100K = 99.9% dense. What does
   its bitmap∩bitmap AND cost vs the measured 97 µs two-pointer for
   t0∧t1? Predict before implementing (1024·2 words ANDed…).
3. Galloping (skewed array∩array) vs container probing (array∩bitmap):
   both are O(small·log/const). When does roaring still win despite
   equal asymptotics? (memory traffic of the big side)
4. M20 tie-in: a bitmap container IS a dense GraphBLAS vector chunk;
   array container = sparse. Roaring's per-chunk format switch is
   GraphBLAS's sparse↔bitmap format lattice at 64K granularity —
   compare the switch thresholds (4096/65536 vs GB_conform's).
5. M23: full-text hit set → roaring → feed as mask into a matrix
   traversal. What conversion does FalkorDB pay today going
   RediSearch → node-id set → GraphBLAS vector, and what would a
   native roaring-masked mxv save?
