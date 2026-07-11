//! Bloom filter — provided (the topic's build work is the SST + compaction;
//! Monkey experiments need per-level bits-per-key, hence the parameter).
//!
//! Double hashing like lsm-tree (h1 += h2 per probe): one xxh3 call, k probes.

use xxhash_rust::xxh3::xxh3_128;

pub struct Bloom {
    bits: Vec<u64>,
    nbits: u64,
    k: u32,
}

impl Bloom {
    pub fn new(n_keys: usize, bits_per_key: f64) -> Self {
        let nbits = ((n_keys as f64 * bits_per_key).ceil() as u64).max(64);
        let k = ((bits_per_key * std::f64::consts::LN_2).round() as u32).clamp(1, 16);
        Self { bits: vec![0; nbits.div_ceil(64) as usize], nbits, k }
    }

    fn hashes(key: &[u8]) -> (u64, u64) {
        let h = xxh3_128(key);
        (h as u64, (h >> 64) as u64 | 1)
    }

    pub fn insert(&mut self, key: &[u8]) {
        let (mut h1, h2) = Self::hashes(key);
        for _ in 0..self.k {
            let bit = h1 % self.nbits;
            self.bits[(bit / 64) as usize] |= 1 << (bit % 64);
            h1 = h1.wrapping_add(h2);
        }
    }

    pub fn maybe_contains(&self, key: &[u8]) -> bool {
        let (mut h1, h2) = Self::hashes(key);
        for _ in 0..self.k {
            let bit = h1 % self.nbits;
            if self.bits[(bit / 64) as usize] & (1 << (bit % 64)) == 0 {
                return false;
            }
            h1 = h1.wrapping_add(h2);
        }
        true
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.bits.len() * 8);
        out.extend_from_slice(&self.nbits.to_le_bytes());
        out.extend_from_slice(&self.k.to_le_bytes());
        for w in &self.bits {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Self {
        let nbits = u64::from_le_bytes(b[0..8].try_into().unwrap());
        let k = u32::from_le_bytes(b[8..12].try_into().unwrap());
        let bits = b[12..]
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        Self { bits, nbits, k }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_false_negatives_and_reasonable_fpr() {
        let mut b = Bloom::new(10_000, 10.0);
        for i in 0..10_000u64 {
            b.insert(&i.to_be_bytes());
        }
        for i in 0..10_000u64 {
            assert!(b.maybe_contains(&i.to_be_bytes()));
        }
        let fp = (10_000..30_000u64).filter(|i| b.maybe_contains(&i.to_be_bytes())).count();
        let fpr = fp as f64 / 20_000.0;
        assert!(fpr < 0.02, "fpr {fpr} too high for 10 bits/key");
    }

    #[test]
    fn roundtrip() {
        let mut b = Bloom::new(100, 10.0);
        b.insert(b"hello");
        let b2 = Bloom::from_bytes(&b.to_bytes());
        assert!(b2.maybe_contains(b"hello"));
    }
}
