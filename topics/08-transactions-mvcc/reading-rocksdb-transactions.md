# OCC and 2PL, same skeleton: RocksDB transactions

RocksDB ships BOTH optimistic and pessimistic transactions over the same
base class — the cleanest side-by-side of the two concurrency schools
you'll find in production code. Before the code, this chapter builds the
shared machine step by step: what write buffering buys, how LSM sequence
numbers give snapshots for free, and how the two schools bolt onto the
same skeleton — differing only in WHEN conflicts are detected. Then it
hands you the file:line anchors to watch both.

**Every line number below was re-verified against
`facebook/rocksdb@7c80a5a`** (check with `python3 tools/pinned-source.py
ref rocksdb`). Everything is under `utilities/transactions/` unless the
path says otherwise.

## The problem in one sentence

Two transactions write the same key concurrently and only one outcome is
allowed to survive — you can pay for that conflict at *access time* (take
a lock, maybe wait) or at *commit time* (validate, maybe throw away all
the work), and which is cheaper flips with the conflict rate.

## The concepts, step by step

### Step 1 — write buffering: a transaction is a private diff

> **In:** the atomicity requirement — all my writes appear together or none
> do — and a storage engine that has no notion of "partially applied".
> **Out:** a private, indexed write batch that makes rollback free and
> uncommitted state invisible, plus a precise statement of what buffering
> does *not* solve.

Definitions used from here on:

- A **transaction** is a group of reads and writes that must appear to
  happen all-at-once or not at all.
- A **snapshot** is a reader's frozen definition of "what had committed
  when I started".
- **Isolation** is the guarantee about which other transactions' effects
  you can see.
- A **conflict** is two transactions whose effects cannot both be kept.

Atomicity is easiest if the database never sees partial state — so a
transaction buffers every write in a private, in-memory container and
applies the whole batch to the DB atomically at commit. RocksDB's container
is a `WriteBatchWithIndex`: an ordered batch of key/value operations plus a
small index over itself, so the transaction's own reads check the batch
first (read-your-own-writes), then fall through to the DB. The switch is
visible in the base class:

```cpp
// utilities/transactions/transaction_base.h — indexing_enabled_, 455-459
   455   // If true, future Put/PutEntity/Merge/Delete operations will be indexed in
   456   // the WriteBatchWithIndex. If false, future Put/PutEntity/Merge/Delete
   457   // operations will be inserted directly into the underlying WriteBatch and not
   458   // indexed in the WriteBatchWithIndex.
   459   bool indexing_enabled_;
```

The index is what costs; a transaction that never reads its own writes can
turn it off with `DisableIndexing` (`transaction_base.h:274`).

Rollback becomes free (drop the batch), and nothing a transaction does is
visible to anyone before commit. What buffering does NOT solve: two
transactions buffering writes to the same key, each validating against a
world that does not yet contain the other. That is the conflict problem,
Steps 3–5.

Why it matters: every design below inherits this. Neither school has to
worry about undoing partially-applied work, which is why both fit on one
base class.

### Step 2 — sequence numbers: the LSM gives MVCC for free

> **In:** an LSM tree (topic 4) that never overwrites in place, and a
> global write counter.
> **Out:** a snapshot that is *one integer*, and the one thing it costs.

Every write in RocksDB is stamped with a global, monotonically increasing
**sequence number** (seq), and because the LSM never overwrites in place,
old values remain present as entries with older seqs. So a snapshot is just
one integer: "the seq at the moment I began". A read at snapshot S returns,
for each key, the newest entry with seq ≤ S; entries newer than S are
skipped during the merge across memtable and SST files.

```
 key k in the LSM:      (k, seq=91) ── (k, seq=87) ── (k, seq=52)
 snapshot S = 88 reads: skip 91 ────► return 87
```

No version chains to maintain, no vacuum to schedule — old versions are
garbage-collected by compaction, and a registered snapshot pins them
against that GC. Postgres built visibility machinery
([`reading-postgres-heapam.md`](reading-postgres-heapam.md) Steps 2–5);
RocksDB inherited it from its storage layout.

Snapshots can be taken eagerly or lazily:

```cpp
// utilities/transactions/transaction_base.h — snapshot API, 264-272
   264   void SetSnapshot() override;
   265   void SetSnapshotOnNextOperation(
   266       std::shared_ptr<TransactionNotifier> notifier = nullptr) override;
   267
   268   void ClearSnapshot() override {
   269     snapshot_.reset();
   270     snapshot_needed_ = false;
   271     snapshot_notifier_ = nullptr;
   272   }
```

`snapshot_needed_` is declared at `transaction_base.h:463` with the comment
"SetSnapshotOnNextOperation() has been called and the snapshot has not yet
been reset" — line 270 above is where `ClearSnapshot` resets it. And note
`TransactionOptions::set_snapshot` defaults to **false**
(`include/rocksdb/utilities/transaction_db.h:299`): by default a
transaction has no snapshot at all until it takes one, and each read sees
the latest committed data.

Why it matters: the cost of this design is on the *other* side. A
long-lived snapshot pins every version newer than it against compaction, so
"a reader that never blocks a writer" is paid for in space, not in waiting.
The same bill postgres pays through vacuum, RocksDB pays through
compaction.

### Step 3 — the shared skeleton, and the one fork in the road

> **In:** Steps 1 and 2 — buffered writes and integer snapshots.
> **Out:** the base class both flavors *are*, and the single question that
> separates them.

Combine Steps 1–2 and you have the whole base class
(`transaction_base.{h,cc}`): reads go through the batch, then the DB at the
snapshot; writes buffer into the batch; commit applies the batch. Both
transaction flavors ARE this class, with one method overridden differently.

The only question left is write-write conflicts, and it has exactly two
answers, named by their attitude:

- **pessimistic** — assume conflicts happen: detect at *access time* by
  locking each key before buffering the write. The **2PL** school
  (two-phase locking: acquire locks in a growing phase, release only in a
  shrinking phase; **strict 2PL** releases them all at commit).
- **optimistic** — assume they do not: detect at *commit time* by checking
  whether any buffered key was overwritten since your snapshot. The **OCC**
  school (optimistic concurrency control, Kung & Robinson 1981:
  read/validate/write phases).

Why it matters: this is the whole taxonomy. Everything in Steps 4 and 5 is
a consequence of *when* the check runs, not *what* it checks.

### Step 4 — OCC: validate against the memtable, abort on doubt

> **In:** a committed-ready write batch and a snapshot seq.
> **Out:** the memtable-only validation, the two distinct failure statuses
> it can return, and the config knob that trades memory for abort rate.

At commit the optimistic flavor asks, for each key it wrote: "has this key
been written with a seq newer than my snapshot?" The dispatch:

```cpp
// utilities/transactions/optimistic_transaction.cc — Commit, 60-74
    60  Status OptimisticTransaction::Commit() {
    64    switch (txn_db_impl->GetValidatePolicy()) {
    65      case OccValidationPolicy::kValidateParallel:
    66        return CommitWithParallelValidate();
    67      case OccValidationPolicy::kValidateSerial:
    68        return CommitWithSerialValidate();
    71    }
```

- **Serial** (`optimistic_transaction.cc:76`) hands a callback to
  `WriteWithCallback` so validation runs inside RocksDB's single writer
  queue — correct by serialization, at the price of holding that queue.
- **Parallel** (`optimistic_transaction.cc:93`) takes striped bucket
  mutexes over the write set first, then validates, then writes. The
  comment at `:122-124` names the discipline that keeps it safe: "in a
  single txn, all bucket-locks are taken in ascending order. In this way,
  txns from different threads all obey this rule so that deadlock can be
  avoided."

Same serialize-vs-stripe trade as topic 5's group commit.

The validation itself goes `CheckTransactionForConflicts`
(`optimistic_transaction.cc:192`) → `TransactionUtil::CheckKeysForConflicts`
(`transaction_util.cc:154`) → `TransactionUtil::CheckKey`
(`transaction_util.cc:50`, called at `:188`), with `cache_only = true`:

```cpp
// utilities/transactions/optimistic_transaction.cc — CheckTransactionForConflicts, 192-201
   192  Status OptimisticTransaction::CheckTransactionForConflicts(DB* db) {
   195    // Since we are on the write thread and do not want to block other writers,
   196    // we will do a cache-only conflict check.  This can result in TryAgain
   197    // getting returned if there is not sufficient memtable history to check
   198    // for conflicts.
   199    return TransactionUtil::CheckKeysForConflicts(db_impl, *tracked_locks_,
   200                                                  true /* cache_only */);
   201  }
```

`cache_only` means **memtable only** — the LSM's in-RAM write buffer, which
holds the most recent writes. Never touch SSTs during validation; that is
the whole point. Which forces two different failure modes:

```cpp
// utilities/transactions/transaction_util.cc — CheckKey, 67-104 and 130-147 (elided)
    67    // Since it would be too slow to check the SST files, we will only use
    68    // the memtables to check whether there have been any recent writes
    69    // to this key after it was accessed in this transaction.  But if the
    70    // Memtables do not contain a long enough history, we must fail the
    71    // transaction.
    85    } else if (snap_seq < earliest_seq || min_uncommitted <= earliest_seq) {
    88      need_to_read_sst = true;
    90      if (cache_only) {
    91        // The age of this memtable is too new to use to check for recent
    92        // writes.
   104        result = Status::TryAgain(msg);
   130      } else if (found_record_for_key) {
   131        bool write_conflict = snap_checker == nullptr
   132                                  ? snap_seq < seq
   133                                  : !snap_checker->IsVisible(seq);
   145        if (write_conflict) {
   146          result = Status::Busy();
   147        }
```

Line 85 is "I cannot answer", line 132 is "the answer is yes". They return
**different statuses** and mean different things:

- `Status::Busy()` (line 146) — a real conflict. Someone committed over
  you. Retrying will probably lose again.
- `Status::TryAgain()` (line 104) — no information. The memtable was
  flushed and rotated since your snapshot, so its history no longer covers
  your era. Retrying immediately will very likely succeed.

Work it on numbers. Take a transaction that took its snapshot at seq
**1000** and commits later:

| Memtable `earliest_seq` | Key's latest seq | Line that fires | Result |
|---|---|---|---|
| 500 | none found | — | **OK** — no one touched the key |
| 500 | 1200 | 132: `1000 < 1200` | **`Busy`** — real write-write conflict |
| 500 | 900 | 132: `1000 < 900` false | **OK** — the write predates my snapshot |
| 5000 | *irrelevant* | 85: `1000 < 5000` | **`TryAgain`** — memtable too young |

Row 4 is the one to remember: **the key was never touched by anybody, and
the commit still fails.** Any transaction that outlives one memtable
rotation gets `TryAgain` on every key, deterministically. The error message
at `transaction_util.cc:99-101` even names the fix — "Increasing the value
of the `max_write_buffer_size_to_maintain` option could reduce the
frequency of this error" — which is the actual trade: **memory retained for
flushed memtables, bought against spurious aborts.**

Why it matters: OCC's validation is cheap because it refuses to do I/O, and
the price of that refusal is a false-abort rate that scales with
transaction *duration* rather than with contention. That is a different
failure axis from the one OCC is usually criticised on.

### Step 5 — 2PL: lock at access, hold to the end

> **In:** the same base class, with conflict detection moved to access time.
> **Out:** a striped lock table, the timeout that is the *default* defence,
> and the deadlock detector that is not on by default.

The pessimistic flavor pays up front: every write locks the key **before**
buffering it. The ordering is explicit:

```cpp
// utilities/transactions/pessimistic_transaction.cc — WriteCommittedTxn::Operate, 489-519 (elided)
   489  Status WriteCommittedTxn::Operate(ColumnFamilyHandle* column_family,
   490                                    const TKey& key, const bool do_validate,
   491                                    const bool assume_tracked,
   492                                    TOperation&& operation) {
   493    Status s;
   494    if constexpr (std::is_same_v<Slice, TKey>) {
   495      s = TryLock(column_family, key, /*read_only=*/false, /*exclusive=*/true,
   496                  do_validate, assume_tracked);
   503    if (!s.ok()) {
   504      return s;
   505    }
   519    return operation();
```

Lock at line 495, buffer at line 519, and bail out in between if the lock
fails. Every `Put`, `Delete`, `Merge` and `SingleDelete` in
`pessimistic_transaction.cc` routes through this one function.
`PessimisticTransaction::TryLock` itself is at `:1151`, and `GetForUpdate`
(`:164` and `:172`, both forwarding to `GetForUpdateImpl` at `:182`) is the
read-side equivalent — it takes an exclusive lock by default
(`include/rocksdb/utilities/transaction.h:411`, `bool exclusive = true`).

The locks live in a `PointLockManager`
(`lock/point/point_lock_manager.h:110`), which is a **striped** hash table:

```cpp
// utilities/transactions/lock/point/point_lock_manager.cc — LockMap::GetStripe, 441-443
   441  size_t LockMap::GetStripe(const std::string& key) const {
   443    return FastRange64(GetSliceNPHash64(key), num_stripes_);
```

Each stripe has its own mutex and condition variable, so ordinary lock
traffic does not serialize on one latch. The default is
`num_stripes = 16` (`include/rocksdb/utilities/transaction_db.h:171`) — a
number worth remembering, because it is small. Question 2 makes you work
the pathology.

**Three defaults that change the story**, all from
`include/rocksdb/utilities/transaction_db.h`:

| Option | Line | Default | Consequence |
|---|---|---|---|
| `transaction_lock_timeout` | 181 | **1000 ms** | a blocked lock gives up after 1 s and returns `TimedOut` |
| `deadlock_detect` | 304 | **false** | **deadlock detection is off unless you ask for it** |
| `deadlock_detect_depth` | 351 | 50 | how far the wait-for graph is walked when it *is* on |

So the out-of-the-box defence against deadlock is **the timeout**, not the
detector. Turn detection on and `AcquireWithTimeout`
(`point_lock_manager.h:208`, definition around
`point_lock_manager.cc:630-640`) calls `IncrementWaiters`
(`point_lock_manager.cc:840`), which does a bounded breadth-first walk of
the wait-for graph:

```cpp
// utilities/transactions/lock/point/point_lock_manager.cc — IncrementWaiters, 870-888 and 921-932 (elided)
   870    for (int tail = 0, head = 0; head < txn->GetDeadlockDetectDepth(); head++) {
   883      if (tail == head) {
   884        return false;                       // ran out of edges: no deadlock
   887      auto next = queue_values[head];
   888      if (next == id) {                     // found a cycle back to me
   921    // Wait cycle too big, just assume deadlock.
   930    dlock_buffer_.AddNewPath(DeadlockPath(deadlock_time, true));
   932    return true;
```

Read lines 921–932 carefully: if the search exhausts
`deadlock_detect_depth` without closing a cycle, RocksDB **declares a
deadlock anyway**. That is a deliberate false positive — a wait chain 51
transactions long aborts somebody even when no cycle exists. Detected
cycles are recorded in a bounded ring (`DeadlockInfoBufferTempl`,
`point_lock_manager.h:31-101`) readable via `GetDeadlockInfoBuffer`
(`point_lock_manager.h:156`).

Locks release only after commit's write lands (`Commit` —
`pessimistic_transaction.cc:681`); that is **strict 2PL**, which is what
makes commit order equal lock order.

Two limits to carry away:

- **Keys are locked, not predicates.** A lock manager over an
  order-preserving keyspace with no gap or range locks cannot stop
  phantoms — contrast InnoDB's next-key locks.
- **Neither flavor tracks a general read set.** Reads are only validated
  for keys you explicitly `GetForUpdate` (which calls `ValidateSnapshot`,
  `pessimistic_transaction.cc:1290`, which in turn calls
  `TransactionUtil::CheckKeyForConflicts`, `transaction_util.cc:20`). A
  plain `Get` is invisible to conflict detection, so write skew is entirely
  possible — question 3, and see
  [`reading-ssi-postgres.md`](reading-ssi-postgres.md) for what tracking
  reads properly costs.

Why it matters: "pessimistic transactions have deadlock detection" is the
kind of true-sounding claim that is false by default. Read the option
defaults before you reason about a system's failure modes.

### Step 6 — the design plane: where each school pays

> **In:** two implementations of one interface.
> **Out:** the 2×2 that explains why both exist, worked on one concrete
> pair of transactions.

Both flavors, one 2×2 — the cost just moves between the columns:

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

Run the same pair of transactions through both. T1 and T2 each increment
key `k`, currently value 5 written at seq 900. Both take a snapshot at seq
**1000**.

**Under OCC:**

1. T1 reads k = 5, buffers `put(k, 6)`. T2 does exactly the same. Neither
   has touched anything shared.
2. T1 commits. Validation: no key written after seq 1000 → OK. The batch
   lands at seq **1001**.
3. T2 commits. `CheckKey(k, snap_seq = 1000)` →
   `GetLatestSequenceForKey` returns 1001 → `transaction_util.cc:132`,
   `1000 < 1001` → **`Status::Busy()`**.
4. T2 has done 100% of its work and keeps 0% of it. Its retry starts from
   scratch.

**Under 2PL:**

1. T1 calls `Operate` → `TryLock(k)` at `pessimistic_transaction.cc:495` →
   acquired.
2. T2 calls `Operate` → `TryLock(k)` → the stripe mutex for `k` is
   contended; T2 blocks in `AcquireWithTimeout` for up to
   `transaction_lock_timeout` = **1000 ms**.
3. T1 commits at `:681` and releases. T2 wakes, acquires, re-reads k = 6,
   writes 7.
4. T2 kept 100% of its work and paid for it in wall-clock time.

Same outcome, opposite currency. Low contention: OCC wins — zero lock
traffic and validation almost always passes. High contention: OCC burns
whole transactions per abort while 2PL merely queues. That crossover is the
entire "which school" decision, and RocksDB exposing both behind one API is
the admission that no single answer exists.

Why it matters: notice a third case the 2×2 does not have a column for —
Step 4's `TryAgain`, which is an OCC abort caused by *neither* contention
*nor* the workload, only by elapsed time. Real systems fail in ways the
textbook taxonomy has no cell for.

## Where each step lives in the code

All anchors verified at `facebook/rocksdb@7c80a5a`; ~1.5 h.

| Step | File | Lines | What |
|---|---|---|---|
| 1, 3 | `utilities/transactions/transaction_base.h` | 274-278, 455-459 | `WriteBatchWithIndex`, `indexing_enabled_`, `DisableIndexing` |
| 2 | `utilities/transactions/transaction_base.h` | 264-272, 463 | `SetSnapshot`, `SetSnapshotOnNextOperation`, `ClearSnapshot`, `snapshot_needed_` |
| 2, 5 | `include/rocksdb/utilities/transaction_db.h` | 171, 181, 299, 304, 325, 341, 351 | `num_stripes`, `transaction_lock_timeout`, `set_snapshot`, `deadlock_detect`, `lock_timeout`, `deadlock_timeout_us`, `deadlock_detect_depth` |
| 4 | `utilities/transactions/optimistic_transaction.cc` | 60-74, 76-91, 93-134, 192-201 | `Commit` dispatch; serial vs parallel validate; ascending bucket-lock order at 122-124; `CheckTransactionForConflicts` |
| 4 | `utilities/transactions/optimistic_transaction.h` | 67, 76, 78 | the three declarations |
| 4 | `utilities/transactions/transaction_util.cc` | 50-152, 154, 188 | `CheckKey` — the `TryAgain` branch at 85-104, the `Busy` branch at 130-147; `CheckKeysForConflicts` |
| 5 | `utilities/transactions/pessimistic_transaction.cc` | 164, 172, 182, 489-519, 681, 1151, 1290 | `GetForUpdate`, `Operate` (lock-then-buffer), `Commit`, `TryLock`, `ValidateSnapshot` |
| 5 | `utilities/transactions/transaction_util.cc` | 20 | `CheckKeyForConflicts` — the *pessimistic* read-validation entry point |
| 5 | `utilities/transactions/lock/point/point_lock_manager.h` | 26-28, 31-101, 110, 156, 169-198, 208, 218 | `LockInfo`/`LockMap`/`LockMapStripe`, deadlock ring buffer, `PointLockManager`, the mandated lock order, `AcquireWithTimeout`, `IncrementWaiters` |
| 5 | `utilities/transactions/lock/point/point_lock_manager.cc` | 195, 326-358, 441-443, 630-640, 840-932 | stripe struct, `LockMap`, `GetStripe`, the deadlock-detect call site, `IncrementWaiters` |
| 5 | `include/rocksdb/utilities/transaction.h` | 402-406, 408-412 | the documented status codes; `GetForUpdate`'s `exclusive = true` default |

## Questions for notes.md

1. Why can OCC validation use the memtable only? What property of LSM seq
   numbers makes "not in memtable ⇒ too old to conflict" sound — and what
   exactly does `transaction_util.cc:85` add to that sentence? Then price
   the `TryAgain` retry loop: if a transaction takes longer than one
   memtable rotation, what is its steady-state commit rate?
2. The lock manager stripes by key hash into `num_stripes = 16` stripes
   (`point_lock_manager.cc:441-443`,
   `include/rocksdb/utilities/transaction_db.h:171`). Work the pathology for
   a graph workload where every transaction touches the same super-node's
   adjacency entries: how many of the 16 stripe mutexes carry the load, and
   does raising `num_stripes` help?
3. Neither flavor tracks a general read set — only keys passed to
   `GetForUpdate` are validated (`pessimistic_transaction.cc:1290`). So what
   isolation level do you actually get, and construct the write skew that
   sneaks through. Compare with `reading-ssi-postgres.md`.
4. `deadlock_detect` defaults to false
   (`include/rocksdb/utilities/transaction_db.h:304`). What happens to a
   genuine two-transaction deadlock under the defaults, how long does it
   take, and which status does the loser get? Now read
   `point_lock_manager.cc:921-932` — what does the detector do that the
   timeout does not, and what false positive does it introduce?
5. FalkorDB angle: GRAPH.QUERY writes are single-threaded today (one
   writer). If M8 keeps single-writer, which of these two machineries do you
   still need? (Hint: none for write-write; what about read-write validation
   for serializable reads?)

## Takeaway

One base class, one fork. Buffer the writes and snapshot with an integer,
then choose when to look for conflicts: at access time with a striped lock
table, a 1-second timeout and an optional bounded deadlock walk; or at
commit time with a memtable-only check that returns `Busy` for real
conflicts and `TryAgain` when it simply cannot see far enough back. The
first pays in waiting, the second in wasted work, and the second has a
third failure mode — elapsed time — that the textbook 2×2 has no cell for.

## Connections to this topic's experiment

The exercise in `experiments/src/mvcc.rs` is the OCC half of this guide.
`first_committer_wins_on_write_write_conflict` is Step 6's OCC trace with
`CommitError::WriteConflict` standing in for `Status::Busy()`, and
`Mode::Serializable` adds the read-set tracking that RocksDB deliberately
does not do (`CommitError::ReadConflict`,
`experiments/src/mvcc.rs:105`).

The topic's *measured* lane is something else, and worth stating exactly.
It benchmarks a single global `Mutex<HashMap>` — 4 threads × 50 000
transactions × 4 operations — and on an Apple M3 Pro (measured 2026-07-28,
recorded in [`notes.md`](notes.md)) it returns:

| Workload | Keys | mutex txn/s |
|---|---|---|
| read-heavy 95/5 | 10 000 | 623 454 |
| write-heavy 50/50 | 10 000 | 594 264 |
| write-heavy 50/50 | 64 (hot) | 676 691 |

Those numbers are **flat**: about 12% spread across workloads that differ
completely in read/write mix and key skew. That is the negative result
recorded in [`FINDINGS.md`](../../FINDINGS.md) row 8 — the global mutex had
already serialized everything, so nothing about the workload could reach
the measurement. Note especially that the *hot-key* row is the fastest one;
under any of the schemes in this guide, 64 hot keys is the case that hurts,
and the mutex does not notice, because it has no per-key structure to
contend on.

This repo has **not** measured OCC or 2PL beating that mutex — the
`mvcc txn/s` and `aborts` columns in `notes.md` are `stub`. When you fill
them, the `aborts` column is the one that matters: Step 6 says OCC's cost
is wasted work, and a throughput number alone cannot tell you whether you
bought it.

## Done when

Answer each before unfolding it.

- [ ] Explain, with file:line, where each school pays its conflict cost,
      and why both can share one write-buffering base class.

<details><summary>Answer</summary>

**2PL pays at access time**: `WriteCommittedTxn::Operate`
(`pessimistic_transaction.cc:489`) calls `TryLock` at line **495** and only
reaches the buffering call, `operation()`, at line **519**. **OCC pays at
commit time**: `OptimisticTransaction::Commit`
(`optimistic_transaction.cc:60`) dispatches to a validate-then-write path,
and the check is `CheckTransactionForConflicts`
(`optimistic_transaction.cc:192`) → `CheckKey` (`transaction_util.cc:50`).

They share a base class because Step 1's write buffering makes conflict
detection *orthogonal* to atomicity: nothing a transaction does is visible
until the batch is applied, so it makes no difference to reads, rollback or
durability whether the conflict was caught at line 495 or at line 146 of
`transaction_util.cc`.

</details>

- [ ] A transaction snapshots at seq 1000, writes one key nobody else
      touches, and commits after a memtable flush leaves `earliest_seq =
      5000`. What happens, and why is it not a bug?

<details><summary>Answer</summary>

It fails with **`Status::TryAgain`**, at `transaction_util.cc:85`:
`snap_seq (1000) < earliest_seq (5000)` → `need_to_read_sst = true`, and
since `cache_only` is true the error at line 104 is returned instead. It is
not a bug because OCC's validation *refuses to read SSTs* — the comment at
`:67-71` says checking SST files "would be too slow", so when the memtable
history no longer covers the snapshot's era, RocksDB cannot prove the
absence of a conflict and refuses conservatively. The message at
`:99-101` names the tuning knob: `max_write_buffer_size_to_maintain`,
trading memory for a lower spurious-abort rate. Note that `TryAgain` is a
*different status* from `Busy` (line 146) precisely so callers can tell
"retry me, I'll probably win" from "someone beat you".

</details>

- [ ] Does RocksDB's pessimistic transaction detect deadlocks? Answer with
      the option and its default.

<details><summary>Answer</summary>

**Not by default.** `TransactionOptions::deadlock_detect = false`
(`include/rocksdb/utilities/transaction_db.h:304`). Out of the box, a real
deadlock is resolved by `transaction_lock_timeout` (line 181, default
**1000 ms**), and the loser gets `Status::TimedOut`. If you enable
detection, `AcquireWithTimeout` calls `IncrementWaiters`
(`point_lock_manager.cc:840`), a breadth-first walk of the wait-for graph
bounded by `deadlock_detect_depth` (line 351, default 50), and the loser
gets `Status::Busy` with `SubCode::kDeadlock`
(`point_lock_manager.cc:634`). The detector also has a deliberate false
positive: `point_lock_manager.cc:921-932`, "Wait cycle too big, just assume
deadlock" — a wait chain longer than the depth limit aborts a transaction
even with no cycle present.

</details>

- [ ] The lock table has 16 stripes by default. What breaks when every
      transaction in the workload touches the same key?

<details><summary>Answer</summary>

`LockMap::GetStripe` (`point_lock_manager.cc:441-443`) is
`FastRange64(GetSliceNPHash64(key), num_stripes_)` — a pure function of the
*key*. One key hashes to exactly one stripe, so all lock traffic serializes
on that stripe's single mutex and the other 15 of 16 stripes
(`transaction_db.h:171`) sit idle. **Raising `num_stripes` does not help at
all**, because striping only spreads *distinct* keys; it is a fix for lock
table contention, not for key contention. The only fixes are workload-side:
shard the hot key, or stop taking a lock on it (which is what an
optimistic scheme does — and then it pays in aborts instead).

</details>

- [ ] Trace T1 and T2 both incrementing key `k` (value 5 at seq 900, both
      snapshotting at seq 1000) under each school. Which one throws work
      away?

<details><summary>Answer</summary>

**OCC**: both buffer `put(k, 6)` with no interaction. T1 commits at seq
1001. T2's `CheckKey` hits `transaction_util.cc:132` — `1000 < 1001` →
`Status::Busy()`. T2 discards everything it did and starts over. **2PL**:
T1's `TryLock` at `pessimistic_transaction.cc:495` succeeds; T2's blocks in
`AcquireWithTimeout` for up to 1000 ms; T1 commits at `:681` and releases;
T2 wakes, re-reads k = 6, writes 7 and keeps all of its work. **OCC throws
work away; 2PL spends wall-clock time.** Same correct outcome, opposite
currency — which is exactly why RocksDB ships both.

</details>

- [ ] State this topic's measured result and what it does not show.

<details><summary>Answer</summary>

A global `Mutex<HashMap>` baseline measures 623 454 / 594 264 / 676 691
txn/s for read-heavy 10K-key, write-heavy 10K-key and write-heavy
64-hot-key workloads (Apple M3 Pro, 2026-07-28; `notes.md`). It is
**flat** — that is the negative finding in
[`FINDINGS.md`](../../FINDINGS.md) row 8: the mutex had already serialized
everything, so workload shape could not influence throughput. The hot-key
row being *fastest* is the tell. It does **not** show OCC or 2PL beating a
mutex; those columns are `stub`, and any such comparison would need the
`aborts` column too, since OCC's cost is wasted work rather than lower
throughput per completed transaction.

</details>

## References

**Code** — all anchors verified at
[`facebook/rocksdb@7c80a5a`](https://github.com/facebook/rocksdb)

| File | Lines | What |
|---|---|---|
| `utilities/transactions/transaction_base.h` | 264-278, 455-463 | shared skeleton: snapshots, indexing |
| `utilities/transactions/optimistic_transaction.h` | 67, 76, 78 | OCC declarations |
| `utilities/transactions/optimistic_transaction.cc` | 60-134, 192-201 | commit dispatch, both validate modes, conflict check |
| `utilities/transactions/transaction_util.cc` | 20, 50-152, 154, 188 | `CheckKey` and its two entry points |
| `utilities/transactions/pessimistic_transaction.cc` | 164-200, 489-519, 681, 1151, 1290 | lock-then-buffer, commit, `TryLock`, `ValidateSnapshot` |
| `utilities/transactions/lock/point/point_lock_manager.h` | 26-101, 110-218 | lock table types, deadlock buffer, API |
| `utilities/transactions/lock/point/point_lock_manager.cc` | 195, 326-358, 441-443, 630-640, 840-932 | striping and deadlock detection |
| `include/rocksdb/utilities/transaction_db.h` | 171, 181, 296-351 | the defaults that decide behaviour |
| `include/rocksdb/utilities/transaction.h` | 402-412 | documented status codes |

**Papers**

- Kung & Robinson — *On Optimistic Methods for Concurrency Control*
  (TODS 1981) — the OCC school's founding paper (read/validate/write
  phases); RocksDB's `OptimisticTransaction` is that structure, with the
  validate phase restricted to the memtable.

**In this repo**

| Where | What |
|---|---|
| [`notes.md`](notes.md) | the measured mutex baseline and the `stub` columns |
| [`FINDINGS.md`](../../FINDINGS.md) row 8 | flat ~600k txn/s, because the mutex already serialized everything |
| `experiments/src/mvcc.rs:105` | `commit()` — where first-committer-wins and read validation go |
| [`reading-postgres-heapam.md`](reading-postgres-heapam.md) | visibility built by hand, instead of inherited from the storage layout |
| [`reading-ssi-postgres.md`](reading-ssi-postgres.md) | what it costs to track read sets properly |
