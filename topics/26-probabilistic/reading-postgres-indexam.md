# Postgres index AMs: nbtree, GIN, BRIN — the exact baseline

Every structure in this chapter answers the same question our
filters and sketches answer — "where might X be?" — but with exactness
paid for in space and cache misses. Read nbtree, GIN, and BRIN as the
*prices* the probabilistic structures undercut. This chapter builds each
AM step by step — what an access method is, what an exact tree probe
costs, how an inverted index compresses, and what the smallest possible
index looks like — then points you at the postgres sources.

Every code anchor below is postgres at commit `701f021`, the revision
this repo pins — PostgreSQL **20devel** (`meson.build` `version:
'20devel'`) — quoted with the line numbers the code occupies in that
version. The `IndexAmRoutine` struct and its callback list have grown
across releases, so the anchors in Step 1 are stated against this tree
specifically.

## The problem in one sentence

Exactness has a price list: a postgres btree probe is 3–4 *page* reads
cold (299 ns for an in-memory BTreeMap on the motivation bench,
[FINDINGS.md](../../FINDINGS.md) row 26), every insert dirties a leaf page
plus WAL, and the tree itself costs ~50–100 bits per key — each
probabilistic structure in this topic undercuts exactly one line of that
bill.

## The concepts, step by step

### Step 1 — what an index AM is: three price points behind one API

> **In:** nothing yet — this step fixes the common interface the three
> AMs plug into.
> **Out:** the `IndexAmRoutine` callback vtable, and the three AMs
> (nbtree/GIN/BRIN) that fill it at different points on the exactness
> spectrum.

An index **access method** (AM) is a pluggable index implementation
behind a common postgres interface — build, insert, and "give me
candidate row locations (**TIDs** — tuple identifiers, physical
page/offset addresses) for this predicate." Postgres ships several;
three of them span the exactness spectrum this topic cares about:
nbtree (exact position, most expensive), GIN (exact *set* of TIDs per
key, amortized writes), and BRIN (a one-sided "maybe in this page range"
— barely an index at all). Same question as a bloom filter — "where
might X be?" — three different bills.

The "common interface" is literally a struct of function pointers:
`IndexAmRoutine` (`src/include/access/amapi.h:233-326` in this tree),
whose fields are the callbacks every AM must supply — `aminsert` (:298),
and the two scan entry points `amgettuple` (:312, "can be NULL") and
`amgetbitmap` (:313, "can be NULL"). Each AM exports one handler function
that allocates and fills this struct: `brinhandler` (`brin.c:254`) sets
`.amgetbitmap = bringetbitmap` at `brin.c:301`, while nbtree's `bthandler`
supplies `amgettuple` instead. That NULL-vs-set choice — tuple-at-a-time
ordered scan (nbtree) versus bitmap-only (BRIN, GIN) — is the first
visible fork between "exact position" and "candidate set."

### Step 2 — nbtree descent: what 3–4 page reads buy you

> **In:** the `amgettuple` scan callback from Step 1.
> **Out:** the root→leaf descent (`_bt_search` → per-page `_bt_binsrch`)
> and the *exact position* it returns, priced at 3–4 page reads.

A btree probe walks root→leaf: read a page, binary-search *within* the
page to find the child pointer, follow it, repeat — `_bt_search`
(nbtsearch.c:100) calling `_bt_binsrch` (defined at :343, called at :153)
per level. The in-page search is an ordinary invariant-carrying binary
search, worth reading in the original because every filter in this topic
is trying to avoid it:

```c
// src/backend/access/nbtree/nbtsearch.c:388-404 (_bt_binsrch, postgres@701f021)
   388  	high++;						/* establish the loop invariant for high */
   389
   390  	cmpval = key->nextkey ? 0 : 1;	/* select comparison value */
   391
   392  	while (high > low)
   393  	{
   394  		OffsetNumber mid = low + ((high - low) / 2);
   395
   396  		/* We have low <= mid < high, so mid points at a real slot */
   397
   398  		result = _bt_compare(rel, key, page, mid);
   399
   400  		if (result >= cmpval)
   401  			low = mid + 1;
   402  		else
   403  			high = mid;
   404  	}
```

On a 10M-key index that's 3–4 page visits, each a cache-or-disk miss
chain — the in-memory analogue (a `BTreeMap` point miss) measured 299 ns
([FINDINGS.md](../../FINDINGS.md) row 26). What the misses buy is the
strongest possible answer: the *exact* position of the key, plus
ordered iteration from it (range scans), on *any* key distribution, with
no error to verify. That "no verification needed" property is exactly
what every structure in this topic gives up first.

### Step 3 — what exactness costs under concurrency and on writes

> **In:** the working descent from Step 2.
> **Out:** the three bills a filter never pays — lock-free concurrency,
> suffix truncation/dedup, and per-insert write amplification.

Three things bloom/PGM never have to deal with, all visible in nbtree:

- **Concurrency**: `_bt_moveright` (defined at :242; the doc comment
  explaining the move is at :211) — a reader racing a page split
  recovers by walking right-links (Lehman & Yao); no lock coupling on the
  descent. The README's L&Y section is the payoff read.
- **Suffix truncation & deduplication**: internal keys are truncated
  separators, duplicate leaf keys share a posting list
  (`_bt_binsrch_posting` defined at :603, called at :573) — nbtree has
  been absorbing compressed-postings ideas from the GIN/roaring world.
- **Write path**: every insert dirties a leaf (WAL, FPIs, topic 3) — the
  write amplification that makes "just add another index" a real bet.

The write line is the one to price: one row insert into a table with 5
btree indexes dirties 5 leaf pages plus WAL records plus possible
full-page images — the standing tax that makes cheap, approximate
alternatives worth wanting.

### Step 4 — GIN: an inverted index is topic 23 wearing a trench coat

> **In:** nbtree's "one key, one TID" model from Steps 2–3.
> **Out:** GIN's key → sorted-TID posting list, its varbyte delta
> compression, and the pending-list write buffer.

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

> **In:** the exact AMs of Steps 2–4.
> **Out:** BRIN's per-range min/max summary and its *one-sided*
> `amgetbitmap` that prunes page ranges, never confirms rows.

BRIN stores per-block-range summaries: min/max per 128-page range.
`brininsert` (brin.c:349) folds a new heap tuple's values into the
covering range's summary — an in-memory `BrinMemTuple`
(`src/include/access/brin_tuple.h:44-56`; two summaries merge via
`union_tuples`, brin.c:225; the build-time holder `BrinBuildState` is
brin.c:159-172). `bringetbitmap` (registered as the `amgetbitmap`
callback at brin.c:301, defined at brin.c:572) returns *candidate page
ranges*, never rows. It is exactly topic 12's zone map, and it is
*already* probabilistic in the useful direction: *one-sided* — it can say
"range definitely has no qualifying rows," never "row definitely exists."

The entire query-side logic fits in a filter:

```rust
// ILLUSTRATION — not quoted from postgres; the real callback is
// bringetbitmap (src/backend/access/brin/brin.c:572), which ANDs the
// scankeys against each range summary and emits candidate page ranges.
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

> **In:** the three AMs from Steps 2–5.
> **Out:** the one table that names, per AM, the exact-cost column each
> probabilistic structure in this topic undercuts.

Line the three AMs up and the whole topic's thesis appears: each
probabilistic structure shadows one exact AM and undercuts one column of
its bill.

| AM | granularity | answer type | write cost | shadow in this topic |
|---|---|---|---|---|
| nbtree | row (TID) | exact | leaf dirty + WAL per insert | the 246/299 ns baseline lanes |
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
| `amapi.h` `IndexAmRoutine` :233-326 | 1 | the callback vtable every AM fills (`aminsert` :298, `amgettuple` :312, `amgetbitmap` :313) |
| `nbtree/README` | 2–3 | genuinely one of the best docs in any codebase; read it fully (the Lehman & Yao section is the payoff) |
| `nbtree/nbtsearch.c` | 2–3 | the descent: `_bt_search` :100, `_bt_binsrch` def :343 (called :153), `_bt_moveright` def :242, `_bt_binsrch_posting` def :603 |
| `gin/ginpostinglist.c` + `gin/README` | 4 | varbyte posting lists: `ginCompressPostingList` :196, decode :284/:297 |
| `brin/brin.c` + `brin/README` | 5 | block-range summaries: `brinhandler` :254, `brininsert` :349, `bringetbitmap` def :572 (registered :301), `BrinBuildState` :159-172 |

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

## Done when

Answer each before unfolding it.

- [ ] You can explain what an index AM is and name the three price points behind the one API.

  <details><summary>Answer</summary>

  An AM is a pluggable index behind the `IndexAmRoutine` callback vtable
  (`amapi.h:233-326`): build, `aminsert` (:298), and a scan callback —
  `amgettuple` (:312) for ordered tuple-at-a-time or `amgetbitmap` (:313)
  for a candidate bitmap. The three price points are nbtree (exact
  position, most expensive), GIN (exact TID *set* per key, amortized
  writes), and BRIN (one-sided "maybe in this page range," nearly free).

  </details>

- [ ] You can say what nbtree's cache misses buy you that a filter cannot.

  <details><summary>Answer</summary>

  The 3–4 page reads of `_bt_search` (nbtsearch.c:100) / `_bt_binsrch`
  (:343) return the *exact* position — no false positives to verify — plus
  ordered iteration for range scans, on any key distribution. The
  in-memory analogue is a 299 ns `BTreeMap` point miss
  ([FINDINGS.md](../../FINDINGS.md) row 26). A filter only ever says
  "definitely absent / maybe present"; it can neither locate the row nor
  scan in order.

  </details>

- [ ] You can explain what exactness costs under concurrency and on writes.

  <details><summary>Answer</summary>

  Concurrency: `_bt_moveright` (def :242) walks right-links so a reader
  racing a split needs no lock coupling (Lehman & Yao). Writes: every
  insert dirties a leaf page plus WAL (and possible full-page images), so
  N btree indexes on a table cost N dirtied leaves per row insert — the
  write amplification a probabilistic structure avoids. Dedup shares
  duplicate leaf keys via posting lists (`_bt_binsrch_posting` :603).

  </details>

- [ ] You can explain why GIN is an inverted index and BRIN is an admitted filter.

  <details><summary>Answer</summary>

  GIN maps key → sorted posting list of TIDs (a search engine's term →
  docIDs), delta-compressed with varbyte (`ginCompressPostingList`
  ginpostinglist.c:196), buffered through a pending list — the inverted
  index of topic 23. BRIN keeps only a min/max summary per 128-page range
  (`brininsert` brin.c:349) and its `amgetbitmap` (`bringetbitmap` :572)
  returns candidate page ranges only, never rows — a one-sided filter, the
  same "definitely not here" shape as a bloom filter.

  </details>

- [ ] You can state the precise condition under which BRIN on a column is useful.

  <details><summary>Answer</summary>

  BRIN prunes a range iff that range's `[min, max]` does not overlap the
  query interval, so it only helps when the column is correlated with
  physical (heap) order — append-only timestamps prune well; a random UUID
  v4 makes every range's `[min, max]` span the whole domain, so nothing
  prunes. That is why BRIN is ~10,000× smaller than a bloom filter
  (128 pages per entry) yet entirely workload-dependent.

  </details>

- [ ] You wrote answers to all questions in notes.md, including the M26 synthesis — and you have this topic's in-memory baseline (BTreeMap miss at 299 ns) to put beside postgres's page-based numbers.

  <details><summary>Answer</summary>

  Self-check: map each M26 piece onto the AM it shadows — range index →
  nbtree, LSM blooms → none (postgres has no LSM to hang them on), roaring
  label filters → GIN, HLL count-distinct → none (postgres computes
  `count(DISTINCT)` exactly). The absence that hurts a graph workload most
  is the missing per-file bloom, priced by point-miss cost (299 ns) ×
  MATCH miss rate — the product topic 4 already measured.

  </details>

## References

**Code** ([postgres](https://github.com/postgres/postgres), `src/backend/access/`)
- `nbtree/README` — genuinely one of the best docs in any codebase;
  read it fully (the Lehman & Yao section is the payoff)
- `nbtree/nbtsearch.c` — the descent
- `gin/ginpostinglist.c` + `gin/README` — varbyte posting lists
- `brin/brin.c` + `brin/README` — block-range summaries
