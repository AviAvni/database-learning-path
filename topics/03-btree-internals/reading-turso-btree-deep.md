# Inside the slotted page: freeblocks, overflow, balance

Topic 1's turso chapter traced the cursor/seek/insert surface; this one
descends into the page mechanics that surface glossed over — the freeblock
chain, the exact overflow-spill formulas, the resumable balance state
machines, varints, and the whole-page freelist. Budget: 2–3 h across
`core/storage/btree.rs`, `sqlite3_ondisk.rs`, and `pager.rs`.

## 1. Slotted page operations

- Header parsing: `btree.rs:76–124` (offsets in README §1).
- **Free-slot search**: `btree.rs:7592–7680` — `find_free_slot()` walks the
  freeblock chain (each freeblock: 2B next-ptr + 2B size, threaded through the
  content area). Minimum slot 4 bytes; smaller leftovers become the header's
  `fragmented_bytes` counter.
- **Defragment**: `btree.rs:8273–8444` — fast path when ≤2 freeblocks, slow path
  compacts everything. Question while reading: what triggers defrag, and why is
  it correct to move cells but never the pointer array?

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

## 2. Overflow — the exact SQLite formulas

- Thresholds: `btree.rs:9019–9042` —
  `max_local(index) = (usable−12)·64/255 − 23`, `max_local(table) = usable − 35`,
  `min_local = (usable−12)·32/255 − 23`.
- Spill rule: `sqlite3_ondisk.rs:2130–2148` — keep
  `min_local + (payload − min_local) % (usable − 4)` bytes local.
- Chain format: `sqlite3_ondisk.rs:951–961` — last 4 local bytes = next overflow
  page number (0 terminates).
- Why 64/255 and 32/255? Work it out: they bound local payload so a page always
  fits ≥4 cells — fanout survives fat keys.

## 3. Balance — the state machines

Turso's twist on SQLite: balancing is a **resumable state machine** (`IOResult`)
instead of synchronous recursion, because every page touch may yield for async IO.

- `balance_root()` — `btree.rs:4774–4852`: root overflow ⇒ copy root into a new
  child, root becomes interior pointing at it (tree grows up).
- `balance_non_root()` — `btree.rs:2995–4087`: the ≤3-sibling pool-and-
  redistribute. Sibling pick at :3305–3375 (left preferred, dividers pulled from
  parent into the pool); redistribution + new-sibling creation at :3430–3680.
- Trigger: insert overflows the page (`btree.rs:2903` — split path after
  `split_cell()` can't fit).

```
 balance_non_root, 2 siblings + overfull page:

 parent:      [ ... D1 ... D2 ... ]         D = divider cells
                 │      │      │
        [sib L]   [OVERFULL]   [sib R]
        └──────── pool: L + D1 + full + D2 + R ────────┘
                     redistribute evenly ⇒ 2–4 pages, new dividers up
```

## 4. Varints and the record format

- `read_varint` / `write_varint` — `sqlite3_ondisk.rs:1304–1336 / 1379–1421`:
  7 bits/byte big-endian, 9th byte carries a full 8 bits (max 9 bytes for u64).
- Record: header-size varint, then per-column **serial type** varints, then the
  values (`sqlite3_ondisk.rs:1101–1237`). Serial types encode type AND length in
  one number — schema-less pages.

## 5. Freelist (whole-page recycling)

- Trunk page: `sqlite3_ondisk.rs:89–93` — next-trunk u32, leaf-count u32, then
  leaf page numbers. Leaves are just free page IDs.
- `allocate_page()` — `pager.rs:5250–5448`: pop a leaf; if trunk empty, the trunk
  page ITSELF becomes the allocated page. `add_page_to_freelist()` —
  `pager.rs:5101–5145`.

## 6. Cell formats

Table interior `child u32 ∥ rowid varint`; table leaf `size ∥ rowid ∥ payload`;
index interior `child ∥ size ∥ payload`; index leaf `size ∥ payload`
(structs `sqlite3_ondisk.rs:775–812`, parsing :826–930). Note: **no prefix/suffix
truncation anywhere** — turso (like SQLite) stores full keys. That's your
experiment's opening.

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
