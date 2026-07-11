//! Provided: columnar test data.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::NUM_GROUPS;

/// Struct-of-arrays table. Columns:
///   k: group key, dense in 0..NUM_GROUPS
///   v: value to sum
///   f: filter column, uniform in 0..100 (so `f < t` has selectivity t%)
pub struct Table {
    pub k: Vec<u32>,
    pub v: Vec<i32>,
    pub f: Vec<u32>,
}

impl Table {
    pub fn len(&self) -> usize {
        self.k.len()
    }

    pub fn is_empty(&self) -> bool {
        self.k.is_empty()
    }

    pub fn generate(rows: usize, seed: u64) -> Table {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut t = Table {
            k: Vec::with_capacity(rows),
            v: Vec::with_capacity(rows),
            f: Vec::with_capacity(rows),
        };
        for _ in 0..rows {
            t.k.push(rng.gen_range(0..NUM_GROUPS as u32));
            t.v.push(rng.gen_range(-1000..1000));
            t.f.push(rng.gen_range(0..100));
        }
        t
    }
}
