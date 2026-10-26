//! semijoin_bench — a bad join order, priced in tuples, and three ways to
//! make it harmless.
//!
//! cargo run --release --bin semijoin_bench

use semijoin_experiments::{
    bitmap::{bitmap_cascade, CascadeReport, Pushdown},
    bloom::{Bloom, RegisterBlocked},
    gen::{social_graph, GraphParams, BAD, GOOD},
    join::{left_deep, PlanRun},
    rel::Query,
    transfer::{bloom_transfer, TransferReport},
    yannakakis::{full_reduce, JoinTree, Reduction},
};
use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
}

/// Best of three wall-clock runs, in milliseconds, and the last result.
fn best3<R>(mut f: impl FnMut() -> R) -> (f64, R) {
    let mut best = f64::MAX;
    let mut out = None;
    for _ in 0..3 {
        let t = Instant::now();
        let r = black_box(f());
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        out = Some(r);
    }
    (best, out.unwrap())
}

fn load_average() -> String {
    let mut l = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 doubles into the array we pass.
    let n = unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
    if n == 3 {
        format!("{:.2} {:.2} {:.2}", l[0], l[1], l[2])
    } else {
        "unavailable".into()
    }
}

fn route(q: &Query, order: &[usize]) -> String {
    order.iter().map(|&i| q.rels[i].name).collect::<Vec<_>>().join("→")
}

fn main() {
    quiet_stubs();
    println!("=== semijoin_bench — topic 47 ===");
    println!("load average (1/5/15 min) at start: {}\n", load_average());

    let q = social_graph(GraphParams::LANE1);
    let tree = JoinTree::largest_root(&q.rels);
    let red = lane1(&q, &tree);
    lane1b();
    lane2(&q, &tree, &red);
    lane3(&q);

    println!("\nload average (1/5/15 min) at end: {}", load_average());
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/transfer.rs (lane 2) and src/bitmap.rs (lane 3) to unlock the lanes marked STUB]");
    }
}

/// Lane 1b: the filter itself. Same bit budget, two layouts, one million
/// absent keys each. Keys are i·0x9E3779B1 (a bijection on u32), so the
/// inserted set and the probed set are disjoint by construction.
fn lane1b() {
    println!("\n-- lane 1b: the filter itself — false positives per absent key, equal bit budgets --");
    println!(
        "   {:>7} {:<34} {:>8} {:>9} {:>3} {:>13} {:>10}",
        "keys", "filter", "bits", "bits/key", "k", "measured FPR", "formula"
    );
    const PROBES: u32 = 1_000_000;
    let key = |i: u32| i.wrapping_mul(0x9E37_79B1);
    for n in [1_000u32, 10_000, 100_000] {
        let mut rb = RegisterBlocked::new(n as usize);
        let bpk = rb.bits() as f64 / n as f64;
        let mut cl = Bloom::with_bits_per_key(n as usize, bpk);
        for i in 0..n {
            rb.insert(key(i));
            cl.insert(key(i));
        }
        let absent = |f: &dyn Fn(u32) -> bool| (n..n + PROBES).filter(|&i| f(key(i))).count() as f64 / PROBES as f64;
        let fp_cl = absent(&|k| black_box(&cl).contains(k));
        let fp_rb = absent(&|k| black_box(&rb).contains(k));
        println!(
            "   {:>7} {:<34} {:>8} {:>9.2} {:>3} {:>13.5} {:>10.5}",
            n, "classic Bloom", cl.bits(), cl.bits() as f64 / n as f64, cl.k(), fp_cl, cl.theoretical_fpr()
        );
        println!(
            "   {:>7} {:<34} {:>8} {:>9.2} {:>3} {:>13.5} {:>10}",
            n, "register-blocked (DuckDB layout)", rb.bits(), bpk, 4, fp_rb, "—"
        );
    }
}

fn row(label: &str, r: &PlanRun, ms: f64) {
    let js: Vec<String> = r.join_sizes.iter().map(|s| format!("{s:>10}")).collect();
    println!(
        "   {:<38}{} {:>14} {:>8.2} {:>11} {:>9.2}",
        label,
        js.join(""),
        r.intermediates(),
        r.intermediates() as f64 / r.out as f64,
        r.build_rows,
        ms
    );
}

fn lane1(q: &Query, tree: &JoinTree) -> Reduction {
    println!("-- lane 1: one bad join order, before and after a full Yannakakis reduction --");
    println!("   MATCH (a)-[:KNOWS]->(b)-[:LIKES]->(m)-[:HAS_TAG]->(t:Tag) WHERE t.class = 'rare' RETURN count(*)");
    let n = q.total_rows();
    println!(
        "   knows {} · likes {} · has_tag {} · rare_tag {} (1% of {} tags)   N = {} input rows",
        q.rels[0].len(),
        q.rels[1].len(),
        q.rels[2].len(),
        q.rels[3].len(),
        GraphParams::LANE1.tags,
        n
    );
    println!("   join tree rooted at {} (the largest relation)\n", q.rels[tree.root].name);

    let rels = black_box(&q.rels);
    let (ms_good, good) = best3(|| left_deep(rels, q.nvars(), &GOOD));
    let (ms_bad, bad) = best3(|| left_deep(rels, q.nvars(), &BAD));
    let (ms_red, red) = best3(|| full_reduce(rels, tree));
    let (ms_rgood, rgood) = best3(|| left_deep(&red.rels, q.nvars(), &GOOD));
    let (ms_rbad, rbad) = best3(|| left_deep(&red.rels, q.nvars(), &BAD));
    let out = good.out;

    println!("   OUT = {out} output rows\n");
    println!(
        "   {:<38}{:>10}{:>10}{:>10} {:>14} {:>8} {:>11} {:>9}",
        "left-deep plan", "join 1", "join 2", "join 3", "intermediates", "× OUT", "build rows", "ms"
    );
    row(&format!("good  {}", route(q, &GOOD)), &good, ms_good);
    row(&format!("bad   {}", route(q, &BAD)), &bad, ms_bad);
    row("good, after full reduction", &rgood, ms_rgood);
    row("bad,  after full reduction", &rbad, ms_rbad);

    let (f, b) = (red.forward, red.backward);
    println!(
        "\n   reduction (6 semi-joins): forward {} inserts + {} probes, backward {} + {}; total {} hash ops = {:.2} × N, {:.2} ms",
        f.inserts,
        f.probes,
        b.inserts,
        b.probes,
        f.total() + b.total(),
        (f.total() + b.total()) as f64 / n as f64,
        ms_red
    );
    println!("\n   {:<10} {:>10} {:>14} {:>14}", "relation", "base", "after fwd", "after both");
    for i in 0..q.rels.len() {
        println!(
            "   {:<10} {:>10} {:>14} {:>14}",
            q.rels[i].name,
            q.rels[i].len(),
            red.after_forward[i],
            red.rels[i].len()
        );
    }

    let ratio_before = bad.intermediates() as f64 / good.intermediates() as f64;
    let ratio_after = rbad.intermediates() as f64 / rgood.intermediates() as f64;
    println!(
        "\n   bad ÷ good intermediates: {:.1}× before the reduction, {:.2}× after",
        ratio_before, ratio_after
    );
    println!(
        "   bad order + reduction: {} hash ops + {} intermediates = {} — vs {} for the bad order alone ({:.1}× less)",
        f.total() + b.total(),
        rbad.intermediates(),
        f.total() + b.total() + rbad.intermediates(),
        bad.intermediates(),
        bad.intermediates() as f64 / (f.total() + b.total() + rbad.intermediates()) as f64
    );

    // sanity
    let same = [&bad, &rgood, &rbad].iter().all(|r| r.out == out && r.checksum == good.checksum);
    println!(
        "\n   sanity: four plans, one answer — OUT {} and checksum {:016x} in all four: {}",
        out,
        good.checksum,
        if same { "agree" } else { "DISAGREE" }
    );
    let bounded = rgood.join_sizes.iter().chain(&rbad.join_sizes).all(|&s| s <= out);
    println!(
        "   sanity: after reduction every join emits ≤ OUT ({}): {}",
        out,
        if bounded { "holds" } else { "VIOLATED" }
    );
    let tput = bad.intermediates() as f64 / (ms_bad / 1e3);
    println!(
        "   sanity: bad plan materialised {:.0} M tuples/s — below 1,000 M/s, so the timer saw real work: {}",
        tput / 1e6,
        if tput < 1e9 { "ok" } else { "SUSPECT" }
    );
    assert!(same && bounded, "lane 1 sanity check failed");
    red
}

fn lane2(q: &Query, tree: &JoinTree, red: &Reduction) {
    println!("\n-- lane 2: predicate transfer — the same two passes with Bloom filters --");
    let exact_total: usize = red.rels.iter().map(|r| r.len()).sum();
    println!("   exact reduction keeps {exact_total} rows in total\n");
    println!(
        "   {:>8} {:>10} {:>11} {:>10} {:>10} {:>16} {:>18} {:>9}",
        "bits/key", "survivors", "not exact", "filter FP", "inherited", "worst FPR meas.", "bad-order interm.", "ms"
    );
    let mut detail = None;
    for bpk in [2.0, 4.0, 8.0, 16.0] {
        let r = catch_unwind(AssertUnwindSafe(|| best3(|| bloom_transfer(&q.rels, tree, bpk, &red.rels))));
        match r {
            Ok((ms, rep)) => {
                print_transfer(q, bpk, &rep, exact_total, ms);
                if bpk == 8.0 {
                    detail = Some(rep);
                }
            }
            Err(_) => {
                println!("   {bpk:>8}   STUB");
            }
        }
    }
    let Some(rep) = detail else { return };
    println!("\n   detail at 8 bits/key — filters in the order they were built:");
    println!(
        "   {:<4} {:<22} {:>6} {:>8} {:>3} {:>10} {:>9} {:>9} {:>12} {:>9}",
        "pass", "from → to", "keys", "bits", "k", "neg. keys", "FP keys", "FP rows", "FPR (keys)", "theory"
    );
    for f in &rep.filters {
        println!(
            "   {:<4} {:<22} {:>6} {:>8} {:>3} {:>10} {:>9} {:>9} {:>12.4} {:>9.4}",
            f.pass,
            format!("{} → {}", f.from, f.to),
            f.keys,
            f.bits,
            f.k,
            f.negative_keys,
            f.fp_keys,
            f.false_positives,
            f.measured_fpr(),
            f.theory_fpr
        );
    }
    println!("\n   {:<10} {:>8} {:>8} {:>9} {:>10} {:>10}", "relation", "base", "exact", "survived", "filter FP", "inherited");
    for r in &rep.per_rel {
        println!(
            "   {:<10} {:>8} {:>8} {:>9} {:>10} {:>10}",
            r.name, r.rows_before, r.exact, r.survived, r.filter_fp, r.inherited
        );
    }
}

fn print_transfer(q: &Query, bpk: f64, rep: &TransferReport, exact_total: usize, ms: f64) {
    let surv: usize = rep.per_rel.iter().map(|r| r.survived).sum();
    let ffp: usize = rep.per_rel.iter().map(|r| r.filter_fp).sum();
    let inh: usize = rep.per_rel.iter().map(|r| r.inherited).sum();
    let worst = rep
        .filters
        .iter()
        .filter(|f| f.negative_keys >= 1000)
        .map(|f| (f.measured_fpr(), f.theory_fpr))
        .fold((0.0f64, 0.0f64), |a, b| if b.0 > a.0 { b } else { a });
    let bad = left_deep(&rep.rels, q.nvars(), &BAD);
    println!(
        "   {:>8} {:>10} {:>11} {:>10} {:>10} {:>8.4} ({:.4}) {:>18} {:>9.2}",
        bpk,
        surv,
        surv - exact_total,
        ffp,
        inh,
        worst.0,
        worst.1,
        bad.intermediates(),
        ms
    );
}

fn lane3(q: &Query) {
    println!("\n-- lane 3: no reduction phase — bitmaps pushed from each build side into its probe subtree --");
    println!(
        "   {:<40} {:<18} {:>9} {:>9} {:>9} {:>9} {:>11} {:>13} {:>14} {:>8}",
        "plan", "pushdown", "scan 0", "build 1", "build 2", "build 3", "build rows", "bitmap probes", "intermediates", "ms"
    );
    for (name, order) in [("good", GOOD), ("bad", BAD)] {
        for (mname, mode) in [("probe spine only", Pushdown::ProbeSpineOnly), ("into lower builds", Pushdown::IntoLowerBuilds)] {
            let label = format!("{name}  {}", route(q, &order));
            let r = catch_unwind(AssertUnwindSafe(|| best3(|| bitmap_cascade(&q.rels, q.nvars(), &order, mode))));
            match r {
                Ok((ms, c)) => print_cascade(&label, mname, &c, ms),
                Err(_) => println!("   {label:<40} {mname:<18} STUB"),
            }
        }
    }
}

fn print_cascade(label: &str, mode: &str, c: &CascadeReport, ms: f64) {
    println!(
        "   {:<40} {:<18} {:>9} {:>9} {:>9} {:>9} {:>11} {:>13} {:>14} {:>8.2}",
        label,
        mode,
        c.scanned[0],
        c.scanned[1],
        c.scanned[2],
        c.scanned[3],
        c.build_rows,
        c.bitmap_probes,
        c.intermediates(),
        ms
    );
}
