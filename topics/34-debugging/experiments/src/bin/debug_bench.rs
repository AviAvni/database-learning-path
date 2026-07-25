use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use debug_experiments::histogram::LogHistogram;
use debug_experiments::slowlog::SlowLog;
use debug_experiments::workload::{closed_loop, open_loop, percentile, StallModel};

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

fn fmt_ns(ns: u64) -> String {
    if ns >= 1_000_000 {
        format!("{:.1} ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1} µs", ns as f64 / 1e3)
    } else {
        format!("{ns} ns")
    }
}

/// Lane 1 (PROVIDED): coordinated omission. One service, one stall
/// pattern, two measurement protocols.
fn lane1_coordinated_omission() {
    println!("== lane 1: coordinated omission — closed vs open loop ==");
    let m = StallModel { service_ns: 1_000, stall_every: 100_000, stall_ns: 100_000_000 };
    let n = 1_000_000u64;
    let interval = 10_000u64; // 100K ops/s intended
    println!(
        "   {n} ops, service 1 µs, 100 ms stall every 100K ops, arrivals every 10 µs"
    );
    println!("   protocol      p50        p99        p99.9      p99.99     max");
    for (name, mut lat) in [
        ("closed-loop", closed_loop(n, m)),
        ("open-loop  ", open_loop(n, m, interval)),
    ] {
        println!(
            "   {name}   {:>8}   {:>8}   {:>8}   {:>8}   {:>8}",
            fmt_ns(percentile(&mut lat, 50.0)),
            fmt_ns(percentile(&mut lat, 99.0)),
            fmt_ns(percentile(&mut lat, 99.9)),
            fmt_ns(percentile(&mut lat, 99.99)),
            fmt_ns(*lat.last().unwrap()),
        );
    }
    println!();
}

/// Lane 2 (needs histogram.rs): what the log-bucketed histogram costs
/// and what it gets wrong, vs keeping and sorting every sample.
fn lane2_histogram_priced() {
    println!("== lane 2: log-bucketed histogram vs sort-everything ==");
    let n = 10_000_000usize;
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let samples: Vec<u64> = (0..n)
        .map(|_| {
            // latency-shaped: mostly µs, occasionally ms
            let base = rng.gen_range(500..5_000u64);
            if rng.gen_ratio(1, 1000) { base * 1000 } else { base }
        })
        .collect();

    let t = Instant::now();
    let mut h = LogHistogram::new(5);
    for &v in &samples {
        h.record(v);
    }
    let record = t.elapsed();

    let t = Instant::now();
    let mut exact = samples.clone();
    exact.sort_unstable();
    let sort = t.elapsed();

    println!("   {n} samples; histogram sub_bits=5 (≤3.1% error)");
    println!(
        "   record: {:.1} ns/op   sort-everything: {:.1} ns/op",
        record.as_nanos() as f64 / n as f64,
        sort.as_nanos() as f64 / n as f64
    );
    println!(
        "   memory: histogram {} KB vs samples {} MB",
        h.bucket_count() * 8 / 1024,
        n * 8 / (1024 * 1024)
    );
    println!("   pct      exact        histogram");
    for p in [50.0, 99.0, 99.9, 99.99] {
        let rank = ((p / 100.0) * n as f64).ceil() as usize - 1;
        println!(
            "   p{p:<6} {:>10}   {:>10}",
            fmt_ns(exact[rank]),
            fmt_ns(h.percentile(p))
        );
    }
    println!();
}

/// Lane 3 (needs both stubs): the observability tax — ns/op of a hot op
/// loop, bare vs instrumented. This is the number M34's always-on level
/// must keep small.
fn lane3_observability_tax() {
    println!("== lane 3: the observability tax on a hot loop ==");
    let n = 10_000_000u64;
    let work = |i: u64| -> u64 {
        // a cheap "command": a few ns of real work
        let mut x = i.wrapping_mul(0x9E3779B97F4A7C15);
        x ^= x >> 32;
        black_box(x)
    };

    let t = Instant::now();
    for i in 0..n {
        work(i);
    }
    let bare = t.elapsed();

    let t = Instant::now();
    for i in 0..n {
        let s = Instant::now();
        work(i);
        black_box(s.elapsed());
    }
    let timed = t.elapsed();

    let mut h = LogHistogram::new(5);
    let t = Instant::now();
    for i in 0..n {
        let s = Instant::now();
        work(i);
        h.record(s.elapsed().as_nanos() as u64);
    }
    let hist = t.elapsed();

    let mut sl = SlowLog::new(1_000_000, 128); // 1 ms threshold: never fires here
    let mut h2 = LogHistogram::new(5);
    let t = Instant::now();
    for i in 0..n {
        let s = Instant::now();
        work(i);
        let d = s.elapsed().as_nanos() as u64;
        h2.record(d);
        sl.add("op", d);
    }
    let full = t.elapsed();

    let per = |d: std::time::Duration| d.as_nanos() as f64 / n as f64;
    println!("   bare loop            {:>7.2} ns/op", per(bare));
    println!("   + clock pair         {:>7.2} ns/op", per(timed));
    println!("   + histogram.record   {:>7.2} ns/op", per(hist));
    println!("   + slowlog check      {:>7.2} ns/op", per(full));
    println!(
        "   tax of the full surface: {:.2} ns/op ({:.0}%)",
        per(full) - per(bare),
        (per(full) - per(bare)) / per(bare) * 100.0
    );
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_coordinated_omission();
    stub_lane("lane 2", lane2_histogram_priced);
    stub_lane("lane 3", lane3_observability_tax);
}
