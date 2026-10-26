# Turso's WAL: recovery is finding where the log ends

This is SQLite's WAL mode in Rust: commits append whole page images as frames,
a chained checksum makes the log's valid prefix self-evident, and recovery has
no redo and no undo at all — it just decides where the log ends. Of the four
durability designs in this topic, this is the one your experiment should steal
most from, so before the code this chapter builds it piece by piece: the frame,
the commit marker, the checksum chain and salts, the sync call, the read path,
the checkpoint, and finally the recovery loop that all of them exist to make
trivial.

Every line number below was read at **tursodatabase/turso@a820079e8**
(v0.9.0-pre.1, 2026-10-15; `tools/pinned-source.py show turso <path> -r A:B`). Every timing comes from
this topic's provided lane, `cargo run --release --bin fsync_ladder`, on the
Apple M3 Pro / APFS machine recorded in `notes.md`.

**Vocabulary, once, before it is used.** A *WAL* (write-ahead log) is a
sequential file that a change is written to, and made durable in, before the
page holding that change may be overwritten. A *frame* is turso's unit of WAL
content: one 24-byte header plus one whole database page. A *checkpoint* copies
frames back into the main database file so the WAL can be reused. *Idempotent
redo* means replaying a log record twice is harmless — turso gets it for free,
because replaying a page image just writes the same bytes again. And the two
durability calls this chapter distinguishes throughout, with their measured p50
on this machine:

| call | what it guarantees | measured p50 |
|---|---|---|
| `fsync()` (macOS) | bytes handed to the drive; the drive's volatile cache may still hold them | **22.67 µs** |
| `fcntl(fd, F_FULLFSYNC)` | the drive has flushed its cache to stable media | **2.97 ms** |

**131× apart** — 44 109 versus 337 implied commits/s. Turso lets you choose
between them at runtime, which is why this chapter names the rung every time it
says "sync".

## The problem in one sentence

After `kill -9` mid-commit, the tail of the log file is arbitrary garbage —
half-written frames, stale bytes from a previous log generation — and recovery
must decide, from file contents alone, exactly which prefix of the log to
trust, with zero tolerance for accepting one corrupt or uncommitted byte.

## The concepts, step by step

### Step 1 — the frame: log whole page images, not operations

> **In:** a transaction that has modified some set of pages in the buffer pool.
> **Out:** those pages appended verbatim to the WAL file, with the main
> database file untouched.

Turso's WAL is a file of **frames**, and a frame is a complete copy of one
database page (4 KB by default; the WAL header allows a "Power of two between
512 and 65536 inclusive", `sqlite3_ondisk.rs:425`) plus a 24-byte header —
`WAL_FRAME_HEADER_SIZE = 24` at `sqlite3_ondisk.rs:405`. When a transaction
commits, every page it modified is appended to the WAL as a frame; the main
database file is not touched at all.

Compare the alternatives from this topic: postgres logs *deltas* ("change this
tuple's field") and must replay them onto pages at recovery
(`reading-postgres-xlog.md` Step 4); redis logs *commands* and must re-execute
them (`reading-redis-aof-rdb.md` Step 1). Page images are the maximalist
choice, and they buy two things:

- **Recovery needs no replay logic whatsoever.** Nothing is applied; frames are
  the current version of their pages.
- **Torn pages cannot hurt you.** A half-written frame fails its checksum and
  is discarded whole (Step 3), and the previous version of that page still
  exists untouched — either earlier in the WAL or in the database file. This is
  the whole of postgres's full-page-write machinery, obtained by construction
  rather than by logging an extra 8 KB image after each checkpoint.

The cost is volume: WAL bytes ∝ *pages touched*, not bytes changed. A one-byte
`UPDATE` that lands on one page writes 4 KB + 24 bytes. Price it against the
alternative before choosing it for M5:

```
one-byte update, 4 KB pages

  turso-style page image :  4 096 + 24  =  4 120 bytes
  postgres-style delta   :  ~24 (header) + ~30 (block ref + payload) ≈ 55 bytes

  ratio ≈ 75×   — but the delta owes you idempotent redo, an LSN on every
                  page, and full-page images after each checkpoint

break-even: the page-image log wins as soon as a transaction dirties most of a
page, and whenever the engineering cost of a correct redo path matters more
than write bandwidth
```

*Why it matters:* every later step in this chapter is cheap *because* of this
choice. The format prepays the complexity that postgres and ARIES pay at
recovery time.

### Step 2 — the commit marker: db_size turns frames into transactions

> **In:** a run of frames in the WAL, some belonging to a completed
> transaction and some to one that was still being appended when the power
> went.
> **Out:** a single frame index that is the last committed state, derived from
> one `u32` field with no separate commit record.

A transaction is multiple frames, and the log needs to say where one ends —
otherwise recovery could not tell "committed" from "half-appended". The frame
header's **`db_size`** field does it with zero extra records. The struct
documents it exactly:

```rust
// core/storage/sqlite3_ondisk.rs — WalFrameHeader, 477-500 (doc comments elided)
   477  pub struct WalFrameHeader {
   479      pub(crate) page_number: u32,
   483      pub(crate) db_size: u32,
   486      pub(crate) salt_1: u32,
   489      pub(crate) salt_2: u32,
   492      pub(crate) checksum_1: u32,
   495      pub(crate) checksum_2: u32,
   496  }
   497
   498  impl WalFrameHeader {
   499      pub fn is_commit_frame(&self) -> bool {
   500          self.db_size > 0
```

`db_size` is documented at `:481–482` as "For commit records, the size of the
database file in pages after the commit. For all other records, zero." — so a
frame with `db_size != 0` **is** the commit record, and `is_commit_frame()`
(`:499–500`) is literally `db_size > 0`. Six `u32`s, 24 bytes, and one of them
does the job postgres needs a commit record and a transaction id for.

Recovery's rule follows immediately: the state after a crash is defined by the
last *valid* frame with `db_size != 0`. Frames after it may be byte-perfect,
but with no commit marker following them they are invisible. No separate commit
record, no transaction table, no two-phase anything.

*Why it matters:* atomicity here is a property of the reader, not of the
writer. The writer never has to make a multi-frame append atomic; the reader
just refuses to look past the last commit marker.

### Step 3 — the checksum chain and the salts

> **In:** a WAL file whose tail may contain a half-written frame, and whose
> middle may contain intact frames left over from a previous generation of the
> same file.
> **Out:** a single stopping point, computed with two `u32` compares and one
> arithmetic pass per frame.

Each frame carries a checksum, but not an independent one — checksums are
**cumulative**: frame N's checksum is computed over frame N's contents *seeded
with frame N−1's checksum*. One flipped bit anywhere invalidates that frame
*and every frame after it*, which is exactly what you want: the log's
trustworthy prefix ends at the first bad checksum, and nothing past a
corruption can masquerade as valid.

```
 WAL file:  [hdr]  [frame p5][frame p2][frame p9*]  [frame p5][frame p1*] …
                    ── txn 1, *commit (db_size≠0) ──  ── txn 2 ──
 checksum:   c0 ──► c1 ──────► c2 ─────► c3 ─────────► c4 ──────► c5   (chained)
 salts: change on WAL reset — a stale frame from a previous WAL generation
        fails the salt check even if its checksum chain looks plausible
```

The writer builds a frame in two calls. `prepare_wal_frame`
(`sqlite3_ondisk.rs:2156–2174`) first lays down the header fields, then copies
the page in and computes the checksum chain — two functions rather than one
because a page transform (encryption, for instance) may rewrite the page body
between the two, and the checksum must cover the bytes that actually reach the
disk (`:2176–2179`: the caller "must write every byte after
[`WAL_FRAME_HEADER_SIZE`] before computing the frame checksum"). The two halves:

```rust
// core/storage/sqlite3_ondisk.rs — prepare_wal_frame_header and recompute_wal_frame_checksum, 2188-2211
  2188      frame[0..4].copy_from_slice(&page_number.to_be_bytes());
  2189      frame[4..8].copy_from_slice(&db_size.to_be_bytes());
  2190      frame[8..12].copy_from_slice(&wal_header.salt_1.to_be_bytes());
  2191      frame[12..16].copy_from_slice(&wal_header.salt_2.to_be_bytes());
        // ... 2192-2200: end of prepare_wal_frame_header; signature of recompute_wal_frame_checksum ...
  2201      let expects_be = wal_header.magic & 1;
  2202      let use_native_endian = cfg!(target_endian = "big") as u32 == expects_be;
  2203      let header_checksum = checksum_wal(&frame[0..8], wal_header, prev_checksums, use_native_endian);
  2204      let final_checksum = checksum_wal(
  2205          &frame[WAL_FRAME_HEADER_SIZE..],
  2206          wal_header,
  2207          header_checksum,
  2208          use_native_endian,
  2209      );
  2210      frame[16..20].copy_from_slice(&final_checksum.0.to_be_bytes());
  2211      frame[20..24].copy_from_slice(&final_checksum.1.to_be_bytes());
```

The line to focus on is 2203. Read the seeds carefully: `prev_checksums` (the previous frame's result) seeds
the header checksum over `frame[0..8]`, which then seeds the page checksum.
Note what is *not* covered — bytes 8..16, the two salts. They are not
checksummed because they are compared directly, which is the point of Step 3's
second half.

The checksum itself is not a CRC. `checksum_wal`
(`sqlite3_ondisk.rs:2299–2324`) is SQLite's two-word additive rolling sum —
`s0 = s0 + (v0 + s1); s1 = s1 + (v1 + s0)` over 8-byte groups
(`:2312–2313`), with a byte-swapping variant for the non-native endianness
(`:2317–2318`). It is a handful of adds per 8 bytes, which is why checksumming
a 4 KB page is not visible next to the write. It is also weaker than a CRC
against adversarial corruption — an acceptable trade for a format whose threat
model is a torn write, not an attacker.

The chain has one blind spot: the WAL file is *reused* after a checkpoint
(reset, not deleted), so a frame from the file's previous life can sit at
exactly the right offset with an internally consistent checksum. The fix is two
**salts** — values in the WAL header, copied into every frame header, and
changed on every WAL reset. `restart_snapshot_from_authority`
(`wal.rs:1845–1895`) bumps `checkpoint_seq` by one (`:1850`), increments
`salt_1` (`:1851`), and draws a **fresh random** `salt_2` (`:1852`), then resets
`max_frame` and `nbackfills` to 0 (`:1854–1855`). A frame whose salts don't
match the current header's is from a dead generation, whatever its checksum
says. Two `u32` comparisons close the hole.

*Why it matters:* the chain answers "is this frame intact?" and the salts
answer "is this frame *mine*?". Neither question is answerable by the other's
mechanism, and a format that asks only the first is the classic WAL bug.

### Step 4 — commit means sync — and which sync you chose is the guarantee

> **In:** frames written to the WAL file descriptor, sitting in the OS page
> cache.
> **Out:** either a genuinely durable commit at **2.97 ms**, or a commit that
> survives process death but not power loss at **22.67 µs** — selected by one
> pragma.

A commit is durable only when the frames have reached stable storage, so after
appending the commit frame turso syncs the WAL file. The codebase makes the
choice explicit in its type system rather than burying it:

```rust
// core/io/mod.rs — FileSyncType, 126-136
   126  /// Controls which sync mechanism to use for durability.
   127  /// `FullFsync` only has effect on Apple platforms (uses F_FULLFSYNC fcntl).
   128  /// On other platforms, both variants behave the same (regular fsync).
   129  #[derive(Debug, Clone, Copy, PartialEq, Eq, AtomicEnum)]
   130  pub enum FileSyncType {
   131      /// Regular fsync - flushes to disk but may not flush disk write cache on macOS.
   132      Fsync,
   133      /// Full fsync - on macOS uses F_FULLFSYNC to flush disk write cache.
   134      /// On other platforms, behaves the same as Fsync.
   135      FullFsync,
   136  }
```

and honours it at the syscall boundary — `core/io/unix.rs:455–472`, where on
Apple targets `Fsync` becomes `libc::fsync(fd)` (`:460`) and `FullFsync`
becomes `libc::fcntl(fd, libc::F_FULLFSYNC)` (`:462`), while on every other
target both call plain `fsync` (`:470`). The knob is `PRAGMA fullfsync`
(`core/translate/pragma.rs:771–781`), and the whole handler is
`#[cfg(target_vendor = "apple")]` — elsewhere there is nothing to choose.

**The default is `Fsync`** (`core/storage/pager.rs:1977`, and
`get_sync_type()` is a compile-time constant `FileSyncType::Fsync` off Apple,
`:2111–2117`). So an out-of-the-box turso on macOS is on the **middle** rung:
22.67 µs per commit, 44 109 implied commits/s, and the write still in the
drive's volatile cache. `PRAGMA fullfsync=on` moves you to 2.97 ms and 337/s.
That is a 131× price, and it is the price of the guarantee most people assume
they already had.

Whether a commit syncs at all is a second, SQLite-compatible knob, `PRAGMA
synchronous`. The commit state machine issues the WAL sync only under
`SyncMode::Full` (`pager.rs:4740`), which is the default (`database.rs:2623`);
`NORMAL` skips the per-commit sync and leaves a commit durable only once a
later sync covers it, and `OFF` never syncs. Everything in this step assumes the
default.

The sync sits inside a documented three-phase commit protocol
(`wal.rs:4493–4498`): *prepare* — serialise frames and compute checksums;
*write + fsync* — "caller submits I/O and waits for durability"; *commit* —
update the WAL index and page metadata. `prepare_wal_finish`
(`wal.rs:4475–4491`) shows why the ordering is not cosmetic: it only calls
`coordination.mark_initialized()` inside the completion callback, and only `if
res.is_ok()` (`:4484–4486`), because "a failed sync must leave the WAL
uninitialized so the header is re-issued before the next append"
(`:4480–4483`).

In WAL mode, then, every commit pays its own sync, and the commit ceiling is the
inverse of the rung's latency. Turso's other storage mode shows the standard way
past that ceiling. In **MVCC mode** (turso's multi-version concurrency control
engine, which logs each transaction as one **logical-log** record — a
description of the rows changed rather than whole page images), commits go
through a **group commit** queue: a committing transaction takes a ticket and
joins `pending` (`core/mvcc/database/group_commit.rs:139–150`), and whichever
transaction reaches the log first becomes the leader — `take_work` hands it the
front of the queue *and every commit queued behind it* (`:172–180`), so one
append and one sync make the whole batch durable. It is on by default
(`group_commit_enabled: AtomicBool::new(true)`, `:82`) and switchable with
`PRAGMA mvcc_group_commit`. Run it once: with 10 transactions committing
concurrently against the `F_FULLFSYNC` rung, one-sync-per-commit costs
10 × 2.97 ms = 29.7 ms of syncing, and group commit costs one 2.97 ms sync — the
ceiling moves from 337 commits/s toward 10 × 337 = 3 370/s, bounded by how many
commits actually arrive during each sync.

*Why it matters:* a durability design that has not chosen between these two
calls has not chosen its guarantee, and the gap between them is larger than
almost any other decision in the engine.

### Step 5 — reads check the WAL first

> **In:** a read of page P by a transaction with a snapshot bounded by frames
> `[min_frame, max_frame]`.
> **Out:** either a frame number to read from the WAL, or `None`, meaning
> "read it from the database file".

Until frames are copied back into the database file, the newest version of a
page lives in the WAL — so every read consults a frame index first.
`find_frame` (`wal.rs:3637–3709`) does three things in order:

1. **Short-circuit.** If this reader's snapshot is fully backfilled —
   `max_frame < min_frame`, i.e. every frame it may see has already been copied
   into the database file — return `None` immediately and read the database file
   (`:3670–3677`). The comment above it (`:3662–3667`) notes this holds for any
   read mark, not only read-lock 0.
2. **Bound the search by the snapshot.** `min_frame` and `max_frame`
   (`:3678–3679`) are this transaction's visible window; a frame outside it is
   another transaction's and must not be seen.
3. **Look up.** Delegate to the coordination layer's frame index
   (`:3697–3699`), which is a shared-memory index with a local scanned-cache
   fallback used when the shared index has overflowed its reserved space
   (`wal.rs:2020–2027` — "Exhausting the reserved shared index space leaves the
   authority incomplete. Fall back to the local scanned cache rather than
   trusting a truncated shared index"). Just before the lookup,
   `ensure_local_frame_cache_covers` (`:3680–3689`) makes sure that fallback is
   complete for this snapshot.

A hit is followed by `read_frame` (`wal.rs:3713`) against the WAL file; a miss
falls through to the main database file.

The consequence worth pricing: a big uncheckpointed WAL makes *reads* slower.
Every page access pays an index lookup, the index covers more frames, and the
pages themselves are scattered through a growing file instead of sitting at
their home offsets. Checkpointing (Step 6) is therefore a read optimisation,
not just space reclamation — which is the opposite of the intuition that a
checkpoint is pure overhead.

That is why SQLite-family engines checkpoint automatically once the WAL grows
past a threshold, and turso's threshold is SQLite's `PRAGMA wal_autocheckpoint`.
After every commit the pager asks the WAL whether to checkpoint, passing the
connection's setting (`pager.rs:3448`, the check at `:4790–4791`):

```rust
// core/storage/wal.rs — WalFile::should_checkpoint, 4117-4123
  4117      fn should_checkpoint(&self, checkpoint_threshold: u32) -> bool {
  4118          if checkpoint_threshold == 0 {
  4119              return false;
  4120          }
  4121          let snapshot = self.load_coordination_snapshot();
  4122          snapshot.max_frame > u64::from(checkpoint_threshold) + snapshot.nbackfills
  4123      }
```

The line to focus on is 4122: checkpoint once the frames *not yet backfilled*
(`max_frame − nbackfills`) exceed the threshold. The threshold lives on the
`Connection`, defaults to **1000** frames (`database.rs:2590`), and `PRAGMA
wal_autocheckpoint = N` sets it (`translate/pragma.rs:446–457`); a value that
is negative or does not fit a 32-bit signed integer is stored as 0,
which line 4118 reads as "never checkpoint automatically". Run it once with the
default: 1000 frames of 4 KB pages + 24-byte headers is 1000 × 4 120 =
**4 120 000 bytes** (about 3.9 MiB) of WAL before an automatic checkpoint, so a
commit that touches 10 pages triggers one about every 100 commits.

*Why it matters:* left alone, read latency degrades with WAL length, and no
amount of write tuning fixes it. `wal_autocheckpoint` is the dial between read
latency and checkpoint frequency, and setting it to 0 hands checkpointing to
the application.

### Step 6 — checkpoint: moving frames home

> **In:** a WAL holding frames `[nbackfills+1 … max_frame]` and a database file
> that is behind.
> **Out:** those pages written into the database file, `nbackfills` advanced,
> and — in Restart/Truncate mode — a WAL whose entire contents have been
> invalidated at once by changing two `u32`s.

A checkpoint copies committed frames from the WAL back into the main database
file ("backfill"), after which the WAL can be reset. Turso implements the four
SQLite modes (`CheckpointMode`, `wal.rs:158–175`), each documented in place:

| mode | behaviour (paraphrasing `wal.rs:161–174`) |
|---|---|
| `Passive` | copy as many frames as possible without waiting for any reader or writer; never blocks either |
| `Full` | block until there is no writer and all readers are on the newest snapshot, then checkpoint everything and sync the DB file |
| `Restart` | as `Full`, then block until all readers read from the database file only, so the next writer restarts the log |
| `Truncate` | as `Restart`, then physically truncate the WAL file to zero bytes |

`Passive` and `Truncate` also take an optional `upper_bound_inclusive` frame
number (`:165`, `:174`) for callers that want to checkpoint only a prefix.
`should_restart_log()` (`:178–183`) is true for exactly `Restart` and
`Truncate`; `require_all_backfilled()` (`:186–188`) is true for everything
except `Passive`.

**The ordering.** The work list is built by `iter_latest_frames(min_frame,
max_frame)` — the latest visible frame per page — and then sorted:

```rust
// core/storage/wal.rs — checkpoint_inner, CheckpointState::Start, 5022-5029
  5022                      let mut to_checkpoint = if oc_max_frame < oc_min_frame {
  5023                          Vec::new()
  5024                      } else {
  5025                          self.coordination
  5026                              .iter_latest_frames(oc_min_frame, oc_max_frame)
  5027                      };
  5028                      // sort by frame_id for read locality
  5029                      to_checkpoint.sort_unstable_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)));
```

The line to focus on is 5029. The list is `Vec<(u64, u64)>` of "page_id +
frame_id combinations" (`wal.rs:2675–2676`, destructured as `let (page_id,
target_frame)` at `:5103`), so `a.1` is the **frame id**: the sort orders the
work by position in the WAL, giving sequential *reads*, not ascending page
order. (The in-process index returns the list sorted by page id,
`wal.rs:1006`; line 5029 overrides that.)

**The durability barrier.** Before a single page is copied, the checkpoint
fsyncs the WAL (`CheckpointState::SyncWal`, `wal.rs:5064–5068`, entered when
`needs_wal_sync` at `:5038` is true). The reasoning is in the comment at
`:5030–5037`: under `PRAGMA synchronous=NORMAL` commits do not fsync the WAL, so
"the checkpoint owes that fsync itself" — otherwise a crash during backfill
could persist some database pages while recovery drops the unsynced WAL tail
that produced them, "a torn database that matches no committed prefix"
(`:5058–5063`). Under the default `FULL` the barrier still issues its sync, though the
frames it covers were already synced at commit; only `synchronous=OFF` skips
it.

Write locality is obtained separately, and more cleverly. Read pages accumulate
in `pending_writes`, a `WriteBatch` whose `items` is a `BTreeMap<PageId,
Arc<Buffer>>` keyed by page id (`wal.rs:2566–2568`), and `write_pages_vectored`
(`sqlite3_ondisk.rs:676`) coalesces consecutive page ids into `writev` runs. The
comment above the loop states the split (`wal.rs:5069`: "reading is ordered by
frame ID, and writing ordered by page ID"), and `write_pages_vectored`'s own
comment does the arithmetic:

```rust
// core/storage/sqlite3_ondisk.rs — write_pages_vectored's contract, 663-676
   663  /// Write a batch of pages to the database file.
   664  ///
   665  /// we have a batch of pages to write, lets say the following:
   666  /// (they are already sorted by id thanks to BTreeMap)
   667  /// `[1,2,3,6,7,9,10,11,12]`
   668  //
   669  /// we want to collect this into runs of:
   670  /// `[1,2,3]`, `[6,7]`, `[9,10,11,12]`
   671  /// and submit each run as a `writev` call,
   672  /// for 3 total syscalls instead of 9.
   673  ///
   674  /// Each run's write is added to `group` before it is submitted. Returns
   675  /// the number of runs submitted.
   676  pub fn write_pages_vectored(
```

The line to focus on is 672.

Nine pages, three syscalls — a **3×** reduction on that example, and the
BTreeMap gives the ordering for free rather than by an explicit sort. So the
checkpoint gets sequential reads from the frame-id sort *and* sequential-ish
writes from the page-id map, on opposite sides of the same loop.

Restart and Truncate then change the **salts** (Step 3, `wal.rs:1850–1852`) —
that is how every old frame in the reused file dies at once, without a single
byte being erased.

*Why it matters:* checkpointing is where a WAL design's costs come due, and the
two sorts above are the difference between a checkpoint that streams and one
that random-seeks twice.

### Step 7 — recovery: find the cliff edge

> **In:** a WAL file of unknown validity and a database file that may be behind
> it.
> **Out:** one number — `max_frame`, the index of the last valid commit frame —
> after a single forward pass with no redo and no undo.

Now the payoff. Recovery validates the WAL header's checksum, then walks frames
in order, checking salts and the cumulative checksum chain, remembering the
position of the last frame with `db_size != 0`. First bad frame ⇒ stop. The
answer is the last valid **commit**, not the last valid frame.

Both stopping conditions and the commit rule are in one loop
(`sqlite3_ondisk.rs:1883–1959`):

```rust
// core/storage/sqlite3_ondisk.rs — StreamingWalReader::process_frames, 1908-1955
  1908              if s1 != header.salt_1 || s2 != header.salt_2 {
        // ... 1909-1919: tracing::debug! calls ...
  1920                  break;
  1921              }
  1922
  1923              let seed = checksum_wal(&fh[0..8], header, st.cumulative_checksum, use_native);
  1924              let calc = checksum_wal(page, header, seed, use_native);
  1925              if calc != (c1, c2) {
        // ... 1926-1933: tracing::debug! ...
  1934                  break;
  1935              }
  1936
  1937              st.cumulative_checksum = calc;
  1938              let frame_idx = st.frame_idx;
  1939              st.pending_frames
  1940                  .entry(page_no as u64)
  1941                  .or_default()
  1942                  .push(frame_idx);
  1943
  1944              if db_size > 0 {
  1945                  st.last_valid_frame = st.frame_idx;
  1946                  st.last_valid_checksum = calc;
        // ... 1947-1952: tracing::debug! ...
  1953                  self.flush_pending_frames(&mut st);
  1954              }
  1955              st.frame_idx += 1;
```

Trace the three exits. A zero page number stops the scan (`:1902–1907`). A salt
mismatch stops it (`:1908`) — that frame belongs to a previous generation. A
chained-checksum mismatch stops it (`:1925`) — that frame, and therefore
everything after it, is untrustworthy. And note `pending_frames`
(`:1939–1942`): frames are *staged* per page and only published by
`flush_pending_frames` when a commit frame arrives (`:1944–1953`). A
half-written transaction's intact frames are read, staged, and then simply
never published.

`finalize_loading` (`:1986–2029`) commits the answer, and says the essential
thing in a comment:

```rust
// core/storage/sqlite3_ondisk.rs — finalize_loading, 1996-2016
  1996          let max_frame = st.last_valid_frame;
  1997          if max_frame > 0 {
  1998              let mut frame_cache = wfs.runtime.frame_cache.lock();
  1999              for frames in frame_cache.values_mut() {
  2000                  frames.retain(|&f| f <= max_frame);
  2001              }
        // ... 2002-2013: drop empty entries; record overflow-fallback coverage ...
  2014          wfs.metadata.max_frame.store(max_frame, Ordering::SeqCst);
  2015          // use checksum of last valid commit frame, not necessarily the last frame
  2016          wfs.metadata.last_checksum = st.last_valid_checksum;
```

The chain is resumed from the last valid **commit** frame's checksum (`:2016`),
not the last frame that happened to verify — so the next append continues a
chain that recovery will agree with.

Place it on the topic's axis: postgres must *redo* (its log holds deltas, not
page images); ARIES must redo *and* undo; LMDB does nothing at all (the
meta-page flip made commit atomic). Turso's recovery is deciding where the log
ends — the entire complexity was prepaid in the format, in Steps 1–3.

*Why it matters:* this is the cheapest correct recovery in the topic, and the
reason is not cleverness in the recovery code. It is that Steps 1, 2 and 3 each
removed a class of question recovery would otherwise have had to answer.

## Where each step lives in the code

Anchors verified at tursodatabase/turso@a820079e8.

- **Step 1 — the format**: `WAL_FRAME_HEADER_SIZE = 24`
  `sqlite3_ondisk.rs:405`; `WalHeader` `:411–444` (magic, format version, page
  size, `checkpoint_seq`, the two salts, header checksum); `WalFrameHeader`
  `:472–496`.
- **Step 2 — commit marker**: `db_size` documented `:481–482`;
  `is_commit_frame()` `:499–501`.
- **Step 3 — checksums and salts**: `prepare_wal_frame` `:2156–2174`, header
  fields `prepare_wal_frame_header` `:2180–2193`, chain seeding
  `recompute_wal_frame_checksum` `:2196–2213`; `checksum_wal` `:2299–2324`; salt
  regeneration `wal.rs:1845–1895` (`checkpoint_seq+1` `:1850`, `salt_1+1`
  `:1851`, random `salt_2` `:1852`).
- **Step 4 — commit + sync**: `FileSyncType` `core/io/mod.rs:126–136`; the
  syscalls `core/io/unix.rs:455–472` (`fsync` `:460`, `F_FULLFSYNC` `:462`,
  non-Apple `:470`); `PRAGMA fullfsync` `core/translate/pragma.rs:771–781`;
  default `Fsync` `core/storage/pager.rs:1977` and `:2111–2117`; commit sync only
  under `SyncMode::Full` `pager.rs:4740`, default `Full`
  `core/database.rs:2623`; `prepare_wal_finish` `wal.rs:4475–4491`; three-phase
  protocol comment `wal.rs:4493–4498`. MVCC group commit
  `core/mvcc/database/group_commit.rs` — enabled by default `:82`, `enqueue`
  `:139–150`, `take_work` `:163–185`.
- **Step 5 — reads**: `find_frame` `wal.rs:3637–3709` — short-circuit
  `:3670–3677`, snapshot window `:3678–3679`, delegation `:3697–3699`; index
  with fallback `wal.rs:2013–2030`; `read_frame` `:3713`. Auto-checkpoint:
  `should_checkpoint` `wal.rs:4117–4123`; threshold default 1000
  `database.rs:2590`; passed per commit `pager.rs:3448`, checked `:4790–4791`;
  `PRAGMA wal_autocheckpoint` `translate/pragma.rs:446–457`.
- **Step 6 — checkpoint**: `CheckpointMode` `wal.rs:158–175`,
  `should_restart_log` `:178–183`, `require_all_backfilled` `:186–188`;
  `checkpoint_inner` `:4946`, work list and frame-id sort `:5022–5029`, WAL
  durability barrier `:5030–5038` and `:5058–5068`, `pages_to_checkpoint` type
  `:2675–2676`, processing loop `:5074–5208`; `WriteBatch` `:2566–2571`;
  `write_pages_vectored` `sqlite3_ondisk.rs:663–676`; header restart
  `wal.rs:6053–6059`.
- **Step 7 — recovery**: `StreamingState` `sqlite3_ondisk.rs:1707–1718`;
  header validation `handle_header_read` `:1796–1859` (checksum compare
  `:1826`); `process_frames` `:1883–1959` — zero page `:1902`, salt `:1908`,
  chained checksum `:1923–1925`, commit `:1944–1954`; `finalize_loading`
  `:1986–2029`, the answer at `:2014–2016`.

## Questions to answer in notes.md

1. Why do frames carry whole page images instead of deltas? Name the two things
   this buys (no redo logic; torn-page immunity — a torn frame fails its
   checksum and everything after it is discarded) and the one it costs. Then do
   the arithmetic from Step 1 for *your* M5 workload: bytes/txn under page
   images versus under deltas.
2. Why salts AND checksums? Construct the failure that checksums alone miss.
   (WAL reset reuses the file; an old frame at the right offset can have a valid
   *internal* checksum — but was chained from stale salts. Note that
   `prepare_wal_frame` checksums `frame[0..8]` and the page, never the salt
   bytes at 8..16.)
3. Turso's default is `FileSyncType::Fsync`. On the machine in `notes.md`, what
   is the single-connection commit ceiling on the default, and on `PRAGMA
   fullfsync=on`? Which of those two numbers would you quote in a durability
   claim, and why?
4. For your experiment's WAL: page images or logical records? Decide and justify
   with the M5 workload — small graph mutations ⇒ logical records win on volume,
   but then you owe idempotent redo, which means an LSN on every page and a
   full-page-image scheme for torn writes.

## Done when

Answer each before unfolding it.

- [ ] Narrate recovery over a WAL that contains, in order: two committed
      transactions, a complete-but-uncommitted transaction, and a torn frame.
      Say what survives.

  <details><summary>Answer</summary>

  The scan verifies the header checksum (`sqlite3_ondisk.rs:1826`), then walks frames. Both
  committed transactions pass the salt and chain checks; each ends in a frame
  with `db_size > 0`, so at each one `last_valid_frame` advances and the staged
  `pending_frames` are published (`:1944–1953`). The third transaction's frames
  also verify and are staged — but no commit frame ever follows, so they are
  never published. The torn frame fails the chained checksum and `break`s the
  loop (`:1925–1935`). `finalize_loading` sets `max_frame` to the *second*
  transaction's last frame and resumes the chain from *its* checksum (`:2016`).
  Both damaged suffixes vanish; nothing is undone, because nothing was ever
  applied.

  </details>

- [ ] Explain why a valid checksum is not sufficient to accept a frame.

  <details><summary>Answer</summary>

  Because the WAL file is *reset*, not deleted, at Restart/Truncate checkpoints.
  A frame from the file's previous life can sit at exactly the right offset with
  a checksum that verifies against a chain seeded by the *old* header. The salts
  are the generation stamp: `restart_snapshot_from_authority`
  (`wal.rs:1845–1895`) increments `salt_1` and draws a fresh random `salt_2`, so
  a stale frame's copied salts no longer match the header's and the scan stops
  at `:1908` before it ever computes a checksum.

  </details>

- [ ] Turso's checkpoint sorts its work list. Say what it sorts by and what
      that buys — and what gives it locality on the *other* side of the copy.

  <details><summary>Answer</summary>

  It sorts by **frame id**, not page id — `sort_unstable_by(|a, b| (a.1,
  a.0).cmp(&(b.1, b.0)))` with the comment "sort by frame_id for read locality"
  (`wal.rs:5028–5029`). That makes the *reads* from the WAL sequential. Write
  locality comes from a different mechanism: pages accumulate in a
  `BTreeMap<PageId, Arc<Buffer>>` keyed by page id, and `write_pages_vectored`
  merges consecutive runs into `writev` calls — its example turns nine pages
  into three syscalls (`sqlite3_ondisk.rs:663–676`).

  </details>

- [ ] Someone benchmarks turso on a Mac, sees 40 000 commits/s, and calls it
      durable. What is wrong?

  <details><summary>Answer</summary>

  They are on the default `FileSyncType::Fsync` (`pager.rs:1977`), which on
  Apple is `libc::fsync` (`io/unix.rs:460`). Turso's own doc comment says it
  "may not flush disk write cache on macOS" (`io/mod.rs:131`). 40 000/s is right
  next to the measured `fsync` ceiling of 44 109/s — the giveaway. Real
  power-loss durability needs `PRAGMA fullfsync=on`
  (`translate/pragma.rs:771–781`) → `fcntl(F_FULLFSYNC)` (`io/unix.rs:462`),
  measured at 2.97 ms, a ceiling of **337/s**. Their number is 131× too high
  for the guarantee they claimed.

  </details>

- [ ] Turso has no full-page-write machinery and no `checkpoint_timeout`
      sawtooth. Say why, in one sentence, and name what it pays instead.

  <details><summary>Answer</summary>

  Because it never overwrites a page in place: a commit appends whole page
  images to the WAL, so a torn frame is discarded by its checksum and the
  previous version of the page is still intact elsewhere — there is no
  half-updated page for a full-page image to repair. It pays for this in steady
  state instead of in bursts: every commit writes 4 KB + 24 bytes per dirtied
  page, whatever the transaction changed, and read latency degrades with WAL
  length until a checkpoint runs (Step 5).

  </details>

## References

**Code** — all anchors read at `tursodatabase/turso@a820079e8`; local clone at
`~/repos/turso`, pin recorded in `resources/codebases.md`.

| file | what this chapter took from it |
|---|---|
| `core/storage/sqlite3_ondisk.rs` | frame and header layout (Steps 1–2), checksum chain (Step 3), the recovery scan (Step 7), `write_pages_vectored` (Step 6) |
| `core/storage/wal.rs` | salt regeneration (Step 3), sync sequencing (Step 4), `find_frame` (Step 5), checkpoint modes and the backfill loop (Step 6) |
| `core/io/mod.rs`, `core/io/unix.rs` | `FileSyncType` and the two system calls behind it (Step 4) |
| `core/translate/pragma.rs`, `core/storage/pager.rs`, `core/database.rs` | `PRAGMA fullfsync`, the default rung and `synchronous` (Step 4); `PRAGMA wal_autocheckpoint` and the per-commit checkpoint check (Step 5) |
| `core/mvcc/database/group_commit.rs` | MVCC-mode group commit — one sync for a queue of commits (Step 4) |

**Format specification** — the on-disk layout is SQLite's, documented at
<https://sqlite.org/walformat.html>; turso's structs carry the same field names
and the same big-endian encoding, and its checkpoint modes match
<https://www.sqlite.org/c3ref/wal_checkpoint_v2.html> (cited in the source at
`wal.rs:5179`).

**Measurements** — `topics/05-durability-wal/notes.md`, "Baseline (provided
lane, Apple M3 Pro / APFS, measured 2026-07-28)", produced by
`experiments/src/bin/fsync_ladder.rs`. `FINDINGS.md` row 5 carries the
headline.
