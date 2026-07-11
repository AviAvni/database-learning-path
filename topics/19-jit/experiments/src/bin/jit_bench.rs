//! Three-way bench: AST interpreter vs vectorized vs cranelift JIT.
//! PLAN §19: find the (expr depth × row count) crossover where each
//! wins — INCLUDING compile time.
//!
//! cargo run --release --bin jit_bench

use jit_experiments::{expr::gen_expr, gen_cols, interp, jit, to_rows, vectorized};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

const N_COLS: usize = 4;

fn main() {
    println!("=== jit_bench: interpreter vs vectorized vs JIT ===\n");

    for depth in [2usize, 4, 6, 8, 10] {
        let e = gen_expr(depth, N_COLS, depth as u64);
        let nodes = e.node_count();
        println!("-- depth {depth} ({nodes} nodes) --");
        println!(
            "{:>10} | {:>12} {:>12} | {:>12} {:>12} | {:>10}",
            "rows", "interp M/s", "vector M/s", "jit M/s", "compile µs", "winner e2e"
        );

        for log_rows in [10usize, 14, 18, 21] {
            let n_rows = 1usize << log_rows;
            let cols = gen_cols(N_COLS, n_rows, 99);
            let rows = to_rows(&cols);

            // interpreter (per-row), 3 reps
            let (interp_us, sink_i) = time_best(3, || {
                let mut acc = 0.0f64;
                for row in &rows {
                    acc += interp::eval(&e, row);
                }
                acc
            });

            // vectorized (column-at-a-time), 3 reps
            let (vec_us, sink_v) = time_best(3, || {
                eval_sum(&vectorized::eval_batch(&e, &cols))
            });
            assert!((sink_i - sink_v).abs() < 1e-6 * sink_i.abs().max(1.0));

            // JIT: compile once (timed), then run
            let jit_res = catch_unwind(AssertUnwindSafe(|| {
                let t0 = Instant::now();
                let compiled = jit::compile(&e);
                let compile_us = t0.elapsed().as_secs_f64() * 1e6;
                let (run_us, sink_j) = time_best(3, || {
                    let mut acc = 0.0f64;
                    for row in &rows {
                        acc += compiled.eval(row);
                    }
                    acc
                });
                assert!((sink_i - sink_j).abs() < 1e-6 * sink_i.abs().max(1.0));
                (compile_us, run_us)
            }));

            let mrows = |us: f64| n_rows as f64 / us;
            match jit_res {
                Ok((compile_us, jit_us)) => {
                    // end-to-end winner includes compile time in the JIT lane
                    let lanes = [
                        ("interp", interp_us),
                        ("vector", vec_us),
                        ("jit", jit_us + compile_us),
                    ];
                    let winner = lanes
                        .iter()
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .unwrap()
                        .0;
                    println!(
                        "{:>10} | {:>12.2} {:>12.2} | {:>12.2} {:>12.1} | {:>10}",
                        n_rows,
                        mrows(interp_us),
                        mrows(vec_us),
                        mrows(jit_us),
                        compile_us,
                        winner
                    );
                }
                Err(_) => {
                    println!(
                        "{:>10} | {:>12.2} {:>12.2} | {:>12} {:>12} | {:>10}",
                        n_rows,
                        mrows(interp_us),
                        mrows(vec_us),
                        "STUB",
                        "-",
                        "-"
                    );
                }
            }
        }
        println!();
    }

    println!("crossover math: break-even rows = compile_µs / (µs/row_interp - µs/row_jit)");
    println!("fill notes.md prediction table BEFORE implementing jit.rs");
}

fn eval_sum(v: &[f64]) -> f64 {
    v.iter().sum()
}

/// best-of-n wall time in µs, plus the last result as an anti-DCE sink
fn time_best(reps: usize, mut f: impl FnMut() -> f64) -> (f64, f64) {
    let mut best = f64::INFINITY;
    let mut sink = 0.0;
    for _ in 0..reps {
        let t0 = Instant::now();
        sink = std::hint::black_box(f());
        best = best.min(t0.elapsed().as_secs_f64() * 1e6);
    }
    (best, sink)
}
