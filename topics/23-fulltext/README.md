# Topic 23 — Full-Text Search & Inverted Indexes

The third great index family after trees and hash tables: term →
sorted posting list, plus the machinery that makes it fast (compressed
blocks, FST dictionaries, BM25, block-max WAND) and the architecture
that makes it writable (Lucene's LSM-in-disguise segments). Home-turf
bonus: RediSearch — what FalkorDB delegates full-text to — is being
rewritten in Rust, and its inverted index crate is readable tonight.

## Anatomy

```
  query "quick fox"
      │ analyzer: tokenize → lowercase → stem → stopwords
      ▼
  term dictionary                    posting lists (per term, doc-sorted)
  ┌──────────────┐   TermInfo       ┌────────┬────────┬────────┐
  │ FST: bytes → │ {doc_freq,       │block 0 │block 1 │block 2 │ 128 docs/block,
  │ term ordinal │  postings_range} │Δ-packed│Δ-packed│Δ-packed│ delta + bitpack
  └──────────────┘                  └────────┴────────┴────────┘
                                     skip data: per block
                                     {last_doc, max_score}  ← block-max WAND
  + fieldnorms (doc lengths, for BM25's B)
  + fast fields (columnar doc values — topic 12 inside a text index)
```

```mermaid
graph LR
    subgraph write path — Lucene/tantivy segments = LSM
        W["docs"] --> MB["in-RAM segment<br/>(memtable)"] --> F["flush: immutable<br/>segment on disk"]
        F --> MP["merge policy<br/>(log-size tiers = compaction)"]
    end
    subgraph read path
        Q["query"] --> TD["FST term dict"] --> PL["postings + skips"] --> BMW["block-max WAND<br/>top-k"]
    end
```

Same shape as topic 4's LSM: immutable segments, background merges,
deletes as tombstone bitmaps, a reader that unions segments. Lucene
discovered LSM independently because *inverted indexes are cheap to
build and expensive to update in place* — exactly the LSM bet.

## The two speed tricks

1. **Compression that keeps random access**: doc ids stored as deltas,
   128 per block, bit-packed to the block's max width (tantivy
   `postings/compression/mod.rs:3`; RediSearch varint-encodes instead —
   `redisearch_rs/varint`). Blocks give you skip pointers for free.
2. **Score upper bounds**: BM25 saturates at (K1+1)·idf, so every term
   and every *block* has a precomputable max score. WAND uses term
   maxima to find a pivot; block-max WAND (SIGIR'11) refines with
   per-block maxima and skips whole blocks that provably can't beat
   the current top-k threshold.

## Measured baselines (fts_bench, M3 Pro, 100K docs / 10M tokens / 7.9M postings)

| lane | result |
|---|---|
| index build | gen 236 ms + build 335 ms (single thread) |
| oracle top-10, common∧common (272K postings) | 8.75 ms |
| oracle top-10, common∧rare (100K postings) | 6.34 ms |
| oracle top-10, rare∧rare (159 postings) | 8 µs |
| vec AND t0∧t1 (100K∧98K) | 97 µs |
| vec AND t0∧t5000 (100K∧172) | 52 µs — walks the dense list anyway |

The 6.34 ms common∧rare lane is WAND's whole reason to exist: the
rare term's idf ≈ 9 dominates, so almost none of the common term's
100K postings can reach the top-10 — an exhaustive scorer touches
them all anyway. The 52 µs dense∧sparse AND is roaring/galloping's
reason: two-pointer intersection is O(|dense|), not O(|sparse|).

## Reading guides

- [reading-zobel-moffat.md](reading-zobel-moffat.md) — Inverted indexes: the whole design space in one survey
- [reading-bm25.md](reading-bm25.md) — BM25: a derivation, not folklore
- [reading-blockmax-wand.md](reading-blockmax-wand.md) — Block-max WAND: skip everything that provably can't win
- [reading-roaring.md](reading-roaring.md) — Roaring bitmaps: no single set representation wins
- [reading-tantivy.md](reading-tantivy.md) — tantivy: Lucene's architecture in readable Rust
- [reading-redisearch.md](reading-redisearch.md) — RediSearch in Rust: a mutable inverted index
- topic 4: LSM guides — segment merging is the same design
- topic 14: `reading-hnsw.md` — the other half of M23's hybrid search

## Experiments

| file | status | what it shows |
|---|---|---|
| `corpus.rs` | provided | Zipfian corpus (term id = rank), tokenizer |
| `index.rs` | provided | postings + 128-block metas with per-block max BM25 |
| `bm25.rs` | provided | K1/B, saturating tf, exhaustive term-at-a-time oracle |
| `wand.rs` `wand_topk` | **stub** | block-max WAND: same top-k, fraction of the work |
| `postings.rs` `Roaring` | **stub** | array/bitmap containers, AND/OR vs vec oracle |
| `bin/fts_bench.rs` | provided | all lanes, stubs in catch_unwind |

## M23 checklist (capstone)

- [ ] full-text index on node/edge string properties: analyzer +
      segment-per-milestone postings, BM25 top-k procedure
- [ ] hybrid search: RRF fusion of BM25 top-k with M14's HNSW top-k
      (`score = Σ 1/(60 + rank_i)`)
- [ ] posting lists ARE the graph trick: doc ids = node ids, so
      full-text hits feed directly into M20's masked matrix ops
