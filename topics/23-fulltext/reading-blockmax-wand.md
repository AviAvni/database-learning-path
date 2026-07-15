# Block-max WAND: skip everything that provably can't win

Top-k retrieval doesn't need to score every document — only the
ones whose score *upper bound* beats the current k-th best. That
one observation (WAND, CIKM 2003) plus per-block score ceilings
(Ding & Suel, SIGIR 2011) is what our `wand::wand_topk` stub
implements. This chapter builds the algorithm in five steps — the
threshold, the bounds, the pivot, the block refinement, and the
traps — so the two papers read as confirmations. Prereq: the BM25
chapter's saturation ceiling; then read the original WAND paper's
§2 — it's 3 pages.

## The problem in one sentence

Our exhaustive scorer spends 6.34 ms on a common∧rare query
(100K postings) even though the rare term's idf ≈ 9 guarantees
almost none of the common term's postings can reach the top-10 —
block-max WAND returns the *identical* top-10 while fully scoring
under 25% of the docs (the stub's test demands it), and the papers
report 2.5–4× at TREC scale.

## The concepts, step by step

### Step 1 — the threshold θ: top-k means most docs don't matter

A top-k query keeps a min-heap of the k best scores seen so far
(a heap whose root is the *smallest* of the k — the score to beat),
and θ (theta) names that k-th best score. Once the heap is full, a
doc scoring ≤ θ changes nothing — it is discarded on arrival. So
the real question is never "what does doc d score?" but "can doc d
possibly beat θ?" — and θ only rises as better docs arrive, so
docs get *easier* to rule out as the query progresses. Exhaustive
scoring (the TAAT oracle) answers the first question 100K times;
everything below answers the second, usually without scoring.

### Step 2 — upper bounds make skipping safe

If you know a **ceiling** for each term — a value its BM25
contribution can never exceed for any doc — then the sum of the
query terms' ceilings bounds any doc's total score, and a doc whose
bound is ≤ θ can be skipped *with zero risk to correctness*. BM25
hands us the ceiling for free (previous chapter): tf saturates, so
score(t, d) ≤ idf(t)·(K1+1), computable at index time. Concretely
for our `[t0, t12000]` query: t0's ceiling ≈ 0.7·2.2 ≈ 1.5, t12000's
≈ 9·2.2 ≈ 20 — once θ passes 1.5, *no doc containing only t0 can
ever win*, and 100K postings become skippable in principle. The
magic word is **safe-to-k**: WAND returns the EXACT top-k, not an
approximation — correctness needs only that the bounds are true,
not tight.

### Step 3 — the pivot: turning bounds into a jump target

WAND runs doc-at-a-time: one cursor per term over its doc-sorted
posting list. Each round, sort cursors by their current doc id and
accumulate ceilings down the list until they exceed θ; the cursor
where that happens marks the **pivot** — the smallest doc id that
could possibly beat θ:

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

Every doc before the pivot is provably a loser: it can contain only
the terms whose cursors precede the pivot, and their summed
ceilings don't reach θ. So either all leading cursors already sit
on the pivot doc (score it fully — the only place real scoring
happens) or one of them leaps forward over the dead zone. One
round of the loop, in code:

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

The cost profile: sorting a handful of cursors and one `seek()` per
round, versus decoding and scoring thousands of postings.

### Step 4 — block-max: per-block ceilings fix the pessimistic bound

Term-level max_score is one global ceiling — for a common term it's
set by its single best doc, wildly pessimistic everywhere else
(t0's one lucky tf=30 doc inflates the bound for all 100K
postings). Ding & Suel's 2011 fix: postings are already stored in
128-doc compressed blocks, so store max_score per block too, as
uncompressed metadata next to the compressed postings:

- pivot found with term maxima as before (cheap, monotone);
- then REFINE with the current blocks' maxima: if Σ block-max ≤ θ,
  the pivot is a false positive — skip to
  `min(block boundary) + 1` without decompressing anything (§4's
  "shallow" vs "deep" pointer movement: moving a block cursor doesn't
  decode the block).
- §5's numbers: ~2.5-4× over WAND at TREC scale, more at deeper k.

The shallow/deep distinction is the engineering payload: a block
cursor can *move* (shallow — just read skip metadata) without
*decoding* (deep — decompress 128 postings), so false-positive
pivots cost almost nothing. Our `BlockMeta { last_doc, max_score }`
in `index.rs` is exactly their metadata; tantivy's is
`postings/skip.rs:175` (`block_max_score`) + `:186`
(`last_doc_in_block`).

### Step 5 — the traps (learned by others, cheaply)

Four failure modes every WAND implementation rediscovers — check
your `wand_topk` against each:

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

## How to read the papers (with the concepts in hand)

Two papers, one evening, in order:

- **Broder et al. (CIKM 2003), §2 first — 3 pages.** The pivot idea
  (Step 3) in its original two-level form: a cheap bound pass over
  cursors, then full evaluation only at pivots. The rest of the
  paper (their production context, approximate variants) is
  optional.
- **Ding & Suel (SIGIR 2011).** §4 is the payload — block metadata
  and shallow vs deep pointer movement (Step 4); §5's numbers set
  your expectations (2.5–4× over WAND, more at deeper k). Skim
  their list-caching and layout discussion.
- Then the shipped version, mapped:

| paper concept | tantivy anchor |
|---|---|
| pivot selection | `query/boolean_query/block_wand_union.rs:8-24` `find_pivot_doc` — walks scorers sorted by doc, accumulates max_weight until > threshold |
| block metadata | `postings/skip.rs:93` `SkipReader`, `:175/:186` |
| term upper bound | `Scorer::max_score` per term weight |
| union top-k | `block_wand_union.rs` (OR queries), `block_wand_intersection.rs` (AND) |

Compare tantivy's `find_pivot_doc` with your stub only *after*
implementing — it's the same loop as Step 3's code with the traps
already fixed.

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
