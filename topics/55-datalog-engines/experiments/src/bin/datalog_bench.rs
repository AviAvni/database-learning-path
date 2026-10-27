//! datalog_bench — naive vs semi-naive evaluation priced in derivations, and
//! (stubbed lanes) the same fixpoint over sorted relations and as masked
//! sparse matrix products.
//!
//! cargo run --release --bin datalog_bench

use datalog_experiments::{
    algebra::{sg_algebra, tc_algebra},
    eval::{
        expected_linear_semi_naive_derivations, reach_from, sg, tc_linear, tc_nonlinear, tc_pairs, Mode,
        Nonlinear, Run,
    },
    gen::{sg_datasets, tc_datasets, tc_size_bfs, Dataset},
    sorted::{expected_derivations, tc_sorted, Form},
};
use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
}

/// Best of three runs by the run's own timer; the last result otherwise.
fn best3(mut f: impl FnMut() -> Run) -> Run {
    let mut best: Option<Run> = None;
    for _ in 0..3 {
        let r = black_box(f());
        if best.as_ref().map_or(true, |b| r.stats.ms < b.stats.ms) {
            best = Some(r);
        }
    }
    best.unwrap()
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

fn row(label: &str, r: &Run) {
    let s = &r.stats;
    println!(
        "   {:<34} {:>6} {:>13} {:>11} {:>13} {:>7.2} {:>9.2}",
        label,
        s.iterations,
        s.derivations,
        s.new_tuples,
        s.duplicates(),
        s.derivations as f64 / s.new_tuples.max(1) as f64,
        s.ms
    );
}

fn header(prog: &str) {
    println!(
        "   {:<34} {:>6} {:>13} {:>11} {:>13} {:>7} {:>9}",
        prog, "iters", "derivations", "new", "duplicates", "d/new", "ms"
    );
}

fn main() {
    quiet_stubs();
    println!("=== datalog_bench — topic 55 ===");
    println!("load average (1/5/15 min) at start: {}\n", load_average());

    let tc = tc_datasets();
    let sgd = sg_datasets();
    let mut lane1_tc_ms: Vec<f64> = Vec::new();
    let mut lane1_sg_ms: Vec<f64> = Vec::new();
    lane1(&tc, &sgd, &mut lane1_tc_ms, &mut lane1_sg_ms);
    lane2(&tc);
    lane3(&tc, &sgd, &lane1_tc_ms, &lane1_sg_ms);

    println!("\nload average (1/5/15 min) at end: {}", load_average());
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/sorted.rs (lane 2) and src/algebra.rs (lane 3) to unlock the lanes marked STUB]");
    }
}

fn lane1(tc: &[Dataset], sgd: &[Dataset], tc_ms: &mut Vec<f64>, sg_ms: &mut Vec<f64>) {
    println!("-- lane 1: naive vs semi-naive over hash-indexed relations (provided) --");
    println!("   derivation = one successful rule-body match; new = distinct head tuples not yet known;");
    println!("   duplicates = derivations - new. Best of 3 per row; counts are exact.\n");

    for d in tc {
        let g = &d.graph;
        println!("   transitive closure — {}  (n = {}, arc = {})", d.name, g.n, g.edges.len());
        header("program / mode");
        let naive = best3(|| tc_linear(g, Mode::Naive));
        let semi = best3(|| tc_linear(g, Mode::SemiNaive));
        row("tc linear  naive", &naive);
        row("tc linear  semi-naive", &semi);
        tc_ms.push(semi.stats.ms);
        // Nonlinear TC squares the closure's reach, so it is only run where
        // the graph is small enough for the naive row to finish quickly.
        if g.edges.len() < 4000 {
            let nn = best3(|| tc_nonlinear(g, Nonlinear::Naive));
            let tb = best3(|| tc_nonlinear(g, Nonlinear::TextbookDelta));
            let od = best3(|| tc_nonlinear(g, Nonlinear::OldDelta));
            row("tc nonlinear  naive", &nn);
            row("tc nonlinear  semi-naive, textbook", &tb);
            row("tc nonlinear  semi-naive, old-delta", &od);
            let ok = nn.checksum == semi.checksum && tb.checksum == semi.checksum && od.checksum == semi.checksum;
            println!("   sanity: nonlinear variants and linear agree on size {} and checksum: {}", semi.size, if ok { "agree" } else { "DISAGREE" });
        }
        let oracle = tc_size_bfs(g);
        let pairs = tc_pairs(g);
        let want = expected_linear_semi_naive_derivations(g, &pairs);
        println!(
            "   sanity: |tc| = {} vs BFS oracle {}: {}",
            semi.size,
            oracle,
            if semi.size == oracle && naive.size == oracle { "agree" } else { "DISAGREE" }
        );
        println!(
            "   sanity: semi-naive derivations {} vs |arc| + sum over tc of outdeg(z) = {}: {}",
            semi.stats.derivations,
            want,
            if semi.stats.derivations == want { "exact" } else { "MISMATCH" }
        );
        println!(
            "   sanity: naive/semi-naive checksum {:016x} / {:016x}: {}; semi-naive {:.1} M derivations/s (< 10,000): {}",
            naive.checksum,
            semi.checksum,
            if naive.checksum == semi.checksum { "agree" } else { "DISAGREE" },
            semi.stats.derivations as f64 / semi.stats.ms / 1e3,
            if semi.stats.derivations as f64 / semi.stats.ms / 1e3 < 10_000.0 { "ok" } else { "IMPLAUSIBLE" }
        );
        println!(
            "   naive/semi-naive: derivations {:.1}x, time {:.1}x",
            naive.stats.derivations as f64 / semi.stats.derivations as f64,
            naive.stats.ms / semi.stats.ms
        );
        let goal = best3(|| reach_from(g, 0));
        row("?- tc(0,y)  goal-directed (reach)", &goal);
        println!(
            "   goal-directed from vertex 0: {} of {} closure tuples, {:.1}% of the full semi-naive derivations, {:.1}x faster\n",
            goal.size,
            semi.size,
            100.0 * goal.stats.derivations as f64 / semi.stats.derivations as f64,
            semi.stats.ms / goal.stats.ms
        );
    }

    for d in sgd {
        let g = &d.graph;
        println!("   same generation — {}  (n = {}, arc = {})", d.name, g.n, g.edges.len());
        header("program / mode");
        let naive = best3(|| sg(g, Mode::Naive));
        let semi = best3(|| sg(g, Mode::SemiNaive));
        row("sg  naive", &naive);
        row("sg  semi-naive", &semi);
        sg_ms.push(semi.stats.ms);
        println!(
            "   sanity: naive/semi-naive size {} / {}, checksum {}",
            naive.size,
            semi.size,
            if naive.checksum == semi.checksum { "agree" } else { "DISAGREE" }
        );
        println!(
            "   naive/semi-naive: derivations {:.1}x, time {:.1}x\n",
            naive.stats.derivations as f64 / semi.stats.derivations as f64,
            naive.stats.ms / semi.stats.ms
        );
    }
}

fn lane2(tc: &[Dataset]) {
    println!("-- lane 2: sorted relations, left-, right-linear and per-round adaptive form (exercise) --");
    for d in tc {
        let g = &d.graph;
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut out = Vec::new();
            for form in [Form::Left, Form::Right, Form::Adaptive] {
                let mut best: Option<datalog_experiments::sorted::SortedRun> = None;
                for _ in 0..3 {
                    let t = Instant::now();
                    let mut run = black_box(tc_sorted(g, form));
                    run.stats.ms = t.elapsed().as_secs_f64() * 1e3;
                    if best.as_ref().map_or(true, |b| run.stats.ms < b.stats.ms) {
                        best = Some(run);
                    }
                }
                out.push((form, best.unwrap()));
            }
            out
        }));
        match r {
            Ok(runs) => {
                println!("   {}", d.name);
                println!("   {:<8} {:>6} {:>13} {:>11} {:>12} {:>9}", "form", "iters", "derivations", "new", "probes", "ms");
                for (form, run) in &runs {
                    let s = &run.stats;
                    println!("   {:<8} {:>6} {:>13} {:>11} {:>12} {:>9.2}", format!("{form:?}"), s.iterations, s.derivations, s.new_tuples, s.probes, s.ms);
                }
                let ex = |f| expected_derivations(g, f);
                println!(
                    "   oracle derivations: left {} / right {} / adaptive {}: {}",
                    ex(Form::Left),
                    ex(Form::Right),
                    ex(Form::Adaptive),
                    if runs.iter().all(|(f, r)| r.stats.derivations == ex(*f)) { "all match" } else { "MISMATCH" }
                );
            }
            Err(_) => println!("   {:<20} STUB — sorted relations not implemented", d.name),
        }
    }
    println!();
}

fn lane3(tc: &[Dataset], sgd: &[Dataset], tc_ms: &[f64], sg_ms: &[f64]) {
    println!("-- lane 3: masked sparse matrix product, complement mask, boolean semiring (exercise) --");
    println!("   {:<22} {:>8} {:>6} {:>13} {:>11} {:>9} {:>14}", "program / dataset", "", "iters", "flops", "new", "ms", "lane-1 semi ms");
    for (i, d) in tc.iter().enumerate() {
        let g = &d.graph;
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut best: Option<datalog_experiments::algebra::AlgebraRun> = None;
            for _ in 0..3 {
                let t = Instant::now();
                let mut run = black_box(tc_algebra(g));
                run.stats.ms = t.elapsed().as_secs_f64() * 1e3;
                if best.as_ref().map_or(true, |b| run.stats.ms < b.stats.ms) {
                    best = Some(run);
                }
            }
            best.unwrap()
        }));
        match r {
            Ok(run) => println!(
                "   tc {:<19} {:>8} {:>6} {:>13} {:>11} {:>9.2} {:>14.2}",
                d.name, "", run.stats.iterations, run.stats.flops, run.stats.new_entries, run.stats.ms, tc_ms[i]
            ),
            Err(_) => println!("   tc {:<19} STUB — tc_algebra not implemented", d.name),
        }
    }
    for (i, d) in sgd.iter().enumerate() {
        let g = &d.graph;
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut best: Option<datalog_experiments::algebra::AlgebraRun> = None;
            for _ in 0..3 {
                let t = Instant::now();
                let mut run = black_box(sg_algebra(g));
                run.stats.ms = t.elapsed().as_secs_f64() * 1e3;
                if best.as_ref().map_or(true, |b| run.stats.ms < b.stats.ms) {
                    best = Some(run);
                }
            }
            best.unwrap()
        }));
        match r {
            Ok(run) => println!(
                "   sg {:<19} {:>8} {:>6} {:>13} {:>11} {:>9.2} {:>14.2}",
                d.name, "", run.stats.iterations, run.stats.flops, run.stats.new_entries, run.stats.ms, sg_ms[i]
            ),
            Err(_) => println!("   sg {:<19} STUB — sg_algebra not implemented", d.name),
        }
    }
}
