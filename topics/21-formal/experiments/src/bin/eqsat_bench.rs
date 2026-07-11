//! eqsat_bench — hand-ordered rewriter vs equality saturation.
//!
//! cargo run --release --bin eqsat_bench

use formal_experiments::{
    eqsat,
    expr::{cost, div, gen_expr, mul, num, var},
    hand::hand_optimize,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    println!("=== eqsat_bench ===\n");
    trap_case();
    sweep();
}

fn trap_case() {
    println!("-- the ordering trap: (a*2)/2 --");
    let e = div(mul(var("a"), num(2)), num(2));
    let t0 = Instant::now();
    let (h, steps) = hand_optimize(&e);
    let us = t0.elapsed().as_secs_f64() * 1e6;
    println!("hand: cost {} ({} firings, {:.1} µs) — {:?}", cost(&h), steps, us, h);
    match catch_unwind(AssertUnwindSafe(|| {
        let t0 = Instant::now();
        let r = eqsat::egg_optimize(&e);
        (r, t0.elapsed().as_secs_f64() * 1e6)
    })) {
        Ok((r, us)) => println!(
            "egg:  cost {} ({} iters, {} enodes / {} eclasses, {:.1} µs, stop={}) — {:?}",
            r.cost, r.iters, r.enodes, r.eclasses, us, r.stop, r.best
        ),
        Err(_) => println!("egg:  STUB"),
    }
    println!();
}

fn sweep() {
    println!("-- random exprs, 20 seeds per depth --");
    println!(
        "{:>6} {:>9} {:>10} {:>10} {:>9} {:>9} {:>10} {:>10}",
        "depth", "in cost", "hand cost", "hand µs", "firings", "egg cost", "enodes", "egg µs"
    );
    for depth in [4usize, 6, 8, 10] {
        let exprs: Vec<_> = (0..20).map(|s| gen_expr(depth, 100 + s)).collect();
        let in_cost: usize = exprs.iter().map(cost).sum();
        let t0 = Instant::now();
        let hand: Vec<_> = exprs.iter().map(hand_optimize).collect();
        let hand_us = t0.elapsed().as_secs_f64() * 1e6 / exprs.len() as f64;
        let hand_cost: usize = hand.iter().map(|(e, _)| cost(e)).sum();
        let firings: usize = hand.iter().map(|(_, s)| s).sum();
        let egg = catch_unwind(AssertUnwindSafe(|| {
            let t0 = Instant::now();
            let rs: Vec<_> = exprs.iter().map(eqsat::egg_optimize).collect();
            let us = t0.elapsed().as_secs_f64() * 1e6 / exprs.len() as f64;
            let c: usize = rs.iter().map(|r| r.cost).sum();
            let n: usize = rs.iter().map(|r| r.enodes).sum();
            (c, n / rs.len(), us)
        }));
        match egg {
            Ok((ec, en, eus)) => println!(
                "{:>6} {:>9} {:>10} {:>10.1} {:>9} {:>9} {:>10} {:>10.1}",
                depth,
                in_cost / 20,
                hand_cost / 20,
                hand_us,
                firings / 20,
                ec / 20,
                en,
                eus
            ),
            Err(_) => println!(
                "{:>6} {:>9} {:>10} {:>10.1} {:>9} {:>9} {:>10} {:>10}",
                depth,
                in_cost / 20,
                hand_cost / 20,
                hand_us,
                firings / 20,
                "STUB",
                "-",
                "-"
            ),
        }
    }
}
