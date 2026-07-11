//! Provided: three column shapes with very different compressibility.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Sorted-ish, low cardinality: long runs. RLE heaven.
/// (Think: a status column in a table ordered by status, ts.)
pub fn sorted_low_cardinality(n: usize, seed: u64) -> Vec<u64> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut v = Vec::with_capacity(n);
    let mut current = 0u64;
    while v.len() < n {
        let run = rng.gen_range(1000..50_000).min(n - v.len());
        v.extend(std::iter::repeat(current).take(run));
        current += rng.gen_range(1..4);
    }
    v
}

/// Unsorted, low cardinality (64 distinct values). Dictionary heaven,
/// RLE hell.
pub fn shuffled_low_cardinality(n: usize, seed: u64) -> Vec<u64> {
    let mut rng = StdRng::seed_from_u64(seed);
    // distinct values are LARGE so bit-packing raw values can't win
    let dict: Vec<u64> = (0..64).map(|_| rng.gen()).collect();
    (0..n).map(|_| dict[rng.gen_range(0..64)]).collect()
}

/// Uniform random in a small range (0..4096): bit-packing heaven
/// (12 bits/value), dictionary mediocre (4096 entries), RLE hell.
pub fn small_range_random(n: usize, seed: u64) -> Vec<u64> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n).map(|_| rng.gen_range(0..4096)).collect()
}
