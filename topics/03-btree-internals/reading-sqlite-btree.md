# btree.c: twenty years of production scars

You already know the format from turso — this guided skim (2 h) reads **the
original** for the parts turso simplified and for comments that carry two
decades of production experience: the balance_quick fast path, the "25%
faster" right-bias tweak, pointer maps, predecessor-swap deletes. This
chapter builds those production tricks one step at a time, then hands you the
reading route — don't read its 11,633 lines linearly.

## The problem in one sentence

btree.c must make the single most common write on Earth — appending the next
sequential rowid to a table — nearly free, inside **11,633 lines** of C that
also survive every crash, corrupt page, and pathological key distribution
that twenty years on billions of devices can produce; one measured
distribution tweak in its balance code is worth "about 25%" of whole-database
speed.

## The concepts, step by step

### Step 1 — MemPage: parse the page once, dispatch without branching

`MemPage` is the in-memory representation of one disk page (a fixed-size
4 KB block): the raw bytes plus decoded header fields, built once when the
page enters the cache so no later operation re-parses the header. Two
production tricks live in this struct:

- `xCellSize` / `xParseCell` are **function pointers** picked once per page
  type at init — a table leaf gets the table-leaf parser, an index interior
  gets the index-interior parser — so the per-cell inner loop never
  re-checks "what kind of page am I?". Devirtualized dispatch, 1994 style.
- `nFree` (the page's free-byte count) is computed **lazily** — held at −1
  until someone actually needs it, because computing it means walking the
  freeblock list (Step 3) and most page visits never ask.

The companion struct `CellInfo` (`nKey`, `pPayload`, `nLocal`, `nSize`) is
what `xParseCell` fills in: one cell (a single key+row entry on the page)
decoded into fields. Why it matters: cell parsing is the innermost loop of
every search, insert, and balance — this is where cycles go.

### Step 2 — the search path: descend, binary-search, and skip work when hinted

A lookup descends from the root page, binary-searching each page's sorted
cells to pick the child pointer to follow, until it lands on a leaf. Two
optimizations mark this as production code:

- `sqlite3BtreeTableMoveto` takes a **bias hint** parameter: a caller that
  knows it's appending (rowids arriving in order — the common case) skips
  the binary search entirely and probes the rightmost cell first. When
  `lwr >= nCell` after the search, the descent takes the page's rightmost
  pointer.
- `sqlite3BtreeIndexMoveto` compares full keys, and uses an
  `xRecordCompare` **callback specialized per key shape** — the comparator
  for "one integer column" is a different function than the general one.
  Same devirtualization move as Step 1.

Why it matters: comparisons are the entire CPU cost of a descent; picking the
specialized comparator once per query instead of branching per comparison is
free money.

### Step 3 — free space within a page: freeblocks, merged on free

When a cell is deleted, its bytes become a **freeblock** — a hole inside the
page, threaded into a linked list (each freeblock stores a 2-byte pointer to
the next hole and its own 2-byte size) so later inserts can reuse the space.

- `allocateSpace` satisfies an insert from the freeblock list, then from the
  gap between the pointer array and cell content, and only then compacts.
- `freeSpace` **merges adjacent freeblocks** as it inserts the new hole into
  the (address-ordered) list — deletes actively fight fragmentation instead
  of deferring everything.
- `defragmentPage` is the last resort: rewrite all cells contiguously, reset
  the list.

Why it matters: this is the machinery that makes delete cheap (unlink a
2-byte pointer, thread a hole) while keeping pages usable for decades of
churn without a vacuum.

### Step 4 — the overflow-cell trick: a page is never physically overfull

When an insert doesn't fit even after Step 3's efforts, SQLite does *not*
grow or reallocate the page — the incoming cell is parked **in memory,
beside the page**, in a small `apOvfl[]` array, and the caller is obligated
to run balance (Step 5) before releasing the page. Balance drains `apOvfl[]`
into its redistribution pool immediately.

```rust
// insertCell's trick: a page is never physically overfull
fn insert_cell(page: &mut MemPage, i: usize, cell: Cell) {
    match page.allocate_space(cell.len()) {     // freeblocks → gap → defrag
        Some(off) => page.write_cell(off, i, &cell),
        None => {
            page.ap_ovfl.push((i, cell));       // parked IN MEMORY, beside the page
            // caller must run balance() before the page is released: the
            // balance pool drains ap_ovfl while redistributing ≤3 siblings,
            // so the on-disk format never needs an "overfull" representation
        }
    }
}
```

Why it matters: the on-disk format never needs an "overfull page"
representation, so every page on disk is always valid — a crash-safety and
simplicity win bought with one tiny in-memory array.

### Step 5 — balance: read it for the engineering, not the algorithm

**Balance** is what runs when a page overflows (or underflows): pool the
cells of the problem page and its neighbors, redistribute them across
enough pages, push new separator keys to the parent. The `balance()`
dispatcher picks between three production-shaped paths:

- `balance_quick` — the rightmost-leaf append gets its **own dedicated
  path**: just allocate one new leaf on the right and put the new cell there,
  touching the minimum possible pages. Sequential inserts are THE common
  case — fillseq from topic 1 — so it gets its own code.
- `balance_nonroot` — the general case: pool the overfull page with up to
  `NB = 3` siblings and redistribute. Find the comment near :8738: the
  right-bias optimization — packing pages fuller on the left so the
  rightmost page has room for the *next* append — "makes the database about
  25% faster". A one-line distribution tweak, measured. Topic-0 lesson in
  the wild.
- `balance_deeper` — root split: the root's content moves into a new child
  and the tree grows *up* by one level, the only operation that increases
  height.

Why it matters: the algorithm is in every textbook; the fast path, the bound
NB=3, and the measured 25% tweak are what two decades of production look like.

### Step 6 — pointer maps: the reverse index turso doesn't have

A **pointer map** is a reverse index — for each page number, which page
points *at* it (its parent, or the overflow page before it) — stored in
dedicated ptrmap pages when auto-vacuum is enabled.

B-trees only have downward pointers, so relocating a page (which vacuum must
do to shrink the file) would otherwise require searching the whole tree for
whoever points at it. The cost: one ptrmap page every ~⌊usable/5⌋ pages of
the file. Why it matters: it's a concrete example of paying a permanent
format tax for one management operation — and of why turso hasn't
implemented it (yet).

### Step 7 — interior deletes become leaf deletes: the predecessor swap

Deleting a key that lives on an *interior* page can't just remove the cell —
that cell is also the separator routing searches between two subtrees. So
SQLite swaps in the key's **predecessor** (the largest key in the left
subtree, always on a leaf), overwriting the interior cell, then deletes the
predecessor from its leaf and rebalances there.

Why it matters: every delete's structural work happens at leaf level, where
balance (Step 5) already knows what to do — one mechanism instead of two.

## Where each step lives in the code

**Start with btreeInt.h:1–215** — the file-format spec as a comment: page
layout diagram, cell formats, freeblock list, overflow, freelist. This is
the best on-disk-format documentation in open source. Read it entire before
any function.

- **Step 1**: `MemPage` — btreeInt.h:273–303 (note the `xCellSize` /
  `xParseCell` function pointers and lazy `nFree`); `CellInfo` —
  btreeInt.h:480–486: `nKey`, `pPayload`, `nLocal`, `nSize`.
- **Step 2**: `sqlite3BtreeTableMoveto` — btree.c:5837–5978. Binary search
  :5917–5954; child descent :5965–5971 (`lwr >= nCell` ⇒ rightmost pointer);
  the bias-hint parameter. `sqlite3BtreeIndexMoveto` — btree.c:6068–6295
  with its per-key-shape `xRecordCompare` callback.
- **Step 3**: `allocateSpace` — btree.c:1846–1944; `freeSpace` — :1945–2050
  (merges adjacent freeblocks!); `defragmentPage` — :1640–1837.
- **Step 4**: `insertCell` — btree.c:7363–7450 (the `apOvfl[]` parking).
- **Step 5**: `balance()` dispatcher — btree.c:9162–9225; `balance_quick` —
  btree.c:8039–8150; `balance_nonroot` — btree.c:8277–8826 with `NB = 3` at
  :7552 and the "about 25% faster" comment near :8738; `balance_deeper` —
  btree.c:9081.
- **Step 6**: pointer maps (auto-vacuum) — btreeInt.h:653–668,
  btree.c:1098–1170.
- **Step 7**: delete — btree.c:9873–10050 (:9954 leaf check, :9956
  predecessor fetch).

## Questions to answer in notes.md

1. `fillInCell` (btree.c:7106) builds the overflow chain BEFORE the cell is
   inserted into the page. What crash-safety property makes that ordering safe?
   (Pages only become durable at commit via pager/WAL — nothing here is.)
2. Why does `balance_quick` exist when `balance_nonroot` handles the same case?
   Estimate the work saved for a fillseq insert (pages touched, cells copied).
3. SQLite computes `nFree` lazily and validates cells only under
   `SQLITE_DEBUG`. What does that say about where btree.c sits on the
   trust-the-page-vs-verify spectrum, and what's the corruption story?
   (`PRAGMA integrity_check` exists for a reason.)

## Done when

You can explain why NB=3 (bounded work per split, adjacent redistribution beats
cascading splits) and name the two fast paths (bias hint, balance_quick) that
serve sequential inserts.

## References

**Code**
- [sqlite](https://github.com/sqlite/sqlite) — `src/btree.c` (11,633
  lines; don't read linearly) and `src/btreeInt.h` (746 lines) —
  btreeInt.h:1–215 is the best on-disk-format documentation in open
  source; read that comment entire before any function
