//! Topic 17 experiments — SIMD kernels, four rungs each.
//!
//! Rungs: scalar → autovec-friendly → portable (`wide`) → NEON
//! intrinsics. The provided rungs work; the stubs are yours.

pub mod dot;
pub mod filter;
pub mod unpack;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Deterministic f32 data in [0, 1).
pub fn gen_f32(n: usize, seed: u64) -> Vec<f32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n).map(|_| rng.gen::<f32>()).collect()
}

/// Deterministic bytes (two 4-bit values each).
pub fn gen_bytes(n: usize, seed: u64) -> Vec<u8> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n).map(|_| rng.gen::<u8>()).collect()
}

/// Threshold that selects ~`pct`% of uniform [0,1) values.
pub fn threshold_for_selectivity(pct: u32) -> f32 {
    pct as f32 / 100.0
}
