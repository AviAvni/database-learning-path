//! A classic Bloom filter (PROVIDED).
//!
//! m bits, k hash functions derived by double hashing (h1 + i·h2 mod m).
//! No false negatives; a false-positive probability that the textbook
//! approximation predicts as (1 − e^(−kn/m))^k for n inserted keys. Lane 2
//! measures the real rate against that formula.
//!
//! The production filters this topic reads are not this one: DuckDB's
//! (`table_filter_bloom_function.cpp:23-131` at the pinned commit) is
//! *register-blocked* — every key sets 4 bits inside one 64-bit word — and
//! RPT+'s is cache-sectorized (§5.4). Both trade a little accuracy for one
//! memory access per probe. This one is the accuracy reference.

use crate::hash::mix64;

#[derive(Clone, Debug)]
pub struct Bloom {
    words: Vec<u64>,
    m: u64,
    k: u32,
    n: usize,
}

impl Bloom {
    /// A filter sized for `expected_keys` at `bits_per_key` bits each,
    /// with the k that minimises the false-positive rate: round(b · ln 2).
    pub fn with_bits_per_key(expected_keys: usize, bits_per_key: f64) -> Bloom {
        let m_bits = ((expected_keys.max(1) as f64 * bits_per_key).ceil() as u64).max(64);
        let m = m_bits.div_ceil(64) * 64;
        let k = ((bits_per_key * std::f64::consts::LN_2).round() as u32).max(1);
        Bloom { words: vec![0; (m / 64) as usize], m, k, n: 0 }
    }

    #[inline]
    fn hashes(key: u32) -> (u64, u64) {
        let h1 = mix64(key as u64 ^ 0xA076_1D64_78BD_642F);
        let h2 = mix64(key as u64 ^ 0xE703_7ED1_A0B4_28DB) | 1;
        (h1, h2)
    }

    pub fn insert(&mut self, key: u32) {
        let (h1, h2) = Self::hashes(key);
        for i in 0..self.k as u64 {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % self.m;
            self.words[(bit / 64) as usize] |= 1 << (bit % 64);
        }
        self.n += 1;
    }

    #[inline]
    pub fn contains(&self, key: u32) -> bool {
        let (h1, h2) = Self::hashes(key);
        (0..self.k as u64).all(|i| {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % self.m;
            self.words[(bit / 64) as usize] & (1 << (bit % 64)) != 0
        })
    }

    pub fn bits(&self) -> u64 {
        self.m
    }

    pub fn k(&self) -> u32 {
        self.k
    }

    /// Insert calls so far (duplicates included — insert distinct keys if
    /// you want `theoretical_fpr` to mean what it says).
    pub fn inserted(&self) -> usize {
        self.n
    }

    /// (1 − e^(−k·n/m))^k for the keys inserted so far.
    pub fn theoretical_fpr(&self) -> f64 {
        let (k, n, m) = (self.k as f64, self.n as f64, self.m as f64);
        (1.0 - (-k * n / m).exp()).powf(k)
    }
}

/// A register-blocked Bloom filter in the shape DuckDB uses at the pinned
/// commit (`table_filter_bloom_function.cpp:23-131`): at least 12 bits per
/// key, rounded up to a power of two; the low hash bits pick one 64-bit
/// word (a "sector"); four 6-bit fields from the high bytes pick the bits set
/// inside it. One memory access per probe, at the price of accuracy — every
/// key shares its word with its neighbours. The hash is this crate's
/// `mix64`, not DuckDB's, so the layout is DuckDB's and the numbers are ours.
#[derive(Clone, Debug)]
pub struct RegisterBlocked {
    words: Vec<u64>,
    sector_mask: u64,
}

impl RegisterBlocked {
    pub const MIN_BITS_PER_KEY: usize = 12;

    pub fn new(expected_keys: usize) -> RegisterBlocked {
        let min_bits = (expected_keys * Self::MIN_BITS_PER_KEY).max(512);
        let sectors = (min_bits.next_power_of_two() >> 6).min(1 << 26);
        RegisterBlocked { words: vec![0; sectors], sector_mask: sectors as u64 - 1 }
    }

    #[inline]
    fn mask(h: u64) -> u64 {
        let shifts = h & 0x3F3F_3F3F_3F3F_3F3F;
        // bytes 4..8, as GetMask's `bit_idx = 8 - N_BITS .. 8` with N_BITS = 4
        (4..8).fold(0u64, |m, byte| m | 1 << ((shifts >> (8 * byte)) & 0x3F))
    }

    pub fn insert(&mut self, key: u32) {
        let h = mix64(key as u64 ^ 0xA076_1D64_78BD_642F);
        self.words[(h & self.sector_mask) as usize] |= Self::mask(h);
    }

    #[inline]
    pub fn contains(&self, key: u32) -> bool {
        let h = mix64(key as u64 ^ 0xA076_1D64_78BD_642F);
        let m = Self::mask(h);
        self.words[(h & self.sector_mask) as usize] & m == m
    }

    pub fn bits(&self) -> u64 {
        self.words.len() as u64 * 64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_false_negatives_and_rate_near_theory() {
        let mut b = Bloom::with_bits_per_key(10_000, 8.0);
        for key in 0..10_000u32 {
            b.insert(key * 7);
        }
        for key in 0..10_000u32 {
            assert!(b.contains(key * 7));
        }
        // probe 200,000 keys known to be absent
        let fp = (0..200_000u32).filter(|&i| b.contains(1_000_000 + i)).count();
        let measured = fp as f64 / 200_000.0;
        let theory = b.theoretical_fpr();
        assert_eq!(b.k(), 6);
        assert!((theory - 0.0216).abs() < 0.001, "theory {theory}");
        assert!(measured > theory * 0.8 && measured < theory * 1.25, "measured {measured} vs {theory}");
    }

    #[test]
    fn register_blocked_has_no_false_negatives_and_power_of_two_size() {
        let mut r = RegisterBlocked::new(1_000);
        assert_eq!(r.bits(), 16_384); // 12,000 rounded up to 2^14
        for key in 0..1_000u32 {
            r.insert(key * 13);
        }
        assert!((0..1_000u32).all(|k| r.contains(k * 13)));
    }
}
