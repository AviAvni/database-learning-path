//! LWW register + map — PROVIDED. This is the "cheap" CRDT everyone
//! ships first (Cassandra cells, Redis CRDT strings, cr-sqlite default
//! column semantics: core/rs/core/src/compare_values.rs). It converges,
//! but by *discarding* one of every pair of concurrent writes. Bench
//! lane 1 measures how often that happens — "LWW's lie" made a number.

use crate::clock::ReplicaId;
use std::collections::HashMap;

/// Last-writer-wins register. Total order over (timestamp, replica):
/// higher timestamp wins, replica id breaks ties. Deterministic on
/// every replica, therefore convergent — and silently lossy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LwwRegister<V> {
    pub value: V,
    pub ts: u64,
    pub replica: ReplicaId,
}

impl<V: Clone> LwwRegister<V> {
    pub fn new(value: V, ts: u64, replica: ReplicaId) -> Self {
        Self { value, ts, replica }
    }

    /// Local write. Callers must supply a timestamp ≥ any they've seen
    /// (an HLC in real systems — topic 29's reading-spanner-hlc.md).
    pub fn set(&mut self, value: V, ts: u64, replica: ReplicaId) {
        if (ts, replica) > (self.ts, self.replica) {
            self.value = value;
            self.ts = ts;
            self.replica = replica;
        }
    }

    /// Merge = keep the (ts, replica)-max. Returns true if `other` won,
    /// i.e. our current value was discarded.
    pub fn merge(&mut self, other: &LwwRegister<V>) -> bool {
        if (other.ts, other.replica) > (self.ts, self.replica) {
            self.value = other.value.clone();
            self.ts = other.ts;
            self.replica = other.replica;
            true
        } else {
            false
        }
    }
}

/// LWW map: independent LWW register per key. This is cr-sqlite's model
/// for a row — each column converges on its own, so concurrent writes
/// to *different* columns of the same row both survive, but concurrent
/// writes to the *same* column don't.
#[derive(Clone, Debug, Default)]
pub struct LwwMap<K: std::hash::Hash + Eq, V> {
    pub entries: HashMap<K, LwwRegister<V>>,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> LwwMap<K, V> {
    pub fn new() -> Self {
        Self { entries: HashMap::new() }
    }

    pub fn set(&mut self, key: K, value: V, ts: u64, replica: ReplicaId) {
        match self.entries.get_mut(&key) {
            Some(reg) => reg.set(value, ts, replica),
            None => {
                self.entries.insert(key, LwwRegister::new(value, ts, replica));
            }
        }
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key).map(|r| &r.value)
    }

    /// Returns how many of *our* values were overwritten by the merge —
    /// each one is a write some user made that no replica remembers.
    pub fn merge(&mut self, other: &LwwMap<K, V>) -> usize {
        let mut lost = 0;
        for (k, reg) in &other.entries {
            match self.entries.get_mut(k) {
                Some(mine) => {
                    if mine.merge(reg) {
                        lost += 1;
                    }
                }
                None => {
                    self.entries.insert(k.clone(), reg.clone());
                }
            }
        }
        lost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_ts_wins_replica_breaks_ties() {
        let mut a = LwwRegister::new("a", 10, 1);
        let b = LwwRegister::new("b", 10, 2);
        assert!(a.merge(&b));
        assert_eq!(a.value, "b");
        let c = LwwRegister::new("c", 9, 3);
        assert!(!a.merge(&c));
        assert_eq!(a.value, "b");
    }

    #[test]
    fn merge_is_commutative_but_lossy() {
        let a = LwwRegister::new("from-a", 10, 1);
        let b = LwwRegister::new("from-b", 10, 2);
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        // Convergence: both orders agree...
        assert_eq!(ab, ba);
        // ...on an answer that forgot replica 1's write entirely.
        assert_eq!(ab.value, "from-b");
    }

    #[test]
    fn map_counts_lost_writes() {
        let mut a = LwwMap::new();
        a.set("x", 1, 5, 1);
        a.set("y", 2, 5, 1);
        let mut b = LwwMap::new();
        b.set("x", 99, 6, 2); // later ts: a's x will be lost
        b.set("z", 3, 5, 2);
        let lost = a.merge(&b);
        assert_eq!(lost, 1);
        assert_eq!(a.get(&"x"), Some(&99));
        assert_eq!(a.get(&"y"), Some(&2));
        assert_eq!(a.get(&"z"), Some(&3));
    }
}
