# Postgres MVCC: every tuple carries its own visibility

Postgres stores versions IN the table: each heap tuple's header names its
creator and deleter, and visibility is a pure function of (tuple header,
snapshot) — no lock manager consulted on the read path. Before you open
heapam, this chapter builds that machine step by step — the versioned
tuple, the header fields, the snapshot, the visibility function that is
the spec of snapshot isolation, the write paths, and the debt collectors —
then hands you the file:line anchors to watch each piece work.

## The problem in one sentence

Let hundreds of readers scan a table while writers update it, with no
reader ever taking a lock — postgres's answer is to never overwrite a row
and to make "can I see this version?" a pure function two integers wide,
paying for it with dead versions that a vacuum process must collect later.

## The concepts, step by step

### Step 1 — versions live in the table itself

In postgres, an UPDATE never modifies the row in place: it inserts a
complete new copy of the row (a new **version**) elsewhere in the table's
storage (the **heap** — the file of 8 KB pages holding the actual rows),
and marks the old copy as superseded. A DELETE just marks. Nothing is
physically removed at delete/update time; old versions sit in the heap
next to live ones until a cleanup pass reclaims them.

Consequence: at any instant the heap contains *several versions of the
same logical row*, and every reader must decide, per version, "is this the
one I should see?" — using only information stored in the version itself
plus the reader's own context. That decision procedure is the rest of this
chapter.

### Step 2 — the tuple header: creator, deleter, and a chain pointer

Each row version (a **tuple**) carries a header naming who made it and who
killed it. The identifiers are **xids** (transaction ids — a global 32-bit
counter assigned to each writing transaction in start order):

- `t_xmin` — the xid of the transaction that *inserted* this version.
- `t_xmax` — the xid of the transaction that *deleted or superseded* it
  (0 = still live).
- `t_ctid` — a pointer to the *newer* version of the same row, forming a
  version chain through the heap.

So the tuple's whole MVCC life is three fields: born at `t_xmin`, died at
`t_xmax`, successor at `t_ctid`. An UPDATE = insert new version + set old
tuple's `t_xmax` + link `t_ctid`. A caveat the source shouts about
(htup_details.h:86–111): the chain can be broken by cleanup, so following
`t_ctid` requires re-checking that the next tuple's `xmin` equals this
tuple's `xmax` — and `t_ctid` is overloaded for speculative insertion
tokens. Chains are walked defensively.

### Step 3 — hint bits: caching "did that transaction commit?"

An xid alone doesn't say whether its transaction committed or aborted —
that lives in the **clog** (commit log — a global bitmap keyed by xid,
two bits per transaction). Checking clog per tuple per read would add a
(cached, but real) lookup to every visibility test. So the first reader
that pays the clog probe writes the answer *back into the tuple header* as
**hint bits**: `HEAP_XMIN_COMMITTED`, `HEAP_XMIN_INVALID`, and the xmax
equivalents. Every later reader tests one bit and skips clog entirely.

The costs, both real: reads now dirty pages (a SELECT can generate write
IO — question 2), and the bits are only hints, so they can be set lazily
and are batched per page by the `SetHintBits` machinery. Reader-writes-
metadata is the same trick as topic 6's buffer usage counters.

### Step 4 — the snapshot: three numbers that freeze time

A **snapshot** is the reader's definition of "now": a compact description
of exactly which transactions had committed when the snapshot was taken.
It is three parts — `xmin`, `xmax`, and `xip[]`:

- every xid `< xmin` — finished before I started: decided (visible if
  committed);
- every xid `>= xmax` — started after me: **invisible**, unconditionally;
- `xip[]` — the list of xids in progress at snapshot time (between xmin
  and xmax but not yet finished): **invisible**, even if they commit later.

Building one means scanning the shared array of running backends
(`GetSnapshotData`) — this scan was postgres's multicore scalability wall
until the 2020 rework, and the in-snapshot test (`XidInMVCCSnapshot`) is a
binary search over `xip[]`, so snapshot cost scales with the number of
concurrent write transactions (question 3 compares Hekaton's one-counter
answer).

### Step 5 — the visibility function: the spec of snapshot isolation

Now combine Steps 2–4: a version is visible iff its creator is visible to
my snapshot AND its deleter (if any) is not. `HeapTupleSatisfiesMVCC` is
that sentence plus a decade of engineering; its logical skeleton:

1. xmin aborted → invisible; xmin in-progress and not me → invisible
2. xmin mine and cid < my command → visible (read-your-own-writes lives
   here, via CommandId — statement-level granularity inside a txn)
3. xmin committed but `XidInMVCCSnapshot(xmin)` → invisible (committed
   AFTER my snapshot — this is the line that makes it "snapshot")
4. then the same dance for xmax to decide "deleted yet, for me?"

The same function, minus the hint-bit engineering:

```rust
fn satisfies_mvcc(t: &Tuple, s: &Snapshot) -> bool {
    // "visible xid" = committed AND not still in flight at snapshot time
    let vis = |xid: Xid| committed(xid) && !in_snapshot(xid, s);
    if t.xmin == s.my_xid {
        if t.cmin >= s.cur_cid { return false; } // later command in my own txn
    } else if !vis(t.xmin) {
        return false;                            // creator invisible to me
    }
    match t.xmax {
        None => true,                            // never deleted
        Some(x) if x == s.my_xid => t.cmax >= s.cur_cid,
        Some(x) => !vis(x),                      // deleter invisible ⇒ row lives
    }
}

fn in_snapshot(xid: Xid, s: &Snapshot) -> bool { // committed AFTER my snapshot?
    xid >= s.xmax || (xid >= s.xmin && s.xip.binary_search(&xid).is_ok())
}
```

Note what's absent: no locks, no waiting, no consulting other backends —
a pure function of two arguments. That purity is the entire read-side
scalability story. (There is a second visibility function,
`HeapTupleSatisfiesUpdate`, used by writers to find the latest version and
report "being updated by someone else" — that's where waiting and the
EvalPlanQual re-check originate.)

### Step 6 — the write paths, and the HOT shortcut

With the machinery above, the write paths are almost anticlimactic:

- **insert** — new tuple: xmin = my xid, xmax = 0. (The WAL record is
  built AFTER the page change, inside the critical section — topic 5's
  reserve-then-copy in action.)
- **delete** — nothing moves; set xmax, clear some flags. A "delete" is a
  metadata write.
- **update** — insert + mark + link, per Step 1… with one big exception.

The exception is **HOT** (heap-only tuple) updates: if no indexed column
changed and the new version fits on the *same page*, skip all index
updates. The index keeps pointing at the chain head; readers walk `t_ctid`
within the page to reach the live version:

```
 HOT chain (one page):        index entry ──► lp 1 (root, HOT_UPDATED)
                                                │ t_ctid
                                              lp 3 (HEAP_ONLY_TUPLE)
                                                │ t_ctid
                                              lp 5 (HEAP_ONLY_TUPLE) ◄ live
 readers walk the chain under the page latch; prune collapses it later.
```

Why it matters: a table with 5 indexes turns every non-HOT update into 6
inserts (heap + 5 index entries); HOT makes it 1. This is why
"UPDATE = INSERT+DELETE" is only *half* true in postgres.

### Step 7 — the debt collectors: prune and vacuum

Every update and delete leaves a dead version in the heap — MVCC's rent.
Two collectors, opportunistic and thorough:

- **page pruning** (`heap_page_prune_opt`) — any *reader* that notices a
  page with prunable garbage cleans that one page in passing: dead
  versions removed, HOT chains collapsed to a redirect line pointer. No
  vacuum needed for the common case.
- **vacuum** (`heap_vacuum_rel` / `lazy_scan_heap`) — the full pass:
  collect dead tuple ids, delete the index entries pointing at them, and
  only THEN mark the heap line pointers reusable. Two-phase because an
  index entry must never point at a reused slot (question 1 makes you
  construct the corruption).

The famous failure mode: xids are 32-bit, so the counter wraps; vacuum
also "freezes" old tuples to keep xid comparisons valid — fall too far
behind and the database refuses writes. That is the operational price of
storing versions in the table.

## Where each step lives in the code

Read `HeapTupleSatisfiesMVCC` in full first — it is the spec of SI — and
the :86–111 comment in htup_details.h before chasing t_ctid. ~2.5 h total.

- **Step 2 — the header** (`src/include/access/htup_details.h`):
  `t_xmin`/`t_xmax` :124–125; `t_ctid` :161; the chain-walking caveats in
  the big comment :86–111.
- **Step 3 — hint bits**: flag definitions htup_details.h:204–208
  (`HEAP_XMIN_COMMITTED / INVALID`, `HEAP_XMAX_*`); SetHintBits machinery
  heapam_visibility.c:83–112 — note `SetHintBitsState`: even hint-bit
  writes are batched now, amortizing BufferBeginSetHintBits over a page.
  The amortize-and-batch pattern, again.
- **Step 4 — the snapshot**: `SnapshotData` — snapshot.h:138–165 (`xmin`
  :153, `xmax` :154, `xip[]`/`xcnt` :164–165); `GetSnapshotData` —
  procarray.c:2114 (and `GetSnapshotDataReuse` :2034 — if nothing
  committed since, reuse the old snapshot wholesale, the 2020 scalability
  fix); `XidInMVCCSnapshot` — snapmgr.c:1869, the three-way check.
- **Step 5 — visibility** (`heapam_visibility.c`):
  `HeapTupleSatisfiesMVCC` :939 — read the whole thing;
  `HeapTupleSatisfiesUpdate` :511 — the writer-side function. Bonus:
  `HeapTupleSatisfiesMVCCBatch` :1690 — visibility vectorized over a page,
  topic 11 foreshadowing.
- **Step 6 — write paths** (`heapam.c`): `heap_insert` :2004;
  `heap_delete` :2717; `heap_update` :3201 — the long one, skim for the
  shape: `HeapDetermineColumnsInfo` :3382 (which indexed columns
  changed?), `use_hot_update` :3233/:3981, and :4029 where
  HEAP_HOT_UPDATED is set and index inserts are skipped entirely.
- **Step 7 — collectors**: `heap_page_prune_opt` — pruneheap.c:271;
  `heap_vacuum_rel` — vacuumlazy.c:624 and `lazy_scan_heap` :1279.

## Questions for notes.md

1. Why must the index-entry deletion happen BEFORE line pointers are
   recycled? Construct the corruption if the order flipped.
2. Hint bits make reads write. Which topic-6 lesson does that complicate
   (think: checksums, dirty buffers from SELECTs)?
3. A snapshot with 10K concurrent writers makes XidInMVCCSnapshot a binary
   search over 10K xids per tuple. What does Hekaton's timestamp design
   pay instead?
4. FalkorDB angle: postgres stores versions IN the table (old versions
   inflate the heap). For a graph whose "table" is a sparse matrix, where
   would old versions live — and is that closer to append-only (postgres)
   or delta (Hekaton per Wu/Pavlo taxonomy)?

## Done when

You can execute `HeapTupleSatisfiesMVCC` on paper for: (a) my own insert,
(b) a commit that landed after my snapshot, (c) a HOT-updated row mid-chain.

## References

**Code**
- [postgres](https://github.com/postgres/postgres) —
  `src/backend/access/heap/heapam.c`, `heapam_visibility.c`,
  `src/include/access/htup_details.h`, `src/include/utils/snapshot.h`,
  `src/backend/utils/time/snapmgr.c`, `pruneheap.c`, `vacuumlazy.c`;
  ~2.5 h — read `HeapTupleSatisfiesMVCC` in full first, it is the spec
  of SI, and the :86–111 comment in htup_details.h before chasing t_ctid
