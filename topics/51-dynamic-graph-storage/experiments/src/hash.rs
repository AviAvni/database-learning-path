//! A tiny fixed-seed multiplicative hasher for `u32` keys, so the delta maps
//! are deterministic run to run (std's default hasher is randomly seeded) and
//! as cheap as a hash probe can be.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub struct Fx(u64);

impl Hasher for Fx {
    #[inline]
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 32)
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }
    #[inline]
    fn write_u32(&mut self, x: u32) {
        self.0 = (x as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

pub type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;
