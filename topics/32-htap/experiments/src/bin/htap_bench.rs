//! htap_bench — three lanes.
//!
//! Lane 1 (PROVIDED, runs today): the interference measurement — the
//! reason HTAP architectures exist. OLTP writes alone vs OLTP writes
//! while an analytical scanner hammers the same store. Watch write p99.
//!
//! Lanes 2-3 need your implementations (todo!-panic until then):
//!   2. the split — columnar replica fed by the changelog: scan speedup
//!      vs the row store + freshness lag vs apply-batch size.
//!   3. learner reads — the price of consistency-on-a-replica: wait
//!      distribution vs apply interval.

use htap_experiments::learner::read_wait;
use htap_experiments::replica::ColumnarReplica;
use htap_experiments::row::{percentile, skewed_key, RowStore};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const KEYS: u64 = 1_000_000;
const LANE1_WINDOW: Duration = Duration::from_secs(2);

fn main() {
    lane1_interference();
    stub_lane("lane 2: columnar replica — scan speedup + freshness", lane2_replica);
    stub_lane("lane 3: learner reads — the price of freshness", lane3_learner);
}

fn stub_lane(name: &str, f: fn()) {
    println!("\n=== {name} ===");
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s in src/ to unlock this lane]");
    }
}

/// One store, one coarse lock (worst case on purpose — the point is the
/// SHAPE of the interference, mitigations are the topic). Fixed 2s window
/// per mode: writer records per-write latency, scanner free-runs full
/// scans. The writes-completed collapse IS the interference.
fn lane1_interference() {
    println!("=== lane 1: OLTP/OLAP interference on one copy ===");
    let run = |with_scans: bool| -> (Vec<u64>, usize) {
        let store = Mutex::new(HashMap::<u64, (i64, i64)>::new());
        {
            let mut s = store.lock().unwrap();
            let mut rng = ChaCha8Rng::seed_from_u64(1);
            for k in 0..KEYS {
                s.insert(k, (rng.gen_range(-100..100), rng.gen_range(0..1000)));
            }
        }
        let done = AtomicBool::new(false);
        let mut lat = Vec::new();
        let mut scans = 0usize;
        std::thread::scope(|sc| {
            let scanner = sc.spawn(|| {
                let mut n = 0;
                let mut sink = 0i64;
                while !done.load(Ordering::Relaxed) {
                    if with_scans {
                        let s = store.lock().unwrap();
                        sink += s
                            .values()
                            .filter(|(_, b)| (100..=200).contains(b))
                            .map(|(a, _)| a)
                            .sum::<i64>();
                        n += 1;
                    } else {
                        std::thread::yield_now();
                    }
                }
                std::hint::black_box(sink);
                n
            });
            let mut rng = ChaCha8Rng::seed_from_u64(2);
            let deadline = Instant::now() + LANE1_WINDOW;
            while Instant::now() < deadline {
                let k = skewed_key(&mut rng, KEYS);
                let (a, b) = (rng.gen_range(-100..100), rng.gen_range(0..1000));
                let t = Instant::now();
                store.lock().unwrap().insert(k, (a, b));
                lat.push(t.elapsed().as_nanos() as u64);
            }
            done.store(true, Ordering::Relaxed);
            scans = scanner.join().unwrap();
        });
        (lat, scans)
    };

    println!(
        "{:>22} {:>10} {:>10} {:>12} {:>10} {:>7}",
        "mode", "p50 ns", "p99 ns", "p99.9 ns", "writes/2s", "scans"
    );
    for (name, with_scans) in [("writes alone", false), ("writes + full scans", true)] {
        let (mut lat, scans) = run(with_scans);
        let n = lat.len();
        println!(
            "{:>22} {:>10} {:>10} {:>12} {:>10} {:>7}",
            name,
            percentile(&mut lat, 50.0),
            percentile(&mut lat, 99.0),
            percentile(&mut lat, 99.9),
            n,
            scans
        );
    }
    println!("(coarse lock on purpose: every full scan is a write outage — the HTAP problem)");
}

/// Feed the changelog to the columnar replica; compare analytical scan
/// cost row-store vs replica (delta-heavy vs merged), and freshness lag
/// vs apply-batch size.
fn lane2_replica() {
    let mut rng = ChaCha8Rng::seed_from_u64(3);
    let mut primary = RowStore::new();
    for _ in 0..1_000_000 {
        let k = skewed_key(&mut rng, KEYS);
        primary.write(k, rng.gen_range(-100..100), rng.gen_range(0..1000));
    }

    let t = Instant::now();
    let row_sum = primary.scan_sum_a(100, 200);
    let row_scan = t.elapsed();

    let mut replica = ColumnarReplica::new();
    replica.apply(&primary.log);
    let t = Instant::now();
    let delta_sum = replica.scan_sum_a(100, 200);
    let delta_scan = t.elapsed();

    let t = Instant::now();
    replica.merge_delta();
    let merge_cost = t.elapsed();
    let t = Instant::now();
    let merged_sum = replica.scan_sum_a(100, 200);
    let merged_scan = t.elapsed();

    assert_eq!(row_sum, delta_sum);
    assert_eq!(row_sum, merged_sum);
    println!("scan sum(a) where b in [100,200] over 1M writes / {KEYS} keys:");
    println!("  row store (oracle): {row_scan:?}");
    println!("  replica, all-delta: {delta_scan:?}");
    println!("  replica, merged:    {merged_scan:?}  (merge cost {merge_cost:?})");

    println!("freshness lag vs apply-batch size (max lsn gap while streaming):");
    for batch in [1_000usize, 10_000, 100_000] {
        let mut r = ColumnarReplica::new();
        let mut max_gap = 0;
        for chunk in primary.log.chunks(batch) {
            // gap right before this batch applies = everything unapplied
            max_gap = max_gap.max(chunk.last().unwrap().lsn - r.applied_lsn);
            r.apply(chunk);
        }
        println!("  batch {batch:>7}: max gap {max_gap:>7} lsns");
    }
}

/// Learner-read wait distribution: replica applies every T ticks; reads
/// arrive at random times demanding the then-current primary lsn
/// (read-your-writes freshness). One lsn is written per tick.
fn lane3_learner() {
    let mut rng = ChaCha8Rng::seed_from_u64(4);
    const HORIZON: u64 = 100_000;
    for interval in [1u64, 10, 100] {
        let schedule: Vec<(u64, u64)> = (1..=HORIZON / interval).map(|i| (i * interval, i * interval)).collect();
        let mut waits: Vec<u64> = (0..50_000)
            .map(|_| {
                let now = rng.gen_range(0..HORIZON - interval);
                read_wait(&schedule, now, now) // demand lsn == now: freshest possible
                    .expect("within horizon")
            })
            .collect();
        println!(
            "apply every {interval:>3} ticks: wait p50 {:>4} p99 {:>4} max {:>4}",
            percentile(&mut waits, 50.0),
            percentile(&mut waits, 99.0),
            *waits.iter().max().unwrap()
        );
    }
    println!("(TiFlash's doLearnerRead wait, as a distribution — batching buys throughput with exactly this coin)");
}
