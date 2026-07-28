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

/// deterministic "random" keys without RNG overhead in the timed region
fn key(i: u64) -> u64 {
    i.wrapping_mul(0x9E3779B97F4A7C15)
}

/// Time N individual inserts into `map`, reporting percentiles and the
/// per-decile max so the spikes can be lined up with the doubling points.
fn measure(name: &str, mut insert: impl FnMut(u64, u64)) -> Histogram<u64> {
    let mut h = Histogram::<u64>::new(3).unwrap();
    let mut decile_max = vec![0u64; 10];
    for i in 0..N {
        let t = Instant::now();
        insert(key(i), i);
        let ns = t.elapsed().as_nanos() as u64;
        h.record(ns).unwrap();
        let d = (i * 10 / N) as usize;
        decile_max[d] = decile_max[d].max(ns);
    }
    percentiles(name, &h);
    println!("  per-decile max (ns): {decile_max:?}\n");
    h
}

/// Run an exercise lane, reporting unimplemented `todo!()`s as a note
/// instead of a crash, so the provided lanes above always print.
fn stub_lane<T>(name: &str, f: impl FnOnce() -> T) -> Option<T> {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    if r.is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
    r.ok()
}

fn main() {
    println!("inserting {N} keys one by one, timing each insert\n");

    // lane 1 (PROVIDED): hashbrown's doubling rehash — the spike we are here for
    let mut hb = hashbrown::HashMap::new();
    let h_hb = measure("hashbrown", |k, v| {
        hb.insert(k, v);
    });

    // lane 2 (EXERCISE): your incremental rehash — same work, no spike
    let h_inc = stub_lane("incremental rehash (src/incremental_map.rs)", || {
        let mut inc = IncrementalMap::new();
        measure("incremental", |k, v| inc.insert(k, v))
    });

    match h_inc {
        Some(h_inc) => println!(
            "headline: max ratio hashbrown/incremental = {:.1}x",
            h_hb.max() as f64 / h_inc.max() as f64
        ),
        None => println!(
            "headline: hashbrown max = {} ns ({:.1} ms). The point of the exercise is\n\
             to get the second row's max down to microseconds without moving p50 much.",
            h_hb.max(),
            h_hb.max() as f64 / 1e6
        ),
    }
}
