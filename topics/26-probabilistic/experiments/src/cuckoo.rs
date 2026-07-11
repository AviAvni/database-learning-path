//! Cuckoo filter (Fan et al., CoNEXT 2014): fingerprints in 4-slot buckets,
//! TWO candidate buckets per key, displacement (kicking) on collision.
//! Buys what bloom can't: DELETION and better FPR at high bits/key.
//! RedisBloom's cuckoo.c is the production reference (getAltHash at
//! cuckoo.c:122 — the partial-key trick below).

use crate::hash::splitmix64;

pub const SLOTS_PER_BUCKET: usize = 4;
pub const MAX_KICKS: usize = 500;

pub struct CuckooFilter {
    /// buckets[i][s]: 0 = empty, else a 12-bit fingerprint (stored in u16).
    pub buckets: Vec<[u16; SLOTS_PER_BUCKET]>,
    pub len: usize,
}

/// 12-bit fingerprint, never 0 (0 means empty slot).
pub fn fingerprint(key: u64) -> u16 {
    let fp = (splitmix64(key) >> 48) as u16 & 0x0fff;
    if fp == 0 {
        1
    } else {
        fp
    }
}

impl CuckooFilter {
    /// STUB — capacity/4 buckets rounded UP to a power of two (the alt-
    /// bucket XOR trick needs a power-of-two bucket count; RedisBloom
    /// asserts the same, cuckoo.c:54).
    pub fn new(_capacity: usize) -> CuckooFilter {
        todo!("power-of-two buckets of 4 u16 slots")
    }

    /// STUB — partial-key cuckoo hashing:
    ///   i1 = hash(key) & mask
    ///   i2 = i1 ^ (hash(fp) & mask)      ← depends only on i1 and fp,
    /// so a stored fingerprint can ALWAYS compute its alternate bucket
    /// without the original key — that's what makes kicking possible.
    /// Try an empty slot in i1 then i2; else kick: evict a random resident
    /// fingerprint, place ours, re-insert the victim in ITS alternate
    /// bucket; up to MAX_KICKS, then return false (filter full).
    pub fn insert(&mut self, _key: u64) -> bool {
        todo!("two buckets, then displacement loop")
    }

    /// STUB — fp present in either candidate bucket.
    pub fn contains(&self, _key: u64) -> bool {
        todo!()
    }

    /// STUB — remove ONE copy of the fingerprint if present (this is why
    /// cuckoo supports delete and bloom fundamentally can't: the
    /// fingerprint is a discrete resident, not smeared bits).
    pub fn remove(&mut self, _key: u64) -> bool {
        todo!()
    }

    pub fn load_factor(&self) -> f64 {
        self.len as f64 / (self.buckets.len() * SLOTS_PER_BUCKET) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_false_negatives_at_90_percent_load() {
        let cap = 1 << 16;
        let mut f = CuckooFilter::new(cap);
        let n = cap * 9 / 10;
        for k in 0..n as u64 {
            assert!(f.insert(k * 2), "insert failed at load {:.2}", f.load_factor());
        }
        for k in 0..n as u64 {
            assert!(f.contains(k * 2));
        }
    }

    #[test]
    fn fpr_under_one_percent_with_12bit_fp() {
        let cap = 1 << 16;
        let mut f = CuckooFilter::new(cap);
        let n = cap * 9 / 10;
        for k in 0..n as u64 {
            f.insert(k * 2);
        }
        // theory: 2 buckets x 4 slots x 1/4096 x load ≈ 0.18%
        let fp = (0..n as u64).filter(|&k| f.contains(k * 2 + 1)).count();
        let fpr = fp as f64 / n as f64;
        assert!(fpr < 0.01, "fpr {:.4}", fpr);
    }

    #[test]
    fn delete_actually_removes() {
        let mut f = CuckooFilter::new(1 << 12);
        let a: Vec<u64> = (0..1500).map(|k| k * 3).collect();
        let b: Vec<u64> = (0..1500).map(|k| k * 3 + 1).collect();
        for &k in a.iter().chain(&b) {
            assert!(f.insert(k));
        }
        for &k in &b {
            assert!(f.remove(k), "remove failed for {}", k);
        }
        // A untouched (deletion must not disturb other residents)...
        for &k in &a {
            assert!(f.contains(k));
        }
        // ...and B is gone up to FPR noise (deleting smeared bloom bits
        // would leave these all true — the test bloom can never pass).
        let still = b.iter().filter(|&&k| f.contains(k)).count();
        assert!(still < 30, "{} of 1500 deleted keys still 'present'", still);
    }

    #[test]
    fn insert_fails_gracefully_when_full() {
        let mut f = CuckooFilter::new(64);
        let mut inserted = 0u64;
        for k in 0..1000u64 {
            if !f.insert(k) {
                break;
            }
            inserted += 1;
        }
        // must accept a decent load before refusing, and must refuse
        // BEFORE claiming more entries than physical slots
        assert!(inserted >= 48, "only {} before failure", inserted);
        assert!((f.len as u64) <= 64 + SLOTS_PER_BUCKET as u64);
    }
}
