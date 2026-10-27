//! Seeded workload generator: a build side R with unique keys and a probe
//! side S of foreign keys into it.
//!
//! * R has `n_build` tuples. Key of the i-th R tuple is `key_of(i)`, a
//!   bijection on u32 (multiply by an odd constant), so the keys are unique,
//!   spread over the whole 32-bit range, and uncorrelated with `i`. R is then
//!   shuffled so its order carries no information.
//! * S has `n_probe` tuples. Each one draws a *rank* in `0..n_build` — uniform,
//!   or Zipf-distributed with exponent `theta` (rank 0 hottest) — and takes
//!   `key_of(rank)`. Because `key_of` scatters ranks, the hot keys are not
//!   adjacent in memory. With `hit_pct < 100`, that percentage of probes
//!   draw a rank, the rest take `key_of(n_build + j)`, a key that is not in R.

use crate::tuple::Tuple;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub const SEED: u64 = 0x544A_4F49_4E54;

/// The i-th key. Odd multiplier => bijection on u32. `i + 1` keeps 0 free;
/// the generator asserts u32::MAX is never produced (it is part of the
/// table's empty marker in lane 1).
#[inline]
pub fn key_of(i: u32) -> u32 {
    (i.wrapping_add(1)).wrapping_mul(0x9E37_79B1)
}

#[derive(Clone, Copy, Debug)]
pub struct Workload {
    pub n_build: usize,
    pub n_probe: usize,
    /// Zipf exponent for the probe ranks; 0.0 means uniform. Must be < 1.0.
    pub theta: f64,
    /// Percentage of probes whose key exists in R.
    pub hit_pct: u32,
}

impl Workload {
    pub fn uniform(n_build: usize, n_probe: usize) -> Self {
        Workload { n_build, n_probe, theta: 0.0, hit_pct: 100 }
    }
    pub fn zipf(n_build: usize, n_probe: usize, theta: f64) -> Self {
        Workload { n_build, n_probe, theta, hit_pct: 100 }
    }
}

/// Build R (shuffled) and S.
pub fn generate(w: &Workload) -> (Vec<Tuple>, Vec<Tuple>) {
    assert!(w.n_build > 0 && w.n_build < (1 << 30));
    let mut rng = ChaCha8Rng::seed_from_u64(SEED);
    let mut r: Vec<Tuple> = (0..w.n_build as u32)
        .map(|i| {
            let key = key_of(i);
            assert_ne!(key, u32::MAX, "key collides with the empty marker");
            Tuple { key, pay: i }
        })
        .collect();
    // Fisher-Yates
    for i in (1..r.len()).rev() {
        let j = rng.gen_range(0..=i);
        r.swap(i, j);
    }

    let zipf = if w.theta > 0.0 { Some(Zipf::new(w.n_build as u64, w.theta)) } else { None };
    let mut s = Vec::with_capacity(w.n_probe);
    for i in 0..w.n_probe as u32 {
        let hit = w.hit_pct >= 100 || rng.gen_range(0..100) < w.hit_pct;
        let key = if hit {
            let rank = match zipf.as_ref() {
                Some(z) => z.sample(&mut rng),
                None => rng.gen_range(0..w.n_build as u64),
            };
            key_of(rank as u32)
        } else {
            key_of((w.n_build as u32).wrapping_add(rng.gen_range(0..w.n_build as u32)))
        };
        s.push(Tuple { key, pay: i });
    }
    (r, s)
}

/// Zipf(n, theta) by the closed-form inversion of Gray et al., "Quickly
/// Generating Billion-Record Synthetic Databases" (SIGMOD 1994), the generator
/// YCSB uses. Restricted to theta in (0, 1).
pub struct Zipf {
    n: f64,
    theta: f64,
    alpha: f64,
    zetan: f64,
    eta: f64,
}

impl Zipf {
    pub fn new(n: u64, theta: f64) -> Self {
        assert!(theta > 0.0 && theta < 1.0, "theta must be in (0, 1)");
        let zetan = zeta(n, theta);
        let zeta2 = zeta(2, theta);
        let alpha = 1.0 / (1.0 - theta);
        let eta = (1.0 - (2.0 / n as f64).powf(1.0 - theta)) / (1.0 - zeta2 / zetan);
        Zipf { n: n as f64, theta, alpha, zetan, eta }
    }

    /// A rank in `0..n`, rank 0 the most likely.
    pub fn sample<R: Rng>(&self, rng: &mut R) -> u64 {
        let u: f64 = rng.gen();
        let uz = u * self.zetan;
        if uz < 1.0 {
            return 0;
        }
        if uz < 1.0 + 0.5f64.powf(self.theta) {
            return 1;
        }
        let r = (self.n * (self.eta * u - self.eta + 1.0).powf(self.alpha)) as u64;
        r.min(self.n as u64 - 1)
    }
}

fn zeta(n: u64, theta: f64) -> f64 {
    (1..=n).map(|i| 1.0 / (i as f64).powf(theta)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let w = Workload::zipf(1000, 5000, 0.9);
        assert_eq!(generate(&w), generate(&w));
    }

    #[test]
    fn build_keys_unique() {
        let (r, _) = generate(&Workload::uniform(10_000, 10));
        let mut k: Vec<u32> = r.iter().map(|t| t.key).collect();
        k.sort_unstable();
        k.dedup();
        assert_eq!(k.len(), 10_000);
    }

    #[test]
    fn zipf_is_skewed_and_uniform_is_not() {
        let n = 100_000;
        let count_top = |theta: f64| {
            let (r, s) = generate(&Workload { n_build: n, n_probe: 200_000, theta, hit_pct: 100 });
            let hot = r.iter().find(|t| t.pay == 0).unwrap().key; // rank 0's key
            s.iter().filter(|t| t.key == hot).count()
        };
        assert!(count_top(0.0) < 20);
        assert!(count_top(0.99) > 5_000);
    }

    #[test]
    fn hit_pct_controls_matches() {
        let w = Workload { n_build: 1000, n_probe: 20_000, theta: 0.0, hit_pct: 25 };
        let (r, s) = generate(&w);
        let keys: std::collections::HashSet<u32> = r.iter().map(|t| t.key).collect();
        let hits = s.iter().filter(|t| keys.contains(&t.key)).count();
        assert!((4_000..6_000).contains(&hits), "hits = {hits}");
    }
}
