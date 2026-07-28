use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use distributed_query_experiments::exchange::{merge_sorted, Exchange, Partitioning};
use distributed_query_experiments::fanout::{
    p_any_slow, percentile, scatter_gather, scatter_gather_frac, seeded_rng,
};
use distributed_query_experiments::hedge::run_trials;

/// Lane 1 (PROVIDED): the fan-out tail — Dean & Barroso's arithmetic,
/// then the simulated version of their Table 1.
fn lane1_fanout() {
    println!("== lane 1: the fan-out tail — P(any leaf slow) and Table 1's shape ==");
    println!("   P(at least one slow) = 1 - (1-p)^n");
    println!("   n        p=1/100   p=1/1000   p=1/10000");
    for n in [1u32, 100, 500, 1000, 2000] {
        println!(
            "   {n:>4}      {:>5.1}%     {:>5.1}%      {:>5.1}%",
            p_any_slow(0.01, n) * 100.0,
            p_any_slow(0.001, n) * 100.0,
            p_any_slow(0.0001, n) * 100.0
        );
    }
    println!();

    println!("   simulated 100-leaf scatter-gather, 1-in-100 slow leaves, 20k queries");
    println!("   (the paper's Table 1: waiting for all vs 95% of leaves)");
    let mut rng = seeded_rng(42);
    let trials = 20_000;
    let mut one: Vec<f64> = (0..trials).map(|_| scatter_gather(&mut rng, 1, 0.01)).collect();
    let mut p95: Vec<f64> = (0..trials)
        .map(|_| scatter_gather_frac(&mut rng, 100, 0.01, 0.95))
        .collect();
    let mut all: Vec<f64> = (0..trials).map(|_| scatter_gather(&mut rng, 100, 0.01)).collect();
    one.sort_by(f64::total_cmp);
    p95.sort_by(f64::total_cmp);
    all.sort_by(f64::total_cmp);
    println!("   wait for            p50        p95        p99");
    for (name, v) in [("one leaf   ", &one), ("95% of 100 ", &p95), ("all 100    ", &all)] {
        println!(
            "   {name}     {:>7.1} ms {:>7.1} ms {:>7.1} ms",
            percentile(v, 0.50),
            percentile(v, 0.95),
            percentile(v, 0.99)
        );
    }
    println!();
}

/// Lane 2 (needs exchange.rs): routing throughput and the merge.
fn lane2_exchange() {
    println!("== lane 2: exchange — routing throughput and the merging exchange ==");
    let rows: Vec<u64> = (0..4_000_000u64).collect();
    for (name, policy) in [
        ("round-robin", Partitioning::RoundRobin),
        ("hash       ", Partitioning::Hash),
    ] {
        let mut ex = Exchange::new(8, policy);
        let t = Instant::now();
        let out = ex.partition(&rows);
        let dt = t.elapsed();
        let sizes: Vec<usize> = out.iter().map(Vec::len).collect();
        let (min, max) = (
            *sizes.iter().min().unwrap() as f64,
            *sizes.iter().max().unwrap() as f64,
        );
        println!(
            "   {name} k=8: {:>6.1} M rows/s, balance max/min = {:.3}",
            rows.len() as f64 / dt.as_secs_f64() / 1e6,
            max / min
        );
    }

    let runs: Vec<Vec<u64>> = (0..8u64)
        .map(|r| (0..500_000u64).map(|i| i * 8 + r).collect())
        .collect();
    let t = Instant::now();
    let merged = merge_sorted(&runs);
    let dt = t.elapsed();
    println!(
        "   merging exchange: 8 sorted runs of 500k -> {} rows in {:.1} ms ({:.1} M rows/s)",
        merged.len(),
        dt.as_secs_f64() * 1e3,
        merged.len() as f64 / dt.as_secs_f64() / 1e6
    );
    println!();
}

/// Lane 3 (needs hedge.rs): hedged requests — tail vs extra load, by delay.
fn lane3_hedge() {
    println!("== lane 3: hedged requests — p99.9 vs extra load, by hedge delay ==");
    println!("   leaf: 1-10 ms fast, 1000 ms stall with p = 0.005 (paper: 1800 -> 74 ms at +2%)");
    let trials = 200_000usize;
    let mut rng = seeded_rng(7);
    let (unhedged, _) = run_trials(&mut rng, trials, None);
    println!(
        "   no hedge:        p50 {:>6.1} ms   p99 {:>7.1} ms   p99.9 {:>7.1} ms",
        percentile(&unhedged, 0.50),
        percentile(&unhedged, 0.99),
        percentile(&unhedged, 0.999)
    );
    for delay in [0.0f64, 5.0, 10.0, 20.0, 50.0] {
        let (lats, sent) = run_trials(&mut rng, trials, Some(delay));
        println!(
            "   hedge at {delay:>4.0} ms: p50 {:>6.1} ms   p99 {:>7.1} ms   p99.9 {:>7.1} ms   +{:.1}% requests",
            percentile(&lats, 0.50),
            percentile(&lats, 0.99),
            percentile(&lats, 0.999),
            (sent as f64 / trials as f64 - 1.0) * 100.0
        );
    }
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce()) {
    // Silence the default panic hook for the duration of the lane: an
    // unimplemented exercise is an expected state, not a crash to report.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    if r.is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_fanout();
    stub_lane("lane 2", lane2_exchange);
    stub_lane("lane 3", lane3_hedge);
}
