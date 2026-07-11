//! rehash_spike — the headline experiment: per-insert tail latency,
//! doubling rehash (hashbrown) vs incremental rehash (yours).
//!
//! HdrHistogram, not criterion: we care about the MAX and p99.9 of individual
//! inserts, which averaging destroys (topic 0 rules).
//!
//! Run: cargo run --release --bin rehash_spike
//!
//! Expected shape:
//!   hashbrown:   p50 tiny, max = milliseconds (the 8M→16M doubling sweep)
//!   incremental: p50 slightly higher (every op pays a bucket migration),
//!                max ~ microseconds — the spike is amortized away
//!
//! Also prints per-decile max so you can SEE the spikes line up with
//! power-of-two boundaries. Paste the table into notes.md.

use hdrhistogram::Histogram;
use std::time::Instant;
use topic02_experiments::IncrementalMap;

const N: u64 = 10_000_000;

fn percentiles(name: &str, h: &Histogram<u64>) {
    println!(
        "{name:<14} p50={:>8}ns p99={:>8}ns p99.9={:>10}ns p99.99={:>10}ns max={:>12}ns",
        h.value_at_quantile(0.5),
        h.value_at_quantile(0.99),
        h.value_at_quantile(0.999),
        h.value_at_quantile(0.9999),
        h.max()
    );
}

fn main() {
    // deterministic "random" keys without RNG overhead in the timed region
    let key = |i: u64| i.wrapping_mul(0x9E3779B97F4A7C15);

    println!("inserting {N} keys one by one, timing each insert\n");

    let mut h_hb = Histogram::<u64>::new(3).unwrap();
    let mut hb = hashbrown::HashMap::new();
    let mut decile_max = vec![0u64; 10];
    for i in 0..N {
        let t = Instant::now();
        hb.insert(key(i), i);
        let ns = t.elapsed().as_nanos() as u64;
        h_hb.record(ns).unwrap();
        let d = (i * 10 / N) as usize;
        decile_max[d] = decile_max[d].max(ns);
    }
    percentiles("hashbrown", &h_hb);
    println!("  per-decile max (ns): {decile_max:?}\n");

    let mut h_inc = Histogram::<u64>::new(3).unwrap();
    let mut inc = IncrementalMap::new();
    let mut decile_max = vec![0u64; 10];
    for i in 0..N {
        let t = Instant::now();
        inc.insert(key(i), i);
        let ns = t.elapsed().as_nanos() as u64;
        h_inc.record(ns).unwrap();
        let d = (i * 10 / N) as usize;
        decile_max[d] = decile_max[d].max(ns);
    }
    percentiles("incremental", &h_inc);
    println!("  per-decile max (ns): {decile_max:?}");

    println!("\nheadline: max ratio hashbrown/incremental = {:.1}x", h_hb.max() as f64 / h_inc.max() as f64);
}
