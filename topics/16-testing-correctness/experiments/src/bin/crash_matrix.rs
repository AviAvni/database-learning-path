//! Provided (runs without stubs): exhaustive seeded crash testing of
//! the KV store — topic 5's crash matrix, simulated. For each bug
//! variant, run many seeded workloads and report what fraction of
//! seeds expose a post-recovery divergence. This inline harness is
//! the "answer key" your dst.rs should roughly match (rates depend
//! on workload weights).

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use testing_experiments::kv::{Bug, KvStore};
use testing_experiments::{Model, Op};

fn gen_ops(seed: u64, len: usize) -> Vec<Op> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut ops: Vec<Op> = (0..len)
        .map(|_| match rng.gen_range(0..10) {
            0..=4 => Op::Put(rng.gen_range(0..16), rng.gen()),
            5..=6 => Op::Delete(rng.gen_range(0..16)),
            7..=8 => Op::Commit,
            _ => Op::Crash,
        })
        .collect();
    ops.push(Op::Commit);
    ops.push(Op::Crash);
    ops
}

fn diverges(seed: u64, len: usize, bug: Bug) -> bool {
    let ops = gen_ops(seed, len);
    let mut kv = KvStore::new(seed, bug);
    let mut model = Model::default();
    for op in &ops {
        kv.apply(op);
        model.apply(op);
        if matches!(op, Op::Crash) && kv.state() != model.expected() {
            return true;
        }
    }
    false
}

/// Seeds needed so that P(at least one seed diverges) >= `conf`, given a
/// per-seed detection rate `p` and independent seeds: ceil(ln(1-conf)/ln(1-p)).
fn seeds_needed(p: f64, conf: f64) -> Option<u64> {
    if p <= 0.0 {
        return None;
    }
    if p >= 1.0 {
        return Some(1);
    }
    Some(((1.0 - conf).ln() / (1.0 - p).ln()).ceil() as u64)
}

fn main() {
    const SEEDS: u64 = 5_000;
    const LEN: usize = 40;
    const BUGS: [Bug; 5] = [Bug::None, Bug::LostDelete, Bug::NoSyncOnCommit, Bug::TornWriteAccepted, Bug::StaleRead];
    println!("{SEEDS} seeds x {LEN} ops (50% put / 20% del / 20% commit / 10% crash)\n");
    println!("{:<20} {:>10} {:>12} {:>14}", "bug", "caught", "rate", "first seed");
    let mut hits: Vec<Vec<bool>> = Vec::new();
    for bug in BUGS {
        let start = std::time::Instant::now();
        let h: Vec<bool> = (0..SEEDS).map(|s| diverges(s, LEN, bug)).collect();
        let caught = h.iter().filter(|&&x| x).count() as u64;
        let first = h.iter().position(|&x| x);
        println!(
            "{:<20} {:>10} {:>11.1}% {:>14} ({:.2}s)",
            format!("{bug:?}"),
            caught,
            100.0 * caught as f64 / SEEDS as f64,
            first.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
            start.elapsed().as_secs_f64(),
        );
        hits.push(h);
    }
    println!("\nBug::None must be 0.0% — anything else is a harness bug (false positive).");

    // Decision-useful form: how many seeds must a CI run budget? With
    // independent seeds, P(miss) = (1-p)^N, so N = ceil(ln(1-conf)/ln(1-p)).
    // The last column checks the formula: carve the 5000 seeds into disjoint
    // blocks of N and count the blocks containing at least one divergence.
    println!("\nseeds needed for a detection probability (p = per-seed rate above)");
    println!("{:<20} {:>8} {:>10} {:>10} {:>22}", "bug", "p", "N @95%", "N @99.9%", "blocks hit (N@95%)");
    for (bug, h) in BUGS.iter().zip(&hits).skip(1) {
        let p = h.iter().filter(|&&x| x).count() as f64 / SEEDS as f64;
        let n95 = seeds_needed(p, 0.95).unwrap();
        let n999 = seeds_needed(p, 0.999).unwrap();
        let blocks: Vec<&[bool]> = h.chunks_exact(n95 as usize).collect();
        let hit = blocks.iter().filter(|b| b.iter().any(|&x| x)).count();
        println!(
            "{:<20} {:>8.4} {:>10} {:>10} {:>14}/{:<7}",
            format!("{bug:?}"), p, n95, n999, hit, blocks.len()
        );
    }

    // Same question along the other axis: per-seed rate as the workload
    // grows. Longer workloads buy detection per seed, at linear cost per seed.
    println!("\nper-seed detection rate vs workload length (first 2000 seeds)");
    print!("{:<20}", "bug");
    const LENS: [usize; 5] = [5, 10, 20, 40, 80];
    for l in LENS {
        print!(" {:>7}", format!("{l} ops"));
    }
    println!();
    for bug in BUGS.iter().skip(1) {
        print!("{:<20}", format!("{bug:?}"));
        for l in LENS {
            let c = (0..2000u64).filter(|&s| diverges(s, l, *bug)).count();
            print!(" {:>6.1}%", 100.0 * c as f64 / 2000.0);
        }
        println!();
    }
}
