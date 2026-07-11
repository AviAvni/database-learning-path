//! Two-table incremental-rehash hash map, redis-style — YOUR implementation.
//!
//! Spec (see reading-redis-dict.md — you are replicating dict.c's scheme):
//! - chaining table: `Vec<Option<Box<Entry>>>` buckets, power-of-two sizes
//! - grow at load factor 1.0: allocate ht[1] at 2× and set rehash_idx = 0
//! - EVERY insert/get performs one rehash step: migrate ≤ 1 bucket from
//!   ht[0] to ht[1], capped at 10 empty-bucket visits (dict.c:406)
//! - during rehash: inserts go ONLY to ht[1]; gets check BOTH tables
//! - when ht[0] is drained: drop it, ht[1] becomes ht[0], rehash_idx = None
//!
//! The point: per-insert worst case stays O(1 bucket chain + 10 empties) —
//! the rehash_spike binary will show your max latency flat while hashbrown
//! spikes at every doubling.

pub struct IncrementalMap {
    // TODO: ht: [table; 2], used: [usize; 2], rehash_idx: Option<usize>, ...
}

impl IncrementalMap {
    pub fn new() -> Self {
        todo!("start with a small ht[0] (e.g. 16 buckets), ht[1] empty")
    }

    pub fn insert(&mut self, key: u64, value: u64) {
        let _ = (key, value);
        todo!("rehash_step(); if rehashing insert into ht[1] else ht[0]; maybe trigger grow")
    }

    pub fn get(&mut self, key: u64) -> Option<u64> {
        let _ = key;
        // &mut because reads also pay the rehash tax, like dictFind (dict.c:779)
        todo!("rehash_step(); probe ht[0] then ht[1] if rehashing")
    }

    pub fn len(&self) -> usize {
        todo!()
    }

    pub fn is_rehashing(&self) -> bool {
        todo!()
    }
}

impl Default for IncrementalMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_roundtrip() {
        let mut m = IncrementalMap::new();
        for i in 0..100_000u64 {
            m.insert(i, i * 3);
        }
        for i in 0..100_000u64 {
            assert_eq!(m.get(i), Some(i * 3), "key {i}");
        }
        assert_eq!(m.get(100_001), None);
        assert_eq!(m.len(), 100_000);
    }

    #[test]
    fn survives_reads_during_rehash() {
        let mut m = IncrementalMap::new();
        for i in 0..1000u64 {
            m.insert(i, i);
        }
        // force a grow, then read old keys while rehash is mid-flight
        m.insert(1000, 1000);
        while m.is_rehashing() {
            // every get both migrates a bucket and must still find old keys
            let k = m.len() as u64 % 1000;
            assert_eq!(m.get(k), Some(k));
        }
    }

    #[test]
    fn overwrite_during_rehash_is_visible() {
        let mut m = IncrementalMap::new();
        for i in 0..10_000u64 {
            m.insert(i, 0);
        }
        for i in 0..10_000u64 {
            m.insert(i, 1); // some of these land mid-rehash
        }
        for i in 0..10_000u64 {
            assert_eq!(m.get(i), Some(1), "stale value for key {i}");
        }
        assert_eq!(m.len(), 10_000);
    }
}
