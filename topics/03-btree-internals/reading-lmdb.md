# LMDB: recovery is choosing a root pointer

LMDB is the anti-SQLite: no WAL, no page cache of its own, no free-space-
within-page — just copy-on-write pages over one big mmap, with crash recovery
reduced to picking the newer of two meta pages. This chapter reads its single
12,846-line file as a *design*, skimming the code (2 h); it is also the
on-disk twin of the capstone reference's in-memory `cow_btree`, which is
exactly M3's comparison exercise.

## 1. The commit protocol — the whole design in one sequence

- Two meta pages at file offsets 0 and 1; txn N writes meta `N % 2`
  (comment mdb.c:1356, `MDB_meta` struct :1358).
- `mdb_txn_commit` → `mdb_page_flush` (write dirty pages) → fsync →
  `mdb_env_write_meta` (mdb.c:4847, slot `txnid & 1` at :4863) → fsync.

```
 crash timeline:                              recovery = nothing:
 write pages ─ fsync ─ write meta ─ fsync     open env, read both metas,
      ▲crash: old meta wins   ▲crash: old     pick larger valid txnid
       (new pages unreachable) meta wins      (mdb_env_pick_meta)
```

No WAL, no redo, no undo. Recovery is *choosing a root pointer*. The price is
paid elsewhere: every commit rewrites the whole root-to-leaf path.

The protocol fits on a napkin:

```rust
fn commit(env: &mut Env, txn: Txn) -> Result<()> {
    write_pages(&txn.dirty)?;               // COW pages at NEW page numbers —
    fsync(env.fd)?;                         //   durable before any root sees them
    let meta = Meta { txnid: txn.id, root: txn.new_root };
    write_meta_slot(env, (txn.id % 2) as usize, &meta)?;  // toggle: never
    fsync(env.fd)                                         //   overwrite live meta
}

fn open(env: &Env) -> Root {
    let (m0, m1) = read_both_metas(env);
    pick_valid_with_larger_txnid(m0, m1).root   // recovery IS this line —
}                                               // a crash anywhere above just
                                                // means the old meta still wins
```

## 2. COW mechanics

- `mdb_page_touch` — mdb.c:3015: first write to a clean page in a txn copies it
  to a fresh page number; the parent's child pointer is updated (parent was
  touched first — the descent touches top-down).
- Dirty pages tracked in `mt_u.dirty_list` (sorted ID list; insert at
  mdb_page_dirty :2670) — flushed sequentially at commit.
- Compare with the capstone reference's in-memory `cow_btree`: same path-copy,
  but Arc refcounts replace the freelist, and "commit" is an atomic root swap
  instead of a meta-page write. Write this comparison in notes — it's M3's core.

## 3. Page reuse — GC as a database

- Freed page IDs go into a **freelist database** (`FREE_DBI`, mdb.c:1345) keyed
  by the txn that freed them.
- `mdb_page_alloc` (mdb.c:2693) reuses freed pages only if freed by a txn older
  than the **oldest active reader** (`mdb_find_oldest` :2640 scans the reader
  table `mti_readers`, MDB_reader struct :869 — one slot per reader in a shared
  lock file, holding a frozen `mr_txnid`).
- Consequence: a stalled reader pins EVERY page version since its snapshot —
  the file grows without bound. (The infamous LMDB "long-lived reader" footgun;
  the reference cow_btree has the same issue as Arc-pinned snapshots.)

## 4. Readers never block writers

- Read txn: `mdb_txn_renew0` (mdb.c:3285) picks the newest meta
  (`mdb_env_pick_meta` :3296), records its txnid in a reader slot — that's the
  entire read-txn setup. No locks on the data pages, ever.
- Single writer at a time (writer mutex) — LMDB doesn't pretend otherwise.

## 5. The mmap

- `mdb_env_map` — mdb.c:5040: one big `PROT_READ` mmap; writes go through
  `pwrite` (default) or a writable map with `MDB_WRITEMAP` (:5097).
- Reads = pointer dereference into the map — zero-copy, no buffer pool, the OS
  page cache IS the cache. Topic 6's mmap-considered-harmful paper will argue
  why this is dangerous for *writes* (no control over write-back order) — note
  that LMDB's default mode avoids exactly that by using pwrite + the meta
  protocol, not the writable map.

## 6. Search/split (skim)

- `mdb_page_search` :7535 → `mdb_node_search` :6689 (binary search per page).
- `mdb_page_split` :10662 — median promotion, cascading up. Simpler than
  SQLite's 3-sibling balance: COW means the path is being rewritten anyway, so
  there's no "redistribute in place to avoid dirtying neighbors" incentive.

## Questions to answer in notes.md

1. Why does LMDB's split not bother with SQLite-style sibling redistribution?
   (COW already dirties the path; also no freeblocks — append-style page builds.)
2. Double meta + fsync ordering: which of the two fsyncs could you drop, under
   what hardware assumption, and what breaks on consumer SSDs?
3. Price a 1-key commit at tree height 4, 4KB pages: bytes written for LMDB vs
   a WAL engine (≈ record + fsync). When does LMDB's model win anyway?
   (Read-heavy, batch-committed writes.)

## Done when

You can narrate a crash at any point in the commit sequence and say which root
survives, and you can state the reader-pins-pages problem and its capstone twin.

## References

**Code**
- [LMDB](https://github.com/LMDB/lmdb) `libraries/liblmdb/mdb.c`
  (12,846 lines, one file; local clone at `~/repos/lmdb`) — read it as a
  design, skim the code; the `MDB_meta` comment (:1356) and the reader
  table (`MDB_reader` :869) carry the whole model
