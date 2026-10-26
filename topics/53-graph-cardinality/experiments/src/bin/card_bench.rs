//! card_bench — where multi-hop cardinality estimation dies, priced in q-error.
//!
//! cargo run --release --bin card_bench
//!
//! Every number is a count or a ratio of counts from a seeded generator, so
//! it reproduces exactly; nothing here is timed.

use graphcard_experiments::{
    catalog,
    cset,
    est::*,
    gen::{generate, Graph, Params, Popularity, TYPES},
    oracle::{path_count, prefix_counts, suffix_counts, triangle_count, walks},
    plan::{edge_orders, order_cost, plan},
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

static STUBBED: AtomicBool = AtomicBool::new(false);
const SEEDS: [u64; 5] = [1, 2, 3, 4, 5];

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
}

fn name(w: &[usize]) -> String {
    w.iter().map(|t| format!("r{t}")).collect::<Vec<_>>().join(">")
}

fn mode_name(p: Popularity) -> &'static str {
    match p {
        Popularity::Shared => "shared popularity (a hub is a hub for every type)",
        Popularity::Independent => "independent popularity (in-degree and out-degree unrelated)",
    }
}

/// One row of a by-hop table: pooled over every pattern of every seed.
fn row(label: &str, hop: usize, samples: &[(f64, f64)]) {
    let q: Vec<f64> = samples.iter().map(|&(e, t)| q_error(e, t)).collect();
    let under = samples.iter().filter(|&&(e, t)| e.max(1.0) < t.max(1.0)).count();
    let n = q.len();
    let (med, p90, max) = summarize(q);
    println!(
        "   {:<26} {:>3} {:>4} {:>12.1} {:>12.1} {:>14.1} {:>7.0}%",
        label,
        hop,
        n,
        med,
        p90,
        max,
        100.0 * under as f64 / n as f64
    );
}

fn header() {
    println!(
        "   {:<26} {:>3} {:>4} {:>12} {:>12} {:>14} {:>8}",
        "estimator", "hop", "n", "median q", "p90 q", "max q", "under"
    );
}

fn main() {
    quiet_stubs();
    println!("=== card_bench — topic 53 ===\n");
    let g0 = generate(&Params::lane1(1, Popularity::Shared));
    println!(
        "graph (lane 1, seed 1): {} nodes in 4 labels {:?}; edges per type after de-duplication {:?}",
        g0.n, g0.label_sizes, g0.edges
    );
    println!("types: r0 L0>L1, r1 L1>L2, r2 L2>L3, r3 L3>L0, r4 L2>L0; Zipf exponents 0.6 (sources) 1.0 (destinations); seeds {SEEDS:?}\n");

    for pop in [Popularity::Shared, Popularity::Independent] {
        lane1(pop);
    }
    lane1_detail();
    lane1_triangles();
    lane1_plans();
    lane1_sanity();
    lane2();
    lane3();

    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/cset.rs (lane 2) and src/catalog.rs (lane 3) to unlock the lanes marked STUB]");
    }
}

fn graphs(pop: Popularity) -> Vec<Graph> {
    SEEDS.iter().map(|&s| generate(&Params::lane1(s, pop))).collect()
}

/// Lane 1, by hop: independence, Memgraph's constant, the max-degree bound.
fn lane1(pop: Popularity) {
    println!("-- lane 1: q-error by hop, {} --", mode_name(pop));
    header();
    let gs = graphs(pop);
    for hop in 2..=4 {
        let mut ind = Vec::new();
        let mut mem = Vec::new();
        let mut mx = Vec::new();
        for g in &gs {
            for w in walks(hop) {
                let t = path_count(g, &w) as f64;
                ind.push((indep_path(g, &w), t));
                mem.push((memgraph_const(g, &w), t));
                mx.push((maxdeg_bound(g, &w), t));
            }
        }
        row("independence (kuzu/neo4j)", hop, &ind);
        row("memgraph constant (3^k)", hop, &mem);
        row("max-degree bound", hop, &mx);
    }
    // One join: the degree-sequence bound and the l2 bound.
    let mut dsb = Vec::new();
    let mut l2 = Vec::new();
    for g in &gs {
        for w in walks(2) {
            let t = path_count(g, &w) as f64;
            dsb.push((dsb_two_hop(g, w[0], w[1]), t));
            l2.push((l2_two_hop(g, w[0], w[1]), t));
        }
    }
    row("degree-sequence bound", 2, &dsb);
    row("l2-norm bound", 2, &l2);
    println!();
}

fn lane1_detail() {
    let g = generate(&Params::lane1(1, Popularity::Shared));
    println!("-- lane 1, detail: seed 1, shared popularity — true count, estimate, signed q-error --");
    println!("   {:<12} {:>16} {:>16} {:>12}", "pattern", "true", "independence", "q (- under)");
    for hop in 2..=4 {
        for w in walks(hop) {
            let t = path_count(&g, &w) as f64;
            let e = indep_path(&g, &w);
            let q = q_error(e, t);
            let sign = if e.max(1.0) < t.max(1.0) { -q } else { q };
            println!("   {:<12} {:>16.0} {:>16.1} {:>12.1}", name(&w), t, e, sign);
        }
    }
    println!();
}

fn lane1_triangles() {
    println!("-- lane 1, triangles r0>r1>r4 (L0>L1>L2>L0): the closing edge --");
    println!(
        "   {:<12} {:>5} {:>12} {:>14} {:>8} {:>14} {:>8} {:>14} {:>8}",
        "popularity", "seed", "true", "independence", "q", "AGM bound", "q", "l2 bound", "q"
    );
    for pop in [Popularity::Shared, Popularity::Independent] {
        for g in graphs(pop).iter().take(3).enumerate().map(|(i, g)| (SEEDS[i], g)) {
            let (s, g) = g;
            let t = triangle_count(g, 0, 1, 4) as f64;
            let e = indep_triangle(g, 0, 1, 4);
            let (agm, l2) = (agm_triangle(g, 0, 1, 4), l2_triangle(g, 0, 1, 4));
            println!(
                "   {:<12} {:>5} {:>12.0} {:>14.2} {:>8.1} {:>14.0} {:>8.1} {:>14.0} {:>8.1}",
                if pop == Popularity::Shared { "shared" } else { "independent" },
                s,
                t,
                e,
                q_error(e, t),
                agm,
                q_error(agm, t),
                l2,
                q_error(l2, t)
            );
        }
    }
    println!();
}

/// Plan every walk of `hop` edges with `est(graph index, graph, sub-walk)` and
/// return (patterns, wrong picks, geometric-mean regret, worst regret).
fn plan_stats(
    gs: &[Graph],
    hop: usize,
    est: &dyn Fn(usize, &Graph, &[usize]) -> f64,
) -> (usize, usize, f64, f64, f64) {
    let orders = edge_orders(hop);
    let (mut n, mut wrong, mut lg, mut worst, mut share) = (0usize, 0usize, 0.0f64, 1.0f64, 0.0f64);
    for (gi, g) in gs.iter().enumerate() {
        for w in walks(hop) {
            let mut tc = vec![vec![0f64; hop]; hop];
            for lo in 0..hop {
                for hi in lo..hop {
                    tc[lo][hi] = path_count(g, &w[lo..=hi]) as f64;
                }
            }
            let p = plan(
                &orders,
                &|lo, hi| tc[lo][hi],
                &|lo, hi| est(gi, g, &w[lo..=hi]),
            );
            let best = orders.iter().map(|o| order_cost(o, &|lo, hi| tc[lo][hi])).fold(f64::INFINITY, f64::min);
            share += tc[0][hop - 1] / best;
            n += 1;
            wrong += p.wrong as usize;
            lg += p.regret.ln();
            worst = worst.max(p.regret);
        }
    }
    (n, wrong, (lg / n as f64).exp(), worst, share / n as f64)
}

fn plan_row(label: &str, pop: Popularity, hop: usize, r: (usize, usize, f64, f64, f64)) {
    println!(
        "   {:<26} {:<12} {:>4} {:>9} {:>12} {:>10.2} {:>13.2} {:>13.1}%",
        label,
        if pop == Popularity::Shared { "shared" } else { "independent" },
        hop,
        r.0,
        r.1,
        r.2,
        r.3,
        100.0 * r.4
    );
}

fn plan_header() {
    println!(
        "   {:<26} {:<12} {:>4} {:>9} {:>12} {:>10} {:>13} {:>14}",
        "estimator", "popularity", "hops", "patterns", "wrong picks", "geomean", "worst regret", "answer share"
    );
}

fn lane1_plans() {
    println!("-- lane 1, plan choice: which edge to start from, picked from the estimates --");
    println!("   a k-edge path has 2^(k-1) left-deep orders; cost = rows materialised (sum of the interval counts);");
    println!("   regret = true cost of the order the estimates pick / true cost of the truly cheapest order;");
    println!("   answer share = the final answer's part of the cheapest order's cost (every order pays it)");
    plan_header();
    for pop in [Popularity::Shared, Popularity::Independent] {
        let gs = graphs(pop);
        for hop in 3..=4 {
            let r = plan_stats(&gs, hop, &|_, g, w| indep_path(g, w));
            plan_row("independence (kuzu/neo4j)", pop, hop, r);
        }
    }
    println!();
}

fn lane1_sanity() {
    let g = generate(&Params::lane1(1, Popularity::Shared));
    let mut ok2 = true;
    for w in walks(2) {
        let s: u64 = g
            .label_range(TYPES[w[0]].1)
            .map(|v| (g.inn[w[0]].deg(v) * g.out[w[1]].deg(v)) as u64)
            .sum();
        ok2 &= s == path_count(&g, &w);
    }
    println!("-- lane 1, sanity --");
    println!("   2-hop oracle (frontier DP) equals sum over middle nodes of in-degree x out-degree for every 2-walk: {}", if ok2 { "agree" } else { "DISAGREE" });
    let mut ok = true;
    for w in walks(4) {
        ok &= prefix_counts(&g, &w).last() == suffix_counts(&g, &w).last();
    }
    println!("   4-hop forward DP and backward DP give the same total for every 4-walk: {}", if ok { "agree" } else { "DISAGREE" });
    let mut one = true;
    for t in 0..5 {
        one &= (indep_path(&g, &[t]) - path_count(&g, &[t]) as f64).abs() < 1e-9;
    }
    println!("   a single edge is estimated exactly by every estimator (q = 1): {}", if one { "holds" } else { "VIOLATED" });
    let mut up = true;
    for hop in 2..=3 {
        for w in walks(hop) {
            up &= maxdeg_bound(&g, &w) >= path_count(&g, &w) as f64;
        }
    }
    println!("   the max-degree bound is never below the truth: {}", if up { "holds" } else { "VIOLATED" });
    println!();
}

fn lane2() {
    println!("-- lane 2: characteristic sets, shared popularity (pooled over seeds) --");
    header();
    let gs = graphs(Popularity::Shared);
    let r = catch_unwind(AssertUnwindSafe(|| {
        let sums: Vec<_> = gs.iter().map(cset::build).collect();
        let mut out = Vec::new();
        for hop in 2..=4 {
            let mut s = Vec::new();
            for (g, cs) in gs.iter().zip(&sums) {
                for w in walks(hop) {
                    s.push((cs.estimate_path(&w), path_count(g, &w) as f64));
                }
            }
            out.push((hop, s));
        }
        (sums[0].num_sets(), out)
    }));
    match r {
        Ok((nsets, out)) => {
            for (hop, s) in out {
                row("characteristic sets", hop, &s);
            }
            println!("   seed 1 summary: {nsets} characteristic sets");
            println!();
            plan_header();
            let sums: Vec<_> = gs.iter().map(cset::build).collect();
            for hop in 3..=4 {
                let r = plan_stats(&gs, hop, &|gi, _, w| sums[gi].estimate_path(w));
                plan_row("characteristic sets", Popularity::Shared, hop, r);
            }
        }
        Err(_) => println!("   STUB"),
    }
    println!();
}

fn lane3() {
    println!("-- lane 3: two-edge catalog (GLogue order 2), shared popularity (pooled over seeds) --");
    header();
    let gs = graphs(Popularity::Shared);
    let r = catch_unwind(AssertUnwindSafe(|| {
        let cats: Vec<_> = gs.iter().map(catalog::build).collect();
        let mut out = Vec::new();
        for hop in 2..=4 {
            let mut s = Vec::new();
            for (g, c) in gs.iter().zip(&cats) {
                for w in walks(hop) {
                    s.push((c.estimate_path(&w), path_count(g, &w) as f64));
                }
            }
            out.push((hop, s));
        }
        (cats[0].size(), out)
    }));
    match r {
        Ok((size, out)) => {
            for (hop, s) in out {
                row("two-edge catalog", hop, &s);
            }
            println!("   catalog size: {size} stored patterns");
            println!();
            plan_header();
            let cats: Vec<_> = gs.iter().map(catalog::build).collect();
            for hop in 3..=4 {
                let r = plan_stats(&gs, hop, &|gi, _, w| cats[gi].estimate_path(w));
                plan_row("two-edge catalog", Popularity::Shared, hop, r);
            }
        }
        Err(_) => println!("   STUB"),
    }
    println!();
}
