//! A tiny Fx-style hasher so lane 1's "hash-indexed relations" do not pay
//! SipHash. Deterministic across runs and machines.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub struct Fx(u64);

const K: u64 = 0x517c_c1b7_2722_0a95;

impl Hasher for Fx {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(K);
        }
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(K);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(K);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FxBuild = BuildHasherDefault<Fx>;
pub type FxMap<K, V> = HashMap<K, V, FxBuild>;
pub type FxSet<K> = HashSet<K, FxBuild>;

/// Pack a binary tuple into one word.
#[inline]
pub fn pack(x: u32, y: u32) -> u64 {
    ((x as u64) << 32) | y as u64
}

/// Order-independent per-tuple mix, summed (wrapping) into a checksum.
#[inline]
pub fn mix(t: u64) -> u64 {
    let mut z = t.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
