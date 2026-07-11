# Reading guide — postgres heapam & visibility (~2.5 h)

Local clone: [`~/repos/postgres`](https://github.com/postgres/postgres) (shallow). Files:
`src/backend/access/heap/heapam.c`, `heapam_visibility.c`,
`src/include/access/htup_details.h`, `src/include/utils/snapshot.h`,
`src/backend/utils/time/snapmgr.c`, `pruneheap.c`, `vacuumlazy.c`.

## 1. The tuple header IS the MVCC state (htup_details.h)

- :124–125 — `t_xmin` (inserting xact), `t_xmax` (deleting/locking xact).
- :161 — `t_ctid`: chain pointer to the newer version of this row. Read the
  big comment at :86–111: following t_ctid requires re-checking that the
  next tuple's xmin equals this tuple's xmax — the chain can be broken by
  vacuum, and t_ctid is overloaded for speculative insertion tokens.
- :204–208 — hint bits: `HEAP_XMIN_COMMITTED / INVALID`, `HEAP_XMAX_*`.
  These are a **cache of clog lookups** written back into the tuple itself
  by readers. First reader pays the clog probe, everyone after reads a bit.
  (Reader-writes-metadata: same trick as topic 6's usage counters.)

## 2. The snapshot (snapshot.h:138–165, snapmgr.c)

`SnapshotData`: `xmin` (:153 — everything below is decided), `xmax` (:154 —
everything at/above is invisible), `xip[]`/`xcnt` (:164–165 — in-progress
xids at snapshot time, invisible). `GetSnapshotData` (procarray.c:2114)
builds it by scanning the proc array — this scan was postgres's scalability
wall until the 2020 rework (note `GetSnapshotDataReuse` :2034: if nothing
committed since, reuse the old snapshot wholesale).

`XidInMVCCSnapshot` (snapmgr.c:1869) — the three-way check. Note it's a
binary search over xip for big arrays: snapshot cost scales with concurrent
write transactions.

## 3. The visibility function (heapam_visibility.c)

`HeapTupleSatisfiesMVCC` :939 — read the whole thing; it is the spec of SI:
1. xmin aborted → invisible; xmin in-progress and not me → invisible
2. xmin mine and cid < my command → visible (read-your-own-writes lives
   here, via CommandId — statement-level granularity inside a txn)
3. xmin committed but `XidInMVCCSnapshot(xmin)` → invisible (committed
   AFTER my snapshot — this is the line that makes it "snapshot")
4. then the same dance for xmax to decide "deleted yet, for me?"

- `HeapTupleSatisfiesUpdate` :511 — the OTHER visibility function, used by
  UPDATE/DELETE to find the latest version and report
  invisible/being-updated — this is where waiting-on-a-lock and the
  EvalPlanQual re-check originate.
- SetHintBits machinery :83–112 — even hint-bit writes are batched now
  (`SetHintBitsState`): amortize BufferBeginSetHintBits over a page. The
  amortize-and-batch pattern, again.
- Bonus: `HeapTupleSatisfiesMVCCBatch` :1690 — visibility checks
  vectorized over a page. Topic 11 foreshadowing.

## 4. Write paths (heapam.c)

- `heap_insert` :2004 — new tuple: xmin = my xid, xmax = 0. Note the WAL
  record is built AFTER the page change, inside the critical section —
  topic 5's reserve-then-copy in action.
- `heap_delete` :2717 — nothing moves; set xmax, clear some flags. The
  "delete" is a metadata write.
- `heap_update` :3201 — the long one. Skim for the shape:
  `HeapDetermineColumnsInfo` :3382 (which indexed columns changed?),
  `use_hot_update` :3233/:3981 (same-page + no indexed cols changed →
  :4029 mark HEAP_HOT_UPDATED, skip index inserts entirely).

```
 HOT chain (one page):        index entry ──► lp 1 (root, HOT_UPDATED)
                                                │ t_ctid
                                              lp 3 (HEAP_ONLY_TUPLE)
                                                │ t_ctid
                                              lp 5 (HEAP_ONLY_TUPLE) ◄ live
 readers walk the chain under the page latch; prune collapses it later.
```

## 5. The debt collectors

- `heap_page_prune_opt` (pruneheap.c:271) — opportunistic: any reader that
  notices a prunable page cleans it, no vacuum needed. HOT chains collapse
  to a redirect line pointer.
- `heap_vacuum_rel` (vacuumlazy.c:624) / `lazy_scan_heap` :1279 — the full
  pass: collect dead TIDs, delete index entries, then mark line pointers
  reusable. Two-phase because an index entry must never point at a reused
  slot.

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
