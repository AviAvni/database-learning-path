//! Provided: the three-engine shootout.
//!
//!   cargo run --release --bin exec_bench
//!
//! Panics on the vectorized/kernel stubs until you implement them —
//! volcano runs regardless, so you can record the baseline first.
//! Predict the ratios in notes.md BEFORE implementing.

use std::time::Instant;

use exec_experiments::data::Table;
use exec_experiments::{kernels, oracle, vectorized, volcano};

const ROWS: usize = 50_000_000;
const REPS: usize = 3;

fn bench(name: &str, table: &Table, threshold: u32, f: impl Fn(&Table, u32) -> Vec<i64>) {
    // correctness first, always
    let small = Table::generate(100_000, 1);
    assert_eq!(f(&small, threshold), oracle(&small, threshold), "{name} is WRONG");

    let mut best = f64::MAX;
    for _ in 0..REPS {
        let start = Instant::now();
        let sums = f(table, threshold);
        let secs = start.elapsed().as_secs_f64();
        std::hint::black_box(sums);
        best = best.min(secs);
    }
    let rows_per_s = ROWS as f64 / best;
    println!(
        "  {name:<12} {best:>8.3} s   {:>8.1} M rows/s",
        rows_per_s / 1e6
    );
}

/// Run an exercise lane, reporting unimplemented `todo!()`s as a note
/// instead of a crash, so the volcano baseline always prints.
fn stub_lane(name: &str, f: impl FnOnce()) -> bool {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    if r.is_err() {
        println!("  [stub — implement the todo!()s to unlock {name}]");
    }
    r.is_ok()
}

fn main() {
    println!("generating {} M rows...", ROWS / 1_000_000);
    let table = Table::generate(ROWS, 42);

    let mut vec_ok = true;
    let mut kern_ok = true;
    for threshold in [50, 5, 95] {
        println!("\nSELECT k, SUM(v) WHERE f < {threshold} GROUP BY k  (selectivity ~{threshold}%)");
        // lane 1 (PROVIDED): tuple-at-a-time volcano — the baseline to beat
        bench("volcano", &table, threshold, volcano::run);
        // lanes 2-3 (EXERCISE): batch-at-a-time, then typed kernels
        if vec_ok {
            vec_ok = stub_lane("vectorized (src/vectorized.rs)", || {
                bench("vectorized", &table, threshold, vectorized::run)
            });
        }
        if kern_ok {
            kern_ok = stub_lane("kernels (src/kernels.rs)", || {
                bench("kernel", &table, threshold, kernels::run)
            });
        }
    }

    println!("\nnotes:");
    println!("- record all three at selectivity 50 in notes.md, plus the ratios");
    println!("- rerun vectorized with BATCH_SIZE 64 / 1024 / 65536 for the X100 U-curve");
    println!("- flamegraph the volcano run: where does the time actually go?");
}
