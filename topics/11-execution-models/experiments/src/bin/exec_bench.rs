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

fn main() {
    println!("generating {} M rows...", ROWS / 1_000_000);
    let table = Table::generate(ROWS, 42);

    for threshold in [50, 5, 95] {
        println!("\nSELECT k, SUM(v) WHERE f < {threshold} GROUP BY k  (selectivity ~{threshold}%)");
        bench("volcano", &table, threshold, volcano::run);
        bench("vectorized", &table, threshold, vectorized::run);
        bench("kernel", &table, threshold, kernels::run);
    }

    println!("\nnotes:");
    println!("- record all three at selectivity 50 in notes.md, plus the ratios");
    println!("- rerun vectorized with BATCH_SIZE 64 / 1024 / 65536 for the X100 U-curve");
    println!("- flamegraph the volcano run: where does the time actually go?");
}
