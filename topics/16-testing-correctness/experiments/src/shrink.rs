//! YOU implement: delta-debugging shrinking (ddmin, simplified).
//! turso has a whole shrink/ module; proptest integrates shrinking
//! into generation. We do the classic: remove chunks while the
//! failure reproduces.
//!
//! Contract:
//! - `replay(ops, file_seed, bug)`: run the EXPLICIT op list against
//!   KvStore + Model (same lockstep + post-Crash checks as
//!   dst::run_case, but ops are given, not generated). True if the
//!   case FAILS. Note: removing ops changes how much of the tear-RNG
//!   is consumed — a candidate only counts if it STILL fails.
//! - `shrink(ops, file_seed, bug)`: ddmin — try dropping chunks
//!   (halves, then quarters, ... down to single ops) as long as
//!   `replay` still fails; return a 1-minimal failing sequence
//!   (removing any single op makes it pass).

use crate::kv::Bug;
use crate::Op;

/// True if this exact op sequence produces a model divergence.
pub fn replay(ops: &[Op], file_seed: u64, bug: Bug) -> bool {
    let _ = (ops, file_seed, bug);
    todo!()
}

pub fn shrink(ops: &[Op], file_seed: u64, bug: Bug) -> Vec<Op> {
    let _ = (ops, file_seed, bug);
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dst::find_bug;

    #[test]
    fn shrunk_case_still_fails() {
        let f = find_bug(Bug::LostDelete, 200, 40).expect("need a failure to shrink");
        let small = shrink(&f.ops, f.seed, Bug::LostDelete);
        assert!(replay(&small, f.seed, Bug::LostDelete), "shrunk case must still fail");
    }

    #[test]
    fn shrunk_case_is_much_smaller() {
        let f = find_bug(Bug::LostDelete, 200, 40).unwrap();
        let small = shrink(&f.ops, f.seed, Bug::LostDelete);
        // minimal LostDelete repro: Put, Commit, Delete, Commit, Crash
        assert!(small.len() <= 10, "expected near-minimal, got {} ops: {:?}", small.len(), small);
        assert!(small.len() < f.ops.len());
    }

    #[test]
    fn shrunk_case_is_one_minimal() {
        let f = find_bug(Bug::LostDelete, 200, 40).unwrap();
        let small = shrink(&f.ops, f.seed, Bug::LostDelete);
        for i in 0..small.len() {
            let mut fewer = small.clone();
            fewer.remove(i);
            assert!(
                !replay(&fewer, f.seed, Bug::LostDelete),
                "removing op {i} still fails — not 1-minimal"
            );
        }
    }
}
