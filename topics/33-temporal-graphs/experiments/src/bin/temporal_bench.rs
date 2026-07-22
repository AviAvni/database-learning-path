//! temporal_bench — three lanes.
//!
//! Lane 1 (PROVIDED, runs today): the lie of the static condensation —
//! how many "reachable" answers a timestamp-blind BFS gets wrong, vs
//! the temporal ground truth. This number is why the topic exists.
//!
//! Lanes 2-3 need your implementations (todo!-panic until then):
//!   2. one-pass earliest-arrival vs the fixpoint oracle: the speedup
//!      a single time-ordered scan buys.
//!   3. AT TIME reads on the anchor+delta store: latency + replay length
//!      vs checkpoint spacing — AeonG's storage/latency dial, measured.

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;
use temporal_experiments::events::{
    earliest_arrival_oracle, gen_contacts, gen_events, percentile, static_reachable, INF,
};
use temporal_experiments::snapshot::AnchorDeltaStore;
use temporal_experiments::temporal_reach::earliest_arrival;

fn main() {
    lane1_false_positives();
    stub_lane("lane 2: one-pass earliest-arrival vs fixpoint oracle", lane2_one_pass);
    stub_lane("lane 3: AT TIME latency vs checkpoint spacing", lane3_at_time);
}

fn stub_lane(name: &str, f: fn()) {
    println!("\n=== {name} ===");
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s in src/ to unlock this lane]");
    }
}

/// For each density, sample sources; count pairs the static BFS calls
/// reachable that no time-respecting path actually serves. Density is
/// the knob: sparse streams leave few detours, so ordering bites hardest.
fn lane1_false_positives() {
    println!("=== lane 1: static condensation vs temporal reachability ===");
    println!(
        "{:>10} {:>10} {:>14} {:>14} {:>16}",
        "nodes", "contacts", "static-reach", "temporal-reach", "false positives"
    );
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    const N: u32 = 2_000;
    for m in [4_000usize, 8_000, 16_000, 64_000] {
        let cs = gen_contacts(&mut rng, N, m, 10_000);
        let (mut stat, mut temp) = (0usize, 0usize);
        for src in (0..N).step_by(100) {
            let s = static_reachable(&cs, N, src);
            let a = earliest_arrival_oracle(&cs, N, src, 0);
            stat += s.iter().filter(|&&r| r).count() - 1;
            temp += a.iter().filter(|&&t| t != INF).count() - 1;
        }
        println!(
            "{:>10} {:>10} {:>14} {:>14} {:>15.1}%",
            N,
            cs.len(),
            stat,
            temp,
            100.0 * (stat - temp) as f64 / stat.max(1) as f64
        );
    }
    println!("(every false positive is a path that exists in space but not in time)");
}

/// Same answers, two costs: the oracle loops over all contacts to
/// fixpoint; the one-pass scans once because the stream is time-sorted.
fn lane2_one_pass() {
    let mut rng = ChaCha8Rng::seed_from_u64(43);
    println!("{:>10} {:>12} {:>12} {:>9}", "contacts", "oracle", "one-pass", "speedup");
    for m in [50_000usize, 200_000, 800_000] {
        let cs = gen_contacts(&mut rng, 10_000, m, 100_000);
        let t = Instant::now();
        let a1 = earliest_arrival_oracle(&cs, 10_000, 0, 0);
        let oracle = t.elapsed();
        let t = Instant::now();
        let a2 = earliest_arrival(&cs, 10_000, 0, 0);
        let one_pass = t.elapsed();
        assert_eq!(a1, a2);
        println!(
            "{:>10} {:>12?} {:>12?} {:>8.1}x",
            cs.len(),
            oracle,
            one_pass,
            oracle.as_secs_f64() / one_pass.as_secs_f64()
        );
    }
}

/// AT TIME reads against anchor+delta stores at different spacings, plus
/// the no-anchor baseline (full replay). Storage = anchors held; read
/// cost = deltas replayed. The dial, priced.
fn lane3_at_time() {
    let mut rng = ChaCha8Rng::seed_from_u64(44);
    let events = gen_events(&mut rng, 1_000, 200_000, 1_000_000);
    println!(
        "{:>14} {:>9} {:>12} {:>12} {:>14}",
        "anchor every", "anchors", "p50 read", "p99 read", "avg replay len"
    );
    for every in [1_000usize, 10_000, 100_000, usize::MAX] {
        let mut store = AnchorDeltaStore::new(1_000, every.min(events.len() + 1));
        for &e in &events {
            store.append(e);
        }
        let probes: Vec<u64> = (0..200).map(|i| i * 5_000).collect();
        let mut lat = Vec::new();
        let mut replayed = 0usize;
        for &t in &probes {
            let start = Instant::now();
            std::hint::black_box(store.at_time(t));
            lat.push(start.elapsed().as_nanos() as u64);
            replayed += store.replay_len(t);
        }
        let label = if every == usize::MAX { "none".into() } else { format!("{every}") };
        println!(
            "{:>14} {:>9} {:>10}us {:>10}us {:>14}",
            label,
            store.anchor_count(),
            percentile(&mut lat.clone(), 50.0) / 1_000,
            percentile(&mut lat, 99.0) / 1_000,
            replayed / probes.len()
        );
    }
    println!("(AeonG's claim, on your laptop: anchors trade storage for bounded replay)");
}
