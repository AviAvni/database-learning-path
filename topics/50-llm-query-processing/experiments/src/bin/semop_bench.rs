//! semop_bench — a semantic filter priced like a query optimizer would price it.
//!
//! cargo run --release --bin semop_bench
//!
//! A SIMULATION: no LLM is called and nothing touches the network. Every model
//! is a seeded function with stated parameters (src/model.rs). Call counts, F1
//! and selectivities follow from the seed and reproduce exactly; dollars and
//! seconds are those counts times MODEL constants.

use llm_query_processing_experiments::{
    gen::Workload,
    join::{self, JoinWorkload},
    model::{wave_seconds, PARALLELISM, P1, P2},
    plan::{self, Band, Cache, Meter},
    rng::Rng,
    threshold,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
}

const SEED: u64 = 50;
const N: usize = 10_000;
const SAMPLE: usize = 200;

fn main() {
    quiet_stubs();
    println!("=== semop_bench — SIMULATED models, see src/model.rs; no LLM is called ===\n");
    let w = Workload::new(SEED, N, vec![P1, P2]);
    let rows: Vec<usize> = (0..N).collect();
    lane1a(&w, &rows);
    lane1c(&w, &rows);
    lane1c_strength(&rows);
    lane1d(&w);
    lane1e(&w, &rows);
    lane2();
    lane3();
    if STUBBED.load(Ordering::Relaxed) {
        println!(
            "\n[stub — implement threshold::learn_thresholds (lane 2) and join::blocked_join (lane 3) to unlock the rows marked STUB]"
        );
    }
}

fn row(name: &str, m: &Meter, got: &[usize], truth: &[usize], gold: &[usize]) {
    let (_, _, f_truth) = plan::prf(got, truth);
    let (_, _, f_gold) = plan::prf(got, gold);
    println!(
        "{:<38} {:>7} {:>7} {:>9.2} {:>8.0} {:>6} {:>9.3} {:>8.3}",
        name, m.oracle_calls, m.proxy_calls, m.dollars, m.seconds, got.len(), f_truth, f_gold
    );
}

fn lane1a(w: &Workload, rows: &[usize]) {
    println!("-- lane 1a: WHERE P1 AND P2 over {N} rows — orderings of the oracle --");
    for (p, pred) in w.preds.iter().enumerate() {
        println!(
            "   {:<32} oracle ${:.4}/call {:.2} s/call, accuracy {:.2}; true selectivity {:.4}, oracle-measured {:.4}",
            pred.name,
            pred.oracle.dollars,
            pred.oracle.seconds,
            pred.oracle_accuracy,
            rows.iter().filter(|&&d| w.truth(d, p)).count() as f64 / rows.len() as f64,
            w.oracle_selectivity(rows, p)
        );
    }
    let truth = w.truth_set(rows);
    let gold = w.gold_set(rows);
    println!("   ground-truth answer: {} rows; gold (oracle on every row): {} rows; parallelism {PARALLELISM}\n", truth.len(), gold.len());
    println!(
        "{:<38} {:>7} {:>7} {:>9} {:>8} {:>6} {:>9} {:>8}",
        "plan", "oracle", "proxy", "$ MODEL", "s MODEL", "rows", "F1 truth", "F1 gold"
    );

    let mut m = Meter::default();
    let a = plan::oracle_all(w, rows, &mut m);
    row("(a) every predicate, every row", &m, &a, &truth, &gold);

    let mut m = Meter::default();
    let written = plan::ordered(w, rows, &[1, 0], &mut m, None);
    row("    short-circuit, written order P2,P1", &m, &written, &truth, &gold);
    let written_calls = m.oracle_calls;

    let sample = Rng::new(SEED ^ 0xBEEF).sample(N, SAMPLE);
    let mut ms = Meter::default();
    let (rank, sel) = plan::rank_order(w, &sample, &mut ms);
    let by_sel = plan::selectivity_order(&sel);

    let mut m = ms.clone();
    let s_only = plan::ordered(w, rows, &by_sel, &mut m, None);
    row(&format!("    selectivity order {}", names(&by_sel)), &m, &s_only, &truth, &gold);

    let mut m = ms.clone();
    let b = plan::ordered(w, rows, &rank, &mut m, None);
    row(&format!("(b) rank order {} (+{SAMPLE}-row sample)", names(&rank)), &m, &b, &truth, &gold);

    println!("\n   the {SAMPLE}-row sample cost {} oracle calls (${:.2}) and estimated:", ms.oracle_calls, ms.dollars);
    for p in 0..w.preds.len() {
        let c = w.preds[p].oracle.dollars;
        println!(
            "   {:<32} selectivity {:.3}  cost ${:.4}  rank (s-1)/c = {:>8.1}",
            w.preds[p].name,
            sel[p],
            c,
            (sel[p] - 1.0) / c
        );
    }

    // Sanity: closed forms.
    let s2 = rows.iter().filter(|&&d| w.oracle(d, 1)).count();
    let same = a == written && a == s_only && a == b && a == gold;
    println!(
        "\n   sanity: written-order calls {} = N + |P2 yes| = {} + {} → {}",
        written_calls,
        N,
        s2,
        if written_calls == (N + s2) as u64 { "exact" } else { "MISMATCH" }
    );
    println!("   sanity: every ordering returns the gold set → {}", if same { "same rows" } else { "DIFFERENT ROWS" });
    let f1 = plan::oracle_f1_closed_form(&w.preds);
    let (_, _, f_meas) = plan::prf(&a, &truth);
    let seeds = 30u64;
    let f_mean = (1..=seeds)
        .map(|s| {
            let ws = Workload::new(SEED + 1000 + s, N, w.preds.clone());
            plan::prf(&ws.gold_set(rows), &ws.truth_set(rows)).2
        })
        .sum::<f64>()
        / seeds as f64;
    println!(
        "   sanity: oracle F1 vs truth — closed form {:.3}; this seed {:.3}; mean over {seeds} other seeds {:.3}",
        f1, f_meas, f_mean
    );
    println!();
}

fn names(order: &[usize]) -> String {
    order.iter().map(|p| format!("P{}", p + 1)).collect::<Vec<_>>().join(",")
}

fn lane1c(w: &Workload, rows: &[usize]) {
    println!("-- lane 1c: proxy → oracle cascade, rank order P1,P2, band [0.5-h, 0.5+h] on both predicates --");
    println!(
        "   proxy {:.5} $/call {:.3} s/call; proxy right at a 0.5 cut: P1 {:.3}, P2 {:.3} (measured)",
        P1.proxy.dollars,
        P1.proxy.seconds,
        proxy_acc(w, rows, 0),
        proxy_acc(w, rows, 1)
    );
    let truth = w.truth_set(rows);
    let gold = w.gold_set(rows);
    println!(
        "\n{:>5} {:>7} {:>7} {:>9} {:>8} {:>8} {:>8} {:>8} {:>9}",
        "h", "oracle", "proxy", "$ MODEL", "s MODEL", "P gold", "R gold", "F1 gold", "F1 truth"
    );
    for h in [0.0, 0.1, 0.2, 0.3, 0.4, 0.45, 0.49, 0.5] {
        let mut m = Meter::default();
        let got = plan::cascade(w, rows, &[0, 1], &[Band::symmetric(h); 2], &mut m);
        let (p, r, f) = plan::prf(&got, &gold);
        let (_, _, ft) = plan::prf(&got, &truth);
        println!(
            "{:>5.2} {:>7} {:>7} {:>9.2} {:>8.0} {:>8.3} {:>8.3} {:>8.3} {:>9.3}",
            h, m.oracle_calls, m.proxy_calls, m.dollars, m.seconds, p, r, f, ft
        );
    }
    let full = plan::cascade(w, rows, &[0, 1], &[Band::symmetric(0.5); 2], &mut Meter::default());
    println!(
        "\n   sanity: h = 0.50 sends every row to the oracle and returns the gold set → {}",
        if full == gold { "same rows" } else { "DIFFERENT ROWS" }
    );
    println!();
}

/// The cascade only pays if the proxy is good — and "good" depends on what
/// you measure against. Same workload seed, same oracle; only the proxy's
/// separation `mu` changes (both predicates). For each strength, the cheapest
/// band on a 0.05 grid that meets each of two quality bars:
///   * F1 vs gold ≥ 0.95 (LOTUS's frame: agree with the oracle);
///   * F1 vs truth ≥ the oracle's own F1 vs truth (be no worse than paying
///     the oracle for every row).
fn lane1c_strength(rows: &[usize]) {
    println!("-- lane 1c': how good must the proxy be? same oracle, proxy separation mu varied, rank order P1,P2 --");
    let base = Workload::new(SEED, N, vec![P1, P2]);
    let mut mb = Meter::default();
    plan::ordered(&base, rows, &[0, 1], &mut mb, None);
    let (_, _, f_oracle) = plan::prf(&base.gold_set(rows), &base.truth_set(rows));
    println!(
        "   baseline: plan (b)'s main pass, oracle only — {} calls, ${:.2}; its F1 vs truth {:.3}\n",
        mb.oracle_calls, mb.dollars, f_oracle
    );
    println!(
        "{:>5} {:>6} | {:<17} | {:<28} | {:<28}",
        "", "proxy", "proxy alone", "cheapest h: F1 gold ≥ 0.95", "cheapest h: F1 truth ≥ oracle"
    );
    println!(
        "{:>5} {:>6} | {:>8} {:>8} | {:>5} {:>7} {:>6} {:>6} | {:>5} {:>7} {:>6} {:>6}",
        "mu", "acc P1", "F1 truth", "F1 gold", "h", "oracle", "F1", "$ v b", "h", "oracle", "F1", "$ v b"
    );
    for mu in [0.5, 1.0, 1.5, 2.0, 2.5, 3.0] {
        let mut p1 = P1;
        let mut p2 = P2;
        p1.proxy_mu = mu;
        p2.proxy_mu = mu;
        let w = Workload::new(SEED, N, vec![p1, p2]);
        let gold = w.gold_set(rows);
        let truth = w.truth_set(rows);
        let acc = proxy_acc(&w, rows, 0);
        let alone = plan::cascade(&w, rows, &[0, 1], &[Band::symmetric(0.0); 2], &mut Meter::default());
        let (_, _, a_t) = plan::prf(&alone, &truth);
        let (_, _, a_g) = plan::prf(&alone, &gold);
        let cheapest = |want: &[usize], bar: f64| {
            for step in 0..=10 {
                let h = step as f64 * 0.05;
                let mut m = Meter::default();
                let got = plan::cascade(&w, rows, &[0, 1], &[Band::symmetric(h); 2], &mut m);
                let (_, _, f) = plan::prf(&got, want);
                if f >= bar - 1e-12 {
                    return (h, m, f);
                }
            }
            unreachable!("h = 0.5 is the oracle itself")
        };
        let (hg, mg, fg) = cheapest(&gold, 0.95);
        let (ht, mt, ft) = cheapest(&truth, f_oracle);
        let pct = |m: &Meter| 100.0 * (m.dollars / mb.dollars - 1.0);
        println!(
            "{:>5.1} {:>6.3} | {:>8.3} {:>8.3} | {:>5.2} {:>7} {:>6.3} {:>5.0}% | {:>5.2} {:>7} {:>6.3} {:>5.0}%",
            mu, acc, a_t, a_g, hg, mg.oracle_calls, fg, pct(&mg), ht, mt.oracle_calls, ft, pct(&mt)
        );
    }
    println!("\n   '$ v b' includes the proxy's own cost (one call per surviving row per predicate).");
    println!();
}

fn proxy_acc(w: &Workload, rows: &[usize], p: usize) -> f64 {
    rows.iter().filter(|&&d| (w.proxy_score(d, p) >= 0.5) == w.truth(d, p)).count() as f64 / rows.len() as f64
}

fn lane1d(w: &Workload) {
    const AGENTS: usize = 10;
    const PROBE: usize = 2_000;
    println!("-- lane 1d: {AGENTS} agents each probe {PROBE} random rows with the same query (rank order P1,P2) --");
    let mut rng = Rng::new(SEED ^ 0xA6E7);
    let probes: Vec<Vec<usize>> = (0..AGENTS).map(|_| rng.sample(N, PROBE)).collect();

    let mut cold = Meter::default();
    let mut answers_cold = Vec::new();
    for pr in &probes {
        answers_cold.push(plan::ordered(w, pr, &[0, 1], &mut cold, None));
    }
    let mut cache = Cache::new();
    let mut warm = Meter::default();
    let mut answers_warm = Vec::new();
    for pr in &probes {
        answers_warm.push(plan::ordered(w, pr, &[0, 1], &mut warm, Some(&mut cache)));
    }
    let mut touched = vec![false; N];
    for pr in &probes {
        for &d in pr {
            touched[d] = true;
        }
    }
    let distinct = touched.iter().filter(|&&t| t).count();
    let p1_yes_distinct = (0..N).filter(|&d| touched[d] && w.oracle(d, 0)).count();
    println!("{:<30} {:>8} {:>10} {:>9} {:>8}", "", "oracle", "cache hits", "$ MODEL", "s MODEL");
    println!("{:<30} {:>8} {:>10} {:>9.2} {:>8.0}", "no cache (each agent alone)", cold.oracle_calls, cold.cache_hits, cold.dollars, cold.seconds);
    println!("{:<30} {:>8} {:>10} {:>9.2} {:>8.0}", "shared (doc, predicate) cache", warm.oracle_calls, warm.cache_hits, warm.dollars, warm.seconds);
    println!(
        "\n   {} rows requested, {distinct} distinct; expected distinct = N·(1 − (1 − {PROBE}/N)^{AGENTS}) = {:.0}",
        AGENTS * PROBE,
        N as f64 * (1.0 - (1.0 - PROBE as f64 / N as f64).powi(AGENTS as i32))
    );
    let expect = (distinct + p1_yes_distinct) as u64;
    println!(
        "   sanity: cached oracle calls = distinct rows + distinct P1-yes rows = {distinct} + {p1_yes_distinct} = {expect} → {}",
        if warm.oracle_calls == expect { "exact" } else { "MISMATCH" }
    );
    println!(
        "   sanity: every agent gets the same answer with and without the cache → {}",
        if answers_cold == answers_warm { "same rows" } else { "DIFFERENT ROWS" }
    );
    println!();
}

fn lane1e(w: &Workload, rows: &[usize]) {
    const K: usize = 10;
    println!("-- lane 1e: SELECT … WHERE P2 LIMIT {K} --");
    let mut all = Meter::default();
    let a = plan::limit_after_all(w, rows, 1, K, &mut all);
    let mut st = Meter::default();
    let s = plan::limit_streaming(w, rows, 1, K, &mut st);
    println!("{:<34} {:>8} {:>9} {:>8}", "", "oracle", "$ MODEL", "s MODEL");
    println!("{:<34} {:>8} {:>9.2} {:>8.0}", "evaluate every row, then LIMIT", all.oracle_calls, all.dollars, all.seconds);
    println!("{:<34} {:>8} {:>9.2} {:>8.0}", "stream in waves of 20, stop at 10", st.oracle_calls, st.dollars, st.seconds);
    let sel = w.oracle_selectivity(rows, 1);
    println!(
        "\n   ratio {:.0}×; expected streaming calls ≈ K / selectivity = {K} / {:.4} = {:.0}, rounded up to whole waves of {PARALLELISM}",
        all.oracle_calls as f64 / st.oracle_calls as f64,
        sel,
        K as f64 / sel
    );
    println!("   sanity: both return {} rows, identical → {}", a.len(), if a == s { "same rows" } else { "DIFFERENT ROWS" });
    println!();
}

fn lane2() {
    use threshold::{naive_thresholds, sweep, learn_thresholds, N as TN, SAMPLE as TS, TARGETS as T};
    println!(
        "-- lane 2: cascade thresholds for P1 alone, targets recall {} precision {} vs gold, δ = {}; {TS}-row sample; 100 workloads of {TN} rows --",
        T.recall, T.precision, T.delta
    );
    println!("{:<28} {:>9} {:>11} {:>11} {:>11} {:>12}", "learner", "missed", "band calls", "mean recall", "mean prec.", "sample calls");
    let (miss, calls, r, p) = sweep(100, TN, TS, T, naive_thresholds);
    println!("{:<28} {:>8.0}% {:>11.0} {:>11.3} {:>11.3} {:>12}", "naive (fit the sample)", miss * 100.0, calls, r, p, TS);
    match catch_unwind(AssertUnwindSafe(|| sweep(100, TN, TS, T, learn_thresholds))) {
        Ok((miss, calls, r, p)) => println!("{:<28} {:>8.0}% {:>11.0} {:>11.3} {:>11.3} {:>12}", "learned (lane 2)", miss * 100.0, calls, r, p, TS),
        Err(_) => println!("{:<28} {:>9} {:>11} {:>11} {:>11} {:>12}", "learned (lane 2)", "STUB", "-", "-", "-", "-"),
    }
    println!("\n   'missed' = fraction of the 100 workloads where recall or precision vs gold fell below target; must be ≤ δ");
    println!();
}

fn lane3() {
    let w = JoinWorkload::new(77, join::LEFT, join::RIGHT);
    println!("-- lane 3: semantic join, {} × {} = {} pairs --", w.n_left, w.n_right, w.pairs());
    let mut m = Meter::default();
    let gold = join::nested_loop(&w, &mut m);
    let gt = join::gold_true(&w, &gold);
    println!(
        "{:<30} {:>8} {:>7} {:>9} {:>8} {:>8} {:>8} {:>11}",
        "plan", "oracle", "embed", "$ MODEL", "s MODEL", "P gold", "R gold", "R gold∩true"
    );
    println!(
        "{:<30} {:>8} {:>7} {:>9.2} {:>8.0} {:>8.3} {:>8.3} {:>11.3}",
        "nested loop (gold algorithm)", m.oracle_calls, m.proxy_calls, m.dollars, m.seconds, 1.0, 1.0, 1.0
    );
    match catch_unwind(AssertUnwindSafe(|| {
        let mut mb = Meter::default();
        let got = join::blocked_join(&w, &mut mb);
        (mb, join::pair_pr(&got, &gold), join::pair_pr(&got, &gt).1)
    })) {
        Ok((mb, (p, r), rt)) => println!(
            "{:<30} {:>8} {:>7} {:>9.2} {:>8.0} {:>8.3} {:>8.3} {:>11.3}",
            "blocked (lane 3)", mb.oracle_calls, mb.proxy_calls, mb.dollars, mb.seconds, p, r, rt
        ),
        Err(_) => println!(
            "{:<30} {:>8} {:>7} {:>9} {:>8} {:>8} {:>8} {:>11}",
            "blocked (lane 3)", "STUB", "-", "-", "-", "-", "-", "-"
        ),
    }
    let true_pairs = (0..w.n_left).flat_map(|i| (0..w.n_right).map(move |j| (i, j))).filter(|&(i, j)| w.truth(i, j)).count();
    println!(
        "\n   real matches {true_pairs}; gold {} pairs = {} the oracle got right + {} it invented on non-matching pairs",
        gold.len(),
        gt.len(),
        gold.len() - gt.len()
    );
    println!(
        "   sanity: nested-loop seconds = ceil({}/{PARALLELISM}) waves × 0.80 s = {:.0} → {}",
        w.pairs(),
        join::simulated_seconds_nested(&w),
        if (m.seconds - wave_seconds(w.pairs(), llm_query_processing_experiments::model::ORACLE_CHEAP)).abs() < 1e-6 { "exact" } else { "MISMATCH" }
    );
}
