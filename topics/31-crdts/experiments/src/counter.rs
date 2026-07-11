//! G-Counter and PN-Counter — YOUR JOB. The "hello world" of state-based
//! CRDTs (Shapiro SSS'11 §3.1). The whole trick: per-replica counters
//! merged with pointwise max form a join semilattice, so merge is
//! associative + commutative + idempotent and every replica converges.
//!
//! Contract fixed by the tests below:
//! - GCounter: increment-only. `incr(r, n)` adds to replica r's slot;
//!   `value()` sums all slots; `merge` is pointwise max.
//! - PNCounter: two G-Counters (p for increments, n for decrements);
//!   `value()` = p.value() - n.value(). Why two counters instead of one
//!   signed slot? max() of signed deltas isn't a semilattice join —
//!   decrements would be swallowed by earlier larger increments.

use crate::clock::ReplicaId;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GCounter {
    pub slots: HashMap<ReplicaId, u64>,
}

impl GCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `n` to this replica's slot. Only ever touch your own slot —
    /// that's the invariant that makes pointwise max correct.
    pub fn incr(&mut self, replica: ReplicaId, n: u64) {
        let _ = (replica, n);
        todo!("bump self.slots[replica] by n")
    }

    pub fn value(&self) -> u64 {
        todo!("sum of all slots")
    }

    /// Join: pointwise max over slots.
    pub fn merge(&mut self, other: &GCounter) {
        let _ = other;
        todo!("for each slot in other, keep the max")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PNCounter {
    pub p: GCounter,
    pub n: GCounter,
}

impl PNCounter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn incr(&mut self, replica: ReplicaId, amount: u64) {
        let _ = (replica, amount);
        todo!("route into p")
    }

    pub fn decr(&mut self, replica: ReplicaId, amount: u64) {
        let _ = (replica, amount);
        todo!("route into n")
    }

    pub fn value(&self) -> i64 {
        todo!("p minus n, as i64")
    }

    pub fn merge(&mut self, other: &PNCounter) {
        let _ = other;
        todo!("merge p with p, n with n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::seq::SliceRandom;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn gcounter_counts() {
        let mut c = GCounter::new();
        c.incr(1, 3);
        c.incr(2, 4);
        c.incr(1, 1);
        assert_eq!(c.value(), 8);
    }

    #[test]
    fn gcounter_merge_is_semilattice() {
        let mut a = GCounter::new();
        a.incr(1, 5);
        let mut b = GCounter::new();
        b.incr(2, 7);
        b.incr(1, 2); // stale view of replica 1

        // Commutative.
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab, ba);
        assert_eq!(ab.value(), 12); // max(5,2) + 7

        // Idempotent.
        let snap = ab.clone();
        ab.merge(&b);
        assert_eq!(ab, snap);

        // Associative.
        let mut c = GCounter::new();
        c.incr(3, 1);
        let mut ab_c = snap.clone();
        ab_c.merge(&c);
        let mut bc = b.clone();
        bc.merge(&c);
        let mut a_bc = a.clone();
        a_bc.merge(&bc);
        assert_eq!(ab_c, a_bc);
    }

    #[test]
    fn pncounter_can_go_negative() {
        let mut c = PNCounter::new();
        c.incr(1, 2);
        c.decr(2, 5);
        assert_eq!(c.value(), -3);
    }

    #[test]
    fn pncounter_converges_under_any_merge_order() {
        // Three replicas each do local ops, then every replica merges
        // the others in a seeded-random order. All must agree — the
        // permutation shuffle is our poor man's proptest.
        let mut rng = ChaCha8Rng::seed_from_u64(31);
        let mut replicas: Vec<PNCounter> = (0..3).map(|_| PNCounter::new()).collect();
        replicas[0].incr(0, 10);
        replicas[0].decr(0, 3);
        replicas[1].incr(1, 4);
        replicas[2].decr(2, 6);

        let mut results = Vec::new();
        for _ in 0..20 {
            let mut order: Vec<usize> = (0..3).collect();
            order.shuffle(&mut rng);
            let mut acc = replicas[order[0]].clone();
            acc.merge(&replicas[order[1]]);
            acc.merge(&replicas[order[2]]);
            results.push(acc.value());
        }
        assert!(results.iter().all(|&v| v == 5), "10 - 3 + 4 - 6 = 5, always");
    }
}
