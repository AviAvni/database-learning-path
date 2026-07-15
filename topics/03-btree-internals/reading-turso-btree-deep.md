# Inside the slotted page: freeblocks, overflow, balance

Topic 1's turso chapter traced the cursor/seek/insert surface; this one
descends into the page mechanics that surface glossed over — the freeblock
chain, the exact overflow-spill formulas, the resumable balance state
machines, varints, and the whole-page freelist. This chapter builds each
mechanism step by step, then maps every step to its anchors. Budget: 2–3 h
across `core/storage/btree.rs`, `sqlite3_ondisk.rs`, and `pager.rs`.

## The problem in one sentence

A **4,096-byte** page must absorb variable-length rows — up to and including
a 100 KB payload — plus arbitrary deletes and re-inserts, forever, without
the on-disk format ever needing a special case: freeblocks, overflow chains,
and 3-sibling balancing are the entire toolkit.

## The concepts, step by step

### Step 1 — the freeblock chain: free space is a linked list in the dead bytes

When a cell (one row's on-disk container) is deleted, its bytes become a
**freeblock** — a hole inside the page, threaded into a linked list *through
the dead space itself*: each freeblock's first 4 bytes hold a 2-byte pointer
to the next freeblock and its own 2-byte size. Allocation for a new cell is
first-fit down that chain.

Two rules keep the bookkeeping in 4 bytes: a freeblock must be at least
**4 bytes** (smaller leftovers can't hold the next-pointer + size), and those
too-small scraps are instead counted in the page header's
`fragmented_bytes` counter.

The freeblock walk, distilled — first-fit through a linked list threaded
through the dead space itself:

```rust
fn find_free_slot(page: &mut Page, need: usize) -> Option<u16> {
    let mut prev = FREEBLOCK_HEAD;           // header bytes 1–2
    let mut off = page.first_freeblock();
    while off != 0 {
        let (next, size) = page.freeblock_at(off);   // 2B next-ptr + 2B size
        if size as usize >= need {
            let rest = size as usize - need;
            if rest < 4 {                    // leftover can't hold a freeblock:
                page.unlink(prev, next);     //   take it all, book the scraps
                page.add_fragmented(rest as u8);      //   (header cap: 60)
                return Some(off);
            }
            page.set_size(off, rest as u16); // carve the tail, keep the block
            return Some(off + rest as u16);
        }
        prev = off; off = next;
    }
    None    // nothing fits: allocate from the middle gap, or defragment
}
```

Why it matters: deletes cost 2 bytes of pointer-array edit plus threading one
hole — the cleanup is deferred to Step 2, and paid only when space actually
runs short.

### Step 2 — defragmentation: compact the holes when first-fit fails

**Defragmentation** rewrites all live cells contiguously at the page's end,
zeroing the freeblock chain and the fragment counter — turning many scattered
holes into one usable gap. Turso has a fast path when there are ≤2
freeblocks and a slow path that compacts everything.

Question to hold while reading: what triggers defrag, and why is it correct
to move cells but never the pointer array? (The pointer array *is* the sorted
index — cells are only ever reached through it, so rewriting cell offsets in
place is invisible to every reader of the page.)

Why it matters: defrag is O(page size) — the fee for Step 1's cheap deletes,
charged rarely and all at once.

### Step 3 — varints and the record format

A **varint** is an integer encoded in 1–9 bytes, 7 bits per byte, big-endian
(most significant group first), high bit meaning "more bytes follow" — the
9th byte, if reached, carries a full 8 bits (max 9 bytes for a u64). Small
numbers (short lengths, low rowids) cost 1 byte instead of 8.

On top of varints sits the **record** — the encoding of one row: a
header-size varint, then one **serial type** varint per column (a single
number encoding both the column's type AND its byte length), then the raw
values. Serial types are why pages are schema-less: any page can be decoded
with no schema in hand.

Why it matters: every cell begins with varints, and every balance or
overflow computation below starts by decoding them.

### Step 4 — the four cell formats

There are two b-trees (table trees keyed by rowid, index trees keyed by
column values) times two page levels (interior, leaf), giving exactly four
cell layouts:

- table interior: `child u32 ∥ rowid varint` — no payload at all;
- table leaf: `size ∥ rowid ∥ payload`;
- index interior: `child ∥ size ∥ payload` — the full key rides along;
- index leaf: `size ∥ payload`.

Note: **no prefix/suffix truncation anywhere** — turso (like SQLite) stores
full keys. That's your experiment's opening. Why it matters: table interior
cells are ~13 bytes, so table trees have enormous fanout; index interior
cells carry whole keys, so fat keys directly cost fanout (question 1 below).

### Step 5 — overflow: the exact spill formulas

When a payload is too big for its page, the excess **overflows** into a
chain of dedicated overflow pages, each holding a 4-byte next-page number
followed by payload bytes (0 terminates the chain); only a prefix of the
payload stays "local" in the cell. The thresholds are exact formulas:

- `max_local(index) = (usable−12)·64/255 − 23`,
  `max_local(table) = usable − 35`,
  `min_local = (usable−12)·32/255 − 23`;
- spill rule: keep `min_local + (payload − min_local) % (usable − 4)` bytes
  local — sized so the *last* overflow page is exactly full;
- chain format: the last 4 local bytes = next overflow page number
  (0 terminates).

Why 64/255 and 32/255? Work it out: they bound local payload so a page
always fits **≥4 cells** — fanout survives fat keys. That's the whole point:
overflow trades extra page reads for one value against tree height for
everyone.

### Step 6 — balance as a resumable state machine

**Balancing** is what runs when an insert overflows a page: pool the cells
of the overfull page, up to two siblings, and the divider cells between them
(the parent's separator entries), then redistribute evenly. Turso's twist on
SQLite: balancing is a **resumable state machine** (`IOResult`) instead of
synchronous recursion, because every page touch may yield for async IO.

- `balance_root()`: root overflow ⇒ copy root into a new child, root becomes
  interior pointing at it (tree grows up).
- `balance_non_root()`: the ≤3-sibling pool-and-redistribute — sibling pick
  prefers the left neighbor, dividers are pulled from the parent into the
  pool, and redistribution may mint one new sibling.

```
 balance_non_root, 2 siblings + overfull page:

 parent:      [ ... D1 ... D2 ... ]         D = divider cells
                 │      │      │
        [sib L]   [OVERFULL]   [sib R]
        └──────── pool: L + D1 + full + D2 + R ────────┘
                     redistribute evenly ⇒ 2–4 pages, new dividers up
```

Why it matters: pooling ≤3 siblings bounds the work per balance while
leaving pages fuller than a naive half/half split — and the state-machine
shape forces every intermediate state to be resumable (question 3 below asks
what invariant that requires).

### Step 7 — the freelist: recycling whole pages

Separately from Step 1's *within-page* holes, whole pages freed by drops and
balances go on the **freelist** — a chain of **trunk pages**, each holding a
next-trunk u32, a leaf-count u32, and then an array of free page numbers
(the "leaves" are just free page IDs, never read).

Allocation pops a leaf number off the current trunk; when a trunk runs
empty, the trunk page ITSELF becomes the allocated page — the list consumes
its own skeleton. Freeing appends to the trunk (or starts a new one).

Why it matters: the file never shrinks on delete; it recycles. This is the
page-granularity mirror of the freeblock story, and the structure your
capstone's pager will need too.

## Where each step lives in the code

Line numbers drift — navigate by symbol name.

- **Step 1 — slotted page + freeblocks**: header parsing `btree.rs:76–124`
  (offsets in README §1); `find_free_slot()` — `btree.rs:7592–7680` walks the
  freeblock chain (each freeblock: 2B next-ptr + 2B size, threaded through
  the content area). Minimum slot 4 bytes; smaller leftovers become the
  header's `fragmented_bytes` counter.
- **Step 2 — defragment**: `btree.rs:8273–8444` — fast path when ≤2
  freeblocks, slow path compacts everything.
- **Step 3 — varints + records**: `read_varint` / `write_varint` —
  `sqlite3_ondisk.rs:1304–1336 / 1379–1421`; record decoding (header-size
  varint, per-column serial-type varints, then values) —
  `sqlite3_ondisk.rs:1101–1237`.
- **Step 4 — cell formats**: structs `sqlite3_ondisk.rs:775–812`, parsing
  :826–930.
- **Step 5 — overflow**: thresholds `btree.rs:9019–9042`; spill rule
  `sqlite3_ondisk.rs:2130–2148`; chain format `sqlite3_ondisk.rs:951–961`.
- **Step 6 — balance**: `balance_root()` — `btree.rs:4774–4852`;
  `balance_non_root()` — `btree.rs:2995–4087`, sibling pick at :3305–3375
  (left preferred, dividers pulled from parent into the pool),
  redistribution + new-sibling creation at :3430–3680. Trigger: insert
  overflows the page (`btree.rs:2903` — split path after `split_cell()`
  can't fit).
- **Step 7 — freelist**: trunk page format `sqlite3_ondisk.rs:89–93`;
  `allocate_page()` — `pager.rs:5250–5448`; `add_page_to_freelist()` —
  `pager.rs:5101–5145`.

## Questions to answer in notes.md

1. Why do table-btree interior cells store only rowids (no payload) while
   index-btree interior cells carry the full key? What does that do to fanout?
2. The freeblock minimum is 4 bytes and `fragmented_bytes` caps at 60 in SQLite —
   what goes wrong without defragmentation? When must `allocateSpace` defrag even
   though total free space suffices?
3. Turso's balance yields mid-operation for IO. What invariant must hold at every
   yield point so a concurrent reader (or a crash) never sees a broken tree?
   (Hint: WAL — pages aren't durable until commit; in-memory the cursor holds refs.)

## Done when

You can write the byte layout of a table-leaf page containing two cells and one
freeblock, from memory, and explain what balance_non_root pools and why ≤3.

## References

**Code**
- [turso](https://github.com/tursodatabase/turso) —
  `core/storage/btree.rs` (slotted-page ops, balance state machines),
  `core/storage/sqlite3_ondisk.rs` (overflow, varints, cell formats),
  `core/storage/pager.rs` (freelist) — local clone at `~/repos/turso`;
  line numbers drift, navigate by symbol name. Extends topic 1's
  [reading-turso-btree.md](../01-storage-engine-landscape/reading-turso-btree.md)
