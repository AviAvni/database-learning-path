//! YOU implement: the DST harness — turso's simulator in miniature.
//!
//! Contract:
//! - `gen_ops(seed, len)`: seeded workload — StdRng::seed_from_u64,
//!   mix of Put (keys 0..16, vals arbitrary), Delete, Commit, Crash.
//!   Weight it: ~50% put, ~20% delete, ~20% commit, ~10% crash, and
//!   ALWAYS end with [Commit, Crash] so every case exercises
//!   recovery and ends at a checkable point.
//! - `run_case(seed, len, bug)`: build KvStore::new(seed, bug) and a
//!   Model; apply ops to both in lockstep; AFTER EVERY Crash op
//!   (post-recovery) assert kv.state() == model.expected(). Return
//!   the first `Failure { seed, ops, step }` or None.
//!   (Between crashes the kv legitimately shows uncommitted state —
//!   only the post-recovery view is pinned.)
//! - `find_bug(bug, max_seeds, len)`: sweep seeds 0..max_seeds,
//!   return the first failure.
//!
//! The tests are the point: a harness that can't catch a KNOWN bug
//! is worse than no harness — it manufactures false confidence.

use crate::kv::Bug;
use crate::Op;

#[derive(Debug, Clone)]
pub struct Failure {
    pub seed: u64,
    pub ops: Vec<Op>,
    /// index of the op whose post-state diverged
    pub step: usize,
}

pub fn gen_ops(seed: u64, len: usize) -> Vec<Op> {
    let _ = (seed, len);
    todo!()
}

pub fn run_case(seed: u64, len: usize, bug: Bug) -> Option<Failure> {
    let _ = (seed, len, bug);
    todo!()
}

pub fn find_bug(bug: Bug, max_seeds: u64, len: usize) -> Option<Failure> {
    (0..max_seeds).find_map(|s| run_case(s, len, bug))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_store_survives_500_seeds() {
        assert!(find_bug(Bug::None, 500, 40).is_none(), "false positive on a correct store");
    }

    #[test]
    fn catches_lost_delete() {
        let f = find_bug(Bug::LostDelete, 200, 40).expect("LostDelete not caught");
        assert!(f.ops.iter().any(|o| matches!(o, Op::Delete(_))));
    }

    #[test]
    fn catches_no_sync_on_commit() {
        assert!(find_bug(Bug::NoSyncOnCommit, 200, 40).is_some(), "NoSyncOnCommit not caught");
    }

    #[test]
    fn catches_torn_write_accepted() {
        assert!(find_bug(Bug::TornWriteAccepted, 200, 40).is_some(), "TornWriteAccepted not caught");
    }

    #[test]
    fn catches_stale_read() {
        assert!(find_bug(Bug::StaleRead, 200, 40).is_some(), "StaleRead not caught");
    }

    #[test]
    fn failures_are_deterministic() {
        let f1 = find_bug(Bug::LostDelete, 200, 40).unwrap();
        let f2 = run_case(f1.seed, 40, Bug::LostDelete).unwrap();
        assert_eq!(f1.ops, f2.ops, "same seed must replay the same failure");
        assert_eq!(f1.step, f2.step);
    }
}
