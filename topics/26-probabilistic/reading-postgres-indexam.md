# Postgres index AMs: nbtree, GIN, BRIN — the exact baseline

Every structure in this chapter answers the same question our
filters and sketches answer — "where might X be?" — but with exactness
paid for in space and cache misses. Read nbtree, GIN, and BRIN as the
*prices* the probabilistic structures undercut. This chapter builds each
AM step by step — what an access method is, what an exact tree probe
costs, how an inverted index compresses, and what the smallest possible
index looks like — then points you at the postgres sources.

## The problem in one sentence

Exactness has a price list: a postgres btree probe is 3–4 *page* reads
cold (218 ns for an in-memory BTreeMap on the motivation bench), every
insert dirties a leaf page plus WAL, and the tree itself costs ~50–100
bits per key — each probabilistic structure in this topic undercuts
exactly one line of that bill.

## The concepts, step by step

### Step 1 — what an index AM is: three price points behind one API

An index **access method** (AM) is a pluggable index implementation
behind a common postgres interface — build, insert, and "give me
candidate row locations (**TIDs** — tuple identifiers, physical
page/offset addresses) for this predicate." Postgres ships several;
three of them span the exactness spectrum this topic cares about:
nbtree (exact position, most expensive), GIN (exact *set* of TIDs per
key, amortized writes), and BRIN (a one-sided "maybe in this page range"
— barely an index at all). Same question as a bloom filter — "where
might X be?" — three different bills.

### Step 2 — nbtree descent: what 23 cache misses buys you

A btree probe walks root→leaf: read a page, binary-search *within* the
page to find the child pointer, follow it, repeat — `_bt_search`
(nbtsearch.c:100) calling `_bt_binsrch` (:33, called at :153) per level.
On a 10M-key index that's 3–4 page visits, each a cache-or-disk miss
chain — the in-memory analogue measured 218 ns. What the misses buy is
the strongest possible answer: the *exact* position of the key, plus
ordered iteration from it (range scans), on *any* key distribution, with
no error to verify. That "no verification needed" property is exactly
what every structure in this topic gives up first.

### Step 3 — what exactness costs under concurrency and on writes

Three things bloom/PGM never have to deal with, all visible in nbtree:

- **Concurrency**: `_bt_moveright` (:211) — a reader racing a page split
  recovers by walking right-links (Lehman & Yao); no lock coupling on the
  descent. The README's L&Y section is the payoff read.
- **Suffix truncation & deduplication**: internal keys are truncated
  separators, duplicate leaf keys share a posting list
  (`_bt_binsrch_posting` :34) — nbtree has been absorbing
  compressed-postings ideas from the GIN/roaring world.
- **Write path**: every insert dirties a leaf (WAL, FPIs, topic 3) — the
  write amplification that makes "just add another index" a real bet.

The write line is the one to price: one row insert into a table with 5
btree indexes dirties 5 leaf pages plus WAL records plus possible
full-page images — the standing tax that makes cheap, approximate
alternatives worth wanting.

### Step 4 — GIN: an inverted index is topic 23 wearing a trench coat

GIN maps key → **posting list** of TIDs — exactly a search engine's
term → docIDs — for the "many keys per row" cases (arrays, JSONB,
full-text). Because posting lists are sorted TIDs, they compress:
`ginCompressPostingList` (ginpostinglist.c:196) packs TID *deltas*
(differences between consecutive TIDs — small numbers) into varbyte
encoding, ≤ 7 bytes each; `ginPostingListDecode` (:284 →
`ginPostingListDecodeAllSegments` :297) unpacks. Big lists graduate from
inline posting *lists* to a posting *tree* (a btree of TID segments).
And because updating many keys per row would mean many random index
writes, writes buffer in a **pending list** merged by (auto)vacuum — a
mini-LSM inside postgres, the same write-absorption move as ALEX's gaps
and the LSM memtable. The price: queries must also scan the pending
list, and a neglected vacuum lets it grow.

### Step 5 — BRIN: the zone map that admits it's a filter

BRIN stores per-block-range summaries: min/max per 128-page range
(`brininsert` brin.c:349 unions new values into the range's `BrinMemTuple`
:157-170; `bringetbitmap` :301 returns *candidate page ranges*, never
rows). It is exactly topic 12's zone map, and it is *already*
probabilistic in the useful direction: *one-sided* — it can say "range
definitely has no qualifying rows," never "row definitely exists."

The entire query-side logic fits in a filter:

```rust
fn bringetbitmap(ranges: &[MinMax], q: (Val, Val)) -> Vec<PageRange> {
    ranges.iter().enumerate()
        .filter(|(_, r)| r.min <= q.1 && q.0 <= r.max)  // overlap ⇒ MAYBE
        .map(|(i, _)| page_range(i))                     // 128 heap pages each
        .collect()   // one-sided: prunes ranges, never confirms rows
}
```

```
                 answers "definitely not here"      bits per key
  bloom          per KEY, any order                 ~10
  BRIN/zone map  per RANGE, needs clustering        ~0.001 (128 pages/entry)
  btree          exact position                     ~50-100 (the whole tree)
```

BRIN is 10,000× smaller than bloom *when the column is correlated with
physical order* (append-only timestamps) and useless when it isn't
(min/max of every range spans everything) — the cheapest index in
postgres is also the most workload-dependent.

### Step 6 — the price list, side by side

Line the three AMs up and the whole topic's thesis appears: each
probabilistic structure shadows one exact AM and undercuts one column of
its bill.

| AM | granularity | answer type | write cost | shadow in this topic |
|---|---|---|---|---|
| nbtree | row (TID) | exact | leaf dirty + WAL per insert | the 167/218 ns baseline lanes |
| GIN | key → TID set | exact set | pending-list amortized | roaring/postings (topic 23) |
| BRIN | 128-page range | one-sided maybe | update range summary | zone maps (topic 12), bloom's cousin |

What postgres conspicuously lacks: per-file bloom filters (it has no
LSM to hang them on) and approximate `count(DISTINCT)` (it computes it
exactly) — question 4 asks which absence hurts a graph workload most.

## Where each step lives in the code

All under [postgres](https://github.com/postgres/postgres)
`src/backend/access/`:

| anchor | step | what it is |
|---|---|---|
| `nbtree/README` | 2–3 | genuinely one of the best docs in any codebase; read it fully (the Lehman & Yao section is the payoff) |
| `nbtree/nbtsearch.c` | 2–3 | the descent: `_bt_search` :100, `_bt_binsrch` :33, `_bt_moveright` :211, `_bt_binsrch_posting` :34 |
| `gin/ginpostinglist.c` + `gin/README` | 4 | varbyte posting lists: `ginCompressPostingList` :196, decode :284/:297 |
| `brin/brin.c` + `brin/README` | 5 | block-range summaries: `brininsert` :349, `BrinMemTuple` :157-170, `bringetbitmap` :301 |

Read the READMEs before the .c files — postgres's in-tree docs are the
rare case where that order pays.

## Questions to answer in notes.md

1. Our motivation table: BTreeMap miss = 218 ns *in memory*. A postgres
   btree probe on a cold cache is 3-4 *page* reads. Where does the
   learned index's "the top of the tree is predictable" claim break for
   postgres? (Hint: pages move; TIDs aren't positions in a sorted array;
   VACUUM.)
2. GIN's varbyte deltas vs roaring's containers
   ([reading-roaring-internals.md](reading-roaring-internals.md)):
   varbyte wins on tight clusters (deltas of 1 → 1 byte), roaring wins on
   random access (galloping needs to *seek*; varbyte must decode linearly
   from a segment boundary). Which does an `&&` (array-overlap) query
   with two selective keys want, and which does a full bitmap scan want?
3. State the precise condition under which a BRIN index on column c
   prunes well, in terms of the overlap of per-range [min, max]
   intervals. Which of: insert timestamp, UUID v4,
   monotonically-allocated node ID, falls where?
4. **(the M26 synthesis)** The capstone milestone wants: range index
   under MVCC + LSM blooms + roaring label filters + HLL count-distinct.
   Map each onto the postgres AM it shadows (nbtree / none — postgres
   lacks LSM blooms / GIN / none — postgres computes count(DISTINCT)
   exactly). Which of the four does postgres's absence hurt most for a
   graph workload, and why is that the one topic 4 already measured?
   (Point-miss cost × miss rate of MATCH lookups.)

## References

**Code** ([postgres](https://github.com/postgres/postgres), `src/backend/access/`)
- `nbtree/README` — genuinely one of the best docs in any codebase;
  read it fully (the Lehman & Yao section is the payoff)
- `nbtree/nbtsearch.c` — the descent
- `gin/ginpostinglist.c` + `gin/README` — varbyte posting lists
- `brin/brin.c` + `brin/README` — block-range summaries
