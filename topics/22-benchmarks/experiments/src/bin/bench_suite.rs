//! bench_suite — TPC-H choke points (Q1/Q6) + YCSB A-F with the
//! uniform-vs-zipfian skew experiment.
//!
//! cargo run --release --bin bench_suite

use bench_experiments::{
    lineitem::gen_lineitem,
    tpch,
    ycsb::{run_workload, Store, WORKLOADS},
    zipf::{Scrambled, Uniform, Zipfian},
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    println!("=== bench_suite ===\n");
    tpch_section();
    ycsb_section();
}

fn tpch_section() {
    println!("-- TPC-H choke points, dbgen-lite --");
    println!(
        "{:>6} {:>10} {:>12} {:>12} {:>12} {:>14}",
        "SF", "rows", "Q1 ms", "Q1 flat ms", "Q6 ms", "Q6 brless ms"
    );
    for sf in [0.05, 0.25] {
        let t = gen_lineitem(sf, 42);
        let q1 = time_ms(|| {
            std::hint::black_box(tpch::q1_oracle(&t));
        });
        let q6 = time_ms(|| {
            std::hint::black_box(tpch::q6_oracle(&t));
        });
        let q1f = lane(|| {
            time_ms(|| {
                std::hint::black_box(tpch::q1_flat(&t));
            })
        });
        let q6b = lane(|| {
            time_ms(|| {
                std::hint::black_box(tpch::q6_branchless(&t));
            })
        });
        println!(
            "{:>6} {:>10} {:>12.1} {:>12} {:>12.1} {:>14}",
            sf,
            t.len(),
            q1,
            q1f,
            q6,
            q6b
        );
        let bytes = t.len() * (8 * 4 + 2 + 4); // cols Q1 touches
        println!(
            "        Q1 effective {:.1} GB/s | Q6 scans {:.1} GB/s (oracle lanes)",
            bytes as f64 / q1 / 1e6,
            (t.len() * (8 * 3 + 4)) as f64 / q6 / 1e6
        );
    }
    println!();
}

fn ycsb_section() {
    println!("-- YCSB A-F, 1M preloaded keys, 500K ops, single thread --");
    println!(
        "{:>18} {:>10} {:>8} {:>8} {:>8} {:>9}  dist",
        "workload", "Mops/s", "p50 ns", "p99 ns", "p999 ns", "elapsed"
    );
    for mix in &WORKLOADS {
        let mut s = Store::preload(1_000_000);
        let mut kg = Uniform::new(7);
        let mut r = run_workload(&mut s, mix, &mut kg, 500_000, 11);
        let (p50, _, p99, p999) = r.hist.report();
        println!(
            "{:>18} {:>10.2} {:>8} {:>8} {:>8} {:>8.2}s  uniform",
            mix.name,
            r.mops(),
            p50,
            p99,
            p999,
            r.elapsed_s
        );
        let z = catch_unwind(AssertUnwindSafe(|| {
            let mut s = Store::preload(1_000_000);
            let mut kg = Scrambled { inner: Zipfian::new(1_000_000, 0.99, 7), items: 1_000_000 };
            let mut r = run_workload(&mut s, mix, &mut kg, 500_000, 11);
            let (p50, _, p99, p999) = r.hist.report();
            (r.mops(), p50, p99, p999, r.elapsed_s)
        }));
        match z {
            Ok((mops, p50, p99, p999, el)) => println!(
                "{:>18} {:>10.2} {:>8} {:>8} {:>8} {:>8.2}s  zipf .99",
                "", mops, p50, p99, p999, el
            ),
            Err(_) => println!("{:>18} {:>10}", "", "zipf STUB"),
        }
    }
}

fn time_ms(f: impl FnOnce()) -> f64 {
    let t0 = Instant::now();
    f();
    t0.elapsed().as_secs_f64() * 1e3
}

fn lane(f: impl FnOnce() -> f64 + std::panic::UnwindSafe) -> String {
    match catch_unwind(f) {
        Ok(ms) => format!("{ms:.1}"),
        Err(_) => "STUB".into(),
    }
}
