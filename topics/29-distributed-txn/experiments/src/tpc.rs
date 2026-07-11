//! STUB 1 — two-phase commit, with the blocking failure mode on display.
//!
//! Textbook 2PC over locking shards (no MVCC here — this is the pre-
//! Percolator world). The coordinator drives: acquire+stage on every
//! participant (PREPARE), log the decision, then apply everywhere (COMMIT).
//! Crash injection is DST-style: `crash` names the point where the
//! coordinator dies mid-protocol; `recover` must then finish or undo the
//! damage using only the durable state (participant staging + coordinator
//! decision log).
//!
//! The lesson the tests force: a coordinator crash AFTER all prepares but
//! BEFORE the decision is logged leaves participants locked and *unable to
//! decide locally* — that's 2PC's blocking window, the reason Percolator
//! moved the decision into the data (primary lock) and FoundationDB/Spanner
//! put the coordinator state on a replicated log.

use crate::kv::{Key, TxnId};
use std::collections::HashMap;

/// A locking (non-MVCC) shard for the 2PC lane.
#[derive(Default)]
pub struct LockShard {
    pub committed: HashMap<Key, i64>,
    /// key -> (txn, staged new value); a staged key is LOCKED.
    pub staged: HashMap<Key, (TxnId, i64)>,
}

pub struct TpcCluster {
    pub shards: Vec<LockShard>,
    /// Coordinator's durable decision log: txn -> committed?
    pub decision_log: HashMap<TxnId, bool>,
    pub next_txn: TxnId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    /// Die after staging on the FIRST shard only (partial prepare).
    AfterFirstPrepare,
    /// Die after all prepares, BEFORE logging the decision (the blocking window).
    AfterAllPrepares,
    /// Die after logging COMMIT, before applying anywhere.
    AfterDecisionLogged,
    /// Die after applying on the FIRST shard only (partial commit).
    AfterFirstApply,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Committed,
    /// Prepare failed (a key was already locked) — cleanly rolled back.
    Aborted,
    /// The coordinator died at `CrashPoint`; state is in limbo until recover().
    Crashed,
}

impl TpcCluster {
    pub fn new(n_shards: usize) -> Self {
        Self {
            shards: (0..n_shards).map(|_| LockShard::default()).collect(),
            decision_log: HashMap::new(),
            next_txn: 0,
        }
    }

    pub fn shard_of(&self, key: Key) -> usize {
        (key % self.shards.len() as u64) as usize
    }

    pub fn read(&self, key: Key) -> i64 {
        *self.shards[self.shard_of(key)].committed.get(&key).unwrap_or(&0)
    }

    pub fn total(&self, keys: &[Key]) -> i64 {
        keys.iter().map(|&k| self.read(k)).sum()
    }

    pub fn locked_keys(&self) -> usize {
        self.shards.iter().map(|s| s.staged.len()).sum()
    }

    /// Run one transaction that sets each (key -> value) atomically across
    /// shards. `crash` injects a coordinator crash at the named point.
    ///
    /// Recipe:
    ///   txn = next_txn++.
    ///   PREPARE: group writes by shard; for each shard in order, if any key
    ///     is already staged by another txn -> undo OWN staging done so far,
    ///     return Aborted. Else stage (lock) each key. Honor
    ///     CrashPoint::AfterFirstPrepare (return Crashed after shard 0's
    ///     staging) and AfterAllPrepares.
    ///   DECIDE: decision_log.insert(txn, true). Honor AfterDecisionLogged.
    ///   APPLY: for each shard, move staged -> committed, clearing locks.
    ///     Honor AfterFirstApply.
    ///   Return Committed.
    pub fn run_txn(
        &mut self,
        _writes: &[(Key, i64)],
        _crash: Option<CrashPoint>,
    ) -> Outcome {
        todo!("stub: 2PC coordinator")
    }

    /// Coordinator recovery: finish or undo every in-limbo transaction using
    /// only durable state.
    ///
    /// Recipe: for every staged (txn, ...) entry across shards:
    ///   decision_log says committed -> roll FORWARD (apply staged value).
    ///   no decision logged        -> roll BACK (drop staging).
    /// Afterwards no locks remain.
    pub fn recover(&mut self) {
        todo!("stub: 2PC recovery")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(from: Key, to: Key, amount: i64, c: &TpcCluster) -> Vec<(Key, i64)> {
        vec![(from, c.read(from) - amount), (to, c.read(to) + amount)]
    }

    fn seeded() -> TpcCluster {
        let mut c = TpcCluster::new(2);
        for k in 0..4u64 {
            let s = c.shard_of(k);
            c.shards[s].committed.insert(k, 100);
        }
        c
    }

    #[test]
    fn clean_commit_moves_money() {
        let mut c = seeded();
        let w = transfer(0, 1, 30, &c); // keys 0 and 1 live on different shards
        assert_eq!(c.run_txn(&w, None), Outcome::Committed);
        assert_eq!((c.read(0), c.read(1)), (70, 130));
        assert_eq!(c.locked_keys(), 0);
    }

    #[test]
    fn conflicting_prepare_aborts_cleanly() {
        let mut c = seeded();
        // txn A crashes holding locks on key 0 and 1
        let w = transfer(0, 1, 10, &c);
        assert_eq!(c.run_txn(&w, Some(CrashPoint::AfterAllPrepares)), Outcome::Crashed);
        // txn B touching key 0 must abort, and must NOT leak partial locks on key 3
        let out = c.run_txn(&[(3, 1), (0, 1)], None);
        assert_eq!(out, Outcome::Aborted);
        assert_eq!(c.locked_keys(), 2, "aborted txn must release its own staging");
    }

    #[test]
    fn every_crash_point_preserves_atomicity_after_recovery() {
        let keys = [0u64, 1, 2, 3];
        for crash in [
            CrashPoint::AfterFirstPrepare,
            CrashPoint::AfterAllPrepares,
            CrashPoint::AfterDecisionLogged,
            CrashPoint::AfterFirstApply,
        ] {
            let mut c = seeded();
            let total_before = c.total(&keys);
            let w = transfer(0, 1, 25, &c);
            assert_eq!(c.run_txn(&w, Some(crash)), Outcome::Crashed, "{crash:?}");
            c.recover();
            assert_eq!(c.locked_keys(), 0, "{crash:?}: locks leaked past recovery");
            assert_eq!(c.total(&keys), total_before, "{crash:?}: money vanished");
            let (a, b) = (c.read(0), c.read(1));
            assert!(
                (a, b) == (100, 100) || (a, b) == (75, 125),
                "{crash:?}: partial application ({a},{b})"
            );
            // decision logged => must roll FORWARD, not back
            if matches!(crash, CrashPoint::AfterDecisionLogged | CrashPoint::AfterFirstApply) {
                assert_eq!((a, b), (75, 125), "{crash:?}: logged decision must win");
            }
        }
    }

    #[test]
    fn blocking_window_demonstrated() {
        // After all prepares, before decision: participants CANNOT decide
        // locally — locks stay until coordinator recovery. This is the test
        // that names 2PC's flaw.
        let mut c = seeded();
        let w = transfer(0, 1, 10, &c);
        c.run_txn(&w, Some(CrashPoint::AfterAllPrepares));
        assert_eq!(c.locked_keys(), 2, "participants must still be blocked");
        // other txns on those keys abort while the window is open
        assert_eq!(c.run_txn(&[(0, 5)], None), Outcome::Aborted);
        c.recover();
        assert_eq!(c.locked_keys(), 0);
        // no decision was logged -> recovery rolls BACK
        assert_eq!((c.read(0), c.read(1)), (100, 100));
    }
}
