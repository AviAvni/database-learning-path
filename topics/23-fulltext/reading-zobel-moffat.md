# Reading guide — "Inverted Files for Text Search Engines" (Zobel & Moffat, CSUR 2006)

The survey. 50 pages that compress 30 years of IR engineering into
one coherent design space. Read it as "the B-tree paper" of text
indexing: everything since (Lucene, tantivy, RediSearch) is an
implementation of choices this paper enumerates.

## The design space in one diagram

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

## What to actually read

| section | why |
|---|---|
| §2-3 | vocabulary + postings anatomy; the doc-id vs word-position granularity trade |
| §4 | compression: deltas are what make postings compressible at all — Zipf gives small gaps for common terms |
| §5 | merge-based construction — recognize topic 4's LSM before Lucene made it famous |
| §6 | query eval: term-at-a-time vs doc-at-a-time (our oracle is TAAT, WAND is DAAT); the accumulator-limiting trick |
| §7 | index maintenance — why everyone chose immutable segments + merge |
| §8 | ranked retrieval + early termination — the WAND lineage starts here |

## Vocabulary decoder ring

- **TAAT** (term-at-a-time): walk each term's full list, accumulate
  scores per doc — our `oracle_topk`, simple, cache-friendly, no
  skipping. **DAAT** (doc-at-a-time): cursors advance in lockstep by
  doc id — enables WAND, needs doc-sorted lists.
- **accumulators**: TAAT's hash map of partial scores; §6's insight
  is you can cap them (only allow ~1% of docs to hold accumulators)
  and lose almost no effectiveness — the 2006 answer to the problem
  WAND solves exactly.
- **impact-sorted**: postings ordered by score contribution, not doc
  id — perfect early termination, terrible AND. Block-max WAND is
  doc-sorted lists with impact metadata bolted on blocks.

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
