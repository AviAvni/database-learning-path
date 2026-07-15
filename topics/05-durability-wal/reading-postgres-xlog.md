# postgres xlog: reserve-then-copy and the flush recheck

Postgres's WAL is 10,000+ lines of C, but it earns its keep with five
mechanisms: the back-linked record format, the reserve-then-copy insertion
trick, group commit via a flush recheck, full-page writes after checkpoints,
and fuzzy checkpointing with redo-only recovery. Before the code, this
chapter builds each mechanism as a concept — what problem it solves and what
it costs — then hands you the exact functions and lines to skim. Do NOT read
the file linearly.

## The problem in one sentence

Hundreds of backend processes must append to one serial log and make their
commits durable, without the log's mutex or its ~1 ms fsync becoming the
ceiling — while also surviving the fact that a 8 KB postgres page can be
half-written when the power dies.

## The concepts, step by step

### Step 1 — the record: a backward-linked list with per-record checksums

A WAL record (postgres calls the WAL "xlog") is a self-describing entry:
total length, the transaction id that wrote it, a CRC (a checksum detecting
corruption), and — the interesting field — **`xl_prev`**, the LSN of the
*previous* record, making the log a backward-linked list even though
recovery reads it forward. Why: postgres recycles old 16 MB WAL segment
files by renaming them for reuse, so the tail of a "new" segment can contain
stale-but-internally-valid records from its previous life; a record whose
`xl_prev` doesn't point at the record actually before it is exposed as a
leftover. Records can also attach **block references** — links to the pages
they modify — and, when needed, a **full-page image** (FPI: a complete copy
of a page, Step 4) with the page's free-space hole elided to save bytes.

### Step 2 — insertion: reserve serially, copy in parallel

The naive design — one mutex around "append my record to the log buffer" —
serializes the memcpy of every backend (Aether's bottleneck A). Postgres
splits the operation in two: **reservation** — a spinlock held for ~3
arithmetic operations hands out a byte range in the log ("your record
occupies LSN X..X+len") — and **copying** — the backend then memcpys its
record into that reserved slice of the shared WAL buffers under one of
`NUM_XLOGINSERT_LOCKS = 8` insertion locks, in parallel with up to 7 other
backends. The sequencing stays serial but is made *tiny*; the heavy part is
made parallel. This is Aether's consolidation insight shipped in
production, and topic 2's incremental-rehash move in disguise: keep the
critical section O(1), amortize/parallelize the heavy part.

### Step 3 — group commit: the flush recheck

Commit requires the log flushed through your commit record's LSN — but not
that *you* do the flushing. `XLogFlush`'s heart is one recheck: after
acquiring the write lock (possibly having waited behind another flusher),
look again at the shared flushed-LSN — while you waited, that other backend
probably fsynced past your LSN, and you return having done zero IO. One
fsync covers every backend that queued behind the lock. Group commit is
just that recheck:

```rust
fn xlog_flush(&self, upto: Lsn) {
    if self.flushed_lsn() >= upto { return; }   // cheap check, no lock
    let _g = self.write_lock.lock();            // maybe wait behind a flusher…
    if self.flushed_lsn() >= upto { return; }   // …RECHECK: their fsync already
                                                // covered our LSN — free ride
    self.write_out_buffers_through(upto);
    self.wal_file.fdatasync();                  // ONE fsync for every backend
    self.advance_flushed_lsn();                 // that queued behind the lock
}
```

Two optional knobs (`commit_delay`/`commit_siblings`) add a pre-flush sleep
to grow the batch further. At 1 ms per fsync and 32 concurrent committers,
the recheck turns ~1K commits/s into ~32K. Your commit_throughput experiment
reimplements exactly this.

### Step 4 — full-page writes: the torn-page defense

A **torn page** is a page half-written at the moment of power loss — the
disk holds 4 KB of new bytes and 4 KB of old, an inconsistent hybrid that
postgres's normal WAL records can't fix, because they are *deltas* ("set
this tuple's field") that assume the page under them is intact. The fix:
the **first** modification of each page after a checkpoint logs a full-page
image instead of a delta (`needs_backup = page_lsn <= RedoRecPtr` — the
page hasn't been touched since the checkpoint's redo point). Recovery then
restores the whole page from the FPI before applying later deltas — a torn
page is simply overwritten wholesale. The cost is the famous **sawtooth**:
WAL volume spikes right after every checkpoint (every hot page owes one
8 KB image), then decays. Alternatives on the same problem: InnoDB's
double-write buffer (write pages twice, once to a scratch area); LMDB and
SQLite-WAL never overwrite pages at all, so they have no torn-page problem
to solve.

### Step 5 — fuzzy checkpoints and redo-only recovery

A postgres checkpoint doesn't stop the world: it sets a **redo point** (the
LSN recovery will start from) under the insert lock, then flushes dirty
buffers over minutes *while WAL keeps rolling* — "fuzzy" because the
checkpoint is a starting point, not a consistent snapshot (ARIES Step 3).
Recovery reads forward from the redo point, dispatching each record to a
per-resource-manager redo handler. Two things to notice:

- **A bad CRC means "end of log", not "corruption error."** After a crash
  the log's tail is *expected* to be garbage (a half-written record);
  per-record checksums are how recovery finds the cliff edge and stops.
- **There is no undo pass.** Postgres MVCC never overwrites tuples in
  place — an update writes a new tuple version, so a loser transaction's
  writes are just dead tuples that vacuum will reap. ARIES's undo machinery
  (CLRs, rollback) isn't needed; the log is redo-only. Read
  reading-aries.md for what postgres is *not* doing.

### Step 6 — the sync method: which fsync do you mean?

The actual durability call is configurable (`wal_sync_method`): `fsync`,
`fdatasync` (skips inode metadata — usually the right default), or
`open_datasync` (the file is opened with O_DSYNC, so every write syncs — no
separate call). These differ by 10× or more on the same hardware, and on
macOS none of them flush the drive cache without F_FULLFSYNC. Your
fsync_ladder experiment measures exactly these — the numbers feed every
design decision in M5.

## Where each step lives in the code

- **Step 1 — `xlogrecord.h:41–53`**: `XLogRecord` — `xl_tot_len`, `xl_xid`,
  `xl_prev`, `xl_crc`. Block references :103; full-page images ride in
  `XLogRecordBlockImageHeader` (:141) — note `hole_offset`: the free space
  in the middle of a page is elided from the FPI.
- **Step 2 — `xlog.c`**: `ReserveXLogInsertLocation` :1149–1193 — the
  spinlock held for ~3 arithmetic ops (:1172–1180) hands out byte ranges.
  `CopyXLogRecordToWAL` :1266 — copy into the reserved slice under one of
  `NUM_XLOGINSERT_LOCKS = 8` (xlog.c:157) insertion locks.
- **Step 3 — `xlog.c:2800–2891`**: `XLogFlush`; the recheck of
  `LogwrtResult.Flush` at :2885; `commit_delay`/`commit_siblings`
  :2901–2906.
- **Step 4 — `xloginsert.c:621–700`**: `XLogRecordAssemble`;
  `needs_backup = (page_lsn <= RedoRecPtr)` at :694.
- **Step 5 — checkpoint + recovery**: `CreateCheckPoint` — xlog.c:7400–7560,
  redo point set under the insert lock (:7561), then dirty buffers flushed
  while WAL keeps rolling. `PerformWalRecovery` —
  xlogrecovery.c:1612–1806; `ApplyWalRecord` (:1782) dispatches to
  per-resource-manager redo handlers. Per-record CRC validation in
  xlogreader.c:1207–1227 — invalid CRC = end of log.
- **Step 6 — `xlog.c:9361–9410`**: `issue_xlog_fsync` — fsync / fdatasync /
  open_datasync.

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

## References

**Code**
- [postgres/postgres](https://github.com/postgres/postgres) —
  `src/backend/access/transam/xlog.c` (10,196 lines — do NOT read
  linearly), `src/backend/access/transam/xloginsert.c`,
  `src/backend/access/transam/xlogrecovery.c`,
  `src/include/access/xlogrecord.h`. Local clone at `~/repos/postgres`.
