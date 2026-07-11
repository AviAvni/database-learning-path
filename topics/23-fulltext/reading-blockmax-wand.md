# Block-max WAND: skip everything that provably can't win

Top-k retrieval doesn't need to score every document — only the
ones whose score *upper bound* beats the current k-th best. That
one observation (WAND, CIKM 2003) plus per-block score ceilings
(Ding & Suel, SIGIR 2011) is what our `wand::wand_topk` stub
implements. Prereq: read the original WAND paper's §2 first — it's
3 pages.

## WAND in one picture

```
 cursors sorted by current doc id; θ = current k-th best score

   term      cur_doc   max_score   Σ max so far
   "fox"        41        1.9         1.9
   "quick"      70        2.3         4.2   ← crosses θ=3.8 HERE
   "the"       193        0.4         —
                ▲
        pivot_doc = 70: no doc < 70 can possibly reach θ
        (docs 41..69 get at most 1.9 + nothing = 1.9 < θ)

   if all cursors before pivot sit AT 70 → score 70 fully
   else → advance "fox" to ≥ 70 (skip 42..69 without scoring)
```

The magic: correctness needs only *upper bounds*. WAND returns the
EXACT top-k (safe-to-k) while scoring a fraction of the docs.

One round of the loop, in code:

```rust
// θ = current k-th best score; upper bounds make skipping SAFE
fn wand_round(cursors: &mut [Cursor], theta: f32) -> Option<DocId> {
    cursors.sort_by_key(|c| c.doc());               // by current doc id
    let mut ub = 0.0;
    let pivot = cursors.iter().position(|c| {
        ub += c.term_max_score;                     // accumulate ceilings
        ub > theta                                  // first cursor to cross θ
    })?;                                            // none crosses ⇒ done
    let pivot_doc = cursors[pivot].doc();           // no doc < pivot_doc can win

    if cursors[..=pivot].iter().all(|c| c.doc() == pivot_doc) {
        // block-max refinement (the 2011 part): if Σ current BLOCK maxima
        // ≤ θ, this pivot is a false positive — jump past
        // min(last_doc_in_block) without decompressing anything
        Some(pivot_doc)                             // else: score it fully
    } else {
        cursors[0].seek(pivot_doc);                 // skip docs, never score
        None
    }
}
```

## Block-max: the 2011 upgrade

Term-level max_score is one global ceiling — for a common term it's
set by its single best doc, wildly pessimistic everywhere else.
Block-max stores max_score per 128-doc block (uncompressed metadata
next to compressed postings):

- pivot found with term maxima as before (cheap, monotone);
- then REFINE with the current blocks' maxima: if Σ block-max ≤ θ,
  the pivot is a false positive — skip to
  `min(block boundary) + 1` without decompressing anything (§4's
  "shallow" vs "deep" pointer movement: moving a block cursor doesn't
  decode the block).
- §5's numbers: ~2.5-4× over WAND at TREC scale, more at deeper k.

Our `BlockMeta { last_doc, max_score }` in `index.rs` is exactly
their metadata; tantivy's is `postings/skip.rs:175`
(`block_max_score`) + `:186` (`last_doc_in_block`).

## Mapped to tantivy

| paper concept | tantivy anchor |
|---|---|
| pivot selection | `query/boolean_query/block_wand_union.rs:8-24` `find_pivot_doc` — walks scorers sorted by doc, accumulates max_weight until > threshold |
| block metadata | `postings/skip.rs:93` `SkipReader`, `:175/:186` |
| term upper bound | `Scorer::max_score` per term weight |
| union top-k | `block_wand_union.rs` (OR queries), `block_wand_intersection.rs` (AND) |

## Traps for the implementation (learned by others, cheaply)

1. θ must only tighten AFTER the heap holds k entries; seeding θ=-∞
   with an empty heap is correct, seeding 0.0 silently drops
   negative-score models (BM25 here is non-negative, but don't).
2. When the block-max check fails, advance past
   `min(last_doc of the cursors' current blocks)` — advancing only
   to pivot_doc re-finds the same dead pivot forever (livelock).
3. Ties at the k-boundary: WAND may return a different doc with an
   EQUAL score — compare scores, not doc ids (our test does).
4. `docs_scored` counts full evaluations; postings_skipped counts
   what you jumped — the paper's Table 4 metric is "docs evaluated",
   make sure yours matches for comparability.

## Questions (answer in notes.md)

1. For our `[t0, t12000]` query (df 99888 vs 83, idf ≈ 0.7 vs 9):
   after the heap fills with rare∧common docs, θ ≈ ? Can t0 alone
   ever cross it? Predict wand's docs_scored (the test demands <25%
   of 99964).
2. Why does block-max help MOST on common terms? Relate to the
   variance of per-block maxima under Zipf tf distributions.
3. The paper stores block maxima uncompressed. At 128 docs/block,
   what's the metadata overhead per posting, and why is quantizing
   maxima to u8 safe but quantizing DOWN unsafe?
4. Block-max WAND is exact top-k. What changes if the scorer adds
   M14's vector similarity (no static bound)? Sketch M23's hybrid:
   WAND for BM25 candidates + RRF, vs a fused traversal.
5. Deletes-as-bitmap (Lucene liveDocs, RediSearch GC): a block's
   max_score may belong to a deleted doc. Is WAND still exact?
   What's the merge-time fix?

## References

**Papers**
- Broder, Carmel, Herscovici, Soffer, Zien — "Efficient Query
  Evaluation using a Two-Level Retrieval Process" (CIKM 2003) —
  read §2 first (3 pages): the pivot idea
- Ding, Suel — "Faster Top-k Document Retrieval Using Block-Max
  Indexes" (SIGIR 2011) — §4 (shallow vs deep pointer movement) and
  §5's numbers

**Code**
- [tantivy](https://github.com/quickwit-oss/tantivy)
  `src/query/boolean_query/block_wand_union.rs` (:8-24
  `find_pivot_doc`), `block_wand_intersection.rs`,
  `src/postings/skip.rs` (:175 `block_max_score`, :186
  `last_doc_in_block`) — the paper, shipped
