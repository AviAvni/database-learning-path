# Turso's B-tree: the canonical page engine, in Rust

turso re-implements the SQLite file format, so this is a reading of *the*
canonical page-oriented engine — slotted pages, cursor descent, multi-sibling
balance, pager + WAL — with Rust types instead of C macros. It is the B-tree
protagonist opposite fjall's LSM. These files are huge and move fast — expect
line-number drift, navigate by symbol name. Two files carry the topic:

| File | Size | Role |
|------|------|------|
| `core/storage/btree.rs` | ~13K lines | B-tree cursor, slotted pages, balance |
| `core/storage/pager.rs` | ~6.6K lines | page cache, dirty tracking, IO |
| `core/storage/wal.rs` | ~10K lines | WAL frames + checkpoint |
| `core/storage/page_cache.rs` | — | SIEVE-eviction page cache |

## 1. The slotted page — read this first

`core/storage/btree.rs:76–124` has an ASCII diagram of the page layout in the source
itself. The shape to internalize:

```
 ┌────────────┬──────────────────────┬────────────┬─────────────────┐
 │ header     │ cell pointer array   │ free space │ cell content    │
 │ 8/12 bytes │ u16 offsets, →grows  │            │ ←grows, actual  │
 │            │ rightward            │            │ records         │
 └────────────┴──────────────────────┴────────────┴─────────────────┘
   two regions grow toward each other; a "full" page = they meet
```

- Cell parsing: `read_btree_cell()` — `core/storage/sqlite3_ondisk.rs:816`.
- Delete fragmentation + fix: `defragment_page()` — `btree.rs:8422`; pointer-array
  maintenance via `copy_within` in `shift_pointers_left()` — `btree.rs:9067`.

This layout is why B-trees have space amplification: the free gap in the middle of
every page is the price of in-place insertion.

The insert, mechanically — two regions growing toward each other until they
meet:

```rust
fn insert_cell(page: &mut Page, idx: usize, cell: &[u8]) -> Result<(), Full> {
    let ptrs_end = page.header_len() + 2 * (page.ncells + 1); // ptr array grows →
    let content_start = page.content_start - cell.len();      // content grows ←
    if content_start < ptrs_end {
        return Err(Full);                       // regions met: time to balance/split
    }
    page.buf[content_start..content_start + cell.len()].copy_from_slice(cell);
    page.shift_pointers_right(idx);             // open slot idx — keys stay sorted
    page.write_u16(page.ptr_slot(idx), content_start as u16);
    page.ncells += 1;
    page.content_start = content_start;
    Ok(())
}
// delete = remove the u16 pointer, LEAVE the bytes → fragmentation,
// reclaimed only by defragment_page() — cheap deletes, deferred cleanup
```

## 2. The cursor — how every operation moves

Main types: `BTreeCursor` (`btree.rs:714`), `CursorContext` (`btree.rs:539`),
`PinGuard` (`btree.rs:375` — pins a page in the cache while the cursor points at it).

- Point lookup: `seek()` — `btree.rs:5681`. Trace one descent: root → interior cells
  binary-searched → child page id → pager fetch → leaf.
- Insert: `insert()` — `btree.rs:5779` → `insert_into_page()` — `btree.rs:2568`.

```mermaid
flowchart LR
    S["seek(key)<br/>btree.rs:5681"] --> D["descend: binary search cells,<br/>follow child ptr"]
    D --> PG["pager.read_page<br/>pager.rs:3240"]
    PG --> L["leaf: insert_into_page<br/>btree.rs:2568"]
    L -- page overflows --> B["balance<br/>btree.rs:2793"]
    B --> BNR["balance_non_root<br/>btree.rs:2995<br/>redistribute ≤3 siblings"]
```

## 3. Balance ≠ naive split

`balance_non_root()` (`btree.rs:2995`) rebalances up to **three sibling pages at
once**, redistributing cells — not the textbook "split one node in two". This is
SQLite's fill-factor trick: fewer, fuller pages ⇒ lower space amplification and
shallower trees. Compare with `balance_root()` (`btree.rs:4774`) which grows the tree
by one level.

## 4. Pager + WAL — where "in place" becomes crash-safe

- `Pager` struct: `pager.rs:1335`. Reads: `read_page()` — `pager.rs:3240` (cache
  first), `read_page_no_cache()` — `pager.rs:3185`.
- Dirty tracking: `add_dirty()` — `pager.rs:3412`. **Aha:** the page is journaled to
  the WAL *before* modification — that's the write-ahead in write-ahead logging,
  visible in code.
- WAL: `WalFile` (`wal.rs:2593`), frames appended in `append_frames_vectored()`
  (`wal.rs:708`), and `checkpoint()` (`wal.rs:3795`) copies frames back into the main
  DB file. So even the in-place family writes out-of-place *first*, then reconciles —
  keep this in mind for the README's "what is authoritative" framing.
- Page cache: `page_cache.rs:99` — SIEVE eviction, default 2000 pages. Buffer-pool
  preview (topic 6).

## Questions to answer

1. How many pages does a point lookup touch on a 1M-row table (page 4KB, ~50 cells
   interior fanout)? Which of those are realistically cached?
2. Why does `balance_non_root` prefer redistribution over splitting? What does it do
   to write amplification (3 dirty pages vs 2)?
3. During checkpoint, what blocks writers? (Read `checkpoint()` far enough to answer.)

## Done when

You can draw the slotted page from memory and explain how one insert can dirty 1 page
(common), 3 pages (balance), or O(height) pages (root split).

## References

**Code**
- [turso](https://github.com/tursodatabase/turso) —
  `core/storage/btree.rs` (~13K lines: cursor, slotted pages, balance),
  `core/storage/pager.rs`, `core/storage/wal.rs`,
  `core/storage/page_cache.rs`, `core/storage/sqlite3_ondisk.rs`
  (shallow clone at `~/repos/turso`; line numbers drift — navigate by
  symbol name)
