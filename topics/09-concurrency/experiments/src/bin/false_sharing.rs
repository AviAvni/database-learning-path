//! False sharing, measured. PROVIDED — runs now:
//!
//!   cargo run --release --bin false_sharing
//!
//! 8 threads, each hammering fetch_add on ITS OWN counter. Three layouts:
//!   packed   — counters adjacent (several per cache line)
//!   pad64    — each counter alone in 64 B (x86 line size)
//!   pad128   — each counter alone in 128 B (Apple M-series line size!)
//!
//! Predict the packed/pad128 ratio in notes.md BEFORE running. Then
//! explain any pad64 vs pad128 gap — that gap is this machine's actual
//! coherence granularity talking.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Instant;

const THREADS: usize = 8;
const INCREMENTS: u64 = 5_000_000;

#[repr(align(64))]
struct Pad64(AtomicU64);

#[repr(align(128))]
struct Pad128(AtomicU64);

fn bench(name: &str, counters: Arc<Vec<&'static AtomicU64>>) {
    let barrier = Arc::new(Barrier::new(THREADS + 1));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let counters = counters.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let c = counters[t];
                for _ in 0..INCREMENTS {
                    c.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    barrier.wait();
    let start = Instant::now();
    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed();
    let total: u64 = counters.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    assert_eq!(total, THREADS as u64 * INCREMENTS);
    println!(
        "{name:<8} {:>8.1} ms   {:>7.1} M inc/s",
        elapsed.as_secs_f64() * 1e3,
        total as f64 / elapsed.as_secs_f64() / 1e6
    );
}

fn main() {
    println!("{THREADS} threads x {INCREMENTS} increments, each on its OWN counter\n");

    let packed: &'static Vec<AtomicU64> =
        Box::leak(Box::new((0..THREADS).map(|_| AtomicU64::new(0)).collect()));
    bench("packed", Arc::new(packed.iter().collect()));

    let p64: &'static Vec<Pad64> =
        Box::leak(Box::new((0..THREADS).map(|_| Pad64(AtomicU64::new(0))).collect()));
    bench("pad64", Arc::new(p64.iter().map(|p| &p.0).collect()));

    let p128: &'static Vec<Pad128> =
        Box::leak(Box::new((0..THREADS).map(|_| Pad128(AtomicU64::new(0))).collect()));
    bench("pad128", Arc::new(p128.iter().map(|p| &p.0).collect()));

    println!("\npacked/pad128 ratio = the cost of sharing a line you never share.");
    println!("Compare redis zmalloc's padded per-thread counters (topic 6).");
}
