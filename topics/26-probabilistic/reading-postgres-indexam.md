# Reading guide — Postgres index AMs: nbtree, GIN, BRIN (the classical baseline)

**Sources (clone at `~/repos/postgres`):**
- `src/backend/access/nbtree/README` — genuinely one of the best docs in
  any codebase; read it fully
- `src/backend/access/nbtree/nbtsearch.c` — the descent
- `src/backend/access/gin/ginpostinglist.c` + `gin/README`
- `src/backend/access/brin/brin.c` + `brin/README`

**Why this guide is in the *probabilistic* topic:** every structure above
answers the same question our filters/sketches answer — "where might X
be?" — but with exactness paid for in space and cache misses. Read these as
the *prices* the probabilistic structures undercut.

## 1. nbtree — what 23 cache misses buys you

`_bt_search` (nbtsearch.c:100) walks root→leaf: read page, `_bt_binsrch`
(:33, called at :153) within the page, follow the downlink, repeat. Three
things bloom/PGM don't have to deal with:

- **Concurrency**: `_bt_moveright` (:211) — a reader racing a page split
  recovers by walking right-links (Lehman & Yao); no lock coupling on the
  descent. The README's L&Y section is the payoff read.
- **Suffix truncation & deduplication**: internal keys are truncated
  separators, duplicate leaf keys share a posting list
  (`_bt_binsrch_posting` :34) — nbtree has been absorbing
  compressed-postings ideas from the GIN/roaring world.
- **Write path**: every insert dirties a leaf (WAL, FPIs, topic 3) — the
  write amplification that makes "just add another index" a real bet.

**Q1.** Our motivation table: BTreeMap miss = 218 ns *in memory*. A
postgres btree probe on a cold cache is 3-4 *page* reads. Where does the
learned index's "the top of the tree is predictable" claim break for
postgres? (Hint: pages move; TIDs aren't positions in a sorted array;
VACUUM.)

## 2. GIN — inverted index = topic 23 wearing a trench coat

GIN maps key → posting list of TIDs, exactly a search engine's term →
docIDs. The compression is varbyte delta encoding:
`ginCompressPostingList` (ginpostinglist.c:196) packs TID deltas into ≤ 7
bytes each; `ginPostingListDecode` (:284 →
`ginPostingListDecodeAllSegments` :297) unpacks. Big lists graduate from
inline posting *lists* to a posting *tree* (a btree of TID segments), and
writes buffer in a **pending list** merged by (auto)vacuum — a mini-LSM
inside postgres, same write-absorption move as ALEX's gaps and the LSM
memtable.

**Q2.** GIN's varbyte deltas vs roaring's containers
([reading-roaring-internals.md](reading-roaring-internals.md)): varbyte
wins on tight clusters (deltas of 1 → 1 byte), roaring wins on random
access (galloping needs to *seek*; varbyte must decode linearly from a
segment boundary). Which does an `&&` (array-overlap) query with two
selective keys want, and which does a full bitmap scan want?

## 3. BRIN — the zone map that admits it's a filter

BRIN stores per-block-range summaries: min/max per 128-page range
(`brininsert` brin.c:349 unions new values into the range's `BrinMemTuple`
:157-170; `bringetbitmap` :301 returns *candidate page ranges*, never
rows). It is exactly topic 12's zone map, and it is *already*
probabilistic in the useful direction: *one-sided* — it can say "range
definitely has no qualifying rows," never "row definitely exists."

```
                 answers "definitely not here"      bits per key
  bloom          per KEY, any order                 ~10
  BRIN/zone map  per RANGE, needs clustering        ~0.001 (128 pages/entry)
  btree          exact position                     ~50-100 (the whole tree)
```

BRIN is 10,000× smaller than bloom *when the column is correlated with
physical order* (append-only timestamps) and useless when it isn't
(min/max of every range spans everything).

**Q3.** State the precise condition under which a BRIN index on column c
prunes well, in terms of the overlap of per-range [min, max] intervals.
Which of: insert timestamp, UUID v4, monotonically-allocated node ID,
falls where?

**Q4 (the M26 synthesis).** The capstone milestone wants: range index under
MVCC + LSM blooms + roaring label filters + HLL count-distinct. Map each
onto the postgres AM it shadows (nbtree / none — postgres lacks LSM blooms /
GIN / none — postgres computes count(DISTINCT) exactly). Which of the four
does postgres's absence hurt most for a graph workload, and why is that the
one topic 4 already measured? (Point-miss cost × miss rate of MATCH
lookups.)

## 5. The one-table summary

| AM | granularity | answer type | write cost | shadow in this topic |
|---|---|---|---|---|
| nbtree | row (TID) | exact | leaf dirty + WAL per insert | the 167/218 ns baseline lanes |
| GIN | key → TID set | exact set | pending-list amortized | roaring/postings (topic 23) |
| BRIN | 128-page range | one-sided maybe | update range summary | zone maps (topic 12), bloom's cousin |
