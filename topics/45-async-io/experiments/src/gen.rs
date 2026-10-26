//! Seeded generators. Every offset sequence and every write stream in this
//! crate comes from here, so the *which pages* half of each figure reproduces
//! exactly; only the timings depend on your hardware.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rand_distr::{Distribution, Zipf};

pub const SEED: u64 = 45;

/// `n` page-aligned byte offsets drawn uniformly from a file of `pages` pages.
pub fn random_offsets(seed: u64, n: usize, pages: u64) -> Vec<u64> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n)
        .map(|_| rng.gen_range(0..pages) * crate::PAGE as u64)
        .collect()
}

/// `n` page ids in `0..pages` drawn from Zipf(theta), then scattered by a
/// fixed permutation so the hot pages are not all at low ids (the papers'
/// YCSB generator scrambles keys the same way). theta = 0 means uniform.
pub fn page_stream(seed: u64, n: usize, pages: u32, theta: f64) -> Vec<u32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    if theta == 0.0 {
        return (0..n).map(|_| rng.gen_range(0..pages)).collect();
    }
    let zipf = Zipf::new(pages as u64, theta).expect("zipf");
    // a seeded Fisher-Yates permutation of the page ids
    let mut perm: Vec<u32> = (0..pages).collect();
    for i in (1..perm.len()).rev() {
        let j = rng.gen_range(0..=i);
        perm.swap(i, j);
    }
    (0..n)
        .map(|_| {
            let rank = zipf.sample(&mut rng) as usize - 1; // 1-based
            perm[rank]
        })
        .collect()
}

/// Seeded file contents, so a checksum over what was read is meaningful.
pub fn fill_bytes(seed: u64, buf: &mut [u8]) {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    rng.fill(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_seeded_and_aligned() {
        let a = random_offsets(SEED, 1000, 1 << 18);
        let b = random_offsets(SEED, 1000, 1 << 18);
        assert_eq!(a, b);
        assert!(a.iter().all(|o| o % crate::PAGE as u64 == 0));
    }

    #[test]
    fn zipf_stream_is_skewed() {
        let s = page_stream(SEED, 100_000, 10_000, 0.8);
        let mut counts = vec![0u32; 10_000];
        for p in &s {
            counts[*p as usize] += 1;
        }
        counts.sort_unstable_by(|a, b| b.cmp(a));
        let top1pct: u32 = counts[..100].iter().sum();
        // uniform would put 1% of the writes on the top 1% of pages
        assert!(top1pct > 10_000, "top 1% got only {top1pct} of 100000");
    }
}
