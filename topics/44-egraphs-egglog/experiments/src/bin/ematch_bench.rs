//! ematch_bench — the same pattern, matched as a graph walk and as a join.
//!
//! cargo run --release --bin ematch_bench

use egraph_db_experiments::{
    backtrack, binary_join,
    gen::{db_delta, edge_graph, Fig2},
    pattern::{compile, number, papp, pvar, Query},
    relational::{self, db_tuples, plan, to_database, Database},
    semi_naive,
};
use std::cell::Cell;
use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
}

fn stub_summary(what: &str) {
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement {what} to unlock the lanes marked STUB]");
    }
}

/// Best of three, in microseconds.
fn best3(mut f: impl FnMut() -> u64) -> (f64, u64) {
    let mut best = f64::MAX;
    let mut work = 0;
    for _ in 0..3 {
        let t = Instant::now();
        let w = f();
        let us = t.elapsed().as_secs_f64() * 1e6;
        if us < best {
            best = us;
        }
        work = w;
    }
    (best, work)
}

fn main() {
    quiet_stubs();
    println!("=== ematch_bench ===\n");
    lane1();
    lane2();
    lane3();
    stub_summary("src/semi_naive.rs and src/binary_join.rs");
}

fn header(q: &Query, db: &Database, name_of: &dyn Fn(u32) -> String) {
    let order = plan(q, db);
    let vars: Vec<String> = order.iter().map(|&v| q.names[v].clone()).collect();
    println!("   {}", q.render(name_of));
    println!("   variable ordering: [{}]", vars.join(", "));
}

fn lane1() {
    for (title, linear) in [
        ("f(a, g(a)) — one equality constraint, N matches", false),
        ("f(a, g(b)) — linear pattern, N^2 matches", true),
    ] {
        println!("-- lane 1: {title} --");
        let probe = Fig2::new(4);
        let (f, gs) = (probe.f, probe.gs);
        let pat = if linear {
            papp(f, vec![pvar("a"), papp(gs, vec![pvar("b")])])
        } else {
            papp(f, vec![pvar("a"), papp(gs, vec![pvar("a")])])
        };
        let q = compile(&[pat.clone()]);
        let names: Vec<String> = [f, gs].iter().map(|&s| probe.g.sym_name(s).to_string()).collect();
        header(&q, &to_database(&probe.g), &|s| {
            names[if s == f { 0 } else { 1 }].clone()
        });
        println!(
            "\n{:>7} {:>9} {:>11} {:>13} {:>12} {:>13} {:>10} {:>10} {:>9}",
            "N", "e-nodes", "matches", "bt visits", "bt µs", "gj probes", "index µs", "gj µs", "speedup"
        );

        for n in [100usize, 200, 400, 800, 1600] {
            let fig = Fig2::new(n);
            let g = &fig.g;
            let pv = number(&pat, &q);
            let prog = backtrack::compile(&[pv], q.n_vars, &q.roots);

            let (bt_us, bt_visits) = best3(|| {
                let visits = Cell::new(0);
                let mut hits = 0u64;
                backtrack::search(g, &prog, &visits, &mut |regs| {
                    black_box(regs);
                    hits += 1;
                });
                black_box(hits);
                visits.get()
            });
            let mut bt_matches = 0u64;
            {
                let visits = Cell::new(0);
                backtrack::search(g, &prog, &visits, &mut |_| bt_matches += 1);
            }

            let db = to_database(g);
            let order = plan(&q, &db);
            let (idx_us, _) = best3(|| {
                let idx = relational::index_query(&q, &db, &order);
                black_box(idx.len() as u64)
            });
            let idx = relational::index_query(&q, &db, &order);
            let (gj_us, gj_probes) = best3(|| {
                let probes = Cell::new(0);
                let mut hits = 0u64;
                relational::generic_join(&order, &idx, q.n_vars, &probes, &mut |s| {
                    black_box(s);
                    hits += 1;
                });
                black_box(hits);
                probes.get()
            });
            let mut gj_matches = 0u64;
            {
                let probes = Cell::new(0);
                relational::generic_join(&order, &idx, q.n_vars, &probes, &mut |_| {
                    gj_matches += 1
                });
            }
            assert_eq!(bt_matches, gj_matches, "the two matchers disagree at N={n}");

            println!(
                "{:>7} {:>9} {:>11} {:>13} {:>12.1} {:>13} {:>10.1} {:>10.1} {:>9}",
                n,
                g.total_nodes(),
                bt_matches,
                bt_visits,
                bt_us,
                gj_probes,
                idx_us,
                gj_us,
                format!("{:.2}x", bt_us / (gj_us + idx_us))
            );
        }
        println!();
    }
}

fn lane2() {
    println!("-- lane 2: one more iteration of saturation — re-derive, or take the delta --");
    let n = 20_000;
    let k = 8;
    let mut fig = Fig2::new(n);
    let before = to_database(&fig.g);
    let (f, gs) = (fig.f, fig.gs);
    fig.grow(k);
    let after = to_database(&fig.g);
    let delta = db_delta(&after, &before);
    let q = compile(&[papp(f, vec![pvar("a"), papp(gs, vec![pvar("a")])])]);
    println!(
        "   e-graph {} tuples + delta of {} tuples ({} new constants)",
        db_tuples(&before),
        db_tuples(&delta),
        k
    );
    println!(
        "\n{:>14} {:>11} {:>13} {:>10}",
        "evaluation", "matches", "probes", "µs"
    );

    let (full_us, full_probes) = best3(|| {
        let p = Cell::new(0);
        black_box(relational::matches(&q, &after, &p).len());
        p.get()
    });
    let full_n = {
        let p = Cell::new(0);
        relational::matches(&q, &after, &p).len()
    };
    println!("{:>14} {:>11} {:>13} {:>10.1}", "naive", full_n, full_probes, full_us);

    match catch_unwind(AssertUnwindSafe(|| {
        let (us, probes) = best3(|| {
            let p = Cell::new(0);
            black_box(semi_naive::delta_matches(&q, &after, &delta, &p).len());
            p.get()
        });
        let n = {
            let p = Cell::new(0);
            semi_naive::delta_matches(&q, &after, &delta, &p).len()
        };
        (n, probes, us)
    })) {
        Ok((n, probes, us)) => println!(
            "{:>14} {:>11} {:>13} {:>10.1}",
            "semi-naive", n, probes, us
        ),
        Err(_) => println!(
            "{:>14} {:>11} {:>13} {:>10}",
            "semi-naive", "STUB", "-", "-"
        ),
    }
    println!();
}

fn lane3() {
    println!("-- lane 3: the triangle multi-pattern {{e(x,y), e(y,z), e(z,x)}} --");
    println!("   (no backtracking column: with three roots to scan it is O(M^3), minutes at M=4000)");
    println!(
        "\n{:>7} {:>7} {:>11} {:>13} {:>10} {:>15} {:>13} {:>10}",
        "V", "E", "matches", "gj probes", "gj µs", "bj intermediate", "bj probes", "bj µs"
    );
    for (v, e) in [(200usize, 1000usize), (400, 2000), (800, 4000), (1600, 8000)] {
        let (g, esym) = edge_graph(v, e, 20 + v as u64);
        let db = to_database(&g);
        let q = compile(&[
            papp(esym, vec![pvar("x"), pvar("y")]),
            papp(esym, vec![pvar("y"), pvar("z")]),
            papp(esym, vec![pvar("z"), pvar("x")]),
        ]);
        let order = plan(&q, &db);
        let idx = relational::index_query(&q, &db, &order);
        let (gj_us, gj_probes) = best3(|| {
            let p = Cell::new(0);
            let mut hits = 0u64;
            relational::generic_join(&order, &idx, q.n_vars, &p, &mut |s| {
                black_box(s);
                hits += 1;
            });
            black_box(hits);
            p.get()
        });
        let mut n_matches = 0u64;
        {
            let p = Cell::new(0);
            relational::generic_join(&order, &idx, q.n_vars, &p, &mut |_| n_matches += 1);
        }
        match catch_unwind(AssertUnwindSafe(|| {
            let p = Cell::new(0);
            let t = Instant::now();
            let r = binary_join::binary_join(&q, &db, &p);
            let us = t.elapsed().as_secs_f64() * 1e6;
            (r.max_intermediate, r.matches.len(), p.get(), us)
        })) {
            Ok((inter, m, probes, us)) => {
                assert_eq!(m as u64, n_matches, "binary join disagrees with generic join");
                println!(
                    "{:>7} {:>7} {:>11} {:>13} {:>10.1} {:>15} {:>13} {:>10.1}",
                    v, e, n_matches, gj_probes, gj_us, inter, probes, us
                );
            }
            Err(_) => println!(
                "{:>7} {:>7} {:>11} {:>13} {:>10.1} {:>15} {:>13} {:>10}",
                v, e, n_matches, gj_probes, gj_us, "STUB", "-", "-"
            ),
        }
    }
}
