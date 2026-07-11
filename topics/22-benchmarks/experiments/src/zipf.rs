//! The YCSB Zipfian generator (Gray et al.'s algorithm, as shipped in
//! go-ycsb `pkg/generator/zipfian.go` and every YCSB port since).
//! This is THE most-copied piece of benchmark code in databases —
//! and the reason "YCSB zipfian" means theta=0.99 everywhere.
//!
//! go-ycsb anchors:
//!   zipfian.go:92-118  constructor: zeta(n), zeta(2), theta, alpha,
//!                      eta precomputation
//!   zipfian.go:135-165 next(): the u*zetan < 1 / < 1+0.5^theta fast
//!                      paths, then rank = n * (eta*u - eta + 1)^alpha
//!   scrambled_zipfian.go  fnv-hash the rank so hot keys scatter
//!                      across the id space instead of clustering

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Keys for the YCSB driver: `next(n)` returns an index in 0..n.
pub trait KeyGen {
    fn next(&mut self, n: usize) -> usize;
}

pub struct Uniform(pub ChaCha8Rng);

impl Uniform {
    pub fn new(seed: u64) -> Self {
        Uniform(ChaCha8Rng::seed_from_u64(seed))
    }
}

impl KeyGen for Uniform {
    fn next(&mut self, n: usize) -> usize {
        self.0.gen_range(0..n)
    }
}

/// STUB — Zipfian over 0..items with skew `theta` (YCSB default 0.99).
/// Precompute in new():
///   zetan  = Σ_{i=1..items} 1/i^theta
///   zeta2  = 1 + 0.5^theta
///   alpha  = 1 / (1 - theta)
///   eta    = (1 - (2/items)^(1-theta)) / (1 - zeta2/zetan)
/// next():
///   u ← U(0,1); uz = u * zetan
///   if uz < 1        → 0
///   if uz < zeta2    → 1
///   else             → floor(items * (eta*u - eta + 1)^alpha)
/// Rank 0 is the hottest key — adjacent ranks are adjacent ids, so
/// the hot set is CLUSTERED (bad for realistic cache behavior; good
/// for seeing worst-case contention). See Scrambled below.
pub struct Zipfian {
    _rng: ChaCha8Rng,
    // add zetan / eta / alpha / theta / items fields
}

impl Zipfian {
    pub fn new(items: usize, theta: f64, seed: u64) -> Self {
        let _ = (items, theta);
        Zipfian { _rng: ChaCha8Rng::seed_from_u64(seed) }
    }
}

impl KeyGen for Zipfian {
    /// NOTE: ignores `n` (fixed item count) — the resizable version
    /// (go-ycsb :135, incremental zeta) is the stretch goal.
    fn next(&mut self, _n: usize) -> usize {
        todo!("Zipfian::next (see struct docs)")
    }
}

/// STUB — scrambled zipfian: item = fnv1a64(rank as 8 LE bytes) % items.
/// Same skew, hot keys scattered — what YCSB actually uses by default.
pub struct Scrambled {
    pub inner: Zipfian,
    pub items: usize,
}

impl KeyGen for Scrambled {
    fn next(&mut self, _n: usize) -> usize {
        todo!("Scrambled::next — fnv1a64 the zipfian rank")
    }
}

pub fn fnv1a64(x: u64) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in x.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn freqs(g: &mut dyn KeyGen, n: usize, draws: usize) -> Vec<f64> {
        let mut c = vec![0usize; n];
        for _ in 0..draws {
            let k = g.next(n);
            assert!(k < n, "draw {k} out of range");
            c[k] += 1;
        }
        c.into_iter().map(|x| x as f64 / draws as f64).collect()
    }

    #[test]
    fn zipfian_head_matches_theory() {
        // independent computation of the expected head probability
        let (n, theta) = (1000usize, 0.99);
        let zetan: f64 = (1..=n).map(|i| 1.0 / (i as f64).powf(theta)).sum();
        let f = freqs(&mut Zipfian::new(n, theta, 42), n, 200_000);
        let expected = 1.0 / zetan;
        assert!(
            (f[0] - expected).abs() < 0.15 * expected,
            "head freq {} vs theory {}",
            f[0],
            expected
        );
        assert!(f[0] > f[9] && f[9] > f[99], "must be skewed by rank");
    }

    #[test]
    fn zipfian_is_deterministic() {
        let mut a = Zipfian::new(100, 0.99, 7);
        let mut b = Zipfian::new(100, 0.99, 7);
        for _ in 0..100 {
            assert_eq!(a.next(100), b.next(100));
        }
    }

    #[test]
    fn scrambled_keeps_skew_but_scatters() {
        let n = 1000;
        let mut s = Scrambled { inner: Zipfian::new(n, 0.99, 42), items: n };
        let f = freqs(&mut s, n, 200_000);
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by(|&a, &b| f[b].partial_cmp(&f[a]).unwrap());
        // same head mass as plain zipfian…
        let zetan: f64 = (1..=n).map(|i| 1.0 / (i as f64).powf(0.99)).sum();
        assert!((f[idx[0]] - 1.0 / zetan).abs() < 0.15 / zetan);
        // …but the hottest key is no longer id 0 (fnv scatters ranks)
        assert_ne!(idx[0], 0);
    }
}
