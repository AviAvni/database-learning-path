//! PROVIDED — the fan-out tail, as arithmetic and as simulation.
//!
//! Dean & Barroso's headline: a server that is slow once in 100 requests
//! is harmless alone, but a query that fans out to 100 such servers and
//! waits for ALL of them is slow with probability 1 − 0.99^100 ≈ 63%.
//! Fan-out converts rare slowness into common slowness; the tail of the
//! component becomes the median of the service.
//!
//! The leaf model here is a two-mode mixture: fast (uniform 1–10 ms) or,
//! with probability `p_slow`, a 1000 ms stall (GC pause, thermal
//! throttle, SSD garbage collection — the paper's list). The numbers are
//! chosen so the story is visible at every percentile.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub const FAST_MIN_MS: f64 = 1.0;
pub const FAST_MAX_MS: f64 = 10.0;
pub const SLOW_MS: f64 = 1_000.0;

/// P(at least one of n leaves is slow) — the closed form behind Figure 1.
pub fn p_any_slow(p_slow: f64, n: u32) -> f64 {
    1.0 - (1.0 - p_slow).powi(n as i32)
}

/// One leaf's latency: fast uniform, or a stall with probability p_slow.
pub fn leaf_latency(rng: &mut impl Rng, p_slow: f64) -> f64 {
    if rng.gen::<f64>() < p_slow {
        SLOW_MS
    } else {
        rng.gen_range(FAST_MIN_MS..FAST_MAX_MS)
    }
}

/// A scatter-gather that waits for ALL n leaves: latency = max of n draws.
pub fn scatter_gather(rng: &mut impl Rng, n: usize, p_slow: f64) -> f64 {
    (0..n)
        .map(|_| leaf_latency(rng, p_slow))
        .fold(0.0, f64::max)
}

/// A scatter-gather that returns once `frac` of the n leaves answered
/// (the paper's "95% of all leaf requests finish" row — good-enough
/// results drop the stragglers).
pub fn scatter_gather_frac(rng: &mut impl Rng, n: usize, p_slow: f64, frac: f64) -> f64 {
    let mut lats: Vec<f64> = (0..n).map(|_| leaf_latency(rng, p_slow)).collect();
    lats.sort_by(f64::total_cmp);
    let k = ((n as f64 * frac).ceil() as usize).clamp(1, n);
    lats[k - 1]
}

pub fn percentile(sorted: &[f64], q: f64) -> f64 {
    let idx = ((sorted.len() as f64 * q) as usize).min(sorted.len() - 1);
    sorted[idx]
}

pub fn seeded_rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The paper's two marked points, as exact arithmetic: 1-in-100
    /// slowness at 100-way fan-out → 63% of requests slow; 1-in-10,000
    /// at 2,000 servers → 18%.
    #[test]
    fn the_63_percent_and_18_percent_points_are_arithmetic() {
        let x = p_any_slow(0.01, 100);
        assert!((x - 0.633_968_f64).abs() < 1e-5, "got {x}");
        let o = p_any_slow(0.0001, 2000);
        assert!((o - 0.181_3_f64).abs() < 1e-3, "got {o}");
    }

    /// Simulation agrees with the closed form.
    #[test]
    fn simulated_fanout_matches_the_closed_form() {
        let mut rng = seeded_rng(7);
        let n_trials = 20_000;
        let slow = (0..n_trials)
            .filter(|_| scatter_gather(&mut rng, 100, 0.01) >= SLOW_MS)
            .count();
        let frac = slow as f64 / n_trials as f64;
        assert!(
            (frac - 0.634).abs() < 0.02,
            "expected ~0.634, got {frac}"
        );
    }

    /// Table 1's shape: the leaf's rare tail becomes the service's
    /// MEDIAN. One leaf's p50 is a few ms; the max over 100 leaves has
    /// a p50 of a full stall.
    #[test]
    fn the_leaf_tail_becomes_the_service_median() {
        let mut rng = seeded_rng(42);
        let mut one: Vec<f64> = (0..10_000).map(|_| leaf_latency(&mut rng, 0.01)).collect();
        let mut all: Vec<f64> = (0..10_000)
            .map(|_| scatter_gather(&mut rng, 100, 0.01))
            .collect();
        one.sort_by(f64::total_cmp);
        all.sort_by(f64::total_cmp);
        let one_p50 = percentile(&one, 0.50);
        let all_p50 = percentile(&all, 0.50);
        assert!(one_p50 < 10.0, "one leaf p50 should be fast, got {one_p50}");
        assert!(
            all_p50 >= SLOW_MS,
            "100-leaf p50 should be a stall, got {all_p50}"
        );
    }
}
