//! Provided: is the Volcano lane's 50%-selectivity peak a branch misprediction?
//!
//!   cargo run --release --bin sorted_filter
//!
//! Runs the same Volcano pipeline at 5 / 50 / 95% selectivity twice: once on
//! the generated (uniform random) `f` column, once after sorting `f` in place.
//! Sorting leaves the work per row identical but makes the filter's branch
//! predictable (a long run of passes, then a long run of rejections). The query
//! answer is different on the sorted table (rows no longer pair up), so this
//! measures time only; correctness of `volcano::run` is covered by its tests.

use std::time::Instant;

use exec_experiments::data::Table;
use exec_experiments::volcano;

const ROWS: usize = 50_000_000;
const REPS: usize = 3;

fn main() {
    println!("generating {} M rows...", ROWS / 1_000_000);
    let mut table = Table::generate(ROWS, 42);
    for (label, sorted) in [("random f", false), ("sorted f", true)] {
        if sorted {
            table.f.sort_unstable();
        }
        for threshold in [5u32, 50, 95] {
            let mut best = f64::MAX;
            for _ in 0..REPS {
                let start = Instant::now();
                let sums = volcano::run(&table, threshold);
                let secs = start.elapsed().as_secs_f64();
                std::hint::black_box(sums);
                best = best.min(secs);
            }
            println!(
                "  {label:<9} sel {threshold:>2}%  {best:>7.3} s  {:>7.1} M rows/s  {:>5.2} ns/row",
                ROWS as f64 / best / 1e6,
                best * 1e9 / ROWS as f64
            );
        }
    }
}
