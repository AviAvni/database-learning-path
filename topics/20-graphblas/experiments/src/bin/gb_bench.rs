//! gb_bench — SpMV bandwidth, SpGEMM hash-vs-SPA, BFS three ways
//! with per-level traces, hypersparse payoff.
//!
//! cargo run --release --bin gb_bench

use graphblas_experiments::{
    bfs::{self, LevelTrace},
    csr::{path, rmat, uniform, Csr},
    hyper::HyperCsr,
    spgemm, spmv,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    println!("=== gb_bench ===\n");
    spmv_sweep();
    spgemm_bench();
    bfs_bench();
    hyper_bench();
}

fn spmv_sweep() {
    println!("-- SpMV (PLUS,TIMES), RMAT edge_factor 8, 5 reps best --");
    println!("{:>6} {:>10} {:>10} {:>10} {:>8}", "scale", "n", "nnz", "µs", "GB/s");
    for scale in [14u32, 16, 18, 20] {
        let a = rmat(scale, 8, 42);
        let x: Vec<f64> = (0..a.n).map(|i| (i % 7) as f64).collect();
        let us = best(5, || {
            std::hint::black_box(spmv::spmv(&a, &x));
        });
        let gbs = spmv::spmv_bytes(&a) as f64 / us / 1e3;
        println!("{:>6} {:>10} {:>10} {:>10.0} {:>8.2}", scale, a.n, a.nnz(), us, gbs);
    }
    println!();
}

fn spgemm_bench() {
    println!("-- SpGEMM C=A*A, RMAT edge_factor 8 --");
    println!(
        "{:>6} {:>10} {:>12} {:>12} {:>12} {:>12}",
        "scale", "nnz(A)", "flops", "nnz(C)", "hash ms", "SPA ms"
    );
    for scale in [10u32, 12, 14] {
        let a = rmat(scale, 8, 43);
        let flops = spgemm::flopcount(&a, &a);
        let t0 = Instant::now();
        let c = spgemm::spgemm_hash(&a, &a);
        let hash_ms = t0.elapsed().as_secs_f64() * 1e3;
        let spa_ms = catch_unwind(AssertUnwindSafe(|| {
            let t0 = Instant::now();
            let c2 = spgemm::spgemm_spa(&a, &a);
            assert_eq!(c2.nnz(), c.nnz());
            t0.elapsed().as_secs_f64() * 1e3
        }));
        let spa = spa_ms.map(|m| format!("{m:.1}")).unwrap_or("STUB".into());
        println!(
            "{:>6} {:>10} {:>12} {:>12} {:>12.1} {:>12}",
            scale,
            a.nnz(),
            flops,
            c.nnz(),
            hash_ms,
            spa
        );
    }
    println!();
}

fn bfs_bench() {
    println!("-- BFS from vertex 0 --");
    let graphs: Vec<(&str, Csr)> = vec![
        ("rmat18", rmat(18, 8, 44)),
        ("uniform 256K×2M", uniform(1 << 18, 1 << 21, 45)),
        ("path 100K", path(100_000)),
    ];
    for (name, g) in &graphs {
        let at = g.transpose();
        let us_scalar = best(3, || {
            std::hint::black_box(bfs::bfs_scalar(g, 0));
        });
        print!("{name}: scalar {us_scalar:.0} µs");
        for (lane, res) in [
            ("push", catch_unwind(AssertUnwindSafe(|| timed(|| bfs::bfs_push(g, 0))))),
            ("pull", catch_unwind(AssertUnwindSafe(|| timed(|| bfs::bfs_pull(&at, 0))))),
            (
                "diropt",
                catch_unwind(AssertUnwindSafe(|| {
                    timed(|| bfs::bfs_diropt(g, &at, 0, 8.0, 8.0, 512.0))
                })),
            ),
        ] {
            match res {
                Ok((us, (_, trace))) => {
                    let checked: usize = trace.iter().map(|t| t.edges_checked).sum();
                    print!(" | {lane} {us:.0} µs ({checked} checks)");
                    if lane == "diropt" {
                        println!();
                        print_trace(&trace);
                    }
                }
                Err(_) => print!(" | {lane} STUB"),
            }
        }
        println!();
    }
    println!();
}

fn print_trace(trace: &[LevelTrace]) {
    for t in trace {
        println!(
            "    level {:>3}: frontier {:>8} {} edges_checked {:>10}",
            t.level,
            t.frontier,
            if t.used_pull { "PULL" } else { "push" },
            t.edges_checked
        );
    }
}

fn hyper_bench() {
    println!("-- hypersparse payoff: 10M-node id space, 100K edges --");
    let a = uniform(10_000_000, 100_000, 46);
    let h = HyperCsr::from_csr(&a);
    println!(
        "index bytes: CSR {:.1} MB vs hyper {:.3} MB ({}x)",
        a.index_bytes() as f64 / 1e6,
        h.index_bytes() as f64 / 1e6,
        a.index_bytes() / h.index_bytes().max(1)
    );
    let us_csr = best(3, || {
        let mut s = 0usize;
        for i in 0..a.n {
            s += a.row(i).0.len();
        }
        std::hint::black_box(s);
    });
    let us_hyp = best(3, || {
        let mut s = 0usize;
        for (_, cols, _) in h.iter_rows() {
            s += cols.len();
        }
        std::hint::black_box(s);
    });
    println!("full row sweep: CSR {us_csr:.0} µs vs hyper iter {us_hyp:.0} µs");
}

fn timed<T>(f: impl FnOnce() -> T) -> (f64, T) {
    let t0 = Instant::now();
    let r = f();
    (t0.elapsed().as_secs_f64() * 1e6, r)
}

fn best(reps: usize, mut f: impl FnMut()) -> f64 {
    let mut b = f64::INFINITY;
    for _ in 0..reps {
        let t0 = Instant::now();
        f();
        b = b.min(t0.elapsed().as_secs_f64() * 1e6);
    }
    b
}
