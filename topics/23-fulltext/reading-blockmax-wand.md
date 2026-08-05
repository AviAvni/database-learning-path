# Block-max WAND: skip everything that provably can't win

Top-k retrieval doesn't need to score every document — only the
ones whose score *upper bound* beats the current k-th best. That
one observation (WAND, Broder et al. CIKM 2003) plus per-block score
ceilings (Ding & Suel, SIGIR 2011) is what our `wand::wand_topk` stub
implements. This chapter builds the algorithm in five steps — the
threshold, the bounds, the pivot, the block refinement, and the
traps — so the two papers read as confirmations.

Prereq: the BM25 chapter's saturation ceiling. Source pins: the code
anchors are tantivy at `7152d53`; the repo stub is
`experiments/src/wand.rs`. The original WAND paper (Broder et al.,
CIKM 2003) is paywalled — if you cannot get it, Ding & Suel's §2
Background re-derives its pivoting, and all algorithm anchors below
are to Ding & Suel, which I verified against the PDF.

## The problem in one sentence

Our exhaustive scorer spends 6.34 ms on a common∧rare query
([t0, t12000], 99,964 postings; notes.md) even though the rare
term's idf ≈ 7.1 guarantees almost none of the common term's
postings can reach the top-10 — block-max WAND returns the
*identical* top-10 while fully scoring under 25% of the docs (the
stub's test demands it), and Ding & Suel report 2.8–3.0× over plain
WAND at TREC scale (Table 1, below).

## The concepts, step by step

### Step 1 — the threshold θ: top-k means most docs don't matter

> **In:** a stream of candidate docs and a request for the top k by BM25.
> **Out:** a rising threshold θ (the k-th best score so far) below which every doc can be discarded unscored — turning "what does d score?" into "can d beat θ?".

A top-k query keeps a min-heap of the k best scores seen so far
(a heap whose root is the *smallest* of the k — the score to beat),
and θ (theta) names that k-th best score. Once the heap is full, a
doc scoring ≤ θ changes nothing — it is discarded on arrival. So
the real question is never "what does doc d score?" but "can doc d
possibly beat θ?" — and θ only rises as better docs arrive, so
docs get *easier* to rule out as the query progresses. Exhaustive
scoring (the TAAT — term-at-a-time — oracle) answers the first
question 100K times; everything below answers the second, usually
without scoring.

### Step 2 — upper bounds make skipping safe

> **In:** BM25's per-term score ceiling from the previous chapter (idf·(K1+1)).
> **Out:** a whole-doc upper bound = sum of query terms' ceilings; any doc whose bound ≤ θ is skippable with zero risk to the exact top-k.

If you know a **ceiling** for each term — a value its BM25
contribution can never exceed for any doc — then the sum of the
query terms' ceilings bounds any doc's total score, and a doc whose
bound is ≤ θ can be skipped *with zero risk to correctness*. BM25
hands us the ceiling for free (previous chapter): tf saturates, so
`score(t, d) ≤ idf(t)·(K1+1)`, computable at index time. Worked for
our `[t0, t12000]` query using the repo's own idf (bm25.rs:15-16,
N=100000):

```
term    df       idf = ln(1+(N-df+0.5)/(df+0.5))   ceiling = idf·(K1+1), K1=1.2
t0      99888    ln(1+0.00113) = 0.0011            0.0011·2.2 = 0.0025
t12000     83    ln(1+1196.6)  = 7.088             7.088·2.2 = 15.59
```

So t0's *entire* contribution to any doc is at most 0.0025 — once
θ climbs past that (which the very first rare-term doc does), *no
doc containing only t0 can ever win*, and its 99,888 postings become
skippable in principle. The magic word is **safe**: WAND returns the
EXACT top-k, not an approximation — correctness needs only that the
bounds are true, not tight. (This is why the "idf ≈ 0.7 vs 9" that
an earlier draft of this guide quoted was not just imprecise but
understated the effect: t0's ceiling is ~0.0025, not ~1.5.)

### Step 3 — the pivot: turning bounds into a jump target

> **In:** one doc-sorted cursor per query term, and the current θ.
> **Out:** the pivot doc — the smallest doc id whose leading terms' ceilings sum past θ — and a decision: score it, or seek the trailing cursor forward over the provable-loser gap.

WAND runs doc-at-a-time (DAAT): one cursor per term over its
doc-sorted posting list. Each round, sort cursors by their current
doc id and accumulate ceilings down the list until they exceed θ;
the cursor where that happens marks the **pivot** — the smallest doc
id that could possibly beat θ:

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
// ILLUSTRATION — one WAND round. You implement this in
// experiments/src/wand.rs:52 (wand_topk); tantivy's real,
// trap-fixed version is find_pivot_doc at block_wand_union.rs:16-43.
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

> **In:** a term-level ceiling set by that term's single best doc — wildly loose everywhere else.
> **Out:** per-128-doc block ceilings that let a false-positive pivot be skipped by *moving* a block cursor (shallow) without *decoding* its 128 postings (deep).

Term-level max_score is one global ceiling — for a common term it's
set by its single best doc, wildly pessimistic everywhere else
(t0's one lucky tf=30 doc inflates the bound for all 100K
postings). Ding & Suel's 2011 fix (their **Block-Max Index**, §3):
postings are already stored in 128-doc compressed blocks, so keep a
per-block score ceiling next to each compressed block as
uncompressed metadata:

- pivot found with term maxima as before (cheap, monotone);
- then REFINE with the current blocks' maxima: if Σ block-max ≤ θ,
  the pivot is a false positive — skip past the nearest block
  boundary without decompressing anything. Ding & Suel §5 names the
  two motions: a **deep pointer movement** decodes a block; a
  **shallow** one (their `NextShallow`, §5) only reads block-boundary
  metadata. The algorithm does shallow moves "instead of deep pointer
  movements whenever possible" (§5).
- the payoff, measured (Table 2, TREC 2006): WAND evaluates 178,391
  docIDs per query; BMW evaluates 21,921 (≈8× fewer), at the cost of
  0.42M deep + 0.76M shallow pointer moves.

The shallow/deep distinction is the engineering payload: a block
cursor can *move* (shallow — just read skip metadata) without
*decoding* (deep — decompress 128 postings), so false-positive
pivots cost almost nothing. Our `BlockMeta { last_doc, max_score }`
in `experiments/src/index.rs:26-30` stores the block ceiling
directly; tantivy instead stores the block's `(fieldnorm_id,
quantized tf)` and *recomputes* the ceiling on demand —
`SkipReader::block_max_score` (skip.rs:175-181) calls
`bm25_weight.score(...)`, and `last_doc_in_block()` is skip.rs:186-187.

### Step 5 — the traps (learned by others, cheaply)

> **In:** a working pivot loop and the wish to match the oracle's exact top-k.
> **Out:** four failure modes (empty-heap θ, livelock on false-positive pivots, score ties, and the metric mismatch) your `wand_topk` must avoid.

Four failure modes every WAND implementation rediscovers — check
your `wand_topk` against each:

1. θ must only tighten AFTER the heap holds k entries; seeding θ=-∞
   with an empty heap is correct, seeding 0.0 silently drops
   negative-score models (BM25 here is non-negative, but don't).
2. When the block-max check fails, advance past
   `min(last_doc of the cursors' current blocks)` — advancing only
   to pivot_doc re-finds the same dead pivot forever (livelock).
   tantivy's `block_max_was_too_low_advance_one_scorer`
   (block_wand_union.rs:49) is exactly this fix.
3. Ties at the k-boundary: WAND may return a different doc with an
   EQUAL score — compare scores, not doc ids (our test does).
4. `docs_scored` counts full evaluations; postings_skipped counts
   what you jumped — Ding & Suel's comparability metric is "evaluated
   docIDs" (Table 2), make sure yours matches theirs.

## How to read the papers (with the concepts in hand)

Two papers, one evening, in order (section numbers verified against
the Ding & Suel PDF):

- **Broder et al. (CIKM 2003) — the original WAND.** The pivot idea
  (Step 3) in its original two-level form: a cheap bound pass over
  cursors, then full evaluation only at pivots. Paywalled; if you
  can't get it, read Ding & Suel **§2 Background**, which re-derives
  the pivot mechanism before extending it.
- **Ding & Suel (SIGIR 2011).** **§3** proposes the Block-Max Index
  (per-block ceilings); **§5 Block-Max WAND Algorithm** is the
  payload — Algorithm 1, and shallow vs deep pointer movement
  (`NextShallow`, `CheckBlockMax`) (Step 4); **§6 Experiments** has
  the numbers (Table 1: BMW 27.9 ms vs WAND 77.6 ms on TREC 2006 =
  2.8×; 21.2 vs 64.4 on TREC 2005 = 3.0×; Table 2: evaluated docIDs).
  (§4 is Related Work, not the algorithm — skip on a first pass.)
- Then the shipped version, mapped:

| paper concept | tantivy anchor |
|---|---|
| pivot selection | `query/boolean_query/block_wand_union.rs:16-43` `find_pivot_doc` — walks scorers sorted by doc, accumulates `max_score` until `> threshold` |
| block metadata | `postings/skip.rs:93` `SkipReader`; block ceiling recomputed at `:175-181`, `last_doc_in_block` at `:186-187` |
| term upper bound | `Bm25Weight::max_score` per term (bm25.rs:184) |
| false-positive skip | `block_wand_union.rs:49` `block_max_was_too_low_advance_one_scorer` |
| union top-k | `block_wand_union.rs` (OR queries), `block_wand_intersection.rs` (AND) |

Compare tantivy's `find_pivot_doc` with your stub only *after*
implementing — it's the same loop as Step 3's code with the traps
already fixed.

## Questions (answer in notes.md)

1. For our `[t0, t12000]` query (df 99888 vs 83, idf ≈ 0.0011 vs
   7.09): after the heap fills with rare∧common docs, θ ≈ ? Can t0
   alone ever cross it? Predict wand's docs_scored (the test demands
   <25% of 99964).
2. Why does block-max help MOST on common terms? Relate to the
   variance of per-block maxima under Zipf tf distributions.
3. The paper stores block maxima uncompressed. At 128 docs/block,
   what's the metadata overhead per posting, and why is quantizing
   maxima UP (tantivy's `encode_block_wand_max_tf`) safe but
   quantizing DOWN unsafe?
4. Block-max WAND is exact top-k. What changes if the scorer adds
   M14's vector similarity (no static bound)? Sketch M23's hybrid:
   WAND for BM25 candidates + RRF, vs a fused traversal.
5. Deletes-as-bitmap (Lucene liveDocs, RediSearch GC): a block's
   max_score may belong to a deleted doc. Is WAND still exact?
   What's the merge-time fix?

## Done when

Answer each before unfolding it.

- [ ] You can explain the threshold θ and why top-k means most documents are provably irrelevant.
  <details><summary>θ and the discard rule</summary>

  θ is the k-th best score seen so far (root of a size-k min-heap).
  Once the heap is full, any doc scoring ≤ θ cannot enter it and is
  discarded unscored. θ only rises, so more docs become skippable as
  the query proceeds.

  </details>
- [ ] You can compute a pivot from term upper bounds and say what makes the jump safe.
  <details><summary>the pivot and its guarantee</summary>

  Sort cursors by doc id; accumulate per-term ceilings; the first
  cursor whose running sum exceeds θ marks pivot_doc. Every doc below
  it can contain only the preceding terms, whose ceilings sum to ≤ θ
  — so skipping to pivot_doc cannot drop a true top-k doc. Safe
  because the bounds are true, not because they are tight.

  </details>
- [ ] You can explain what per-block ceilings fix about the global upper bound.
  <details><summary>looseness of the global max</summary>

  A term's global ceiling is set by its single best doc and is wildly
  loose elsewhere. Per-128-doc block ceilings are tight locally, so
  a pivot whose block maxima sum ≤ θ is exposed as a false positive
  and skipped via a shallow (metadata-only) move — no 128-posting
  decode.

  </details>
- [ ] You can say why block-max helps most on common terms, and check it against this topic's measured oracle: 10.378 ms and 272,310 postings for `t0∧t1∧t5` against 0.009 ms and 159 postings for two rare terms.
  <details><summary>variance of block maxima</summary>

  Common terms have huge, mostly-hopeless posting lists whose
  per-block maxima vary a lot under Zipf tf; block-max prunes the
  low-max blocks that the single global ceiling could never rule out.
  The oracle figures (FINDINGS row 23) show the cost is all in the
  dense lists: 272,310 postings / 10.378 ms vs 159 / 0.009 ms — term
  rarity, not query complexity.

  </details>
- [ ] You can state what breaks if the scorer stops having a ceiling.
  <details><summary>no bound, no skip</summary>

  Every skip in WAND rests on a true static upper bound. A scorer
  without one (a neural/vector similarity, M14) can't be bounded, so
  no doc can be safely skipped — it must run as a second stage over
  WAND's BM25 candidates (M23's hybrid).

  </details>
- [ ] You wrote answers to all five questions in notes.md.
  <details><summary>check</summary>

  Five answers in notes.md, each tied to a measured repo number or a
  Ding & Suel section/table — not to the vague "2.5–4×" folklore.

  </details>

## References

**Papers**
- Broder, Carmel, Herscovici, Soffer, Zien — "Efficient Query
  Evaluation using a Two-Level Retrieval Process" (CIKM 2003) — the
  original WAND pivot idea (paywalled; re-derived in Ding & Suel §2)
- Ding, Suel — "Faster Top-k Document Retrieval Using Block-Max
  Indexes" (SIGIR 2011) — §3 the Block-Max Index, §5 the BMW
  algorithm (shallow vs deep pointer movement), §6 Tables 1–2 the
  numbers

**Code**
- [tantivy](https://github.com/quickwit-oss/tantivy) `@7152d53` —
  `src/query/boolean_query/block_wand_union.rs` (`find_pivot_doc`
  :16-43, `block_max_was_too_low_advance_one_scorer` :49),
  `src/postings/skip.rs` (`SkipReader` :93, `block_max_score`
  :175-181, `last_doc_in_block` :186-187) — the paper, shipped
- This repo — `experiments/src/wand.rs` (`wand_topk` stub :52),
  `experiments/src/index.rs` (`BlockMeta` :26-30)
