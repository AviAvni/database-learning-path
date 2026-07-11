//! Blocked (cache-local) bloom filter — RocksDB's FastLocalBloomImpl shape
//! (util/bloom_impl.h:144): all probes for one key land in ONE 64-byte
//! cache line, so a negative lookup costs exactly one memory access.
//! The price: slightly worse FPR than a standard bloom at the same
//! bits/key (bloom_impl.h:42 CacheLocalFpRate quantifies it — keys are
//! Poisson-distributed over lines, crowded lines have inflated FPR).

use crate::hash::{fastrange32, hash2};

pub struct BlockedBloom {
    /// 8 x u64 = 512 bits = one cache line per block.
    pub blocks: Vec<[u64; 8]>,
    pub num_probes: u32,
}

impl BlockedBloom {
    /// STUB — size the filter at `bits_per_key * n_keys` bits, rounded up
    /// to whole 512-bit blocks. Fix num_probes = 6 (rocksdb's default
    /// sweet spot for ~10 bits/key).
    pub fn new(_n_keys: usize, _bits_per_key: usize) -> BlockedBloom {
        todo!("ceil(n*bpk/512) blocks, num_probes=6")
    }

    /// STUB — h1 picks the block via fastrange32 (NOT modulo); then derive
    /// num_probes bit positions inside the 512-bit block from h2 by
    /// repeated rotation/multiply (rocksdb AddHashPrepared bloom_impl.h:206
    /// uses `h * 0x9e3779b9` then top 9 bits per probe; any scheme works if
    /// probes depend only on h2 and stay in-block). Same derivation must be
    /// used by may_contain.
    pub fn insert(&mut self, _key: u64) {
        todo!("set num_probes bits inside one block")
    }

    /// STUB — true if all probe bits set. No false negatives, ever.
    pub fn may_contain(&self, _key: u64) -> bool {
        todo!("check num_probes bits inside one block")
    }

    pub fn size_bytes(&self) -> usize {
        self.blocks.len() * 64
    }

    /// Helper the bench uses; provided.
    pub fn measured_fpr(&self, absent_keys: &[u64]) -> f64 {
        let fp = absent_keys.iter().filter(|&&k| self.may_contain(k)).count();
        fp as f64 / absent_keys.len() as f64
    }

    #[allow(dead_code)]
    fn _silence(_: (u32, u32)) {
        let _ = (fastrange32(0, 1), hash2(0));
    }
}

/// Standard-bloom theoretical FPR: (1 - e^{-k/b})^k for k probes, b
/// bits/key (rocksdb BloomMath::StandardFpRate, bloom_impl.h:32).
pub fn standard_fpr(bits_per_key: f64, num_probes: u32) -> f64 {
    (1.0 - (-(num_probes as f64) / bits_per_key).exp()).powi(num_probes as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: usize, offset: u64) -> Vec<u64> {
        (0..n as u64).map(|i| i * 2 + offset).collect()
    }

    #[test]
    fn no_false_negatives() {
        let present = keys(100_000, 0); // even keys
        let mut b = BlockedBloom::new(present.len(), 10);
        for &k in &present {
            b.insert(k);
        }
        for &k in &present {
            assert!(b.may_contain(k), "false negative for {}", k);
        }
    }

    // Theory says 0.65% at 10 bits/key k=6; blocked layout costs ~1.5-2x.
    // The contract: below 2.5%, i.e. "still a useful filter".
    #[test]
    fn fpr_near_theory_at_10_bits() {
        let present = keys(100_000, 0);
        let absent = keys(100_000, 1); // odd keys — disjoint
        let mut b = BlockedBloom::new(present.len(), 10);
        for &k in &present {
            b.insert(k);
        }
        let fpr = b.measured_fpr(&absent);
        let theory = standard_fpr(10.0, 6);
        assert!(
            fpr < theory * 4.0 && fpr < 0.025,
            "fpr {:.4} vs theory {:.4}",
            fpr,
            theory
        );
    }

    #[test]
    fn more_bits_fewer_false_positives() {
        let present = keys(50_000, 0);
        let absent = keys(50_000, 1);
        let fpr_at = |bpk: usize| {
            let mut b = BlockedBloom::new(present.len(), bpk);
            for &k in &present {
                b.insert(k);
            }
            b.measured_fpr(&absent)
        };
        let (f8, f16) = (fpr_at(8), fpr_at(16));
        assert!(f16 < f8 / 2.0, "8bpk {:.4} 16bpk {:.4}", f8, f16);
    }

    #[test]
    fn sized_in_whole_cache_lines() {
        let b = BlockedBloom::new(1000, 10);
        assert_eq!(b.size_bytes() % 64, 0);
        assert!(b.size_bytes() >= 1000 * 10 / 8);
    }
}
