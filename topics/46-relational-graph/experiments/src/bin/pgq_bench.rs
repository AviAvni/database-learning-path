//! Topic 46 bench — the many-to-many blow-up, three ways.
//!
//! Lane 1 (provided): a path pattern on a seeded power-law graph as flat binary
//! hash joins (tuples produced), as CSR expansion (same tuples, cheaper
//! steps), and as a factorized representation (values stored, from degrees).
//! Counts reproduce exactly on any machine; timings are this machine's.
//! Lane 2 (exercise): the factorized join operator, `src/factorized.rs`.
//! Lane 3: `a·b*` as a recursive SQL join (provided) and as a product-automaton
//! BFS (exercise, `src/rpq.rs`).

use relational_graph_experiments::{
    expand, factorized, flat, fsize,
    gen::{ffx_fig1, power_law, Edges, PowerLaw},
    graph::Graph,
    rpq,
};
use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
}

fn loadavg() -> String {
    let out = std::process::Command::new("sysctl").args(["-n", "vm.loadavg"]).output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => std::fs::read_to_string("/proc/loadavg").map(|s| s.trim().to_string()).unwrap_or_else(|_| "n/a".into()),
    }
}

/// Best of `reps` wall-clock runs, in milliseconds, plus the last result.
fn best_ms<T>(reps: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut best = f64::INFINITY;
    let mut last = None;
    for _ in 0..reps {
        let t = Instant::now();
        let r = black_box(f());
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        last = Some(r);
    }
    (best, last.unwrap())
}

fn commas(x: u64) -> String {
    let s = x.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

const SEED: u64 = 46;
const SIZES: [u32; 3] = [10_000, 20_000, 40_000];
const AVG_DEGREE: u64 = 8;

fn graph(n: u32, pref: u64) -> (Edges, Graph) {
    let e = power_law(PowerLaw { vertices: n, edges: AVG_DEGREE * n as u64, pref_percent: pref, seed: SEED });
    let g = Graph::new(n, &e);
    (e, g)
}

fn main() {
    quiet_stubs();
    println!("topic 46 — graph queries on relational engines: the many-to-many blow-up");
    println!("load average at start: {}", loadavg());

    lane_1a();
    let big = lane_1b();
    lane_2(&big);
    lane_3(&big);

    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement the todo!()s in src/factorized.rs and src/rpq.rs to unlock the lanes marked STUB]");
    }
}

fn lane_1a() {
    println!("\n-- lane 1a: the paper's own example (Yasser, Dorbani & Mhedhbi, SIGMOD'26, Fig. 1a, §2.2.1) --");
    let e = ffx_fig1();
    let g = Graph::new(9, &e);
    let s = fsize::two_hop(&g);
    let deep = fsize::two_hop_deep(&g);
    let ht = flat::build_by_src(&e);
    let mut sink = 0u64;
    let j = flat::k_hop(&e, &ht, 2, &mut sink);
    println!("   R has {} edges; query R(a1,a2) ⋈ R(a2,a3)", e.len());
    println!("   {:<38} {:>6} {:>9}", "", "here", "paper");
    println!("   {:<38} {:>6} {:>9}", "flat tuples (hash join output)", j.result(), 13);
    println!("   {:<38} {:>6} {:>9}", "flat values (3 per tuple)", j.values(), 39);
    println!("   {:<38} {:>6} {:>9}", "f-tree a1 → a2 → a3 (T1), values", deep, 22);
    println!("   {:<38} {:>6} {:>9}", "f-tree a2 → {a1, a3} (T2), values", s.fact_values, 15);
    let ok = j.result() == 13 && j.values() == 39 && deep == 22 && s.fact_values == 15;
    println!("   sanity: all four reproduce the paper — {}", if ok { "yes" } else { "NO (bug in this crate)" });
}

struct Big {
    edges: Edges,
    g: Graph,
}

fn lane_1b() -> Big {
    println!("\n-- lane 1b: path patterns on seeded power-law graphs (avg out-degree {AVG_DEGREE}, 70% preferential, seed {SEED}) --");
    println!("   counts are exact and reproduce everywhere; ms are best of 3 on this machine");
    println!();
    println!(
        "   {:>6} {:>8} {:>6} {:>6}  {:<6} {:>15} {:>16} {:>13} {:>8}",
        "|V|", "|E|", "maxout", "maxin", "hops", "flat tuples", "flat values", "f-rep values", "flat/f"
    );
    let mut graphs = Vec::new();
    for &n in &SIZES {
        let (e, g) = graph(n, 70);
        let s2 = fsize::two_hop(&g);
        let s3 = fsize::three_hop(&g);
        for (h, s, all_tuples, all_values) in [
            ("2-hop", s2, s2.flat_tuples, s2.flat_values),
            ("3-hop", s3, s2.flat_tuples + s3.flat_tuples, s2.flat_values + s3.flat_values),
        ] {
            let _ = s;
            println!(
                "   {:>6} {:>8} {:>6} {:>6}  {:<6} {:>15} {:>16} {:>13} {:>7.0}x",
                n,
                g.m(),
                g.max_out(),
                g.max_in(),
                h,
                commas(all_tuples),
                commas(all_values),
                commas(s.fact_values),
                all_values as f64 / s.fact_values as f64
            );
        }
        graphs.push((n, e, g));
    }
    // Control: the same size with no preferential attachment — no hubs.
    let (_, gu) = graph(SIZES[2], 0);
    let u2 = fsize::two_hop(&gu);
    let u3 = fsize::three_hop(&gu);
    println!(
        "   {:>6} {:>8} {:>6} {:>6}  {:<6} {:>15} {:>16} {:>13} {:>7.0}x   <- control: uniform (0% preferential)",
        SIZES[2], gu.m(), gu.max_out(), gu.max_in(), "2-hop", commas(u2.flat_tuples), commas(u2.flat_values), commas(u2.fact_values),
        u2.flat_values as f64 / u2.fact_values as f64
    );
    println!(
        "   {:>6} {:>8} {:>6} {:>6}  {:<6} {:>15} {:>16} {:>13} {:>7.0}x",
        SIZES[2], gu.m(), gu.max_out(), gu.max_in(), "3-hop", commas(u2.flat_tuples + u3.flat_tuples),
        commas(u2.flat_values + u3.flat_values), commas(u3.fact_values),
        (u2.flat_values + u3.flat_values) as f64 / u3.fact_values as f64
    );
    println!("   (3-hop flat columns include the 2-hop intermediate the first join produced: every tuple the plan built)");

    println!();
    println!("   time to COUNT the result, ms (best of 3; load {})", loadavg());
    println!(
        "   {:>6}  {:<6} {:>14} {:>14} {:>14} {:>16} {:>11}",
        "|V|", "hops", "hash join", "CSR expand", "factorized", "ns/tuple (hash)", "counts agree"
    );
    let mut all_agree = true;
    let mut min_ns = f64::INFINITY;
    for (n, e, g) in &graphs {
        let ht = flat::build_by_src(e);
        for k in [2usize, 3] {
            let (t_hash, j) = best_ms(3, || {
                let mut s = 0u64;
                let r = flat::k_hop(black_box(e), black_box(&ht), k, &mut s);
                black_box(s);
                r
            });
            let (t_csr, x) = best_ms(3, || {
                let mut s = 0u64;
                let r = expand::k_hop(black_box(g), k, &mut s);
                black_box(s);
                r
            });
            let (t_fact, fcount) = best_ms(3, || {
                if k == 2 {
                    fsize::two_hop(black_box(g)).flat_tuples
                } else {
                    fsize::three_hop(black_box(g)).flat_tuples
                }
            });
            let agree = j.result() == x.result() && x.result() == fcount;
            all_agree &= agree;
            let ns = t_hash * 1e6 / j.tuples() as f64;
            min_ns = min_ns.min(ns);
            println!(
                "   {:>6}  {:<6} {:>14.1} {:>14.1} {:>14.2} {:>16.2} {:>11}",
                n,
                if k == 2 { "2-hop" } else { "3-hop" },
                t_hash,
                t_csr,
                t_fact,
                ns,
                if agree { "yes" } else { "NO" }
            );
        }
    }
    println!("   sanity: hash join, CSR expansion and the degree formula give the same count in every row — {}", if all_agree { "yes" } else { "NO (bug)" });
    println!(
        "   sanity: fastest hash-join row is {:.2} ns per produced tuple — {} (a per-tuple cost under ~0.1 ns would mean the work was optimised away)",
        min_ns,
        if min_ns > 0.1 { "plausible" } else { "IMPLAUSIBLE" }
    );
    let (_, e, g) = graphs.pop().unwrap();
    Big { edges: e, g }
}

fn lane_2(big: &Big) {
    let n = SIZES[2];
    println!("\n-- lane 2: the factorized join operator, packed vectors (src/factorized.rs), |V| = {n} --");
    let ix = factorized::Indexes::new(&big.edges);
    let (t_two, two) = best_ms(3, || factorized::two_hop(black_box(&big.edges), &ix));
    let want2 = fsize::two_hop(&big.g);
    println!(
        "   PROVIDED two_hop: {} values (closed form {}), represents {} tuples, built in {:.1} ms",
        commas(two.values()),
        commas(want2.fact_values),
        commas(two.count()),
        t_two
    );
    let want3 = fsize::three_hop(&big.g);
    match catch_unwind(AssertUnwindSafe(|| {
        let (t_ext, three) = best_ms(3, || factorized::extend(black_box(&two), &ix));
        let (t_cnt, cnt) = best_ms(3, || black_box(&three).count());
        let vals = three.values();
        let mut filtered = three.clone();
        let (t_f, _) = best_ms(1, || filtered.retain_d(|d| d % 8 == 0));
        (t_ext, three.check(), vals, t_cnt, cnt, t_f, filtered.values(), filtered.count(), filtered.check())
    })) {
        Ok((t_ext, chk, vals, t_cnt, cnt, t_f, fvals, fcnt, fchk)) => {
            println!("   extend (bottom-insert R(c,d) under c): {:.1} ms, {} values (closed form {}), invariants {}", t_ext, commas(vals), commas(want3.fact_values), if chk.is_ok() { "ok" } else { "BROKEN" });
            println!("   count() without flattening: {} tuples in {:.3} ms (flat join count: {})", commas(cnt), t_cnt, commas(want3.flat_tuples));
            println!("   retain_d(d % 8 == 0) with cascade: {:.1} ms → {} values, {} tuples, invariants {}", t_f, commas(fvals), commas(fcnt), if fchk.is_ok() { "ok" } else { "BROKEN" });
        }
        Err(_) => println!("   STUB  extend / count / retain_d   [stub — implement src/factorized.rs]"),
    }
}

fn lane_3(big: &Big) {
    let n = SIZES[2];
    let g = rpq::Labeled::new(n, &big.edges, SEED);
    let sources: Vec<u32> = (0..n).step_by((n / 256) as usize).collect();
    println!("\n-- lane 3: the RPQ a·b* from {} sources, |V| = {n} ({} a-edges, {} b-edges; load {}) --", sources.len(), commas(g.a.len() as u64), commas(g.b.len() as u64), loadavg());
    let (t_sn, sn) = best_ms(3, || rpq::semi_naive(black_box(&g), black_box(&sources)));
    println!("   PROVIDED WITH RECURSIVE (semi-naive, hash join + UNION): {} pairs, {} rounds, {} tuples derived, {:.1} ms", commas(sn.pairs), sn.rounds, commas(sn.derived), t_sn);
    println!("   bounded a·b^j, j ≤ k: answer pairs (set semantics) against walks (bag semantics — the rows path enumeration emits)");
    println!("   {:>6} {:>14} {:>28} {:>16}", "k", "pairs", "walks", "walks / pairs");
    for k in [0usize, 1, 2, 3, 4, 6, 8] {
        let pairs: u64 = sn.new_per_round.iter().take(k + 1).sum();
        let w = rpq::walks_up_to(&g, &sources, k);
        let shown = if w == u64::MAX { "≥ 2^64 (saturated)".to_string() } else { commas(w) };
        println!("   {:>6} {:>14} {:>28} {:>15.1}x", k, commas(pairs), shown, w as f64 / pairs as f64);
    }
    match catch_unwind(AssertUnwindSafe(|| best_ms(3, || rpq::product_bfs(black_box(&g), black_box(&sources))))) {
        Ok((t_bfs, r)) => {
            println!(
                "   product-automaton BFS: {} pairs, {} states, {} edges scanned, {:.1} ms ({:.1}x the recursive join)",
                commas(r.pairs),
                commas(r.states),
                commas(r.edges_scanned),
                t_bfs,
                t_sn / t_bfs
            );
            println!(
                "   sanity: same pairs — {}; edges scanned = tuples derived — {}",
                if r.pairs == sn.pairs { "yes" } else { "NO" },
                if r.edges_scanned == sn.derived { "yes" } else { "NO" }
            );
        }
        Err(_) => println!("   STUB  product-automaton BFS   [stub — implement src/rpq.rs]"),
    }
}
