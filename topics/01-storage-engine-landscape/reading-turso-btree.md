# Turso's B-tree: the canonical page engine, in Rust

turso re-implements the SQLite file format, so this is a reading of *the*
canonical page-oriented engine — with Rust types instead of C macros. It is the
B-tree protagonist opposite fjall's LSM. Before touching the code, this chapter
builds the machine step by step: why pages exist, how a tree of pages finds a
row, how one page stores variable-length rows, what one insert does, and how the
whole thing survives a crash. Then it hands you the file and line anchors to
watch each step happen.

Everything below is anchored at `tursodatabase/turso@a820079e8` (v0.9.0-pre.1),
this repo's pin (`python3 tools/pinned-source.py ref turso`). Read any file at
that commit with `python3 tools/pinned-source.py show turso core/storage/btree.rs -r A:B`
— that is more reliable than a local clone, because these files move fast.

## The problem in one sentence

Store a million sorted rows on disk so that finding one costs a handful of disk
reads and inserting one doesn't rewrite the file.

## The concepts, step by step

### Step 1 — the page: disks deal in blocks, so the engine does too

> **In:** a block device that transfers fixed-size chunks and an engine that
> must survive being killed mid-write.
> **Out:** the page as the universal unit — of IO, of caching, of the atomicity
> argument — and the page number as disk's pointer.

Disks and OSes transfer data in fixed-size blocks, and a crash-safe engine wants
a unit it can read, cache and write atomically. So the database file is an array
of fixed-size **pages** (SQLite's default is 4 KB), and "one disk IO" always
means "one page". Every structure that follows is built out of pages that point
at each other by **page number** — a page number is disk's version of a pointer,
and dereferencing one means asking the pager for that page.

Two consequences you will meet again in this guide. First, the *page cache* is
sized in pages, not bytes: turso's default is
`DEFAULT_PAGE_CACHE_SIZE_IN_PAGES = 2000` (`core/storage/page_cache.rs:14`, with
a separate 100,000 for wasm at `:16`) — 8.2 MB at 4 KB pages. Second, the tree
has a hard depth bound derived from page arithmetic:
`BTCURSOR_MAX_DEPTH = 20` (`core/storage/btree.rs:157`), justified in the comment
above it as "a maximum database size of 2^31 pages, a minimum fanout of 2 for a
root-node and 3 for all other internal nodes". Anything deeper is declared
corrupt rather than traversed.

### Step 2 — a tree of pages: fanout is everything

> **In:** a million sorted rows and a budget of a few page reads per lookup.
> **Out:** fanout computed from real SQLite cell sizes, the resulting height,
> and the fraction of the file that has to stay cached to make it work.

To find one row among a million with few page reads, arrange the pages as a
sorted tree. Each **interior page** holds separator keys and child page numbers;
each **leaf page** holds the actual rows. Because one page holds *hundreds* of
keys — not 2, like a binary tree node — the tree is extremely flat: the height
is log-base-*fanout*, not log-base-2.

"Hundreds" is not a hand-wave; it falls out of the page header layout turso
documents at `core/storage/btree.rs:100–148`. Write **P** for page size, **H**
for header bytes (12 interior, 8 leaf — stated on line 100), **s = 2** for a cell
pointer, and **c** for a cell's own bytes. Then

```text
  fanout = floor((P − H) / (c + s))

  Table-interior cell = 4 B child page number + varint rowid (3 B up to ~2 M)
      c = 7   ⇒  (4096 − 12) / (7 + 2)    = 453 children per interior page

  Table-leaf cell, 100-byte row
      = varint payload length (2 B) + varint rowid (3 B) + 100 B payload
      c = 105 ⇒  (4096 −  8) / (105 + 2)  =  38 rows per leaf page

  Index-interior cell, 16-byte key
      = 4 B child + varint payload length (1 B) + 16 B key
      c = 21  ⇒  (4096 − 12) / (21 + 2)   = 177 children per interior page
```

Now the tree for 1,000,000 rows of 100 bytes:

```text
  leaves      = ceil(1,000,000 / 38)  = 26,316 pages   ← 107.8 MB
  interior L1 = ceil(   26,316 / 453) =     59 pages
  root        = ceil(       59 / 453) =      1 page
  ─────────────────────────────────────────────────
  height 3: root → interior → leaf. A point lookup reads 3 pages.
  Interior total = 60 pages = 245.8 KB = 0.23% of the file.
```

That last line is the whole argument for why B-trees stay fast: the navigational
part of the structure is a quarter of a megabyte, so it lives in the page cache
permanently and only the final leaf read is a real IO. With turso's default
2000-page cache you hold all 60 interior pages *and* 1,940 leaves — 7.4% of the
leaf level — for 8.2 MB.

Note the old rule of thumb "fanout ≈ 50" is far too pessimistic for a rowid
table: 50 would require a ~70-byte separator. At fanout 50 the same 26,316
leaves need three interior levels (527 → 11 → 1) and the tree is height 4. So
**fanout is set by key size, and key size sets height** — which is exactly the
lever topic 3 measures. Its worked table for other key/value shapes is in
[topics/03-btree-internals/notes.md](../03-btree-internals/notes.md); topic 3's
own headline is that height stopping at 3 does *not* stop lookups getting
slower, because cache residency, not height, sets what a page touch costs.

That is the entire reason B-trees won: **the tree's shape is dictated by the
page size**, so the memory hierarchy's block transfers are never wasted.

### Step 3 — inside one page: the slotted layout

> **In:** one 4 KB page that must hold variable-length rows, keep them sorted,
> and absorb inserts and deletes in place.
> **Out:** the slotted-page layout, the header fields that implement it, and the
> reason B-trees have space amplification.

Storing rows back-to-back fails: inserting in the middle would shift everything
after it. The fix is one level of indirection — a **slotted page**:

```text
 ┌────────────┬──────────────────────┬────────────┬─────────────────┐
 │ header     │ cell pointer array   │ free space │ cell content    │
 │ 8/12 bytes │ u16 offsets, →grows  │            │ ←grows, actual  │
 │            │ rightward            │            │ records         │
 └────────────┴──────────────────────┴────────────┴─────────────────┘
   two regions grow toward each other; a "full" page = they meet
```

- The rows ("**cells**") are written wherever there's room, from the right. The
  source says why in as many words at `core/storage/btree.rs:136–138`: "SQLite
  strives to place cells as far toward the end of the b-tree page as it can, in
  order to leave space for future growth of the cell pointer array."
- A small array of 2-byte offsets at the front — the **cell pointer array** — is
  kept in sorted-key order. Sorting means moving 2-byte pointers, never the rows
  themselves; binary search runs over the pointer array.
- Delete = remove the pointer, *leave the bytes*. The dead bytes are reclaimed
  lazily ("defragmentation") only when space runs out.

turso spells the header out field by field, with an ASCII diagram, in the
`offset` module — read it, it is the file format in twenty lines:

```rust
// core/storage/btree.rs at tursodatabase/turso@a820079e8, lines 100-148
// (constants only; the doc comments on each are worth reading in full)
   100  /// The B-Tree page header is 12 bytes for interior pages and 8 bytes for leaf pages.
   ... 101-107: the ASCII diagram of the 12 header bytes ...
   108  pub mod offset {
   109      /// Type of the B-Tree page (u8).
   110      pub const BTREE_PAGE_TYPE: usize = 0;
   ... 111-121: doc comment — the freeblock definition ...
   122      pub const BTREE_FIRST_FREEBLOCK: usize = 1;
   ... 123:     blank ...
   124      /// The number of cells in the page (u16).
   125      pub const BTREE_CELL_COUNT: usize = 3;
   ... 126-138: doc comment — the content-area pointer, and why it moves left ...
   139      pub const BTREE_CELL_CONTENT_AREA: usize = 5;
   ... 140-143: doc comment — the fragment definition ...
   144      pub const BTREE_FRAGMENTED_BYTES_COUNT: usize = 7;
   ... 145:     blank ...
   146      /// The right-most pointer (saved separately from cells) (u32)
   147      pub const BTREE_RIGHTMOST_PTR: usize = 8;
   148  }
```

The six constants are byte offsets into the header, and their widths are in
the doc comments: the page type is one byte at 0; the first-freeblock pointer
(head of the freeblock chain), the cell count (how many pointers the array
holds) and the content-area start (where the leftward-growing content begins)
are `u16`s at 1, 3 and 5; the fragment count is one byte at 7; and the
rightmost child pointer, a `u32` at 8 (line 147), exists only on interior
pages — which is why interior headers are 12 bytes and leaf headers 8.

Two of those fields exist purely to manage the dead space deletes leave behind,
and the doc comments define them precisely. A **freeblock**
(`BTREE_FIRST_FREEBLOCK`, comment at `:112–121`) is a run of **at least 4 bytes**
inside the cell content area that is no longer in use, chained to the next one —
explicitly *not* the regular free gap in the middle of the page. **Fragments**
(`BTREE_FRAGMENTED_BYTES_COUNT`, `:143`) are "isolated groups of 1, 2, or 3
unused bytes" — too small to be worth chaining, so they are merely counted.
When the counter or the chain gets bad enough, `defragment_page()`
(`core/storage/btree.rs:9842`) compacts the content area.

This layout is also why B-trees have space amplification: the free gap in the
middle of every page, plus the freeblocks and fragments, is the price of
in-place insertion. [FINDINGS.md](../../FINDINGS.md) row 1 is that price
measured on this topic's workload — the same 108 MB of records occupies 48 MB
under fjall's LSM (space amp **0.45×**) and 6.8 GB under redb's copy-on-write
B-tree (**63.28×**), a **140× spread**. redb is not turso, and the mechanism
there is copy-on-write rather than slotted-page slack (per-batch commits copy
every page on the root path), but the direction is the same one this layout
sets up: the in-place family spends space to buy in-place updates.

### Step 4 — one insert, mechanically

> **In:** a cell to add, and a leaf page with its two regions.
> **Out:** the four moves that make the common case dirty exactly one page, and
> the single condition that escalates it.

With Steps 1–3, an insert into a leaf is four small moves:

```rust
// ILLUSTRATION — the shape of turso's insert, not its source. The real path is
// insert() core/storage/btree.rs:6927 → insert_into_page() :3052 →
// insert_into_cell() :10130; the overflow branch is balance() :3291.
1  fn insert_cell(page: &mut Page, idx: usize, cell: &[u8]) -> Result<(), Full> {
2      let ptrs_end = page.header_len() + 2 * (page.ncells + 1); // ptr array grows →
3      let content_start = page.content_start - cell.len();      // content grows ←
4      if content_start < ptrs_end {
5          return Err(Full);                     // regions met: time to balance/split
6      }
7      page.buf[content_start..content_start + cell.len()].copy_from_slice(cell);
8      page.shift_pointers_right(idx);           // open slot idx — keys stay sorted
9      page.write_u16(page.ptr_slot(idx), content_start as u16);
10     page.ncells += 1;
11     page.content_start = content_start;       // BTREE_CELL_CONTENT_AREA, offset 5
12     Ok(())
13 }
14 // delete = remove the u16 pointer, LEAVE the bytes → freeblocks + fragments,
15 // reclaimed only by defragment_page() (btree.rs:9842) — cheap deletes,
16 // deferred cleanup. The mirror of line 8 is shift_pointers_left() (:10582),
17 // which is a single copy_within over the 2-byte pointers.
```

Line 8 is the payoff of the whole layout: keeping the page sorted costs a
`copy_within` over 2-byte pointers, never a move of the records. Line 4 is the
only branch that can escalate — the common case dirties exactly **one page**.
`Err(Full)` is the interesting case, and it is Step 5.

### Step 5 — when the page is full: balance, not naive split

> **In:** a leaf whose two regions have met.
> **Out:** why SQLite redistributes across siblings instead of splitting, the
> two constants that bound the operation, and the resulting gradient of dirty
> pages per insert.

The textbook answer is: split the full page into two half-full pages and add a
separator key to the parent. That works, but it leaves pages 50% full — space
amplification and a deeper tree.

SQLite, and turso in `balance_non_root()` (`core/storage/btree.rs:3494`), does
better: take the full page **and up to two siblings**, pool all their cells, and
redistribute them evenly across the resulting pages. The bound is a named
constant — `MAX_SIBLING_PAGES_TO_BALANCE: usize = 3`
(`core/storage/btree.rs:160`) — and so is its consequence:

```rust
// core/storage/btree.rs at tursodatabase/turso@a820079e8, lines 159-163
   159  /// Maximum number of sibling pages that balancing is performed on.
   160  pub const MAX_SIBLING_PAGES_TO_BALANCE: usize = 3;
   161
   162  /// We only need maximum 5 pages to balance 3 pages, because we can guarantee that cells from 3 pages will fit in 5 pages.
   163  pub const MAX_NEW_SIBLING_PAGES_AFTER_BALANCE: usize = 5;
```

Line 162 is a proof obligation stated as a comment, and it is worth checking
against Step 2's numbers: three full leaves hold at most 3 × 38 = 114 cells;
after balancing, those 114 cells plus the incoming one spread over at most 5
pages, i.e. 23 per page, comfortably inside the 38 a page can take. The reason
the bound is 5 rather than 4 is the divider cells that have to be pushed into
the parent.

Fewer, fuller pages ⇒ a shallower tree and less slack per page. The costs to
notice: a balance dirties ~3 pages instead of 1, and in the rare worst case a
split propagates upward until the root itself splits — `balance_root()`
(`core/storage/btree.rs:5300`), the only operation that makes the tree taller.

So one insert dirties:

```text
  1 page          common case — cell fits (Step 4, line 4 takes the happy path)
  ~3–5 pages      balance_non_root: 3 siblings pooled into ≤5, parent updated
  O(height)       balance_root: propagation reaches the root, tree grows a level
                  bounded above by BTCURSOR_MAX_DEPTH = 20 (btree.rs:157)
```

Hold that gradient — it is question 2 below, and it is the B-tree half of the
write-amplification story that [FINDINGS.md](../../FINDINGS.md) row 1 measures
from the other end.

### Step 6 — surviving a crash: the pager and the WAL

> **In:** an engine that writes pages in place, and a machine that can lose
> power between two of those writes.
> **Out:** the two components that fix it, the actual call sites, and the
> difference between the two journals a page passes through.

Writing pages in place is exactly what makes crashes dangerous: die mid-write
and the old version is *gone*. Two components fix this.

The **pager** (`Pager`, `core/storage/pager.rs:1610`) owns all page IO: it caches
pages in memory, hands them to the B-tree, and tracks which are **dirty**
(modified but not yet written). Reads go through `read_page()`
(`core/storage/pager.rs:3648`, a one-line wrapper over `read_page_into()` at
`:3654`, which tries the page cache first) and fall through to
`read_page_no_cache()` (`:3586`) on a miss; `add_dirty()` (`:3888`) marks a page
modified.

The **WAL** (write-ahead log, `WalFile` at `core/storage/wal.rs:2798`) is an
append-only file. The rule that gives it its name: a page's new version is
appended to the WAL *before* the database file is ever touched. Commit = the WAL
append is durable. Later, a **checkpoint** (`core/storage/wal.rs:4126`) copies
WAL frames back into the main file — which is the only time the main file is
written at all.

It is tempting to read `add_dirty()` as the write-ahead rule made visible —
the page is about to change, so log it. It does *not* write to the WAL. Read it:

```rust
// core/storage/pager.rs at tursodatabase/turso@a820079e8, lines 3888-3896
  3888      pub fn add_dirty(&self, page: &Page) -> Result<()> {
  3889          turso_assert!(
  3890              page.is_loaded(),
  3891              "page must be loaded in add_dirty() so its contents can be subjournaled",
  3892              { "page_id": page.get().id() }
  3893          );
  3894          self.subjournal_page_if_required(page)?;
  3895          let mut dirty_pages = self.dirty_pages.write();
  3896          dirty_pages.insert(page.get().id() as u32);
```

The line to focus on is 3894: it writes the page's *pre-image* to a
**subjournal** (`core/storage/subjournal.rs`, held at `pager.rs:1637`), which exists so a
`SAVEPOINT` or a failed statement can be rolled back *within* an open
transaction. That is a different mechanism from the WAL, with a different
lifetime. The WAL frames are appended later, on the commit path: `cacheflush()`
(`core/storage/pager.rs:3927`) collects the dirty set into batches and hands
each to `flush_page_batch()` (`:4175`), which calls
`wal.append_frames_vectored(pages, page_sz)` at `pager.rs:4186`; the
cache-spill path, `spill_append_frames_to_wal()` (`:4362`), appends at `:4378`.
Both land in `core/storage/wal.rs:4666`. So there are *two* journals
here — one for intra-transaction rollback, one for crash recovery — and reading
`add_dirty()` as "the write-ahead rule, visible in code" conflates them.

The punchline for the topic's B-tree-vs-LSM framing is sharper for the
distinction: even the in-place family writes out-of-place *first*,
then reconciles. The difference is what is **authoritative** — here the B-tree
file is (the WAL is a temporary patch that `checkpoint()` folds back in); in an
LSM the log-structured files *are* the database and nothing is ever folded back.

## Where each step lives in the code

All at `tursodatabase/turso@a820079e8`. Line counts are that commit's; these files
move fast, so navigate by symbol name if you read a different revision.

| File | Lines | Role (steps) |
|------|-------|------|
| `core/storage/btree.rs` | 15,479 | cursor, slotted pages, balance (2–5) |
| `core/storage/wal.rs` | 11,276 | WAL frames + checkpoint (6) |
| `core/storage/pager.rs` | 7,271 | page cache, dirty tracking, IO (1, 6) |
| `core/storage/sqlite3_ondisk.rs` | 2,748 | cell parsing — the byte format (3) |
| `core/storage/page_cache.rs` | 1,964 | SIEVE-eviction page cache (1, 6) |

- **Step 1**: `DEFAULT_PAGE_CACHE_SIZE_IN_PAGES = 2000` —
  `page_cache.rs:14`; `BTCURSOR_MAX_DEPTH = 20` — `btree.rs:157`.
- **Step 3**: the header field map and its ASCII diagram — `btree.rs:100–148`;
  cell parsing in `read_btree_cell()` — `sqlite3_ondisk.rs:847`; delete
  fragmentation fixed by `defragment_page()` — `btree.rs:9842` (with
  `defragment_page_fast` at `:9693`, `_full` at `:9819`, `_for_insert` at
  `:9832`); pointer-array maintenance via `copy_within` in
  `shift_pointers_left()` — `btree.rs:10582`.
- **Steps 2 + 4 — the cursor**: every operation moves via `BTreeCursor`
  (`btree.rs:873`), with `CursorContext` (`btree.rs:576`, key enum at `:567`)
  and `PinGuard` (`btree.rs:410` — pins a page in the cache while the cursor
  points at it). Trace one descent in `seek()` (`btree.rs:6812`; declared in
  `CursorTrait` at `:807`): root → binary search the cell pointer array → child
  page number → pager fetch → leaf. Insert: `insert()` (`btree.rs:6927`) →
  `insert_into_page()` (`btree.rs:3052`) → `insert_into_cell()` (`btree.rs:10130`).
  Every one of these returns `IOResultOr<T>` (`core/types.rs:3723`, a `Result`
  over `IOResult::Done(T)` or `IOResult::IO(..)` — "finished", or "here is the
  IO to wait on before calling again") because turso's cursor is a resumable
  state machine, not a blocking call.

```mermaid
flowchart LR
    S["seek(key)<br/>btree.rs:6812"] --> D["descend: binary search cells,<br/>follow child ptr"]
    D --> PG["pager.read_page<br/>pager.rs:3648"]
    PG --> L["leaf: insert_into_page<br/>btree.rs:3052"]
    L -- page overflows --> B["balance<br/>btree.rs:3291"]
    B --> BNR["balance_non_root<br/>btree.rs:3494<br/>≤3 siblings → ≤5 pages"]
    BNR -- propagates to root --> BR["balance_root<br/>btree.rs:5300<br/>tree grows a level"]
```

- **Step 5**: `MAX_SIBLING_PAGES_TO_BALANCE = 3` — `btree.rs:160`;
  `MAX_NEW_SIBLING_PAGES_AFTER_BALANCE = 5` — `btree.rs:163`; `balance()` —
  `btree.rs:3291`; `balance_non_root()` — `btree.rs:3494`; `balance_root()` —
  `btree.rs:5300`.
- **Step 6**: `Pager` — `pager.rs:1610`; `read_page()` — `pager.rs:3648`;
  `read_page_no_cache()` — `pager.rs:3586`; `add_dirty()` — `pager.rs:3888`
  (**subjournal**, not WAL — see Step 6); the subjournal handle itself —
  `pager.rs:1637`. Commit path: `cacheflush()` — `pager.rs:3927`, which calls
  `flush_page_batch()` (`pager.rs:4175`) → `append_frames_vectored` at
  `pager.rs:4186`. WAL: `WalFile` — `wal.rs:2798` (shared state
  `WalFileShared` — `wal.rs:3009`); `append_frames_vectored()` impl —
  `wal.rs:4666` (trait declaration `wal.rs:741`); `checkpoint()` impl —
  `wal.rs:4126` (trait declaration `wal.rs:752`); `CheckpointMode` —
  `wal.rs:160`. Page cache: `PageCache` — `page_cache.rs:100`, SIEVE eviction
  described at `:90–98`, `spill_threshold` at `:111` (buffer-pool preview,
  topic 6).

## Questions to answer in notes.md

Each needs the source open. `python3 tools/pinned-source.py show turso
core/storage/btree.rs -r A:B` is the fastest way in.

1. Redo Step 2's fanout arithmetic for a **16-byte index key** instead of a
   rowid table (index-interior cell = 4 B child + 1 B varint length + key). What
   height does 1M rows give, how many pages is the interior level, and how much
   of turso's default 2000-page cache does it consume? Then say which of those
   two numbers — height or cached fraction — topic 3 found actually predicts
   lookup latency.
2. Why does `balance_non_root()` (`btree.rs:3494`) prefer redistribution over
   splitting? Check the claim on `btree.rs:162` — that cells from 3 pages always
   fit in 5 — against your Step 2 cell sizes, and say what the choice does to
   write amplification (≈3–5 dirty pages per balance versus 2 for a naive
   split) *and* to space amplification.
3. During a checkpoint, what blocks writers? Read `checkpoint()`
   (`core/storage/wal.rs:4126`) far enough to name the mode enum
   (`CheckpointMode`, declared at `wal.rs:160`) and say which of its variants
   waits for readers.
4. `add_dirty()` (`pager.rs:3888`) subjournals a page; `cacheflush()`
   (`pager.rs:3927`) appends WAL frames. Write down, for a transaction that
   modifies one page and then hits a statement error, exactly which bytes each
   of the two journals holds and when each is discarded. Which one would you
   have to disable to get an honest write-amplification measurement?
5. `BTCURSOR_MAX_DEPTH = 20` (`btree.rs:157`) is justified by "2^31 pages,
   minimum fanout of 2 for a root and 3 for other internal nodes". Work that
   bound: what tree size does depth 20 at fanout 3 actually cover, and how far
   is that from the 2^31-page limit? What does the slack tell you about how
   defensive this constant is?

## Done when

Answer each before unfolding it.

- [ ] You can draw the slotted page from memory, name the six header fields, and say which two exist only to manage dead space.

<details>
<summary>Answer</summary>

Header (12 bytes interior, 8 leaf — `btree.rs:100`), then a rightward-growing
array of 2-byte cell pointers in key order, then free space, then the cell
content area growing leftward from the end of the page.

The six fields (`btree.rs:108–148`): `BTREE_PAGE_TYPE` (0, u8),
`BTREE_FIRST_FREEBLOCK` (1, u16), `BTREE_CELL_COUNT` (3, u16),
`BTREE_CELL_CONTENT_AREA` (5, u16), `BTREE_FRAGMENTED_BYTES_COUNT` (7, u8),
`BTREE_RIGHTMOST_PTR` (8, u32, interior pages only).

The two dead-space fields are `BTREE_FIRST_FREEBLOCK` — head of a chain of
unused runs of **at least 4 bytes** *inside* the content area, explicitly not
the free gap in the middle (`:112–121`) — and `BTREE_FRAGMENTED_BYTES_COUNT`,
which merely counts "isolated groups of 1, 2, or 3 unused bytes" (`:143`) too
small to chain. `defragment_page()` (`btree.rs:9842`) is what reclaims them.

</details>

- [ ] You can compute fanout from page size and cell size, and give the height of a 1M-row rowid table with 100-byte rows.

<details>
<summary>Answer</summary>

`fanout = floor((P − H) / (c + s))` with P = 4096, H = 12 interior / 8 leaf,
s = 2 for the cell pointer.

Interior (4 B child + 3 B varint rowid, c = 7): (4096 − 12)/9 = **453**.
Leaf (2 B varint length + 3 B varint rowid + 100 B, c = 105):
(4096 − 8)/107 = **38**.

1,000,000 / 38 = **26,316 leaves** (107.8 MB); 26,316 / 453 = **59** interior;
59 / 453 = **1** root. **Height 3** — a point lookup reads 3 pages. The 60
interior pages are 245.8 KB, **0.23%** of the file, so they stay resident in the
2000-page (8.2 MB) default cache along with 7.4% of the leaves.

</details>

- [ ] You can explain how one insert can dirty 1 page, 3–5 pages, or O(height) pages, and name the constant that bounds the middle case.

<details>
<summary>Answer</summary>

**1 page**: the cell fits — the pointer array and the content area have not met,
so the write is `copy_from_slice` into the content area plus a `copy_within` over
the 2-byte pointers.

**3–5 pages**: the regions met, so `balance_non_root()` (`btree.rs:3494`) pools
the full page with up to two siblings — `MAX_SIBLING_PAGES_TO_BALANCE = 3`,
`btree.rs:160` — and redistributes into at most
`MAX_NEW_SIBLING_PAGES_AFTER_BALANCE = 5` pages (`btree.rs:163`), updating the
parent's divider cells too.

**O(height)**: the balance propagates upward until `balance_root()`
(`btree.rs:5300`) splits the root, which is the only operation that makes the
tree taller. Bounded above by `BTCURSOR_MAX_DEPTH = 20` (`btree.rs:157`).

</details>

- [ ] You can state where a B-tree's space amplification comes from, and quote this repo's measured number for it.

<details>
<summary>Answer</summary>

Structurally: the free gap between the two regions on every page, plus the
freeblocks and fragments deletes leave behind — the price of in-place insertion.
Balancing across 3 siblings instead of splitting is the mitigation; it keeps
pages fuller than the 50% a naive split leaves.

Measured, from [FINDINGS.md](../../FINDINGS.md) row 1 (`./verify.sh 01`): the
same 108.0 MB of records occupies **48.4 MB** under fjall's LSM (space amp
**0.45×**, below 1.0 because sorted runs are LZ4'd) and **6,833.9 MB** under
redb's copy-on-write B-tree (**63.28×**) — a **140× spread**. redb's mechanism
is copy-on-write rather than page slack: per `topics/01-storage-engine-landscape/notes.md`,
each of 1080 batch commits copies every page on the path to the root under
random-order inserts. Same direction, harsher constant.

</details>

- [ ] You can distinguish turso's two journals and say which one implements the write-ahead rule.

<details>
<summary>Answer</summary>

The **subjournal** (`core/storage/subjournal.rs`, handle at `pager.rs:1637`,
written by `subjournal_page_if_required()` inside `add_dirty()`,
`pager.rs:3894`) holds page *pre-images* so a `SAVEPOINT` or a failed statement
can be undone **within** an open transaction. It is discarded when the
transaction ends.

The **WAL** (`WalFile`, `wal.rs:2798`) holds page *post-images* for crash
recovery. Frames are appended on the commit path — `cacheflush()`
(`pager.rs:3927`) → `flush_page_batch()` → `append_frames_vectored`
(`pager.rs:4186` → `wal.rs:4666`) — and folded back into the main database
file only by `checkpoint()` (`wal.rs:4126`).

The write-ahead rule is the WAL's: the new version of a page reaches the log
before the main file is touched at all. Reading `add_dirty()` as the write-ahead
site conflates the two: `add_dirty()` saves the *old* bytes for undo, and the
WAL receives the *new* bytes only at commit.

</details>

## References

**Code** (all at `tursodatabase/turso@a820079e8`, v0.9.0-pre.1 — this repo's
pin table entry; confirm with `python3 tools/pinned-source.py ref turso`)
- [turso](https://github.com/tursodatabase/turso) — `core/storage/btree.rs`
  (15,479 lines: header map `100–148`, `BTCURSOR_MAX_DEPTH:157`,
  `MAX_SIBLING_PAGES_TO_BALANCE:160`, `PinGuard:410`, `CursorContext:576`,
  `BTreeCursor:873`, `insert_into_page:3052`, `balance:3291`,
  `balance_non_root:3494`, `balance_root:5300`, `seek:6812`, `insert:6927`,
  `defragment_page:9842`, `insert_into_cell:10130`, `shift_pointers_left:10582`),
  `core/storage/wal.rs` (11,276 — `CheckpointMode:160`, `WalFile:2798`,
  `checkpoint:4126`, `append_frames_vectored:4666`), `core/storage/pager.rs`
  (7,271 — `Pager:1610`, `read_page:3648`, `add_dirty:3888`, `cacheflush:3927`),
  `core/storage/sqlite3_ondisk.rs` (2,748 — `read_btree_cell:847`),
  `core/storage/page_cache.rs` (1,964 — SIEVE, default 2000 pages at `:14`)

**This repo**
- [reading-fjall.md](reading-fjall.md) — the LSM protagonist opposite this one;
  read both before answering the topic's shootout predictions
- [reading-comer-btree.md](reading-comer-btree.md) — Comer 1979, where the
  fanout-and-height argument of Step 2 is proved rather than worked
- [topics/03-btree-internals/notes.md](../03-btree-internals/notes.md) — the
  same page arithmetic across other key/value shapes, and the measurement that
  height alone does not predict lookup latency
- [FINDINGS.md](../../FINDINGS.md) row 1 — LSM vs copy-on-write B-tree space
  amplification, 0.45× vs 63.28×; `./verify.sh 01`
