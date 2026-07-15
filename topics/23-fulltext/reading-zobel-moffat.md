# Inverted indexes: the whole design space in one survey

Zobel & Moffat's CSUR 2006 survey compresses 30 years of IR
engineering into 50 coherent pages. Read it as "the B-tree paper"
of text indexing: everything since (Lucene, tantivy, RediSearch) is
an implementation of choices this paper enumerates — which makes it
the right first chapter of this topic. Before you open it, this
chapter builds each axis of the design space from zero — what an
inverted index even is, what a posting carries, why deltas compress,
why construction is a merge, and how queries actually walk the
lists — so the survey reads as a map instead of a wall.

## The problem in one sentence

Given 100K documents (10M tokens in our corpus), answer "which
documents contain *quick* and *fox*, ranked" in microseconds —
scanning the text is 10M token comparisons per query, so the index
must pre-invert the corpus, and every design choice after that is a
size/speed/updatability trade.

## The concepts, step by step

### Step 1 — the inverted index: flip document→words into word→documents

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
                                       (§8; block-max WAND got the best of both)

  compression:        Golomb/Rice → variable-byte → word-aligned (Simple-9)
                      (2006's menu; today: PForDelta / bitpacking / roaring)

  construction:       in-memory inversion → sort-based → MERGE-BASED
                      (§5: build runs, merge them = Lucene segments = LSM)

  update:             rebuild / merge / in-place
                      (§7 concludes merge wins — Lucene's whole architecture)
```

Steps 2–6 take these axes one at a time.

### Step 2 — granularity: what each posting carries

A posting can be just a doc id, or a doc id plus payload — and each
addition buys query types with index bytes:

- **doc ids only** — boolean AND/OR/NOT; the filter lane.
- **+ frequencies** (how often the term occurs in that doc) —
  enables ranking (BM25 needs tf; next chapter).
- **+ positions** (word offsets within the doc) — enables phrase
  ("quick fox" adjacent) and proximity queries, at ~3× the index
  size.
- **+ fields** (which attribute: title vs body) — per-field
  weighting and filtering.

This ladder is literally a directory listing in RediSearch's Rust
crate (eleven codecs from `doc_ids_only` to `full` —
reading-redisearch.md). The cost rule: pay for the payload only
where a query type needs it.

### Step 3 — posting order: doc-sorted vs impact-sorted

Doc-sorted lists (postings ordered by doc id) make intersection
cheap — two sorted lists merge in one pass, and a cursor can *skip
ahead* to any doc id. **Impact-sorted** lists (postings ordered by
score contribution, best first) make top-k trivially early-terminating
— read from the front until the tail can't matter — but wreck AND:
neither list is in id order, so intersection needs a hash. 2006
presents them as a fork in the road; the resolution came later —
block-max WAND (this topic's third chapter) keeps doc-sorted lists
and bolts per-block impact metadata on top, getting both.

### Step 4 — compression: store the gaps, not the ids

Doc-sorted ids compress because you store **deltas** (gaps between
consecutive ids) instead of raw 32-bit ids — and Zipf's law makes
the gaps small exactly where the lists are long: a term appearing
in half the docs has average gap 2, fitting in 2–3 bits instead of
32. The 2006 menu is Golomb/Rice (bit-optimal, slow),
variable-byte (byte-aligned, fast), word-aligned Simple-9; today's
answers are 128-block bitpacking (tantivy), PForDelta, and roaring
(this topic's fourth chapter). Why it matters: postings dominate
index size, and decompression speed is the scan speed of the whole
query engine — pick wrong and topic 17's GB/s ceiling drops by 10×.

### Step 5 — construction and update: it's an LSM

You can't build a big inverted index by inserting into one giant
in-memory map — it doesn't fit. §5's merge-based construction:
invert as much as fits in RAM, flush the sorted **run** to disk,
repeat, then merge runs into the final index. §7 reaches the
matching update conclusion: of rebuild / in-place / merge, **merge
wins** — keep new documents in a RAM index, flush as immutable
runs, merge in the background.

That is topic 4's LSM tree, rediscovered independently: run =
memtable flush, merge pass = compaction, immutable segments +
tombstoned deletes. Lucene's entire architecture (and tantivy's —
this topic's fifth chapter) is §5 + §7 productionized. Inverted
indexes are cheap to build and expensive to update in place —
exactly the LSM bet.

### Step 6 — query evaluation: TAAT vs DAAT

Two ways to walk multiple posting lists:

- **TAAT** (term-at-a-time): process one term's *entire* list before
  the next, accumulating partial scores per doc in a map of
  **accumulators**. Simple, sequential, cache-friendly — and no
  skipping is possible, since you don't know a doc's full score
  until every term has been walked. Our `oracle_topk`, and the
  baseline every later chapter tries to beat:

```rust
// term-at-a-time: walk each term's WHOLE list, accumulate per doc
fn taat_topk(terms: &[PostingList], k: usize) -> Vec<(DocId, f32)> {
    let mut acc: HashMap<DocId, f32> = HashMap::new();  // §6's accumulators
    for t in terms {
        for p in t.postings() {              // every posting, every term —
            *acc.entry(p.doc).or_default()   //   no skipping possible
                += bm25(t.idf, p.tf, p.doc_len);
        }
    }
    top_k(acc, k)
    // §6's insight: CAP the accumulator map (~1% of docs) and lose
    // almost nothing — the 2006 answer to what WAND later solved exactly
}
```

- **DAAT** (doc-at-a-time): one cursor per term, all advancing in
  lockstep by doc id, finishing each doc's score before moving on —
  needs doc-sorted lists (Step 3), and enables skipping: that's
  WAND's home.

§6's accumulator-limiting trick — allow only ~1% of docs to hold
accumulators, lose almost no ranking quality — is the heuristic
2006 answer to bounding work; WAND (Step 3's lineage, §8) is the
exact answer. Measured stakes from fts_bench: TAAT on
common∧rare (100K postings) takes 6.34 ms even though the rare
term's idf ≈ 9 means almost none of the common term's postings can
reach the top-10 — all that work is provably skippable.

## How to read the paper (with the concepts in hand)

50 pages, but it's a survey — the section map, with the step each
one expands:

| section | why (step) |
|---|---|
| §2-3 | vocabulary + postings anatomy; the doc-id vs word-position granularity trade (1, 2) |
| §4 | compression: deltas are what make postings compressible at all — Zipf gives small gaps for common terms (4) |
| §5 | merge-based construction — recognize topic 4's LSM before Lucene made it famous (5) |
| §6 | query eval: term-at-a-time vs doc-at-a-time (our oracle is TAAT, WAND is DAAT); the accumulator-limiting trick (6) |
| §7 | index maintenance — why everyone chose immutable segments + merge (5) |
| §8 | ranked retrieval + early termination — the WAND lineage starts here (3, 6) |

Read §2-3 fast, slow down for §5-§7 (the architecture payload), and
treat §8 as the setup for the block-max WAND chapter. The
compression specifics in §4 are 2006's menu — read for the *why*
(deltas + Zipf), not the codec details.

## Questions (answer in notes.md)

1. Delta+compress works because Zipf makes common-term gaps small.
   What's the expected gap for a term with df = n/2, and why does
   bitpacking 128-blocks (tantivy) beat per-posting varint
   (RediSearch) on exactly those terms?
2. §6's capped accumulators vs WAND: both bound work; which gives an
   exactness guarantee and what does the other buy instead?
3. Merge-based construction (§5) vs topic 4's LSM: map runs/merge
   passes onto memtable/flush/compaction. Where does Lucene's
   tiered merge policy differ from leveled compaction and why does
   full-text tolerate it?
4. Positions multiply index size ~3×. For M23's node/edge property
   search, when do you actually need them (phrase queries on
   `description` props?) and what's the cheaper substitute?
5. The survey predates learned/neural retrieval entirely. Which of
   its cost models still bind a BM25+vector hybrid (M23), and which
   are obsoleted by the ANN side?

## References

**Papers**
- Zobel, Moffat — "Inverted Files for Text Search Engines" (ACM
  Computing Surveys 2006) — read §2-8 with the section map above;
  §5 and §7 are where Lucene's architecture comes from
