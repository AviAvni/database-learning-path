//! A small, fixed hash (PROVIDED).
//!
//! Every hash map and set in this crate is used for lookups only — nothing
//! ever iterates one — so the choice of hasher cannot change a count. A fixed
//! function (rather than std's randomly seeded SipHash) keeps the Bloom
//! filters in lane 2 bit-for-bit reproducible, which their false-positive
//! counts depend on.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// splitmix64's finaliser: a bijection on u64 with good avalanche.
#[inline]
pub fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Hasher for u32 keys: one `mix64` per key.
#[derive(Default, Clone, Copy)]
pub struct MixHasher(u64);

impl Hasher for MixHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = mix64(self.0 ^ b as u64);
        }
    }
    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.0 = mix64(self.0 ^ n as u64);
    }
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.0 = mix64(self.0 ^ n);
    }
    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.0 = mix64(self.0 ^ n as u64);
    }
}

pub type Build = BuildHasherDefault<MixHasher>;
pub type FastMap<K, V> = HashMap<K, V, Build>;
pub type FastSet<K> = HashSet<K, Build>;
