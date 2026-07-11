# Reading turso's WAL — frames, checksum chain, checkpoint (1.5–2 h)

Repo: [`~/repos/turso`](https://github.com/tursodatabase/turso). Files: `core/storage/wal.rs`,
`core/storage/sqlite3_ondisk.rs`, `core/io/mod.rs`. This is SQLite's WAL mode
in Rust — the design your experiment should steal most from.

## 1. The format — page images, not operations

- `WalHeader` — sqlite3_ondisk.rs:411–443: magic, version, **two salts**,
  header checksum.
- `WalFrameHeader` — :477–495: 24 bytes — page_no, **db_size** (non-zero ⇒
  this frame commits), the salts (copied from the header), checksum_1/2.
- Frame write — :2058–2090: checksums are **cumulative**: each frame's checksum
  seeds the next (:2080–2088). One flipped bit invalidates the entire suffix —
  exactly what you want: recovery can trust everything before the break.

```
 WAL file:  [hdr]  [frame p5][frame p2][frame p9*]  [frame p5][frame p1*] …
                    ── txn 1, *commit (db_size≠0) ──  ── txn 2 ──
 checksum:   c0 ──► c1 ──────► c2 ─────► c3 ─────────► c4 ──────► c5   (chained)
 salts: change on WAL reset — a stale frame from a previous WAL generation
        fails the salt check even if its checksum chain looks plausible
```

## 2. Commit + sync

- `prepare_wal_finish` — wal.rs:4130–4145: fsync after the commit frame;
  `FileSyncType` (io/mod.rs:128–134) distinguishes `Fsync` from **`FullFsync`
  (macOS F_FULLFSYNC)** — on your Mac, plain fsync does NOT flush the drive
  cache. Your fsync_ladder experiment will show the gap; it's not subtle.

## 3. Reads check the WAL first

- `find_frame` — wal.rs:3335–3404: in-memory page→latest-frame map; hit ⇒
  `read_frame` (:3409) from the WAL file; miss ⇒ read the main DB.
- Consequence: an uncheckpointed WAL makes every read do a map lookup, and a
  HUGE WAL makes reads slower (more frames to cover) — checkpointing is a read
  optimization, not just space reclamation.

## 4. Checkpoint — moving frames home

- `CheckpointMode` — wal.rs:160–183: Passive / Full / Restart / Truncate.
- `checkpoint_inner` — wal.rs:4594–4672: backfill loop copies frames
  [nbackfills+1 … max_frame] into the DB file, **sorted by frame for locality**.
- Restart/Truncate change the salts — that's how old frames die without being
  erased.

## 5. Recovery — find the cliff edge

- `WalScan` — sqlite3_ondisk.rs:1426–1932: validate header checksum (:1727),
  then walk frames verifying salt + chained checksum (:1830–1831); remember
  the last frame with `db_size > 0` (:1844–1855); final state = last valid
  COMMIT (:1923), not last valid frame — a half-written transaction's frames
  are present but unreachable.

No undo, no redo logic — recovery is *deciding where the log ends*. Compare
postgres (redo required: its log holds deltas, not page images) and LMDB
(nothing at all: the meta flip made commit atomic).

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
