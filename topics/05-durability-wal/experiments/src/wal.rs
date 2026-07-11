//! A write-ahead log with group commit. YOU implement this.
//!
//! On-disk format (fixed by the tests — do not change):
//!
//! ```text
//! record  := lsn:u64le  txn_id:u64le  len:u32le  crc:u32le  payload[len]
//! commit  := a record whose payload is the 8 bytes b"__COMMIT"
//! ```
//!
//! - `crc` is crc32fast over payload ONLY.
//! - Records are appended; the file is never overwritten.
//! - `append` buffers in memory; nothing is durable until `commit(txn_id)`
//!   returns — commit writes the commit record, then fsyncs, then returns.
//! - `commit_many` is the group-commit API: one write + ONE fsync covers all
//!   queued transactions. This is what the crash test and the bench exercise.
//! - Recovery (`replay`) walks the file, verifies each CRC, and stops at the
//!   first invalid/truncated record — everything before the break from
//!   *committed* transactions is returned, in order. Records from
//!   transactions with no commit record are dropped (turso-style: recovery is
//!   deciding where the log ends + which txns crossed the line).
//!
//! Deliberately NOT specified (your design calls — justify in notes.md):
//! - group-commit trigger (size? time? both?)
//! - whether replay needs an LSN→page idempotence check (it doesn't here —
//!   why not? when would it? see reading-aries.md Q3)

use std::io;
use std::path::Path;

pub const COMMIT_PAYLOAD: &[u8] = b"__COMMIT";

pub struct Wal {
    // your fields
}

/// A payload record recovered from the log, from a committed transaction.
#[derive(Debug, PartialEq, Eq)]
pub struct Recovered {
    pub lsn: u64,
    pub txn_id: u64,
    pub payload: Vec<u8>,
}

impl Wal {
    /// Open (or create) the log at `path`. Must NOT truncate an existing log.
    pub fn open(path: &Path) -> io::Result<Wal> {
        let _ = path;
        todo!("open or create, seek to end, remember next LSN")
    }

    /// Buffer a record for `txn_id`. Returns its LSN. Not durable yet.
    pub fn append(&mut self, txn_id: u64, payload: &[u8]) -> io::Result<u64> {
        let (_, _) = (txn_id, payload);
        todo!()
    }

    /// Make everything appended for `txn_id` durable: write buffered records
    /// + a commit record, fsync, return. One fsync per call.
    pub fn commit(&mut self, txn_id: u64) -> io::Result<()> {
        let _ = txn_id;
        todo!()
    }

    /// Group commit: durably commit ALL listed transactions with a single
    /// write + a single fsync. The whole point of this module.
    pub fn commit_many(&mut self, txn_ids: &[u64]) -> io::Result<()> {
        let _ = txn_ids;
        todo!()
    }

    /// How many fsyncs this Wal has issued (for the bench + tests).
    pub fn fsync_count(&self) -> u64 {
        todo!()
    }

    /// Recover: return payload records of committed transactions, in log
    /// order. Tolerates a torn/garbage tail (stop at first bad CRC or
    /// truncated record — that is the end of the log, not an error).
    pub fn replay(path: &Path) -> io::Result<Vec<Recovered>> {
        let _ = path;
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write as _;

    #[test]
    fn commit_then_replay_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(1, b"put a=1").unwrap();
            wal.append(1, b"put b=2").unwrap();
            wal.commit(1).unwrap();
        }
        let recs = Wal::replay(&path).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].payload, b"put a=1");
        assert_eq!(recs[1].payload, b"put b=2");
        assert!(recs.iter().all(|r| r.txn_id == 1));
    }

    #[test]
    fn uncommitted_txn_is_invisible_after_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(1, b"committed").unwrap();
            wal.commit(1).unwrap();
            wal.append(2, b"never committed").unwrap();
            // force the orphan records to disk WITHOUT a commit record
            wal.commit_many(&[]).ok(); // no-op is fine; simulate via drop
        }
        let recs = Wal::replay(&path).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].payload, b"committed");
    }

    #[test]
    fn torn_tail_is_ignored_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(7, b"survives").unwrap();
            wal.commit(7).unwrap();
        }
        // simulate a torn write: garbage bytes at the tail
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02]).unwrap();

        let recs = Wal::replay(&path).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].payload, b"survives");
    }

    #[test]
    fn corrupted_middle_truncates_everything_after() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(1, b"first").unwrap();
            wal.commit(1).unwrap();
            wal.append(2, b"second").unwrap();
            wal.commit(2).unwrap();
        }
        // flip one byte in the middle of the file
        let mut bytes = std::fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        let recs = Wal::replay(&path).unwrap();
        // everything from the corruption point on is gone; nothing panics
        assert!(recs.len() <= 1);
    }

    #[test]
    fn commit_many_uses_one_fsync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        let mut wal = Wal::open(&path).unwrap();
        for txn in 0..64u64 {
            wal.append(txn, format!("txn {txn}").as_bytes()).unwrap();
        }
        let before = wal.fsync_count();
        wal.commit_many(&(0..64).collect::<Vec<_>>()).unwrap();
        assert_eq!(wal.fsync_count() - before, 1, "group commit = ONE fsync");

        drop(wal);
        let recs = Wal::replay(&path).unwrap();
        assert_eq!(recs.len(), 64);
    }

    #[test]
    fn reopen_appends_does_not_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(1, b"old").unwrap();
            wal.commit(1).unwrap();
        }
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(2, b"new").unwrap();
            wal.commit(2).unwrap();
        }
        let recs = Wal::replay(&path).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].payload, b"old");
        assert_eq!(recs[1].payload, b"new");
        assert!(recs[0].lsn < recs[1].lsn, "LSNs monotonic across reopen");
    }
}
