//! Vector clocks and dots — PROVIDED. The causality bookkeeping every
//! CRDT in this crate shares. Automerge's version is
//! rust/automerge/src/clock.rs (covers :109, the partial order :145).

use std::cmp::Ordering;
use std::collections::HashMap;

pub type ReplicaId = u32;

/// A dot: one specific event — the (actor, sequence) pair that makes
/// every operation globally unique. OR-Set tags and RGA element ids are
/// dots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dot {
    pub replica: ReplicaId,
    pub counter: u64,
}

/// Vector clock: for each replica, how many of its events we've seen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VClock {
    pub counters: HashMap<ReplicaId, u64>,
}

impl VClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the next local event for `replica`; returns its Dot.
    pub fn tick(&mut self, replica: ReplicaId) -> Dot {
        let c = self.counters.entry(replica).or_insert(0);
        *c += 1;
        Dot { replica, counter: *c }
    }

    pub fn get(&self, replica: ReplicaId) -> u64 {
        *self.counters.get(&replica).unwrap_or(&0)
    }

    /// Have we seen this dot?
    pub fn covers(&self, dot: Dot) -> bool {
        self.get(dot.replica) >= dot.counter
    }

    /// Pointwise max — the join of the semilattice.
    pub fn merge(&mut self, other: &VClock) {
        for (&r, &c) in &other.counters {
            let e = self.counters.entry(r).or_insert(0);
            *e = (*e).max(c);
        }
    }

    /// The partial order that defines "concurrent": None means neither
    /// clock happened-before the other.
    pub fn partial_cmp(&self, other: &VClock) -> Option<Ordering> {
        let (mut le, mut ge) = (true, true);
        for (&r, &c) in &self.counters {
            match c.cmp(&other.get(r)) {
                Ordering::Less => ge = false,
                Ordering::Greater => le = false,
                Ordering::Equal => {}
            }
        }
        for (&r, &c) in &other.counters {
            if self.get(r) < c {
                ge = false;
            }
        }
        match (le, ge) {
            (true, true) => Some(Ordering::Equal),
            (true, false) => Some(Ordering::Less),
            (false, true) => Some(Ordering::Greater),
            (false, false) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_clocks_are_incomparable() {
        let mut a = VClock::new();
        let mut b = VClock::new();
        a.tick(1);
        b.tick(2);
        assert_eq!(a.partial_cmp(&b), None);
        let mut m = a.clone();
        m.merge(&b);
        assert_eq!(a.partial_cmp(&m), Some(Ordering::Less));
        assert_eq!(m.partial_cmp(&b), Some(Ordering::Greater));
    }

    #[test]
    fn merge_is_idempotent_commutative() {
        let mut a = VClock::new();
        a.tick(1);
        a.tick(1);
        let mut b = VClock::new();
        b.tick(2);
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab, ba);
        let snapshot = ab.clone();
        ab.merge(&b);
        assert_eq!(ab, snapshot);
    }

    #[test]
    fn covers_tracks_dots() {
        let mut a = VClock::new();
        let d1 = a.tick(1);
        let d2 = Dot { replica: 1, counter: 5 };
        assert!(a.covers(d1));
        assert!(!a.covers(d2));
    }
}
