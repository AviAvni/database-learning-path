# LMDB: recovery is choosing a root pointer

LMDB is the anti-SQLite: no WAL, no page cache of its own, no free-space-
within-page — just copy-on-write pages over one big mmap, with crash recovery
reduced to picking the newer of two meta pages. This chapter builds that
design one step at a time — the mmap, copy-on-write, the two-meta commit
protocol, page reuse, and the reader table — then hands you the anchors to
read its single 12,846-line file as a *design*, skimming the code (2 h). It
is also the on-disk twin of the capstone reference's in-memory `cow_btree`,
which is exactly M3's comparison exercise.

## The problem in one sentence

A crash can strike between any two of the hundreds of page writes in a
commit, yet reopening an LMDB database afterwards costs exactly **two page
reads** — read both meta pages, keep the one with the larger valid
transaction id — with no log to replay and no repair step to run.

## The concepts, step by step

### Step 1 — one big mmap: the OS page cache IS the cache

A **page** is a fixed-size block (4 KB by default) — the unit of disk IO —
and **mmap** is the system call that makes a file addressable as ordinary
memory. LMDB maps the entire database file read-only into the process's
address space once, at open.

A read is then just a pointer dereference into the map: zero-copy, no buffer
pool, no page cache of LMDB's own — the OS page cache IS the cache. Writes do
*not* go through the map by default: they go through `pwrite` (an explicit
write-at-offset system call), or through a writable map only if you opt into
`MDB_WRITEMAP`.

Why it matters: topic 6's mmap-considered-harmful paper will argue mmap is
dangerous for *writes* (no control over write-back order) — note that LMDB's
default mode avoids exactly that by using pwrite + the meta protocol of
Step 3, not the writable map.

### Step 2 — copy-on-write: never overwrite a live page

**Copy-on-write** (COW) means a transaction never modifies a page that any
committed version of the tree can reach: the first write to a clean page
inside a transaction copies it to a *fresh page number*, and the parent's
child pointer is updated to point at the copy — which works because the
parent was touched first (the descent touches top-down).

```
 modify one key in a height-4 tree:

 before:  root₀ → int₀ → int₀' → leaf₀        (all shared, read-only)
 after:   root₁ → int₁ → int₁' → leaf₁        (4 NEW pages = 16 KB written)
          root₀ → int₀ → int₀' → leaf₀        (old path still intact)
```

Dirty pages are tracked in a sorted list of page IDs and flushed
sequentially at commit. The cost is right there in the diagram: changing one
key at tree height 4 writes four 4 KB pages — 16 KB — where a WAL engine
would append a ~100-byte log record. That is the price of Step 3's free
recovery.

Compare with the capstone reference's in-memory `cow_btree`: same path-copy,
but Arc refcounts replace the freelist, and "commit" is an atomic root swap
instead of a meta-page write. Write this comparison in notes — it's M3's core.

### Step 3 — the commit protocol: two meta pages, two fsyncs

A **meta page** is a page storing the root page number plus the id of the
transaction (**txnid**) that produced it — the entry point to one complete,
immutable version of the tree. LMDB keeps two, at file offsets 0 and 1, and
txn N writes meta `N % 2` — so the previously committed meta is *never*
overwritten.

Commit is: write the dirty COW pages → fsync (force to disk) → write the
meta page → fsync.

```
 crash timeline:                              recovery = nothing:
 write pages ─ fsync ─ write meta ─ fsync     open env, read both metas,
      ▲crash: old meta wins   ▲crash: old     pick larger valid txnid
       (new pages unreachable) meta wins      (mdb_env_pick_meta)
```

No WAL, no redo, no undo. Recovery is *choosing a root pointer*. The price is
paid elsewhere: every commit rewrites the whole root-to-leaf path (Step 2).

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

### Step 4 — readers never block writers

A **read transaction** is just a claim on one version of the tree: it picks
the newest meta, records that meta's txnid in a **reader slot** (one entry
per reader in a shared lock file), and from then on follows pointers through
pages that — by the COW rule — nobody will ever modify. That's the entire
read-txn setup: no locks on the data pages, ever.

Writes are the opposite extreme: a single writer mutex allows exactly one
write transaction at a time — LMDB doesn't pretend otherwise.

Why it matters: readers cost nothing to run and nothing to writers, which is
what makes LMDB's read path famous — but each reader's frozen txnid becomes a
liability in Step 5.

### Step 5 — page reuse: garbage collection as a database

COW keeps producing dead pages (every superseded path), so LMDB stores freed
page IDs in a **freelist database** — an internal B-tree (`FREE_DBI`) keyed
by the txn that freed them. The allocator reuses a freed page only if it was
freed by a txn *older than the oldest active reader* — found by scanning the
reader table of Step 4 for the smallest frozen txnid.

Consequence: a stalled reader pins EVERY page version since its snapshot —
the file grows without bound. (The infamous LMDB "long-lived reader" footgun;
the reference `cow_btree` has the same issue as Arc-pinned snapshots.)

Why it matters: LMDB has no compaction and no vacuum — this
freed-by-txn bookkeeping is the *only* thing standing between COW and
unbounded file growth, and one forgotten read txn defeats it.

### Step 6 — search and split: COW makes redistribution pointless

Search is the standard descent — walk from the root, binary-search each
page, follow the child pointer. Splits promote the median key upward,
cascading toward the root when a parent is also full.

This is deliberately *simpler* than SQLite's 3-sibling balance: COW means the
root-to-leaf path is being rewritten anyway, so there's no "redistribute in
place to avoid dirtying neighbors" incentive — and with no free-space
management inside pages (no freeblocks; pages are rebuilt append-style),
there's nothing to compact either.

## Where each step lives in the code

One file: `mdb.c`, 12,846 lines. Read it as a design, skim the code — the
`MDB_meta` comment and the reader table carry the whole model.

- **Step 1 — the mmap**: `mdb_env_map` — mdb.c:5040: one big `PROT_READ`
  mmap; writes go through `pwrite` (default) or a writable map with
  `MDB_WRITEMAP` (:5097).
- **Step 2 — COW**: `mdb_page_touch` — mdb.c:3015: first write to a clean
  page in a txn copies it to a fresh page number; the parent's child pointer
  is updated (parent was touched first — the descent touches top-down).
  Dirty pages tracked in `mt_u.dirty_list` (sorted ID list; insert at
  `mdb_page_dirty` :2670) — flushed sequentially at commit.
- **Step 3 — commit protocol**: two meta pages at file offsets 0 and 1; txn N
  writes meta `N % 2` (comment mdb.c:1356, `MDB_meta` struct :1358).
  `mdb_txn_commit` → `mdb_page_flush` (write dirty pages) → fsync →
  `mdb_env_write_meta` (mdb.c:4847, slot `txnid & 1` at :4863) → fsync.
  Recovery: `mdb_env_pick_meta`.
- **Step 4 — readers**: read txn setup in `mdb_txn_renew0` (mdb.c:3285) picks
  the newest meta (`mdb_env_pick_meta` :3296), records its txnid in a reader
  slot — `MDB_reader` struct :869, one slot per reader in a shared lock file,
  holding a frozen `mr_txnid`.
- **Step 5 — page reuse**: freelist database `FREE_DBI` (mdb.c:1345);
  `mdb_page_alloc` (mdb.c:2693) reuses freed pages only if freed by a txn
  older than the oldest active reader (`mdb_find_oldest` :2640 scans the
  reader table `mti_readers`).
- **Step 6 — search/split (skim)**: `mdb_page_search` :7535 →
  `mdb_node_search` :6689 (binary search per page); `mdb_page_split` :10662 —
  median promotion, cascading up.

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
