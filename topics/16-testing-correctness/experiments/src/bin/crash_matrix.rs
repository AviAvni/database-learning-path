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

fn main() {
    const SEEDS: u64 = 5_000;
    const LEN: usize = 40;
    println!("{SEEDS} seeds x {LEN} ops (50% put / 20% del / 20% commit / 10% crash)\n");
    println!("{:<20} {:>10} {:>12} {:>14}", "bug", "caught", "rate", "first seed");
    for bug in [Bug::None, Bug::LostDelete, Bug::NoSyncOnCommit, Bug::TornWriteAccepted, Bug::StaleRead] {
        let start = std::time::Instant::now();
        let mut caught = 0u64;
        let mut first: Option<u64> = None;
        for s in 0..SEEDS {
            if diverges(s, LEN, bug) {
                caught += 1;
                first.get_or_insert(s);
            }
        }
        println!(
            "{:<20} {:>10} {:>11.1}% {:>14} ({:.2}s)",
            format!("{bug:?}"),
            caught,
            100.0 * caught as f64 / SEEDS as f64,
            first.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
            start.elapsed().as_secs_f64(),
        );
    }
    println!("\nBug::None must be 0.0% — anything else is a harness bug (false positive).");
}
