//! normalise_bench — how often does a fixed-order rewriter fail to reach the
//! cheapest form its own rules allow, and what does it cost an e-graph to find it?
//! (provided lane 1b; self-contained, runs today, does not touch src/eqsat.rs)
//!
//! cargo run --release --bin normalise_bench
//!
//! For N seeded random expressions per depth:
//!   * run the hand-ordered rewriter (hand.rs) to its fixpoint;
//!   * run the SAME rules, unordered, as an egg e-graph (plus a constant-folding
//!     Analysis, because hand.rs's R1 folds constants) and extract the minimum
//!     AstSize term;
//!   * count the expressions where hand's fixpoint costs MORE than egg's minimum
//!     — "failed to normalise" — and the control: the same rewriter with two
//!     rules swapped (R4 before R2).
//! Everything printed except the µs column is a count from a seeded generator,
//! so it reproduces exactly. The e-node column is in topic 44's unit (e-nodes
//! in the e-graph), so the two topics' tables can be read side by side.
//!
//! Caveat that belongs next to the numbers: `div-same` rewrites x/x to 1, which
//! is unsound at x = 0; hand.rs's R5 makes the same assumption. The e-graph
//! then occasionally proves one class equal to both 0 and 1; the analysis keeps
//! the first value and the count is printed. Soundness is not what is measured.

use egg::*;
use formal_experiments::{
    eqsat::{to_rec, Math},
    expr::{cost, gen_expr},
    hand::{hand_optimize, hand_optimize_ordered},
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// How many times the constant analysis saw one class proved to be two different constants.
static DISAGREEMENTS: AtomicUsize = AtomicUsize::new(0);

const SEEDS: u64 = 200;
const ITER_LIMIT: usize = 12;
const NODE_LIMIT: usize = 20_000;

#[derive(Default)]
struct ConstFold;

impl Analysis<Math> for ConstFold {
    type Data = Option<i64>;

    fn make(egraph: &EGraph<Math, Self>, enode: &Math) -> Self::Data {
        let x = |i: &Id| egraph[*i].data;
        match enode {
            Math::Num(n) => Some(*n),
            Math::Add([a, b]) => Some(x(a)?.checked_add(x(b)?)?),
            Math::Mul([a, b]) => Some(x(a)?.checked_mul(x(b)?)?),
            Math::Div([a, b]) => {
                let d = x(b)?;
                if d == 0 {
                    None
                } else {
                    Some(x(a)? / d)
                }
            }
            _ => None,
        }
    }

    fn merge(&mut self, to: &mut Self::Data, from: Self::Data) -> DidMerge {
        egg::merge_option(to, from, |a, b| {
            // x/x -> 1 is unsound at x = 0, so a class can be proved both 0 and 1.
            // Keep the first value and count the event instead of aborting.
            if *a != b {
                DISAGREEMENTS.fetch_add(1, Ordering::Relaxed);
            }
            DidMerge(false, false)
        })
    }

    fn modify(egraph: &mut EGraph<Math, Self>, id: Id) {
        if let Some(n) = egraph[id].data {
            let c = egraph.add(Math::Num(n));
            egraph.union(id, c);
        }
    }
}

fn rules() -> Vec<Rewrite<Math, ConstFold>> {
    vec![
        rewrite!("mul2-shl"; "(* ?x 2)" => "(<< ?x 1)"),
        rewrite!("div-reassoc"; "(/ (* ?x ?y) ?z)" => "(* ?x (/ ?y ?z))"),
        rewrite!("div-same"; "(/ ?x ?x)" => "1"),
        rewrite!("div-one"; "(/ ?x 1)" => "?x"),
        rewrite!("mul-one"; "(* ?x 1)" => "?x"),
        rewrite!("mul-zero"; "(* ?x 0)" => "0"),
        rewrite!("add-zero"; "(+ ?x 0)" => "?x"),
        rewrite!("mul-comm"; "(* ?x ?y)" => "(* ?y ?x)"),
        rewrite!("add-comm"; "(+ ?x ?y)" => "(+ ?y ?x)"),
    ]
}

struct Run {
    best_cost: usize,
    /// e-nodes after each iteration (index 0 = the seed graph).
    growth: Vec<usize>,
    stop: String,
}

fn saturate(e: &formal_experiments::expr::Expr) -> Run {
    let rec = to_rec(e);
    let runner = Runner::<Math, ConstFold, ()>::default()
        .with_iter_limit(ITER_LIMIT)
        .with_node_limit(NODE_LIMIT)
        .with_time_limit(std::time::Duration::from_secs(600))
        .with_expr(&rec)
        .run(&rules());
    let ex = Extractor::new(&runner.egraph, AstSize);
    let (c, _) = ex.find_best(runner.roots[0]);
    let mut growth = vec![rec.as_ref().len()];
    growth.extend(runner.iterations.iter().map(|i| i.egraph_nodes));
    Run {
        best_cost: c,
        growth,
        stop: format!("{:?}", runner.stop_reason.as_ref().unwrap()),
    }
}

fn main() {
    println!("=== normalise_bench: {SEEDS} seeded expressions per depth ===");
    println!("rules: 9 unordered rewrites + constant folding; limits: {ITER_LIMIT} iterations, {NODE_LIMIT} e-nodes\n");
    println!(
        "{:>5} {:>7} | {:>9} {:>9} {:>9} | {:>11} {:>11} | {:>8} {:>10} {:>9} | stop reasons",
        "depth", "in", "hand", "hand-R4-1", "egg min", "hand fails", "swap fails", "saturat.", "max enodes", "egg µs"
    );
    let mut swap_total = 0usize;
    let mut growth_depth4: Vec<Vec<usize>> = Vec::new();
    for depth in [2usize, 3, 4, 5, 6] {
        let (mut in_c, mut hand_c, mut swap_c, mut egg_c) = (0usize, 0usize, 0usize, 0usize);
        let (mut hand_fail, mut swap_fail, mut saturated, mut max_nodes) = (0usize, 0usize, 0usize, 0usize);
        let mut swap_differs = 0usize;
        let mut stops: Vec<(String, usize)> = Vec::new();
        let t0 = Instant::now();
        for s in 0..SEEDS {
            let e = gen_expr(depth, 1000 + s);
            let (h, _) = hand_optimize(&e);
            let (w, _) = hand_optimize_ordered(&e, true);
            let r = saturate(&e);
            in_c += cost(&e);
            hand_c += cost(&h);
            swap_c += cost(&w);
            egg_c += r.best_cost;
            // egg's extraction is the minimum over every form it reached; if it
            // did not saturate, that is a bound, not the optimum — still never
            // worse than hand here because every hand rule is an egg rule.
            assert!(r.best_cost <= cost(&h), "egg worse than hand at depth {depth} seed {s}");
            hand_fail += (cost(&h) > r.best_cost) as usize;
            swap_fail += (cost(&w) > r.best_cost) as usize;
            swap_differs += (w != h) as usize;
            if r.stop == "Saturated" {
                saturated += 1;
            }
            max_nodes = max_nodes.max(*r.growth.last().unwrap());
            match stops.iter_mut().find(|(k, _)| *k == r.stop) {
                Some((_, n)) => *n += 1,
                None => stops.push((r.stop.clone(), 1)),
            }
            if depth == 4 && s < 20 {
                growth_depth4.push(r.growth);
            }
        }
        let us = t0.elapsed().as_secs_f64() * 1e6 / SEEDS as f64;
        stops.sort();
        let stops: Vec<String> = stops.iter().map(|(k, n)| format!("{k}={n}")).collect();
        let n = SEEDS as usize;
        swap_total += swap_differs;
        println!(
            "{:>5} {:>7.1} | {:>9.1} {:>9.1} {:>9.1} | {:>5}/{:<5} {:>5}/{:<5} | {:>8} {:>10} {:>9.0} | {}",
            depth,
            in_c as f64 / n as f64,
            hand_c as f64 / n as f64,
            swap_c as f64 / n as f64,
            egg_c as f64 / n as f64,
            hand_fail,
            n,
            swap_fail,
            n,
            saturated,
            max_nodes,
            us,
            stops.join(" ")
        );
    }

    println!(
        "\nrule-order control: swapping R4 ahead of R2 changed the rewriter's output on {swap_total} of {} expressions",
        SEEDS as usize * 5
    );
    {
        use formal_experiments::expr::{div, mul, num, var};
        let trap = div(mul(var("a"), num(2)), num(2));
        let (t, _) = hand_optimize_ordered(&trap, true);
        println!("  and on the trap (a*2)/2 it still returns cost {} ({:?})", cost(&t), t);
    }
    println!("\nconstant-analysis disagreements (class proved both 0 and 1 via x/x): {}", DISAGREEMENTS.load(Ordering::Relaxed));

    // e-graph size growth per iteration, depth 4, mean over the first 20 seeds
    println!("\n-- e-graph growth per iteration (depth 4, mean e-nodes over 20 seeds) --");
    println!("{:>5} {:>10} {:>8}", "iter", "e-nodes", "x prev");
    let max_it = growth_depth4.iter().map(|g| g.len()).max().unwrap();
    let mut prev = 0.0f64;
    for it in 0..max_it {
        // a run that stopped early keeps its final size
        let mean: f64 = growth_depth4
            .iter()
            .map(|g| *g.get(it).unwrap_or(g.last().unwrap()) as f64)
            .sum::<f64>()
            / growth_depth4.len() as f64;
        if it == 0 {
            println!("{:>5} {:>10.1} {:>8}", "seed", mean, "-");
        } else {
            println!("{:>5} {:>10.1} {:>7.2}x", it, mean, mean / prev);
        }
        prev = mean;
    }
}
