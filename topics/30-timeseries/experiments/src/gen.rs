//! Workload generator — PROVIDED. Metrics traffic is *regular*: that
//! regularity is the entire compression story. Three value shapes cover
//! the taxonomy Gorilla (VLDB '15 §2.1) measured at Facebook.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Scrape-style timestamps: nominal `interval_ms` apart with +-`jitter_ms`
/// of uniform noise (real scrapers are almost, not exactly, periodic).
pub fn scrape_timestamps(n: usize, start: i64, interval_ms: i64, jitter_ms: i64, seed: u64) -> Vec<i64> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n as i64)
        .map(|i| {
            let j = if jitter_ms > 0 { rng.gen_range(-jitter_ms..=jitter_ms) } else { 0 };
            start + i * interval_ms + j
        })
        .collect()
}

/// Gauge: bounded random walk (cpu%, temperature, queue depth).
pub fn gauge_values(n: usize, seed: u64) -> Vec<f64> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut v = 50.0f64;
    (0..n)
        .map(|_| {
            v = (v + rng.gen_range(-1.0..1.0)).clamp(0.0, 100.0);
            v
        })
        .collect()
}

/// Counter: monotonically increasing, integer-valued steps, occasional
/// reset to 0 (process restart) — the most common metric type in practice.
pub fn counter_values(n: usize, seed: u64) -> Vec<f64> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut v = 0.0f64;
    (0..n)
        .map(|_| {
            if rng.gen_ratio(1, 10_000) {
                v = 0.0;
            }
            v += rng.gen_range(0..50) as f64;
            v
        })
        .collect()
}

/// Constant: the shockingly common "this gauge never changes" series.
pub fn constant_values(n: usize) -> Vec<f64> {
    vec![1.0; n]
}

/// Adversarial: full-entropy doubles — the workload XOR compression
/// cannot help (compression floor test).
pub fn random_values(n: usize, seed: u64) -> Vec<f64> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n).map(|_| rng.gen::<f64>() * 1e9).collect()
}

/// Displace a fraction `p` of samples backwards by up to `max_lag_ms`
/// (still > watermark if within the OOO window) — the out-of-order
/// ingestion workload. Returns (t, v) in ARRIVAL order.
pub fn with_out_of_order(
    ts: &[i64],
    vs: &[f64],
    p: f64,
    max_lag_ms: i64,
    seed: u64,
) -> Vec<(i64, f64)> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    ts.iter()
        .zip(vs)
        .map(|(&t, &v)| {
            if rng.gen_bool(p) {
                (t - rng.gen_range(1..=max_lag_ms), v)
            } else {
                (t, v)
            }
        })
        .collect()
}

/// Label sets for `n_series` synthetic series: a few low-cardinality
/// labels plus one unique-per-series label (the cardinality bomb).
pub fn label_sets(n_series: usize) -> Vec<Vec<(String, String)>> {
    (0..n_series)
        .map(|i| {
            vec![
                ("job".into(), format!("job-{}", i % 10)),
                ("env".into(), if i % 5 == 0 { "prod".into() } else { "dev".into() }),
                ("region".into(), format!("r{}", i % 3)),
                ("instance".into(), format!("i-{i}")),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_sorted_when_jitter_is_small() {
        let ts = scrape_timestamps(10_000, 0, 10_000, 100, 1);
        assert!(ts.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn counter_mostly_increases() {
        let vs = counter_values(100_000, 2);
        let decreases = vs.windows(2).filter(|w| w[1] < w[0]).count();
        assert!(decreases < 30, "only resets may decrease: {decreases}");
    }

    #[test]
    fn ooo_fraction_is_respected() {
        let ts = scrape_timestamps(100_000, 0, 10_000, 0, 3);
        let vs = constant_values(100_000);
        let arr = with_out_of_order(&ts, &vs, 0.1, 60_000, 4);
        let ooo = arr.windows(2).filter(|w| w[1].0 <= w[0].0).count();
        let frac = ooo as f64 / arr.len() as f64;
        assert!((0.05..0.20).contains(&frac), "ooo fraction {frac}");
    }
}
