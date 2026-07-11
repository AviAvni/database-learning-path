//! MVCC with snapshot isolation over an in-memory KV store. YOU implement this.
//!
//! The contract (fixed by the tests below — read them first):
//! - `begin(mode)` takes a snapshot: the txn sees exactly the versions
//!   committed before its start, plus its own writes.
//! - Writes are buffered in the txn; nothing is visible until commit.
//! - Commit under Mode::Snapshot = first-committer-wins: if any key in MY
//!   write set was committed by someone else after MY snapshot →
//!   Err(WriteConflict). (Berenson '95 §4 — label your tests with P/A numbers.)
//! - Commit under Mode::Serializable = additionally validate the READ set:
//!   if any key I read was committed after my snapshot → Err(ReadConflict).
//!   (Backward OCC validation — stricter than postgres SSI, zero false
//!   negatives for write skew; count the false positives later.)
//! - Dropping a Txn without commit = abort, no effects.
//! - `gc()` drops versions invisible to every active txn; returns # dropped.
//!
//! Suggested shape (not enforced): Inner { versions: HashMap<Key, Vec<Version>>,
//! next_ts: u64, active: BTreeSet<u64> } behind a Mutex — a single global
//! mutex protecting METADATA is fine; the point is transactions don't hold
//! it between operations (unlike the global-lock baseline in txn_bench).
//! A Version is { ts: u64, value: Option<Vec<u8>> } — None = tombstone.
//! Deleted-by is implicit: a newer version shadows. (That's the Wu/Pavlo
//! "append-only, newest-to-oldest" corner; note where postgres/Hekaton differ.)

use std::sync::Arc;

pub type Key = Vec<u8>;
pub type Val = Vec<u8>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Snapshot,
    Serializable,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommitError {
    /// A key in my write set was committed by another txn after my snapshot.
    WriteConflict,
    /// Serializable only: a key in my read set was committed after my snapshot.
    ReadConflict,
}

#[derive(Clone)]
pub struct Mvcc {
    #[allow(dead_code)] // used once you implement Inner
    inner: Arc<Inner>,
}

struct Inner {
    // YOU design this. See the module doc for a suggested shape.
}

pub struct Txn<'a> {
    db: &'a Mvcc,
    // snapshot ts, mode, write buffer, read set, ...
}

impl Mvcc {
    pub fn new() -> Self {
        todo!()
    }

    pub fn begin(&self, mode: Mode) -> Txn<'_> {
        let _ = mode;
        todo!()
    }

    /// Drop versions invisible to every active txn. Returns versions dropped.
    pub fn gc(&self) -> usize {
        todo!()
    }

    /// Total live version records (for the GC test).
    pub fn version_count(&self) -> usize {
        todo!()
    }
}

impl Default for Mvcc {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Txn<'a> {
    /// Own writes first, then newest version with ts <= my snapshot.
    /// Records the key in the read set when mode == Serializable.
    pub fn get(&mut self, key: &[u8]) -> Option<Val> {
        let _ = (key, &self.db);
        todo!()
    }

    pub fn put(&mut self, key: &[u8], val: &[u8]) {
        let _ = (key, val);
        todo!()
    }

    pub fn delete(&mut self, key: &[u8]) {
        let _ = key;
        todo!()
    }

    /// Validate, then install all writes at a single new commit ts.
    pub fn commit(self) -> Result<(), CommitError> {
        todo!()
    }
}

// Dropping a Txn must abort it (deregister from active set so GC advances).

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    fn setup(pairs: &[(&str, &str)]) -> Mvcc {
        let db = Mvcc::new();
        let mut t = db.begin(Mode::Snapshot);
        for (key, val) in pairs {
            t.put(key.as_bytes(), val.as_bytes());
        }
        t.commit().unwrap();
        db
    }

    #[test]
    fn snapshot_reads_are_stable() {
        // A5A read skew must NOT happen: my snapshot never moves.
        let db = setup(&[("x", "1")]);
        let mut reader = db.begin(Mode::Snapshot);
        assert_eq!(reader.get(b"x"), Some(k("1")));

        let mut writer = db.begin(Mode::Snapshot);
        writer.put(b"x", b"2");
        writer.commit().unwrap();

        assert_eq!(reader.get(b"x"), Some(k("1")), "snapshot must be stable");
        // a NEW txn sees the new value
        let mut later = db.begin(Mode::Snapshot);
        assert_eq!(later.get(b"x"), Some(k("2")));
    }

    #[test]
    fn uncommitted_writes_are_invisible() {
        // P1 dirty read must not happen.
        let db = setup(&[]);
        let mut w = db.begin(Mode::Snapshot);
        w.put(b"x", b"dirty");
        let mut r = db.begin(Mode::Snapshot);
        assert_eq!(r.get(b"x"), None);
        drop(w); // abort — still invisible forever
        let mut r2 = db.begin(Mode::Snapshot);
        assert_eq!(r2.get(b"x"), None);
    }

    #[test]
    fn read_your_own_writes_and_deletes() {
        let db = setup(&[("x", "1")]);
        let mut t = db.begin(Mode::Snapshot);
        t.put(b"y", b"2");
        assert_eq!(t.get(b"y"), Some(k("2")));
        t.delete(b"x");
        assert_eq!(t.get(b"x"), None, "own delete visible to self");
        t.commit().unwrap();
        let mut r = db.begin(Mode::Snapshot);
        assert_eq!(r.get(b"x"), None);
        assert_eq!(r.get(b"y"), Some(k("2")));
    }

    #[test]
    fn first_committer_wins_on_write_write_conflict() {
        // P4 lost update must not happen.
        let db = setup(&[("x", "0")]);
        let mut t1 = db.begin(Mode::Snapshot);
        let mut t2 = db.begin(Mode::Snapshot);
        t1.put(b"x", b"from_t1");
        t2.put(b"x", b"from_t2");
        assert!(t1.commit().is_ok());
        assert_eq!(t2.commit(), Err(CommitError::WriteConflict));
        let mut r = db.begin(Mode::Snapshot);
        assert_eq!(r.get(b"x"), Some(k("from_t1")));
    }

    #[test]
    fn write_skew_happens_under_snapshot_isolation() {
        // A5B — this test PASSES when the anomaly OCCURS. SI permits it;
        // you must be able to produce the bug before you prevent it.
        let db = setup(&[("alice_on_call", "1"), ("bob_on_call", "1")]);
        let mut t1 = db.begin(Mode::Snapshot);
        let mut t2 = db.begin(Mode::Snapshot);
        // both check the invariant "someone else is on call"
        assert_eq!(t1.get(b"bob_on_call"), Some(k("1")));
        assert_eq!(t2.get(b"alice_on_call"), Some(k("1")));
        // disjoint write sets — first-committer-wins sees no conflict
        t1.put(b"alice_on_call", b"0");
        t2.put(b"bob_on_call", b"0");
        assert!(t1.commit().is_ok());
        assert!(t2.commit().is_ok(), "SI must ALLOW write skew");
        // invariant broken: nobody on call
        let mut r = db.begin(Mode::Snapshot);
        assert_eq!(r.get(b"alice_on_call"), Some(k("0")));
        assert_eq!(r.get(b"bob_on_call"), Some(k("0")));
    }

    #[test]
    fn serializable_mode_prevents_write_skew() {
        let db = setup(&[("alice_on_call", "1"), ("bob_on_call", "1")]);
        let mut t1 = db.begin(Mode::Serializable);
        let mut t2 = db.begin(Mode::Serializable);
        assert_eq!(t1.get(b"bob_on_call"), Some(k("1")));
        assert_eq!(t2.get(b"alice_on_call"), Some(k("1")));
        t1.put(b"alice_on_call", b"0");
        t2.put(b"bob_on_call", b"0");
        assert!(t1.commit().is_ok());
        assert_eq!(
            t2.commit(),
            Err(CommitError::ReadConflict),
            "t2 read alice_on_call, which t1 wrote after t2's snapshot"
        );
    }

    #[test]
    fn gc_drops_dead_versions_but_respects_active_snapshots() {
        let db = setup(&[("x", "0")]);
        for i in 1..=5 {
            let mut t = db.begin(Mode::Snapshot);
            t.put(b"x", i.to_string().as_bytes());
            t.commit().unwrap();
        }
        assert!(db.version_count() >= 6);

        // an active reader pins its snapshot's version
        let mut pinned = db.begin(Mode::Snapshot);
        let seen = pinned.get(b"x");
        let mut t = db.begin(Mode::Snapshot);
        t.put(b"x", b"7");
        t.commit().unwrap();

        db.gc();
        assert_eq!(pinned.get(b"x"), seen, "GC must not eat a pinned version");
        drop(pinned);

        db.gc();
        assert_eq!(db.version_count(), 1, "only the newest version survives");
        let mut r = db.begin(Mode::Snapshot);
        assert_eq!(r.get(b"x"), Some(k("7")));
    }

    #[test]
    fn threads_can_share_the_store() {
        let db = setup(&[]);
        let handles: Vec<_> = (0..4u32)
            .map(|i| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let mut t = db.begin(Mode::Snapshot);
                    t.put(format!("k{i}").as_bytes(), b"v");
                    t.commit().unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let mut r = db.begin(Mode::Snapshot);
        for i in 0..4u32 {
            assert_eq!(r.get(format!("k{i}").as_bytes()), Some(k("v")));
        }
    }
}
