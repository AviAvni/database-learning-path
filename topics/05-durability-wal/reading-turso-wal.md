# Turso's WAL: recovery is finding where the log ends

This is SQLite's WAL mode in Rust: commits append whole page images as frames,
a chained checksum makes the log's valid prefix self-evident, and recovery has
no redo or undo at all — it just decides where the log ends. Of the four
durability designs in this topic, this is the one your experiment should steal
most from — so before the code, this chapter builds it piece by piece: the
frame, the commit marker, the checksum chain and salts, the read path, the
checkpoint, and finally the recovery loop that all of them exist to make
trivial.

## The problem in one sentence

After `kill -9` mid-commit, the tail of the log file is arbitrary garbage —
half-written frames, stale bytes from a previous log generation — and
recovery must decide, from file contents alone, exactly which prefix of the
log to trust, with zero tolerance for accepting one corrupt or uncommitted
byte.

## The concepts, step by step

### Step 1 — the frame: log whole page images, not operations

Turso's WAL is a file of **frames**, and a frame is a complete copy of one
4 KB database page plus a 24-byte header (which page number this is, plus
the fields in Steps 2–3). When a transaction commits, every page it
modified is appended to the WAL as a frame — the main database file is not
touched at all. Compare the alternatives from this topic: postgres logs
*deltas* ("change this tuple") and must replay them onto pages at recovery;
redis logs *commands* and must re-execute them. Page images are the
maximalist choice: the log **is** the data, so recovery needs no
replay logic whatsoever — and, as a free bonus, torn pages can't hurt you
(a half-written frame fails its checksum and is discarded whole; the
previous version of the page still exists untouched elsewhere). The cost is
volume: WAL bytes ∝ *pages touched*, not bytes changed — a 1-byte update
logs 4 KB.

### Step 2 — the commit marker: db_size turns frames into transactions

A transaction is multiple frames, and the log needs to say where one ends —
otherwise recovery couldn't tell "committed" from "half-appended". The
frame header's **db_size** field does it with zero extra records: it is 0
on ordinary frames and holds the database's new size (in pages) on the
*last* frame of a transaction. A frame with `db_size != 0` **is** the
commit record. Recovery's rule follows immediately: the state after a crash
is defined by the last valid frame with `db_size != 0` — frames after it
may be perfectly intact, but with no commit marker following them they are
invisible. No separate commit record, no transaction table.

### Step 3 — the checksum chain and the salts: making the valid prefix self-evident

Each frame carries a checksum, but not an independent one — checksums are
**cumulative**: frame N's checksum is computed over frame N's contents
*seeded with frame N−1's checksum*. One flipped bit anywhere invalidates
that frame *and every frame after it* — which is exactly what you want:
the log's trustworthy prefix ends at the first bad checksum, and nothing
past a corruption can masquerade as valid.

```
 WAL file:  [hdr]  [frame p5][frame p2][frame p9*]  [frame p5][frame p1*] …
                    ── txn 1, *commit (db_size≠0) ──  ── txn 2 ──
 checksum:   c0 ──► c1 ──────► c2 ─────► c3 ─────────► c4 ──────► c5   (chained)
 salts: change on WAL reset — a stale frame from a previous WAL generation
        fails the salt check even if its checksum chain looks plausible
```

The chain has one blind spot: the WAL file is *reused* after a checkpoint
(reset, not deleted), so a frame from the file's previous life can sit at
exactly the right offset with an internally consistent checksum. The fix is
two **salts** — random values in the WAL header, regenerated on every WAL
reset and copied into every frame header. A frame whose salts don't match
the current header's is from a dead generation, whatever its checksum says.
Two cheap u32 comparisons close the hole.

### Step 4 — commit means fsync — and on macOS, which fsync matters

A commit is durable only when the OS confirms the frames reached stable
storage, so after appending the commit frame turso fsyncs the WAL file. The
trap this codebase makes explicit in its types: on macOS, plain `fsync()`
does **not** flush the drive's write cache — data can sit in the SSD's
volatile buffer and vanish on power loss. Turso's `FileSyncType`
distinguishes `Fsync` from `FullFsync` (the macOS `F_FULLFSYNC` fcntl,
which does flush the drive cache). Your fsync_ladder experiment will show
the gap; it's not subtle — plain fsync is ~100 µs-fast and weak,
F_FULLFSYNC is ms-scale and honest. A durability design that hasn't chosen
between them hasn't chosen its guarantee.

### Step 5 — reads check the WAL first

Until frames are copied back into the database file, the newest version of
a page lives in the WAL — so every read consults an in-memory map of
page number → latest frame; a hit reads the frame from the WAL file, a miss
falls through to the main database file. The consequence worth pricing: a
big uncheckpointed WAL makes *reads* slower — more frames for the map to
cover, an extra lookup on every page access. Checkpointing (Step 6) is
therefore a read optimization, not just space reclamation.

### Step 6 — checkpoint: moving frames home

A checkpoint copies committed frames from the WAL back into the main
database file ("backfill"), after which the WAL can be reset. Turso
implements the four SQLite modes — Passive (copy what you can, never
block), Full, Restart, and Truncate (progressively stronger: finish the
backfill, force the next writer to start a fresh WAL, physically shrink the
file) — and copies frames **sorted by page number for locality**: the
database file is written in ascending page order, sequential-ish IO instead
of commit-order scatter. Restart/Truncate change the **salts** — that is
how every old frame in the reused file dies at once (Step 3) without a
single byte being erased.

### Step 7 — recovery: find the cliff edge

Now the payoff — recovery is a single forward scan with no redo and no
undo: validate the WAL header's checksum, then walk frames in order,
checking salts (Step 3) and the cumulative checksum chain, remembering the
position of the last frame with `db_size != 0` (Step 2). First bad frame ⇒
stop — that's the cliff edge; the answer is the last valid **commit**, not
the last valid frame. A half-written transaction's frames are physically
present but unreachable. The whole recovery, in one loop:

```rust
// Walk frames; the answer is the last valid COMMIT, not the last valid frame.
fn recover(frames: &[Frame], hdr: &WalHeader) -> u64 {
    let mut c = hdr.checksum;
    let mut last_commit = 0;
    for (i, f) in frames.iter().enumerate() {
        if f.salts != hdr.salts { break; }     // stale frame from an old WAL generation
        c = chain(c, f);                       // cumulative: one bad bit ends the log
        if c != f.checksum { break; }          // torn frame ⇒ the cliff edge
        if f.db_size != 0 {                    // commit frame
            last_commit = i as u64 + 1;        // a half-written txn's frames stay
        }                                      // present but UNREACHABLE
    }
    last_commit
}
```

Place it on the topic's axis: postgres must *redo* (its log holds deltas,
not page images); ARIES must redo *and undo*; LMDB does nothing at all (the
meta-page flip made commit atomic). Turso's recovery is deciding where the
log ends — the entire complexity was prepaid in the format.

## Where each step lives in the code

- **Step 1 — the format**: `WalHeader` — sqlite3_ondisk.rs:411–443 (magic,
  version, the two salts, header checksum); `WalFrameHeader` — :477–495
  (24 bytes — page_no, db_size, salts, checksum_1/2).
- **Steps 2–3 — frame write**: sqlite3_ondisk.rs:2058–2090 — cumulative
  checksums, each frame's checksum seeding the next (:2080–2088).
- **Step 4 — commit + sync**: `prepare_wal_finish` — wal.rs:4130–4145
  (fsync after the commit frame); `FileSyncType` — io/mod.rs:128–134
  (`Fsync` vs `FullFsync`).
- **Step 5 — reads**: `find_frame` — wal.rs:3335–3404 (the in-memory
  page→latest-frame map); hit ⇒ `read_frame` (:3409) from the WAL file.
- **Step 6 — checkpoint**: `CheckpointMode` — wal.rs:160–183 (Passive /
  Full / Restart / Truncate); `checkpoint_inner` — wal.rs:4594–4672 —
  backfill loop copies frames [nbackfills+1 … max_frame] into the DB file,
  sorted by frame for locality; Restart/Truncate change the salts.
- **Step 7 — recovery**: `WalScan` — sqlite3_ondisk.rs:1426–1932: validate
  header checksum (:1727), walk frames verifying salt + chained checksum
  (:1830–1831), remember the last frame with `db_size > 0` (:1844–1855);
  final state = last valid COMMIT (:1923), not last valid frame.

## Questions to answer in notes.md

1. Why do frames carry whole page images instead of deltas? Name the two
   things this buys (no redo logic; torn-page immunity — a torn frame fails
   its checksum and everything after is discarded) and the one it costs (WAL
   volume ∝ pages touched, not bytes changed).
2. Why salts AND checksums? Construct the failure that checksums alone miss.
   (WAL reset reuses the file; an old frame at the right offset can have a
   valid *internal* checksum — but chains from stale salts.)
3. For your experiment's WAL: page images or logical records? Decide and
   justify with the M5 workload (small graph mutations ⇒ logical records win
   on volume, but then you owe idempotent redo — LSN-stamped pages).

## Done when

You can narrate recovery over a WAL containing a torn frame mid-transaction
and a complete-but-uncommitted transaction, and say what survives (everything
up to the last valid commit frame; both damaged suffixes vanish).

## References

**Code**
- [tursodatabase/turso](https://github.com/tursodatabase/turso) —
  `core/storage/wal.rs`, `core/storage/sqlite3_ondisk.rs`,
  `core/io/mod.rs`. Local clone at `~/repos/turso`.
