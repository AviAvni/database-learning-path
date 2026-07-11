//! Space amplification: fill each engine with N random-key records, sync,
//! report logical bytes vs bytes on disk. Run: `cargo run --release [N]`

use engine_shootout::{value_for, Engine, FjallEngine, RedbEngine, VALUE_SIZE};
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
use std::path::Path;

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.metadata().unwrap();
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

fn measure(mut engine: Box<dyn Engine>, dir: &Path, items: &[(u64, Vec<u8>)]) {
    let logical = items.len() as u64 * (8 + VALUE_SIZE) as u64;
    for chunk in items.chunks(1000) {
        engine.put_batch(chunk);
    }
    engine.sync();
    let physical = dir_size(dir);
    println!(
        "{:8} logical {:>12} B  on-disk {:>12} B  space-amp {:.2}x",
        engine.name(),
        logical,
        physical,
        physical as f64 / logical as f64
    );
}

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);

    let mut rng = StdRng::seed_from_u64(42);
    let mut keys: Vec<u64> = (0..n).collect();
    keys.shuffle(&mut rng);
    let items: Vec<(u64, Vec<u8>)> = keys.iter().map(|&k| (k, value_for(k))).collect();

    let fjall_dir = tempfile::tempdir().unwrap();
    measure(
        Box::new(FjallEngine::open(fjall_dir.path())),
        fjall_dir.path(),
        &items,
    );

    let redb_dir = tempfile::tempdir().unwrap();
    measure(
        Box::new(RedbEngine::open(redb_dir.path())),
        redb_dir.path(),
        &items,
    );
}
