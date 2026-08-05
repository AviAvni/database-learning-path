# postgres xlog: reserve-then-copy and the flush recheck

Postgres's WAL is 10,196 lines of C in one file, but it earns its keep with
six mechanisms: the back-linked record format, the reserve-then-copy insertion
trick, group commit via a flush recheck, full-page writes after checkpoints,
fuzzy checkpointing with redo-only recovery, and a configurable sync call that
decides *which rung of the durability ladder* you are actually standing on.
Before the code, this chapter builds each mechanism as a concept — what problem
it solves and what it costs — then hands you the exact functions and lines to
skim. Do NOT read the file linearly.

Every line number below was read at **postgres/postgres@701f021**
(`tools/pinned-source.py show postgres <path> -r A:B`). Every timing below
comes from this topic's own provided lane, `cargo run --release --bin
fsync_ladder`, on the Apple M3 Pro / APFS machine recorded in `notes.md`.

**Vocabulary, once, before it is used.** *WAL* (write-ahead log) is the rule
that a change is described in a sequential log, and that log made durable,
*before* the page holding the change may be written; postgres calls its WAL the
"xlog". An *LSN* (log sequence number) is a record's byte offset in that
ever-growing log, so LSNs are monotone and comparable. A *checkpoint* is a
periodic marker saying "everything logged before here is already in the data
files"; recovery need not read behind it. *Group commit* is the trick of
letting one durability call serve many transactions at once. And three system
calls that people say "fsync" for, which are three different things:

| call | what it guarantees | measured p50 here |
|---|---|---|
| `write()` alone | bytes are in the OS page cache | **1.17 µs** |
| `fdatasync()` / macOS `fsync()` | bytes handed to the *drive*; the drive's volatile cache may still hold them | **22.67 µs** |
| macOS `fcntl(fd, F_FULLFSYNC)` | the drive has flushed its cache to stable media | **2.97 ms** |

That is a **19.4×** step from the first to the second and a further **131×**
step from the second to the third (856 898 → 44 109 → 337 implied commits/s).
Whenever this chapter says a cost, it says which of the three rungs it means.
One precision, because this rung is the one everyone conflates: the middle row
was measured on macOS as `fsync(2)`. There is no `fdatasync` on this machine —
`fsync_ladder.rs` compiles that lane out — so 22.67 µs is a macOS `fsync`
number, and `fdatasync` is named here only because it occupies the same rung on
Linux.

## The problem in one sentence

Hundreds of backend processes must append to one serial log and make their
commits durable, without the log's spinlock or its sync call becoming the
ceiling — a per-commit `F_FULLFSYNC` caps the whole server at **337
commits/s** on this machine — while also surviving the fact that an 8 KB
postgres page can be half-written when the power dies.

## The concepts, step by step

### Step 1 — the record: a backward-linked list with per-record checksums

> **In:** a WAL segment file, possibly recycled from a previous life, being
> read forward byte by byte after a crash.
> **Out:** a decision, per record, of *this is a real record of mine* versus
> *this is where my log ends* — reached with a 24-byte header and no index.

A WAL record is self-describing: total length, the transaction id that wrote
it, a CRC (a checksum over the record used to detect corruption), and — the
interesting field — **`xl_prev`**, the LSN of the *previous* record, making the
log a backward-linked list even though recovery reads it forward. The fixed
header is `SizeOfXLogRecord` = **24 bytes** (`xlogrecord.h:55`).

Why carry a back-pointer you never follow? Because postgres does not delete
old WAL segments; it **renames them for reuse**. `InstallXLogFileSegment` is
documented as being used "both to install a newly-created segment (from a temp
file) and to recycle an old segment" and the file is "renamed into place"
(`xlog.c:3586–3600`). A segment is `DEFAULT_XLOG_SEG_SIZE` = 16 MB
(`pg_config_manual.h:20`), so the tail of a "new" segment holds up to 16 MB of
stale-but-internally-valid records from its previous life, each with a
perfectly good CRC. `xl_prev` is what exposes them. The reader says so in
its own words:

```c
// src/backend/access/transam/xlogreader.c — ValidXLogRecordHeader, the
// sequential-read branch, 1173-1188
  1173  	else
  1174  	{
  1175  		/*
  1176  		 * Record's prev-link should exactly match our previous location. This
  1177  		 * check guards against torn WAL pages where a stale but valid-looking
  1178  		 * WAL record starts on a sector boundary.
  1179  		 */
  1180  		if (record->xl_prev != PrevRecPtr)
  1181  		{
  1182  			report_invalid_record(state,
  1183  								  "record with incorrect prev-link %X/%08X at %X/%08X",
  1184  								  LSN_FORMAT_ARGS(record->xl_prev),
  1185  								  LSN_FORMAT_ARGS(RecPtr));
  1186  			return false;
  1187  		}
  1188  	}
```

Note the two strengths of the test. Reading sequentially (`randAccess ==
false`), the prev-link must match **exactly** (`xlogreader.c:1180`). Seeking to
an arbitrary LSN, all postgres can demand is `record->xl_prev < RecPtr`
(`xlogreader.c:1164`, whose comment concedes "we can't exactly verify the
prev-link") — a back-pointer into the future is impossible, but a stale one is
not detectable without knowing where you came from.

Records can also attach **block references** — links to the pages they
modify (`xlogrecord.h:103`) — and, when needed, a **full-page image** (FPI: a
complete byte-for-byte copy of a page, Step 4) whose header
(`xlogrecord.h:141`) carries a `hole_offset` (`:144`) so the free-space hole in
the middle of the page is elided rather than logged.

*Why it matters:* the log has no index and no table of contents. Every
structural guarantee recovery gets — where a record starts, where the log ends,
that this record is not litter — is squeezed out of 24 bytes per record.

### Step 2 — insertion: reserve serially, copy in parallel

> **In:** N backends each holding an assembled WAL record of a few dozen to a
> few thousand bytes, all wanting to append to one shared buffer.
> **Out:** each backend's bytes in the shared WAL buffers at a unique LSN
> range, with the serial section reduced to five assignments.

The naive design — one mutex around "append my record to the log buffer" —
serializes the memcpy of every backend. That is the bottleneck Aether calls
**(D) log buffer contention** (Johnson et al., VLDB 2010, §1.1). Postgres
splits the operation in two.

**Reservation** hands out a byte range. The comment above it states the design
goal outright — "the duration the spinlock needs to be held is minimized by
minimizing the calculations that have to be done while holding the lock …
reserving X bytes from WAL is almost as simple as `CurrBytePos += X`"
(`xlog.c:1163–1170`) — and the critical section delivers on it:

```c
// src/backend/access/transam/xlog.c — ReserveXLogInsertLocation, 1172-1184
  1172  	SpinLockAcquire(&Insert->insertpos_lck);
  1173  
  1174  	startbytepos = Insert->CurrBytePos;
  1175  	endbytepos = startbytepos + size;
  1176  	prevbytepos = Insert->PrevBytePos;
  1177  	Insert->CurrBytePos = endbytepos;
  1178  	Insert->PrevBytePos = startbytepos;
  1179  
  1180  	SpinLockRelease(&Insert->insertpos_lck);
  1181  
  1182  	*StartPos = XLogBytePosToRecPtr(startbytepos);
  1183  	*EndPos = XLogBytePosToEndRecPtr(endbytepos);
  1184  	*PrevPtr = XLogBytePosToRecPtr(prevbytepos);
```

Count what is inside the lock: one addition and four field moves, nine lines.
The two conversions from "usable byte position" to a real `XLogRecPtr` — which
must skip page headers, and are the expensive part — happen at 1182–1184,
*outside*. Note also that this is where `xl_prev` gets its value: the
reservation both allocates the range and tells you what came before you
(`xlog.c:899–903`), which is why the record's CRC can only be finished
afterwards (`xlog.c:950–953`).

**Copying** is the parallel half. The backend memcpys its record into that
reserved slice under one of `NUM_XLOGINSERT_LOCKS = 8` insertion locks
(`xlog.c:157`), acquired at `xlog.c:860` before the reservation and released
after `CopyXLogRecordToWAL` (`xlog.c:1266`, called at `:959`). Which of the 8
you get is a hash of your backend number — `MyProcNumber %
NUM_XLOGINSERT_LOCKS` (`xlog.c:1430`) — with migration to another slot on
contention (`:1448`). Eight backends copy at once; only the five assignments
above are serial. The whole two-step design is spelled out in the comment at
`xlog.c:826–855`.

A caution the guide used to get wrong: this is Aether's **§5.2 "Decoupling
Buffer Fill"** — release the buffer-allocation mutex immediately and let fills
pipeline — not its §5.1 consolidation array, in which threads combine their
requests via CAS in an auxiliary slot array *before* touching the mutex.
Postgres has no consolidation array. See `reading-aether.md` Step 5.

*Why it matters:* this is topic 2's incremental-rehash move in disguise — keep
the unavoidably serial section O(1) and constant-sized, then parallelize the
part whose cost scales with the data.

### Step 3 — group commit: the flush recheck

> **In:** a backend that has just inserted its commit record at LSN `record`,
> and must not reply to the client until the log is durable through it.
> **Out:** the client's reply, having usually performed **zero** I/O itself.

Commit requires the log flushed through your commit record's LSN — but not that
*you* do the flushing. `XLogFlush`'s heart is one recheck: after acquiring the
write lock (possibly having waited behind another flusher), look again at the
shared flushed-LSN. While you waited, that other backend probably synced past
your LSN, and you return having done nothing. One sync covers every backend
that queued behind the lock. That is all group commit is:

```c
// src/backend/access/transam/xlog.c — XLogFlush's loop, 2848-2891 (comments elided)
  2848  	for (;;)
  2849  	{
  2853  		RefreshXLogWriteResult(LogwrtResult);
  2854  		if (record <= LogwrtResult.Flush)
  2855  			break;
  2865  		insertpos = WaitXLogInsertionsToFinish(WriteRqstPtr);
  2874  		if (!LWLockAcquireOrWait(WALWriteLock, LW_EXCLUSIVE))
  2875  		{
  2881  			continue;
  2882  		}
  2884  		/* Got the lock; recheck whether request is satisfied */
  2885  		RefreshXLogWriteResult(LogwrtResult);
  2886  		if (record <= LogwrtResult.Flush)
  2887  		{
  2888  			LWLockRelease(WALWriteLock);
  2889  			break;
  2890  		}
```

There are in fact *three* exits before any I/O: a check before the loop is
entered at all (`xlog.c:2820–2821`), the top-of-loop check at 2853–2855, and
the post-lock recheck at 2885–2886. `LWLockAcquireOrWait` (2874) is the unusual
primitive — it returns false when the lock became free without being handed to
you, which sends you back around to re-read the flushed LSN rather than
acquiring a lock you may not need. Postgres's own comment (2867–2872) names the
purpose: "This helps to maintain a good rate of group committing when the
system is bottlenecked by the speed of fsyncing."

Two optional knobs, `commit_delay` and `commit_siblings`, add a deliberate
pre-flush sleep to grow the batch further (`xlog.c:2901–2906`), guarded by
`MinimumActiveBackends(CommitSiblings)` so a lightly-loaded server never pays
the latency. Then, and only then, `XLogWrite` does the write and the sync
(`xlog.c:2925`).

**Work the arithmetic on the measured numbers.** Let *T* be the cost of one
sync and λ the rate at which commit records arrive. A flush that starts now
serves everyone who arrives during the previous flush, so the steady-state
batch size is λ·*T* and the durable throughput is min(λ, batch/*T*) — the
system is stable at *any* λ, and the batch grows to absorb it:

```
rung = F_FULLFSYNC, T = 2.967 ms (measured p50)

   λ = 1 000 commits/s   →  batch = 1 000 × 0.002967 =    2.97 commits per sync
   λ = 5 000             →  batch =                      14.84
   λ = 20 000            →  batch =                      59.34
   λ = 100 000           →  batch =                     296.70

without group commit, every commit pays its own 2.967 ms:
   ceiling = 1 / 0.002967 s = 337 commits/s, flat, at every λ above

rung = macOS fsync, T = 22.67 µs (measured p50)
   ceiling without group commit = 1 / 0.00002267 = 44 109 commits/s
   λ = 100 000  →  batch = 100 000 × 0.00002267 = 2.27 commits per sync
```

Two lessons fall out. First, group commit is not an optimization, it is what
makes the durable ceiling a function of λ rather than a constant. Second, the
batch is large exactly when the sync is slow: on the `F_FULLFSYNC` rung a
20 000/s workload rides 59 transactions per sync, but on the `fsync` rung the
same workload rides 0.45 — group commit does almost nothing there, because
there is nothing to hide.

Beware the number that used to be in this file: "at 1 ms per fsync and 32
concurrent committers, ~1K commits/s becomes ~32K." The arithmetic is fine and
the 1 ms is from nowhere. On the machine in `notes.md`, 32 committers on the
`F_FULLFSYNC` rung get 337 × 32 = **10 784** commits/s at best, and that is a
ceiling on the *batch*, not a promise — it needs the 32 to actually overlap.
Your `commit_throughput` experiment reimplements exactly this recheck.

*Why it matters:* the recheck is nine lines and it is the difference between a
server that does 337 commits/s and one that does tens of thousands.

### Step 4 — full-page writes: the torn-page defense

> **In:** a page on disk that was half-written when the power died — 4 KB of
> new bytes and 4 KB of old, with a CRC that matches neither.
> **Out:** a correct page, reconstructed without ever reading the broken one.

A **torn page** is a page half-written at the moment of power loss. Postgres's
normal WAL records cannot fix it, because they are *deltas* ("set this tuple's
field") that assume the page under them is intact — a physiological record,
logical within a page and physical about which page. The fix: the **first**
modification of each page after a checkpoint logs a full-page image instead of
a delta. The test is one comparison:

```c
// src/backend/access/transam/xloginsert.c — XLogRecordAssemble, 678-694
   678  		/* Determine if this block needs to be backed up */
   679  		if (regbuf->flags & REGBUF_FORCE_IMAGE)
   680  			needs_backup = true;
   681  		else if (regbuf->flags & REGBUF_NO_IMAGE)
   682  			needs_backup = false;
   683  		else if (!doPageWrites)
   684  			needs_backup = false;
   685  		else
   686  		{
   692  			XLogRecPtr	page_lsn = PageGetLSN(regbuf->page);
   693  
   694  			needs_backup = (page_lsn <= RedoRecPtr);
```

Three overrides come first — an explicit force, an explicit suppress, and
`full_page_writes = off` — and only then the real test. `page_lsn <=
RedoRecPtr` reads as "this page has not been touched since the checkpoint's
redo point", so its on-disk image is the one the checkpoint left and recovery
may have to rebuild it. Recovery restores the whole page from the FPI before
applying later deltas; a torn page is simply overwritten wholesale, never
read.

The cost is the famous **sawtooth**: WAL volume spikes right after every
checkpoint, because every hot page owes one 8 KB image, then decays as the
working set is covered. Alternatives on the same problem: InnoDB's double-write
buffer (write every page twice, once to a scratch area, so one intact copy
always exists); LMDB and SQLite-WAL never overwrite a page in place at all, so
they have no torn-page problem to solve — see `reading-turso-wal.md`.

*Why it matters:* full-page writes are the clearest case in the topic of buying
recovery correctness with steady-state write bandwidth, and the exchange rate
is set by one tunable (`checkpoint_timeout`).

### Step 5 — fuzzy checkpoints and redo-only recovery

> **In:** a running server with a dirty buffer pool and a log that never stops.
> **Out:** a durable marker saying "recovery may start here", produced without
> pausing writes.

A **fuzzy checkpoint** is a checkpoint that does not stop the world: it fixes a
**redo point** (the LSN recovery will start from) and then flushes dirty
buffers over minutes *while WAL keeps rolling*. "Fuzzy" because the result is a
starting point, not a consistent snapshot — the data files at the end of the
checkpoint match no single instant. ARIES §5.4 is where this comes from
(`reading-aries.md` Step 3).

How postgres fixes the redo point depends on the kind of checkpoint, and this
is the detail the guide previously got wrong. For a **shutdown** checkpoint the
redo point is taken while the insert locks are held (`xlog.c:7529–7562`, the
assignment at `:7561`). For a normal **online** checkpoint the insert locks are
released first (`:7568`) and the redo point is instead the LSN of a dedicated
`XLOG_CHECKPOINT_REDO` record inserted into the log (`:7579–7593`, with
`checkPoint.redo = RedoRecPtr` at `:7601`). Only afterwards does
`CheckPointGuts(checkPoint.redo, flags)` (`:7715`) flush the dirty buffers, and
the checkpoint record itself is inserted at `:7750` and flushed at `:7754`.
`CreateCheckPoint` runs `xlog.c:7400–7897`.

Recovery reads forward from the redo point in `PerformWalRecovery`
(`xlogrecovery.c:1612`), whose loop calls `ApplyWalRecord` at `:1782`;
`ApplyWalRecord` itself is defined at `:1883` and dispatches with
`GetRmgr(record->xl_rmid).rm_redo(xlogreader)` at `:1966`. Two things to
notice:

- **A bad CRC means "end of log", not "corruption error."** After a crash the
  log's tail is *expected* to be garbage — a record half-written when the power
  went. `ValidXLogRecord` (`xlogreader.c:1205–1227`) recomputes the CRC and
  compares at `:1218`; a mismatch is the cliff edge, and recovery stops there
  having replayed everything before it.
- **There is no undo pass.** Postgres MVCC never overwrites a tuple in place —
  an update writes a new tuple version — so a loser transaction's writes are
  just dead tuples that vacuum will reap. ARIES's undo machinery (CLRs,
  rollback) is not needed; the log is redo-only. Read `reading-aries.md` for
  what postgres is deliberately *not* doing, and what it gives up by not doing
  it.

*Why it matters:* checkpoint interval is the one knob that trades steady-state
cost (FPI volume, Step 4) against recovery time, and "fuzzy" is what makes the
knob cheap enough to turn.

### Step 6 — the sync method: which rung are you on?

> **In:** a decision that a byte range of WAL must survive a power cut.
> **Out:** one of five different system-call sequences, spanning **19.4×** in
> cost between the two you are most likely to be running.

The durability call is configurable via `wal_sync_method`, and
`issue_xlog_fsync` (`xlog.c:9361`) has **five** cases, not three
(`xlog.c:9383–9409`):

| `wal_sync_method` | what it calls | rung |
|---|---|---|
| `fsync` | `pg_fsync_no_writethrough` → `fsync()` | middle |
| `fsync_writethrough` | `pg_fsync_writethrough` → `fcntl(fd, F_FULLFSYNC)` (`fd.c:467`) | **top** |
| `fdatasync` | `fdatasync()` — skips inode metadata | middle |
| `open_sync` | nothing; the file was opened `O_SYNC`, so `issue_xlog_fsync` asserts unreachable (`:9399–9403`) | middle |
| `open_datasync` | nothing; the file was opened `O_DSYNC` | middle |

So the old claim that "on macOS none of them flush the drive cache without
F_FULLFSYNC" is wrong in the specific: postgres ships that rung as
`fsync_writethrough`. What is true is that it is never the default. The default
is `open_datasync` where `O_DSYNC` exists and differs from `O_SYNC`, otherwise
`fdatasync` (`xlogdefs.h:78–84`); neither `src/template/linux` nor
`src/template/darwin` overrides it. A stock postgres on macOS therefore sits on
the **middle** rung — 22.67 µs, 44 109 implied commits/s, and a drive cache
that has not been flushed. Turning on `fsync_writethrough` moves you to
2.97 ms and 337/s, a **131×** price for the guarantee most people assume they
already had. The macOS `fsync(2)` manual page is the primary source: "while
fsync() will flush all data from the host to the drive … the drive itself may
not physically write the data to the platters for quite some time … For
applications that require tighter guarantees … Mac OS X provides the
F_FULLFSYNC fcntl."

Your `fsync_ladder` experiment measures exactly these rungs; the numbers feed
every design decision in M5.

*Why it matters:* every durability claim in this topic — and most of the ones
you will read elsewhere — is meaningless until it says which of these five
lines it ran.

## Where each step lives in the code

Anchors verified at postgres/postgres@701f021.

- **Step 1 — `xlogrecord.h:41–53`**: `XLogRecord` — `xl_tot_len`, `xl_xid`,
  `xl_prev`, `xl_info`, `xl_rmid`, `xl_crc`; `SizeOfXLogRecord` = 24 at `:55`.
  Block references `:103`; FPI header `XLogRecordBlockImageHeader` `:141`, with
  `hole_offset` at `:144`. Prev-link enforcement:
  `xlogreader.c:1139–1191` (`ValidXLogRecordHeader`), the exact-match branch at
  `:1173–1188`, the random-access branch at `:1160–1171`. Segment recycling:
  `xlog.c:3586–3600`; segment size `pg_config_manual.h:20`.
- **Step 2 — `xlog.c`**: `ReserveXLogInsertLocation` `:1149–1193`, spinlock
  `:1172–1180`. `CopyXLogRecordToWAL` `:1266`, called from `:959`.
  `NUM_XLOGINSERT_LOCKS = 8` at `:157`; `WALInsertLockAcquire` `:1410–1450`,
  slot hash `:1430`, migration `:1448`. Design comment `:826–855`.
- **Step 3 — `xlog.c:2800–2930`**: `XLogFlush`; pre-loop exit `:2820–2821`;
  loop `:2848`; top-of-loop check `:2853–2855`; `LWLockAcquireOrWait` `:2874`;
  **the recheck at `:2885–2886`**; `commit_delay`/`commit_siblings`
  `:2901–2906`; `XLogWrite` `:2925`. Cost of extra insertion locks:
  `WaitXLogInsertionsToFinish` `:1545`, its loop over all 8 at `:1597`.
- **Step 4 — `xloginsert.c:621`**: `XLogRecordAssemble`; the four-branch backup
  decision `:679–694`; `needs_backup = (page_lsn <= RedoRecPtr)` at `:694`.
- **Step 5 — checkpoint + recovery**: `CreateCheckPoint` `xlog.c:7400–7897`;
  shutdown redo point under the insert locks `:7529–7562`; online path releases
  them at `:7568` and inserts `XLOG_CHECKPOINT_REDO` at `:7579–7593`, assigning
  `checkPoint.redo` at `:7601`; `CheckPointGuts` `:7715`; checkpoint record
  `:7750`, flushed `:7754`. `PerformWalRecovery` `xlogrecovery.c:1612`, calling
  `ApplyWalRecord` at `:1782`; `ApplyWalRecord` defined `:1883`, rmgr dispatch
  `:1966`. CRC validation `xlogreader.c:1205–1227`, compare at `:1218`.
- **Step 6 — `xlog.c:9361`**: `issue_xlog_fsync`, five cases at `:9383–9409`;
  `pg_fsync_writethrough` → `fcntl(F_FULLFSYNC)` at
  `src/backend/storage/file/fd.c:467`; default choice `xlogdefs.h:78–84`.

## Questions to answer in notes.md

1. Why is `xl_prev` needed when records are read forward anyway? (Detects a
   valid-looking record left over from a recycled 16 MB segment file —
   `xlogreader.c:1176–1179` says so in the source.) Follow-up: why does the
   random-access path only check `xl_prev < RecPtr`?
2. FPI sawtooth: `checkpoint_timeout` ↑ ⇒ WAL volume ↓ but recovery time ↑.
   Write the trade as a formula in (pages dirtied per second, checkpoint
   interval, page size) and evaluate it for 500 pages/s and intervals of 5 and
   30 minutes.
3. The 8 insertion locks: what workload would make you raise the number, and
   what does postgres pay for each extra lock at flush time? (Every flush walks
   all of them — `WaitXLogInsertionsToFinish`, the loop at `xlog.c:1597`.)
4. You set `wal_sync_method = fsync_writethrough` on the machine in
   `notes.md`. Using the ladder, what is the new single-threaded commit
   ceiling, and how many concurrent committers do you need before group commit
   restores the throughput you had on the default setting?

## Done when

Answer each before unfolding it.

- [ ] Name the two halves of a WAL insert, and say exactly how many operations
      postgres performs inside the reservation spinlock.

  <details><summary>Answer</summary>

  Reservation and copying. Inside `SpinLockAcquire`/`SpinLockRelease`
  (`xlog.c:1172–1180`) there is **one addition and four field moves** — read
  `CurrBytePos`, add `size`, read `PrevBytePos`, store both back. The two
  byte-position-to-`XLogRecPtr` conversions, which have to skip page headers,
  are deliberately outside at `:1182–1184`. Copying then happens under one of
  `NUM_XLOGINSERT_LOCKS = 8` insertion locks, so eight backends memcpy at once.

  </details>

- [ ] Point at the single line that makes group commit work, and say what a
      backend that hits it has just avoided.

  <details><summary>Answer</summary>

  `xlog.c:2885–2886` — `RefreshXLogWriteResult(LogwrtResult); if (record <=
  LogwrtResult.Flush)` immediately after `LWLockAcquireOrWait` succeeds. A
  backend that breaks there has avoided the write **and the sync**: another
  backend's flush already covered its LSN. On the `F_FULLFSYNC` rung that is
  2.97 ms of latency it did not pay, and it is why N committers can exceed the
  337 commits/s that one committer is capped at.

  </details>

- [ ] State the `needs_backup` test in words, and say what recovery does with
      the resulting record.

  <details><summary>Answer</summary>

  `needs_backup = (page_lsn <= RedoRecPtr)` (`xloginsert.c:694`) — "this page
  has not been modified since the checkpoint's redo point." When true, the
  record carries a **full-page image** instead of a delta. Recovery writes that
  whole 8 KB page over whatever is on disk before applying any later delta to
  it, so a page that was torn mid-write is never read, only overwritten. The
  price is the post-checkpoint WAL sawtooth: one 8 KB image per hot page.

  </details>

- [ ] Postgres recovery has no undo pass. Say what makes that possible and
      what it costs.

  <details><summary>Answer</summary>

  MVCC: an update writes a *new* tuple version rather than overwriting the old
  one, so an aborted transaction leaves dead tuples rather than corrupt data.
  Recovery therefore replays forward from the redo point and stops — no CLRs,
  no rollback machinery (contrast ARIES, `reading-aries.md`). The cost is paid
  elsewhere: dead tuples must be reclaimed by vacuum, tables bloat between
  vacuums, and long-running readers pin old versions.

  </details>

- [ ] A colleague says "postgres fsyncs on every commit, and an fsync costs
      about a millisecond." Correct both halves.

  <details><summary>Answer</summary>

  *Neither half is anchored.* (a) Postgres does not fsync per commit — it
  flushes per *batch*, and any committer whose LSN is already covered performs
  no I/O at all (`xlog.c:2885`). (b) "An fsync" is not one thing:
  `issue_xlog_fsync` has five cases (`xlog.c:9383–9409`), and on the machine in
  `notes.md` the middle rung (`fdatasync`, `open_datasync`, macOS `fsync`) is
  **22.67 µs** while the top rung (`fsync_writethrough` →
  `fcntl(F_FULLFSYNC)`, `fd.c:467`) is **2.97 ms** — 131× apart. The default is
  `open_datasync` or `fdatasync` (`xlogdefs.h:78–84`), i.e. the middle rung,
  which on macOS leaves the data in the drive's volatile cache.

  </details>

## References

**Code** — all anchors read at `postgres/postgres@701f021`; local clone at
`~/repos/postgres`, pin recorded in `resources/codebases.md`.

| file | what this chapter took from it |
|---|---|
| `src/backend/access/transam/xlog.c` (10,196 lines — do NOT read linearly) | insertion (Step 2), flush and group commit (Step 3), checkpoints (Step 5), `issue_xlog_fsync` (Step 6) |
| `src/backend/access/transam/xloginsert.c` | `XLogRecordAssemble` and the `needs_backup` test (Step 4) |
| `src/backend/access/transam/xlogreader.c` | prev-link and CRC validation, i.e. "where does the log end" (Steps 1, 5) |
| `src/backend/access/transam/xlogrecovery.c` | `PerformWalRecovery`, `ApplyWalRecord`, rmgr dispatch (Step 5) |
| `src/backend/storage/file/fd.c` | `pg_fsync_writethrough` → `F_FULLFSYNC` (Step 6) |
| `src/include/access/xlogrecord.h`, `xlogdefs.h`, `src/include/pg_config_manual.h` | record layout, default sync method, 16 MB segment size |

**Measurements** — `topics/05-durability-wal/notes.md`, "Baseline (provided
lane, Apple M3 Pro / APFS, measured 2026-07-28)", produced by
`experiments/src/bin/fsync_ladder.rs`. `FINDINGS.md` row 5 carries the
headline.

**Papers** — Mohan et al., "ARIES", *ACM TODS* 17(1), 1992, §5.4 for fuzzy
checkpoints. Johnson et al., "Aether: A Scalable Approach to Logging", *PVLDB*
3(1), 2010, §1.1 for the bottleneck taxonomy and §5.2 for decoupled buffer
fill.

**Manual pages** — macOS `fsync(2)`, the paragraph beginning "Note that while
fsync() will flush all data from the host to the drive".
