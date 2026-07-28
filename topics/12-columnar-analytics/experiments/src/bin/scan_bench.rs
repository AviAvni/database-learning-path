//! Provided: does scanning ENCODED data beat scanning raw?
//!
//!   cargo run --release --bin scan_bench
//!
//! Lane 1 (the raw baseline) runs today. The encoded lanes are the
//! exercise: they print a `[stub — ...]` note until `encodings.rs` is
//! implemented, so the baseline above them always survives.
//!
//! Predict in notes.md before running: for each (shape, encoding), is the
//! encoded scan faster or slower than raw, and why (bytes moved vs decode
//! work vs shortcuts)?
//!
//! Note on the timing loop (topic 0's lesson, learned the hard way here):
//! every timed closure runs its input through `black_box`. Without it LLVM
//! hoists these pure folds *out* of the repetition loop — it computes the
//! sum once, reuses it for the remaining reps, and the fastest rep clocks
//! in at 0.000 s. This benchmark used to print 19,047,619 GB/s for the raw
//! lane, which is roughly 20,000× the machine's memory bandwidth and was
//! entirely a measurement artifact.

use std::hint::black_box;
use std::time::Instant;

use columnar_experiments::data;
use columnar_experiments::encodings::{BitPacked, Dict, Rle};

const N: usize = 100_000_000;
const REPS: usize = 3;

/// Below this, the reported rate is not a measurement — it is timer noise
/// or an elided loop, and we say so rather than print a figure.
const MIN_CREDIBLE_SECS: f64 = 1e-4;

fn time<T>(mut f: impl FnMut() -> T) -> (f64, T) {
    let mut best = f64::MAX;
    let mut out = None;
    for _ in 0..REPS {
        let start = Instant::now();
        let r = black_box(f());
        best = best.min(start.elapsed().as_secs_f64());
        out = Some(r);
    }
    (best, out.unwrap())
}

fn report(name: &str, raw_bytes: usize, enc_bytes: usize, secs: f64, sum: u64) {
    let mb = enc_bytes as f64 / 1e6;
    if secs < MIN_CREDIBLE_SECS {
        println!(
            "  {name:<22} {mb:>7.1} MB  {secs:>7.3} s  {:>6} GB/s(raw-equiv)  sum={sum}",
            "n/a"
        );
        println!("      ^ below timer resolution — treat as unmeasured, not as fast");
        return;
    }
    let gbps = raw_bytes as f64 / secs / 1e9;
    println!("  {name:<22} {mb:>7.1} MB  {secs:>7.3} s  {gbps:>6.1} GB/s(raw-equiv)  sum={sum}");
}

/// Lane 1 (PROVIDED): the raw baseline — 800 MB of u64 through a fold.
/// This is the memory-bandwidth floor every encoded scan is measured against.
fn lane1_raw(values: &[u64]) {
    let raw_bytes = values.len() * 8;
    let (t, s) = time(|| {
        black_box(values)
            .iter()
            .copied()
            .fold(0u64, u64::wrapping_add)
    });
    report("raw sum", raw_bytes, raw_bytes, t, s);
}

/// Lanes 2-3 (EXERCISE): scans over the encoded forms. Returns false the
/// first time it hits an unimplemented encoding, so the caller can stop
/// re-announcing the same stub for every shape.
fn encoded_lanes(values: &[u64]) {
    let raw_bytes = values.len() * 8;

    // lane 2a: RLE — sum on the encoding itself, one multiply-add per run
    let rle = Rle::encode(values);
    let (t, s) = time(|| black_box(&rle).sum());
    report("rle sum (no decode)", raw_bytes, rle.size_bytes(), t, s);
    let (t, s) = time(|| {
        black_box(&rle)
            .decode()
            .iter()
            .copied()
            .fold(0u64, u64::wrapping_add)
    });
    report("rle decode+sum", raw_bytes, rle.size_bytes(), t, s);

    // lane 2b: dictionary — process-compressed, sum via per-code counts
    let dict = Dict::encode(values);
    let (t, s) = time(|| {
        let dict = black_box(&dict);
        let mut counts = vec![0u64; dict.dict.len()];
        for &c in &dict.codes {
            counts[c as usize] += 1;
        }
        counts
            .iter()
            .zip(&dict.dict)
            .fold(0u64, |acc, (&n, &v)| acc.wrapping_add(n.wrapping_mul(v)))
    });
    report("dict sum (codes only)", raw_bytes, dict.size_bytes(), t, s);

    // lane 3: frame-of-reference bit-packing — decode then sum
    let bp = BitPacked::encode(values);
    let (t, s) = time(|| {
        black_box(&bp)
            .decode()
            .iter()
            .copied()
            .fold(0u64, u64::wrapping_add)
    });
    report("bitpack decode+sum", raw_bytes, bp.size_bytes(), t, s);

    println!(
        "  sizes: raw {} MB | rle {} MB | dict {} MB | bitpack {} MB",
        raw_bytes / 1_000_000,
        rle.size_bytes() / 1_000_000,
        dict.size_bytes() / 1_000_000,
        bp.size_bytes() / 1_000_000
    );
}

/// Run an exercise lane, reporting unimplemented `todo!()`s as a note
/// instead of a crash, so the provided lanes always print. Returns false
/// if the lane is still stubbed.
fn try_lane(f: impl FnOnce()) -> bool {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    r.is_ok()
}

fn main() {
    let shapes: [(&str, fn(usize, u64) -> Vec<u64>); 3] = [
        ("sorted low-cardinality", data::sorted_low_cardinality),
        ("shuffled low-cardinality", data::shuffled_low_cardinality),
        ("small-range random", data::small_range_random),
    ];

    let mut encoded_ok = true;
    for (name, gen) in shapes {
        // one shape live at a time: 100 M u64 is 800 MB
        let values = gen(N, 42);
        println!(
            "\n== {name} ({} M values, {} MB raw)",
            N / 1_000_000,
            N * 8 / 1_000_000
        );
        lane1_raw(&values);
        if encoded_ok {
            encoded_ok = try_lane(|| encoded_lanes(&values));
        }
    }

    if !encoded_ok {
        println!(
            "\n[stub — implement src/encodings.rs to unlock the encoded scan lanes]\n\
             \x20 `cargo test` shows the contract: round-trips, exact sizes, width-0\n\
             \x20 bit-packing, and Rle::sum operating on runs without decoding."
        );
    }

    println!("\nnotes:");
    println!("- 'raw-equiv GB/s' = raw bytes / time: >memory-bandwidth means the");
    println!("  encoding beat the memory bus — compression IS performance");
    println!("- the raw lane IS your machine's scan bandwidth; compare it to the");
    println!("  DRAM figure from topic 0's cache_ladder before trusting either");
    println!("- record the full table in notes.md");
}
