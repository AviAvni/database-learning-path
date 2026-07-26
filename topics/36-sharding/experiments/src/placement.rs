//! PROVIDED — the problem, measured.
//!
//! Two facts every sharding design answers to:
//!
//!  1. `hash(key) mod N` couples *placement* to *N*: growing 4 shards to
//!     5 moves 80% of all keys — exactly, provably (see the tests). The
//!     whole point of consistent hashing is to make that 1/(N+1).
//!  2. Hashing balances *keys*, not *load*: under a Zipf-skewed workload
//!     the rank-1 key's entire traffic lands on one shard, and no hash
//!     function can split a single key. Only range splitting (or caching)
//!     addresses a hot key.

/// A cheap, deterministic 64-bit mixer (Steele et al.). Used everywhere
/// in this crate so runs are reproducible without an RNG.
pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

/// Fraction of keys whose shard changes when `key mod from` becomes
/// `key mod to`.
pub fn modn_movement<I: Iterator<Item = u64>>(keys: I, from: u64, to: u64) -> f64 {
    let (mut moved, mut total) = (0u64, 0u64);
    for k in keys {
        total += 1;
        if k % from != k % to {
            moved += 1;
        }
    }
    moved as f64 / total as f64
}

/// Zipf(s) sampler over ranks `0..n` via the harmonic CDF and binary
/// search — no rand_distr needed, and the CDF makes the skew explicit:
/// P(rank r) = (1/(r+1)^s) / H_{n,s}.
pub struct Zipf {
    cdf: Vec<f64>,
}

impl Zipf {
    pub fn new(n: usize, s: f64) -> Self {
        let mut cdf = Vec::with_capacity(n);
        let mut acc = 0.0;
        for r in 1..=n {
            acc += 1.0 / (r as f64).powf(s);
            cdf.push(acc);
        }
        for c in &mut cdf {
            *c /= acc;
        }
        Zipf { cdf }
    }

    /// 0-based rank; rank 0 is the hottest key.
    pub fn sample(&self, rng: &mut impl rand::Rng) -> usize {
        let u: f64 = rng.gen();
        self.cdf.partition_point(|&c| c < u).min(self.cdf.len() - 1)
    }
}

/// Traffic share of the hottest of `shards` hash shards when `samples`
/// requests over `n_keys` keys follow Zipf(s). Ideal is 1/shards.
pub fn hot_shard_share(n_keys: usize, s: f64, shards: usize, samples: usize, seed: u64) -> f64 {
    use rand::SeedableRng;
    let zipf = Zipf::new(n_keys, s);
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
    let mut counts = vec![0u64; shards];
    for _ in 0..samples {
        let rank = zipf.sample(&mut rng);
        let shard = (splitmix64(rank as u64 + 1) % shards as u64) as usize;
        counts[shard] += 1;
    }
    *counts.iter().max().unwrap() as f64 / samples as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The closed form: k mod 4 == k mod 5 iff k mod 20 < 4, so exactly
    /// 4 of every 20 consecutive keys stay put — 80% move. 100_000 is a
    /// multiple of 20, so the fraction is exact.
    #[test]
    fn modn_4_to_5_moves_exactly_80_percent() {
        let m = modn_movement(0..100_000u64, 4, 5);
        assert!((m - 0.80).abs() < 1e-12, "got {m}");
    }

    /// Same story for hashed (uniform) keys: P(h mod 4 == h mod 5) = 4/20
    /// by CRT, so movement concentrates near 80%.
    #[test]
    fn modn_movement_hashed_keys_same_story() {
        let m = modn_movement((0..100_000u64).map(splitmix64), 4, 5);
        assert!(m > 0.78 && m < 0.82, "got {m}");
    }

    /// Zipf(1.0) over 10k keys: the rank-0 key alone carries
    /// 1/H_10000 ≈ 10.2% of traffic. Whatever shard its hash lands on is
    /// far above the 6.25% ideal — and hashing can never split it.
    #[test]
    fn zipf_hot_shard_far_exceeds_ideal() {
        let share = hot_shard_share(10_000, 1.0, 16, 200_000, 42);
        assert!(share > 0.12, "got {share}");
        assert!(share < 0.35, "got {share}");
    }
}
