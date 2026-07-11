# Reading SQLite `btree.c` — the classic (guided skim, 2 h)

Repo: [`~/repos/sqlite`](https://github.com/sqlite/sqlite) (shallow clone), files `src/btree.c` (11,633 lines),
`src/btreeInt.h` (746). Don't read linearly — you already know the format from
turso; here you're reading **the original** for the parts turso simplified and
the comments that carry 20 years of production scars.

## 1. Start with btreeInt.h:1–215

The file-format spec as a comment: page layout diagram, cell formats, freeblock
list, overflow, freelist. This is the best on-disk-format documentation in open
source. Read it entire.

Key structs:
- `MemPage` — btreeInt.h:273–303. Note `xCellSize` / `xParseCell` **function
  pointers** picked once per page type at init — devirtualized dispatch, 1994
  style. And `nFree` is computed lazily (−1 until needed).
- `CellInfo` — btreeInt.h:480–486: `nKey`, `pPayload`, `nLocal`, `nSize`.

## 2. The search path

- `sqlite3BtreeTableMoveto` — btree.c:5837–5978. Binary search :5917–5954;
  child descent :5965–5971 (`lwr >= nCell` ⇒ rightmost pointer). Note the
  **bias hint** parameter — appenders skip the binary search.
- `sqlite3BtreeIndexMoveto` — btree.c:6068–6295. Uses an `xRecordCompare`
  callback specialized per key shape — same devirtualization move.

## 3. Balance — read for the engineering, not the algorithm

- `balance()` dispatcher — btree.c:9162–9225.
- `balance_quick` — btree.c:8039–8150: rightmost-leaf append gets its own path
  (sequential inserts are THE common case — fillseq from topic 1).
- `balance_nonroot` — btree.c:8277–8826. `NB = 3` at :7552. Find the comment
  near :8738: the right-bias optimization "makes the database about 25% faster"
  — a one-line distribution tweak, measured. Topic-0 lesson in the wild.
- `balance_deeper` — btree.c:9081: root split = tree grows up.

## 4. Free space within a page

- `allocateSpace` — btree.c:1846–1944; `freeSpace` — :1945–2050 (merges adjacent
  freeblocks!); `defragmentPage` — :1640–1837.
- Overflow-cell trick: an overfull page keeps up to `apOvfl[]` cells **beside**
  the page (insertCell :7363–7450) rather than reallocating — balance consumes
  them immediately. The page is never physically overfull on disk.

## 5. Two things turso doesn't have (yet)

- **Pointer maps** (auto-vacuum) — btreeInt.h:653–668, btree.c:1098–1170:
  a reverse index (page → parent) so vacuum can relocate pages. Costs a ptrmap
  page every ~⌊usable/5⌋ pages.
- **Interior-delete via predecessor swap** — btree.c:9873–10050 (:9954 leaf
  check, :9956 predecessor fetch): interior deletes become leaf deletes + rebalance.

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
