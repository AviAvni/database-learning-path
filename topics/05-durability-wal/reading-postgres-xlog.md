# Reading postgres `xlog.c` — guided skim (2 h)

Repo: `~/repos/postgres`. Files: `src/backend/access/transam/xlog.c` (10,196
lines — do NOT read linearly), `xloginsert.c`, `xlogrecovery.c`,
`src/include/access/xlogrecord.h`. You're here for five mechanisms.

## 1. The record — xlogrecord.h:41–53

`XLogRecord`: `xl_tot_len`, `xl_xid`, **`xl_prev`** (back-pointer — the log is
a backward-linked list; recovery validates forward, xl_prev catches missequenced
segments), `xl_crc`. Block references (:103) attach page references; full-page
images ride in `XLogRecordBlockImageHeader` (:141) — note `hole_offset`: the
free space in the middle of a page is elided from the FPI.

## 2. Insertion — the scalability trick

- `ReserveXLogInsertLocation` — xlog.c:1149–1193: a **spinlock held for ~3
  arithmetic ops** (:1172–1180) hands out byte ranges in the log. Reservation
  is serial (tiny), copying is parallel.
- `CopyXLogRecordToWAL` — xlog.c:1266: copy into the reserved slice of WAL
  buffers under one of `NUM_XLOGINSERT_LOCKS = 8` (xlog.c:157) insertion locks.
- This is Aether's insight shipped: separate *sequencing* from *copying*.
  Compare topic-2's incremental rehash — same move, amortize/parallelize the
  heavy part, keep the critical section O(1).

## 3. Group commit — XLogFlush, xlog.c:2800–2891

The heart: after acquiring the write lock, **recheck `LogwrtResult.Flush`**
(:2885) — another backend probably flushed past your LSN while you waited;
return without an fsync. `commit_delay`/`commit_siblings` (:2901–2906) add an
optional pre-flush sleep to grow the batch. Your experiment reimplements this.

## 4. Full-page writes — xloginsert.c:621–700

`XLogRecordAssemble`: `needs_backup = (page_lsn <= RedoRecPtr)` (:694) — first
modification of a page after a checkpoint logs the whole page (hole elided).
Cost: WAL volume spikes after every checkpoint (the famous sawtooth). This is
the torn-page defense; InnoDB solves the same problem with a double-write
buffer instead; LMDB and SQLite-WAL solve it by never overwriting.

## 5. Checkpoint + recovery

- `CreateCheckPoint` — xlog.c:7400–7560: set redo point under the insert lock
  (:7561), then flush dirty buffers *while WAL keeps rolling* — **fuzzy**: the
  checkpoint is a starting point, not a consistent snapshot.
- `PerformWalRecovery` — xlogrecovery.c:1612–1806: read from the redo point,
  `ApplyWalRecord` (:1782) dispatches to per-resource-manager redo handlers.
  Per-record CRC in xlogreader.c:1207–1227 — an invalid CRC means "end of log",
  not "corruption error". The log's tail is *expected* to be garbage after a
  crash; checksums are how you find the cliff edge.
- No undo pass: postgres MVCC never overwrites tuples in place, so losers are
  just dead tuples awaiting vacuum. ARIES's undo machinery (CLRs, rollback)
  isn't needed. Read reading-aries.md for what postgres is *not* doing.

## 6. Sync methods — issue_xlog_fsync, xlog.c:9361–9410

`wal_sync_method`: fsync / fdatasync / open_datasync (O_DSYNC at open — no
separate sync call). Your fsync_ladder experiment measures exactly these.

## Questions to answer in notes.md

1. Why is `xl_prev` needed when records are read forward anyway? (Detects a
   valid-looking record left over from a recycled segment file.)
2. FPI sawtooth: checkpoint_timeout ↑ ⇒ WAL volume ↓ but recovery time ↑.
   Write the trade as a formula in (dirty rate, checkpoint interval).
3. The 8 insertion locks: what workload would make you raise the number, and
   what does postgres pay for each extra lock at flush time? (Flush must wait
   for all in-progress copies below the target LSN — WaitXLogInsertionsToFinish.)

## Done when

You can explain reserve-then-copy, the flush recheck, and needs_backup in
three sentences total — those three lines are the file.
