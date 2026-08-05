# Inverted indexes: the whole design space in one survey

Zobel & Moffat's ACM Computing Surveys (2006) survey compresses 30
years of IR engineering into ~50 coherent pages. Read it as "the
B-tree paper" of text indexing: everything since (Lucene, tantivy,
RediSearch) is an implementation of choices this paper enumerates —
which makes it the right first chapter of this topic. Before you open
it, this chapter builds each axis of the design space from zero —
what an inverted index even is, what a posting carries, why deltas
compress, why construction is a merge, and how queries actually walk
the lists — so the survey reads as a map instead of a wall.

A note on citing it: this survey is paywalled (ACM Digital Library),
and I could not open a verified copy while writing this guide, so the
map below labels the survey's parts by *theme* rather than by section
number — confirm the exact §N against your own copy. Everything
attributed to a measured number or a line of code, by contrast, is
verified against this repo.

## The problem in one sentence

Given 100K documents (10M tokens in our corpus), answer "which
documents contain *quick* and *fox*, ranked" in microseconds —
scanning the text is 10M token comparisons per query, so the index
must pre-invert the corpus, and every design choice after that is a
size/speed/updatability trade.

## The concepts, step by step

### Step 1 — the inverted index: flip document→words into word→documents

> **In:** a corpus that maps each document to the words it contains.
> **Out:** the inverse map — each term to its sorted list of documents (its posting list) — so a query fetches lists instead of scanning text.

An **inverted index** stores, for every **term** (a normalized word
produced by an analyzer: tokenize → lowercase → stem → drop
stopwords), the sorted list of documents containing it — the
**posting list**. "Inverted" because the raw corpus maps document →
words; the index maps word → documents. A two-term query then never
touches the corpus: fetch two posting lists and combine them.
Concretely, in our corpus a common term's list holds ~100K doc ids
and a rare term's holds ~159 — the query cost is driven by list
lengths, not corpus size. The survey's whole design space hangs off
this one structure:

```
  index granularity:  doc ids only → +frequencies → +positions → +fields
                      (each level: bigger index, more query types)

  posting order:      doc-sorted ─── supports AND/WAND skipping (everyone)
                      frequency-sorted / impact-sorted ─── early termination
                                       (block-max WAND later got the best of both)

  compression:        Golomb/Rice → variable-byte → word-aligned (Simple-9)
                      (2006's menu; today: PForDelta / bitpacking / roaring)

  construction:       in-memory inversion → sort-based → MERGE-BASED
                      (build runs, merge them = Lucene segments = LSM)

  update:             rebuild / merge / in-place
                      (the survey concludes merge wins — Lucene's architecture)
```

Steps 2–6 take these axes one at a time.

### Step 2 — granularity: what each posting carries

> **In:** the choice of what to record per (term, document) pair.
> **Out:** a ladder — ids → +frequencies → +positions → +fields — where each rung buys query types with index bytes.

A posting can be just a doc id, or a doc id plus payload — and each
addition buys query types with index bytes:

- **doc ids only** — boolean AND/OR/NOT; the filter lane.
- **+ frequencies** (**tf**: how often the term occurs in that doc) —
  enables ranking (BM25 needs tf; next chapter).
- **+ positions** (word offsets within the doc) — enables phrase
  ("quick fox" adjacent) and proximity queries, at ~3× the index
  size.
- **+ fields** (which attribute: title vs body) — per-field
  weighting and filtering.

This ladder is literally a directory listing in RediSearch's Rust
crate (ten codec modules from `doc_ids_only` to `full` —
reading-redisearch.md). The cost rule: pay for the payload only
where a query type needs it.

### Step 3 — posting order: doc-sorted vs impact-sorted

> **In:** the freedom to store a posting list in any order.
> **Out:** doc-sorted (cheap intersection + skipping) vs impact-sorted (trivial early termination but no ordered merge) — a fork block-max WAND later reconciles.

Doc-sorted lists (postings ordered by doc id) make intersection
cheap — two sorted lists merge in one pass, and a cursor can *skip
ahead* to any doc id. **Impact-sorted** lists (postings ordered by
score contribution, best first) make top-k trivially
early-terminating — read from the front until the tail can't matter
— but wreck AND: neither list is in id order, so intersection needs
a hash. 2006 presents them as a fork in the road; the resolution
came later — block-max WAND (this topic's third chapter) keeps
doc-sorted lists and bolts per-block impact metadata on top, getting
both.

### Step 4 — compression: store the gaps, not the ids

> **In:** a doc-sorted list of 32-bit ids, mostly redundant because they're increasing.
> **Out:** delta (gap) coding whose values are small exactly where lists are long (Zipf) — a few bits per posting instead of 32.

Doc-sorted ids compress because you store **deltas** (gaps between
consecutive ids) instead of raw 32-bit ids — and Zipf's law makes
the gaps small exactly where the lists are long. The gap is inverse
to frequency; worked for a term in half the corpus:

```
avg gap ≈ N / df       (uniform placement)
df = N/2:   gap ≈ 2      → the delta '2' needs 2 bits, not 32  (16× win)
df = N/100: gap ≈ 100    → ~7 bits
df = 1:     gap = doc id → the full 32 bits (rare terms don't compress)
```

The 2006 menu is Golomb/Rice (bit-optimal, slow), variable-byte
(byte-aligned, fast), word-aligned Simple-9; today's answers are
128-block bitpacking (tantivy), PForDelta, and roaring (this topic's
fourth chapter). Why it matters: postings dominate index size, and
decompression speed is the scan speed of the whole query engine —
pick wrong and topic 17's GB/s ceiling drops by 10×.

### Step 5 — construction and update: it's an LSM

> **In:** a corpus too big to invert into one in-memory map.
> **Out:** invert what fits, flush a sorted run, repeat, merge runs — and keep updates as new runs merged in the background: topic 4's LSM, independently rediscovered.

You can't build a big inverted index by inserting into one giant
in-memory map — it doesn't fit. The survey's merge-based
construction: invert as much as fits in RAM, flush the sorted **run**
to disk, repeat, then merge runs into the final index. Its
maintenance discussion reaches the matching update conclusion: of
rebuild / in-place / merge, **merge wins** — keep new documents in a
RAM index, flush as immutable runs, merge in the background.

That is topic 4's LSM tree, rediscovered independently: run =
memtable flush, merge pass = compaction, immutable segments +
tombstoned deletes. Lucene's entire architecture (and tantivy's —
this topic's fifth chapter) is the survey's construction +
maintenance sections productionized. Inverted indexes are cheap to
build and expensive to update in place — exactly the LSM bet.

### Step 6 — query evaluation: TAAT vs DAAT

> **In:** a multi-term query and two posting lists to combine.
> **Out:** two traversal orders — term-at-a-time (simple, no skipping, our oracle) and doc-at-a-time (needs doc-sorted lists, enables WAND's skipping).

Two ways to walk multiple posting lists:

- **TAAT** (term-at-a-time): process one term's *entire* list before
  the next, accumulating partial scores per doc in a map of
  **accumulators**. Simple, sequential, cache-friendly — and no
  skipping is possible, since you don't know a doc's full score
  until every term has been walked. This is our provided oracle
  (`bm25::oracle_topk`), and the baseline every later chapter tries
  to beat:

```rust
// ILLUSTRATION — the repo's real TAAT oracle is bm25::oracle_topk at
// experiments/src/bm25.rs:28-48 (walk every posting, accumulate in a
// HashMap, sort desc, truncate to k). This is a faithful sketch of it.
fn taat_topk(terms: &[PostingList], k: usize) -> Vec<(DocId, f32)> {
    let mut acc: HashMap<DocId, f32> = HashMap::new();  // the accumulators
    for t in terms {
        for p in t.postings() {              // every posting, every term —
            *acc.entry(p.doc).or_default()   //   no skipping possible
                += bm25(t.idf, p.tf, p.doc_len);
        }
    }
    top_k(acc, k)
    // the survey's insight: CAP the accumulator map (~1% of docs) and lose
    // almost nothing — the 2006 answer to what WAND later solved exactly
}
```

- **DAAT** (doc-at-a-time): one cursor per term, all advancing in
  lockstep by doc id, finishing each doc's score before moving on —
  needs doc-sorted lists (Step 3), and enables skipping: that's
  WAND's home.

The survey's accumulator-limiting trick — allow only ~1% of docs to
hold accumulators, lose almost no ranking quality — is the heuristic
2006 answer to bounding work; WAND (Step 3's lineage) is the exact
answer. Measured stakes from fts_bench (notes.md): TAAT on
common∧rare ([t0, t12000], 99,964 postings) takes 6.34 ms even
though the rare term's idf ≈ 7.1 (repo bm25.rs formula, df=83) means
almost none of the common term's postings can reach the top-10 —
all that work is provably skippable.

## How to read the paper (with the concepts in hand)

~50 pages, but it's a survey — read it by theme, mapping each part to
the step it expands. (The survey numbers these sections; I've labeled
them descriptively rather than assert §N I couldn't verify against an
open copy — match them to your printout as you go.)

| survey theme | why (step) |
|---|---|
| vocabulary + postings anatomy | the doc-id vs frequency vs word-position granularity trade (1, 2) |
| index compression | deltas are what make postings compressible at all — Zipf gives small gaps for common terms (4) |
| merge-based construction | recognize topic 4's LSM before Lucene made it famous (5) |
| query evaluation | term-at-a-time vs doc-at-a-time (our oracle is TAAT, WAND is DAAT); the accumulator-limiting trick (6) |
| index maintenance | why everyone chose immutable segments + merge (5) |
| ranked retrieval + early termination | the WAND lineage starts here (3, 6) |

Read the vocabulary/postings part fast, slow down for
construction + maintenance (the architecture payload), and treat the
ranked-retrieval part as the setup for the block-max WAND chapter.
The compression specifics are 2006's menu — read for the *why*
(deltas + Zipf), not the codec details.

## Questions (answer in notes.md)

1. Delta+compress works because Zipf makes common-term gaps small.
   What's the expected gap for a term with df = n/2 (worked above to
   ≈2), and why does bitpacking 128-blocks (tantivy) beat per-posting
   varint (RediSearch) on exactly those terms?
2. The survey's capped accumulators vs WAND: both bound work; which
   gives an exactness guarantee and what does the other buy instead?
3. Merge-based construction vs topic 4's LSM: map runs/merge passes
   onto memtable/flush/compaction. Where does Lucene's tiered merge
   policy differ from leveled compaction and why does full-text
   tolerate it?
4. Positions multiply index size ~3×. For M23's node/edge property
   search, when do you actually need them (phrase queries on
   `description` props?) and what's the cheaper substitute?
5. The survey predates learned/neural retrieval entirely. Which of
   its cost models still bind a BM25+vector hybrid (M23), and which
   are obsoleted by the ANN side?

## Done when

Answer each before unfolding it.

- [ ] You can explain granularity: what each posting carries and what that costs.
  <details><summary>the ladder</summary>

  doc ids (boolean) → +frequencies (BM25 ranking) → +positions
  (phrase/proximity, ~3× size) → +fields (per-field weighting). Each
  rung buys query types with index bytes; pay only where a query type
  needs it. RediSearch encodes this as ten codec modules.

  </details>
- [ ] You can state the difference between doc-sorted and impact-sorted postings and which query strategy each enables.
  <details><summary>the fork</summary>

  Doc-sorted → cheap ordered intersection + skipping (DAAT/WAND).
  Impact-sorted → trivial top-k early termination but no ordered
  merge (intersection needs a hash). Block-max WAND keeps doc-sorted
  order and adds per-block impact metadata to get both.

  </details>
- [ ] You can explain why storing gaps works, and how Zipf makes it work.
  <details><summary>gaps + Zipf</summary>

  Store deltas between consecutive ids, not the ids. Average gap ≈
  N/df, so the *long* lists (common terms, big df) have the *smallest*
  gaps — a term in half the corpus has gap ≈2, ~2 bits vs 32. Zipf
  concentrates postings in a few common terms, so this wins on the
  bytes that dominate.

  </details>
- [ ] You can explain the TAAT/DAAT distinction and which one this topic's oracle lane implements.
  <details><summary>traversal order</summary>

  TAAT walks each term's whole list into per-doc accumulators — no
  skipping. DAAT advances one cursor per term in doc-id lockstep,
  finishing each doc — enables skipping. The repo oracle
  (`bm25::oracle_topk`, bm25.rs:32) is TAAT; WAND is DAAT.

  </details>
- [ ] You can say what capped accumulators bound and how that differs from WAND's guarantee.
  <details><summary>heuristic vs exact</summary>

  Capping accumulators (~1% of docs) bounds work by dropping most
  docs' partial scores — a heuristic that loses a little ranking
  quality. WAND bounds work via true score ceilings and returns the
  *exact* top-k. Same goal, one approximate and one safe.

  </details>
- [ ] You wrote answers to all five questions in notes.md.
  <details><summary>check</summary>

  Five answers in notes.md, each tied to a concept above or a measured
  repo number — the df=N/2 gap (Q1) worked, the accumulator-vs-WAND
  exactness contrast (Q2) stated.

  </details>

## References

**Papers**
- Zobel, Moffat — "Inverted Files for Text Search Engines" (ACM
  Computing Surveys, 2006) — the whole design-space map above;
  construction + maintenance are where Lucene's architecture comes
  from. Paywalled (ACM Digital Library); read it against the thematic
  map above.

**Code**
- This repo — `experiments/src/bm25.rs:28-48` `oracle_topk`, the
  term-at-a-time baseline the later chapters beat
