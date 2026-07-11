# Reading guide — RocksDB transactions: OCC and 2PL, same skeleton (~1.5 h)

Local clone: [`~/repos/rocksdb`](https://github.com/facebook/rocksdb), dir `utilities/transactions/`. RocksDB ships
BOTH optimistic and pessimistic transactions over the same base class —
the cleanest side-by-side of the two schools you'll find in production code.

Everything hangs off sequence numbers: a RocksDB snapshot is just "the seq
at begin". MVCC comes free from the LSM (topic 4): old versions already
exist as older entries; a snapshot pins them against compaction GC.

## 1. Shared skeleton — transaction_base.{h,cc}

- Writes are buffered in a private `WriteBatchWithIndex` — nothing touches
  the DB until commit. Reads go through the batch first (read-your-own-
  writes), then the DB at the snapshot.
- `SetSnapshot` (transaction_base.h:264) — note `snapshot_needed_` :270:
  snapshots can be taken lazily on first read.
- So: both flavors are "buffer writes, decide at commit". The ONLY
  difference is when conflicts are detected.

## 2. Optimistic — optimistic_transaction.{h,cc}

- `CheckTransactionForConflicts` (h:67) → `TransactionUtil::CheckKeyForConflicts`
  (transaction_util.cc:20) → `CheckKey` :50.
- The validation trick: for each written key, ask "has this key been
  written at a seq > my snapshot seq?" — answered from the **memtable
  only** (`cache_only`): if the memtable's earliest seq is newer than my
  snapshot, RocksDB *can't know* and conservatively aborts (`TryAgain`).
  Cheap validation, bought with spurious aborts on long transactions.
- Commit modes (optimistic_transaction.cc:66):
  `CommitWithSerialValidate` (h:76) — validate inside the single writer
  queue (correct by serialization); `CommitWithParallelValidate` (h:78) —
  take striped locks on the write set, validate, then write. Same
  structure as your topic-5 group commit vs per-commit trade.

## 3. Pessimistic — pessimistic_transaction.{h,cc} + lock/point/

- Every Put/Delete calls `TryLock` (pessimistic_transaction.cc:1151) BEFORE
  buffering (:495 — lock first, then base-class write). GetForUpdate takes
  a read→write lock (:1121).
- `PointLockManager` (lock/point/point_lock_manager.h:110): striped hash of
  key → `LockInfo` (h:26), `AcquireWithTimeout` :208, deadlock detection
  via wait-for graph (h:216) with a bounded deadlock-info buffer (h:75–93).
- `Commit` :681 — locks released only after the write lands: strict 2PL.
- Note what's locked: **keys, not rows** — a lock manager over an
  order-preserving keyspace can't stop phantoms (no gap/range locks here;
  contrast innodb). Snapshot validation (`SetSnapshotOnNextOperation`) is
  layered on top for repeatable reads.

## 4. The design plane

```
 conflict cost paid:   at access time          at commit time
                     ┌────────────────┐      ┌──────────────────┐
 pessimistic 2PL     │ TryLock every  │      │ nothing to check │
                     │ write (+ wait) │      │ (locks held)     │
                     └────────────────┘      └──────────────────┘
 optimistic OCC      │ nothing        │      │ CheckKey per     │
                     │ (buffer only)  │      │ written key      │
                     └────────────────┘      └──────────────────┘
 contention ↑ ⇒ OCC abort rate ↑ (wasted work); 2PL queue depth ↑ (waits).
```

## Questions for notes.md

1. Why can OCC validation use the memtable only? What property of LSM seq
   numbers makes "not in memtable ⇒ too old to conflict... unless memtable
   is too young" sound — and what does the TryAgain path cost a retry loop?
2. The pessimistic lock manager stripes by key hash. What's the pathology
   for a graph workload where every txn touches the same super-node's
   adjacency entries?
3. Neither flavor validates READ sets by default — so what isolation do
   you actually get, and where does write skew sneak in?
4. FalkorDB angle: GRAPH.QUERY writes are single-threaded today (one
   writer). If M8 keeps single-writer, which of these two machineries do
   you still need? (Hint: none for w-w; what about r-w validation for
   serializable reads?)

## Done when

You can explain, with file:line, where each school pays its conflict cost,
and why both can share one write-buffering base class.
