//! Provided: does scanning ENCODED data beat scanning raw?
//!
//!   cargo run --release --bin scan_bench
//!
//! Panics on the stubs until encodings.rs is implemented. The raw
//! baseline runs first regardless. Predict in notes.md before running:
//! for each (shape, encoding), is the encoded scan faster or slower
//! than raw, and why (bytes moved vs decode work vs shortcuts)?

use std::time::Instant;

use columnar_experiments::data;
use columnar_experiments::encodings::{BitPacked, Dict, Rle};

const N: usize = 100_000_000;
const REPS: usize = 3;

fn time<T>(f: impl Fn() -> T) -> (f64, T) {
    let mut best = f64::MAX;
    let mut out = None;
    for _ in 0..REPS {
        let start = Instant::now();
        let r = f();
        best = best.min(start.elapsed().as_secs_f64());
        out = Some(r);
    }
    (best, out.unwrap())
}

fn report(name: &str, raw_bytes: usize, enc_bytes: usize, secs: f64, sum: u64) {
    let gbps = raw_bytes as f64 / secs / 1e9;
    println!(
        "  {name:<22} {:>7.1} MB  {secs:>7.3} s  {gbps:>6.1} GB/s(raw-equiv)  sum={sum}",
        enc_bytes as f64 / 1e6
    );
}

fn bench_shape(name: &str, values: &[u64]) {
    println!("\n== {name} ({} M values, {} MB raw)", N / 1_000_000, N * 8 / 1_000_000);
    let raw_bytes = values.len() * 8;

    let (t, s) = time(|| values.iter().copied().fold(0u64, u64::wrapping_add));
    report("raw sum", raw_bytes, raw_bytes, t, s);

    let rle = Rle::encode(values);
    let (t, s) = time(|| rle.sum());
    report("rle sum (no decode)", raw_bytes, rle.size_bytes(), t, s);
    let (t, s) = time(|| rle.decode().iter().copied().fold(0u64, u64::wrapping_add));
    report("rle decode+sum", raw_bytes, rle.size_bytes(), t, s);

    let dict = Dict::encode(values);
    let (t, s) = time(|| {
        // process-compressed: sum via per-code counts, decode never
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

    let bp = BitPacked::encode(values);
    let (t, s) = time(|| bp.decode().iter().copied().fold(0u64, u64::wrapping_add));
    report("bitpack decode+sum", raw_bytes, bp.size_bytes(), t, s);

    println!(
        "  sizes: raw {} MB | rle {} MB | dict {} MB | bitpack {} MB",
        raw_bytes / 1_000_000,
        rle.size_bytes() / 1_000_000,
        dict.size_bytes() / 1_000_000,
        bp.size_bytes() / 1_000_000
    );
}

fn main() {
    bench_shape("sorted low-cardinality", &data::sorted_low_cardinality(N, 42));
    bench_shape("shuffled low-cardinality", &data::shuffled_low_cardinality(N, 42));
    bench_shape("small-range random", &data::small_range_random(N, 42));

    println!("\nnotes:");
    println!("- 'raw-equiv GB/s' = raw bytes / time: >memory-bandwidth means the");
    println!("  encoding beat the memory bus — compression IS performance");
    println!("- record the full table + your Mac's ~bandwidth in notes.md");
}
