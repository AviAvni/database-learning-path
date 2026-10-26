//! A seeded generator with no dependencies, and a stateless hash.
//!
//! Two kinds of randomness are used and they must not be confused:
//!
//! * [`Rng`] is a stream: sampling rows, drawing a workload. Its output depends
//!   on how many values were drawn before, so it is only used where the order
//!   of draws is fixed by the code.
//! * [`unit`] is a pure function of `(seed, a, b)`. Every per-document fact —
//!   the ground truth, the proxy's score, whether the oracle errs — comes from
//!   it, so the same document gets the same answer no matter which plan asks,
//!   in which order, or how many times. That is the simulation's version of a
//!   temperature-0 model, and it is what makes caching meaningful (lane 1d).

/// SplitMix64 (Steele, Lea & Flood, OOPSLA'14), the finaliser everyone copies.
pub fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A uniform value in [0, 1) that depends only on its three keys.
pub fn unit(seed: u64, a: u64, b: u64) -> f64 {
    let h = mix(seed ^ mix(a.wrapping_mul(0x1000_0000_01B3) ^ mix(b ^ 0xA5A5_5A5A)));
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// A standard normal value that depends only on its keys (Box–Muller).
pub fn normal(seed: u64, a: u64, b: u64) -> f64 {
    let u1 = unit(seed, a, b.wrapping_mul(2)).max(1e-300);
    let u2 = unit(seed, a, b.wrapping_mul(2) + 1);
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// A sequential stream, for draws whose order the code fixes.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.0)
    }
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform in 0..n (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.uniform() * n as f64) as usize % n
    }
    /// `k` distinct indices from 0..n, in increasing order (partial Fisher–Yates).
    pub fn sample(&mut self, n: usize, k: usize) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..n).collect();
        let k = k.min(n);
        for i in 0..k {
            let j = i + self.below(n - i);
            idx.swap(i, j);
        }
        let mut out = idx[..k].to_vec();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_is_a_pure_function() {
        assert_eq!(unit(7, 3, 4), unit(7, 3, 4));
        assert_ne!(unit(7, 3, 4), unit(7, 4, 3));
    }

    #[test]
    fn unit_mean_and_normal_moments_are_sane() {
        let n = 200_000u64;
        let m: f64 = (0..n).map(|i| unit(1, i, 0)).sum::<f64>() / n as f64;
        assert!((m - 0.5).abs() < 0.005, "mean {m}");
        let xs: Vec<f64> = (0..n).map(|i| normal(2, i, 0)).collect();
        let mu = xs.iter().sum::<f64>() / n as f64;
        let var = xs.iter().map(|x| (x - mu) * (x - mu)).sum::<f64>() / n as f64;
        assert!(mu.abs() < 0.01 && (var - 1.0).abs() < 0.02, "mu {mu} var {var}");
    }

    #[test]
    fn sample_is_distinct_and_sorted() {
        let s = Rng::new(9).sample(100, 30);
        assert_eq!(s.len(), 30);
        assert!(s.windows(2).all(|w| w[0] < w[1]));
    }
}
