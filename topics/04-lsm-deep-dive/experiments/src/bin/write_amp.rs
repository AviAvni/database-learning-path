//! write_amp — the topic's headline experiment.
//!
//! Load 10M keys with overwrites (3 passes over 3.3M distinct keys), then
//! report the measured RUM position of each compaction strategy. PREDICT
//! FIRST in notes.md: leveled WA ≈ ratio/2 × levels; tiered WA ≈ levels.
//!
//! Run: cargo run --release --bin write_amp

use rand::prelude::*;
use topic04_experiments::{CompactionStrategy, Lsm};

const DISTINCT: u64 = 3_300_000;
const OPS: u64 = 10_000_000;
const VAL: usize = 100;

fn dir_size(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p)
        .unwrap()
        .flatten()
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

fn run(name: &str, strategy: CompactionStrategy) {
    let dir = tempfile::tempdir().unwrap();
    let mut lsm = Lsm::create(dir.path().to_path_buf(), strategy).unwrap();
    let mut rng = StdRng::seed_from_u64(4);
    let val = vec![0xABu8; VAL];

    let t = std::time::Instant::now();
    for _ in 0..OPS {
        let k: u64 = rng.gen_range(0..DISTINCT);
        lsm.put(&k.to_be_bytes(), &val).unwrap();
    }
    let load_s = t.elapsed().as_secs_f64();

    // read amp: 100K point gets, half present / half absent
    let t = std::time::Instant::now();
    for i in 0..100_000u64 {
        let k = if i % 2 == 0 { rng.gen_range(0..DISTINCT) } else { DISTINCT + i };
        lsm.get(&k.to_be_bytes()).unwrap();
    }
    let read_s = t.elapsed().as_secs_f64();

    let live = DISTINCT * (8 + VAL as u64);
    println!("== {name} ==");
    println!("{}", lsm.describe());
    println!(
        "write amp {:.2} | read amp {:.2} segs/get | bloom saved {:.1}% of probes",
        lsm.stats.write_amp(),
        lsm.stats.read_amp(),
        100.0 * lsm.stats.bloom_negative as f64
            / (lsm.stats.bloom_negative + lsm.stats.segments_probed).max(1) as f64
    );
    println!(
        "space amp {:.2} (dir {} MB / live {} MB) | load {:.1}s ({:.0} Kops/s) | reads {:.1}s\n",
        dir_size(dir.path()) as f64 / live as f64,
        dir_size(dir.path()) >> 20,
        live >> 20,
        load_s,
        OPS as f64 / load_s / 1e3,
        read_s
    );
}

fn main() {
    run("leveled (ratio 10)", CompactionStrategy::Leveled { ratio: 10 });
    run("tiered (K=4)", CompactionStrategy::Tiered { k: 4 });
}
