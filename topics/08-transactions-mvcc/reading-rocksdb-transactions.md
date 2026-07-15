# OCC and 2PL, same skeleton: RocksDB transactions

RocksDB ships BOTH optimistic and pessimistic transactions over the same
base class — the cleanest side-by-side of the two concurrency schools
you'll find in production code. Before the code, this chapter builds the
shared machine step by step: what write buffering buys, how LSM sequence
numbers give snapshots for free, and how the two schools bolt onto the
same skeleton — differing only in WHEN conflicts are detected. Then it
hands you the file:line anchors to watch both.

## The problem in one sentence

Two transactions write the same key concurrently and only one outcome is
allowed to survive — you can pay for that conflict at *access time* (take
a lock, maybe wait) or at *commit time* (validate, maybe throw away all
the work), and which is cheaper flips with the conflict rate.

## The concepts, step by step

### Step 1 — write buffering: a transaction is a private diff

Atomicity ("all my writes appear together, or none do") is easiest if the
database never sees partial state — so a transaction buffers every write
in a private, in-memory container and applies the whole batch to the DB
atomically at commit. RocksDB's container is a `WriteBatchWithIndex`: an
ordered batch of key/value operations plus a small index over itself, so
the transaction's own reads check the batch first (read-your-own-writes),
then fall through to the DB.

Rollback becomes free (drop the batch), and nothing a transaction does is
visible to anyone before commit. What buffering does NOT solve: two
transactions buffering writes to the same key, each validating against a
world that doesn't yet contain the other. That is the conflict problem,
Steps 3–5.

### Step 2 — sequence numbers: the LSM gives MVCC for free

Every write in RocksDB is stamped with a global, monotonically increasing
**sequence number** (seq), and — because the LSM (topic 4) never
overwrites in place — old values remain present as entries with older
seqs. So a **snapshot** is just one integer: "the seq at the moment I
began". A read at snapshot S returns, for each key, the newest entry with
seq ≤ S; entries newer than S are simply skipped during the merge across
memtable and SST files.

```
 key k in the LSM:      (k, seq=91) ── (k, seq=87) ── (k, seq=52)
 snapshot S = 88 reads: skip 91 ────► return 87
```

No version chains to maintain, no vacuum to schedule — old versions are
garbage-collected by compaction, and a registered snapshot pins them
against that GC. Postgres built visibility machinery; RocksDB inherited
it from its storage layout. One cost to notice: a long-lived snapshot
blocks compaction from dropping anything newer than it.

### Step 3 — the shared skeleton, and the one fork in the road

Combine Steps 1–2 and you have the whole base class
(`transaction_base.{h,cc}`): reads go through the batch, then the DB at
the snapshot; writes buffer into the batch; commit applies the batch. Both
transaction flavors ARE this class. Note `SetSnapshot`
(transaction_base.h:264) and `snapshot_needed_` :270 — snapshots can be
taken lazily on first read.

The only question left is write-write conflicts, and it has exactly two
answers, named by their attitude:

- **pessimistic** — assume conflicts happen: detect at *access time* by
  locking each key before buffering the write (the 2PL school — two-phase
  locking: acquire locks as you go, release only at the end).
- **optimistic** — assume they don't: detect at *commit time* by checking
  whether any buffered key was overwritten since your snapshot (the OCC
  school — optimistic concurrency control, Kung & Robinson 1981:
  read/validate/write phases).

### Step 4 — OCC: validate against the memtable, abort on doubt

At commit, the optimistic flavor asks, for each key in its write batch:
"has this key been written with a seq newer than my snapshot?" The trick
is *where* it asks: the **memtable only** (the LSM's in-RAM write buffer,
which holds the most recent writes). If the answer is there, great —
conflict or no conflict. If the memtable's earliest seq is newer than the
snapshot, the memtable is too young to remember the snapshot's era, and
RocksDB *can't know* — so it aborts conservatively with `TryAgain`:

```rust
// CheckKey, conceptually: "was this key written after my snapshot?"
fn validate(&self, snap_seq: u64) -> Result<(), Abort> {
    for key in self.write_batch.keys() {
        if self.db.memtable_min_seq() > snap_seq {
            return Err(Abort::TryAgain);  // memtable too young to answer —
        }                                 // abort conservatively, retry
        if self.db.latest_seq(key, /*memtable_only=*/ true) > snap_seq {
            return Err(Abort::Busy);      // someone committed over me
        }
    }
    Ok(())                                // batch → DB, atomically
}
```

Cheap validation — no disk reads, no lock table — bought with spurious
aborts on long transactions (outlive one memtable flush and every commit
is a `TryAgain`). Two commit modes exist: validate inside the single
writer queue (correct by serialization) or take striped locks on the write
set, validate, then write in parallel — the same serialize-vs-stripe trade
as topic 5's group commit.

### Step 5 — 2PL: lock at access, hold to the end

The pessimistic flavor pays up front: every Put/Delete calls `TryLock` on
the key BEFORE buffering it, and `GetForUpdate` takes a read→write lock.
The locks live in a `PointLockManager` — a striped hash table (many
independently-latched buckets, so lock traffic itself doesn't serialize)
mapping key → `LockInfo`, with acquisition timeouts and **deadlock
detection** via a wait-for graph (T1 waits for T2 waits for T1 ⇒ cycle ⇒
abort somebody; with locks-held-till-end, deadlock is possible and must be
detected, not just avoided). Locks release only after commit's write
lands — that's **strict 2PL**, which is what makes the commit order equal
the lock order.

Note what's locked: **keys, not predicates** — a lock manager over an
order-preserving keyspace can't stop phantoms (no gap/range locks here;
contrast innodb). And neither flavor validates READ sets by default —
snapshot validation (`SetSnapshotOnNextOperation`) is layered on top for
repeatable reads; write skew is entirely possible (question 3).

### Step 6 — the design plane: when each school wins

Both flavors, one 2×2 — the cost just moves between the two columns:

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

Low contention: OCC wins — zero lock traffic, validation almost always
passes. High contention: OCC burns whole transactions per abort while 2PL
merely queues; wasted work vs waiting. That crossover is the entire
"which school" decision, and RocksDB exposing both behind one API is the
admission that no single answer exists.

## Where each step lives in the code

All under `utilities/transactions/`; ~1.5 h.

- **Steps 1+3 — the skeleton** (`transaction_base.{h,cc}`): the private
  `WriteBatchWithIndex` and read-through-batch logic; `SetSnapshot` —
  transaction_base.h:264; lazy `snapshot_needed_` :270.
- **Step 4 — OCC** (`optimistic_transaction.{h,cc}`,
  `transaction_util.cc`): `CheckTransactionForConflicts` (h:67) →
  `TransactionUtil::CheckKeyForConflicts` (transaction_util.cc:20) →
  `CheckKey` :50 — the memtable-only validation with the `TryAgain`
  conservative abort. Commit modes: optimistic_transaction.cc:66;
  `CommitWithSerialValidate` (h:76) vs `CommitWithParallelValidate`
  (h:78).
- **Step 5 — 2PL** (`pessimistic_transaction.{h,cc}`, `lock/point/`):
  `TryLock` — pessimistic_transaction.cc:1151, called before buffering
  (:495 — lock first, then base-class write); `GetForUpdate` read→write
  upgrade :1121. `PointLockManager` — lock/point/point_lock_manager.h:110:
  striped hash of key → `LockInfo` (h:26), `AcquireWithTimeout` :208,
  wait-for-graph deadlock detection (h:216) with a bounded deadlock-info
  buffer (h:75–93). `Commit` :681 — locks released only after the write
  lands: strict 2PL.

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

## References

**Code**
- [rocksdb](https://github.com/facebook/rocksdb) —
  `utilities/transactions/`: `transaction_base.{h,cc}` (shared skeleton),
  `optimistic_transaction.{h,cc}` + `transaction_util.cc` (OCC),
  `pessimistic_transaction.{h,cc}` + `lock/point/point_lock_manager.h`
  (2PL); ~1.5 h

**Papers**
- Kung & Robinson — "On Optimistic Methods for Concurrency Control"
  (TODS 1981) — the OCC school's founding paper (read/validate/write
  phases); RocksDB's OptimisticTransaction is this, verbatim
