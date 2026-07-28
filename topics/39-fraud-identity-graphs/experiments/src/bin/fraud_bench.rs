use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use fraud_experiments::er::{
    candidate_pairs, generate_records, link, naive_pair_count, pair_precision_recall,
};
use fraud_experiments::fraudar::{f_measure, fraudar, Weighting};
use fraud_experiments::review_graph::{
    degree_rank_precision, fraud_instance, obscurity_rank_precision, seeded_rng, FraudConfig,
};

fn bench_cfg(camo: f64) -> FraudConfig {
    FraudConfig {
        n_users: 5_000,
        n_objects: 5_000,
        background_edges: 50_000,
        block_users: 25,
        block_objects: 100,
        block_density: 1.0,
        camo_ratio: camo,
    }
}

/// Lane 1 (PROVIDED): camouflage kills row-based suspicion scores.
fn lane1_camouflage() {
    println!("== lane 1: camouflage vs naive rankers — precision@|fraud users| ==");
    println!("   (5000x5000 Zipf background, 50k edges; 25x100 block at 1.0 density)");
    println!("   camo/fraud edge   degree-rank   obscurity-rank");
    let mut rng = seeded_rng(42);
    for camo in [0.0, 0.5, 1.0, 2.0] {
        let g = fraud_instance(&mut rng, &bench_cfg(camo));
        let d = degree_rank_precision(&g);
        let o = obscurity_rank_precision(&g);
        println!("   {camo:>4.1}              {d:>5.2}          {o:>5.2}");
    }
    println!();
    println!("   the two row heuristics fail in opposite regimes: degree ranking");
    println!("   misses economical fraud (honest power users out-review it) but");
    println!("   lights up once camouflage inflates the fraudster's row; obscurity");
    println!("   ranking (\"active account, only unpopular products\") is the");
    println!("   mirror image — it works until the fraudster buys camouflage");
    println!("   edges to popular products. Both are row scores, the fraudster");
    println!("   controls his row, and he tunes camo to slip between them.");
    println!("   fraudar.rs scores columns instead — which he cannot touch.");
    println!();
}

/// Lane 2 (needs fraudar.rs): column-weighted peeling is camouflage-
/// resistant, and near-linear.
fn lane2_fraudar() {
    println!("== lane 2: FRAUDAR greedy peeling — F-measure on the planted block ==");
    println!("   camo/fraud edge   unweighted g   log-weighted g");
    let mut rng = seeded_rng(7);
    for camo in [0.0, 0.5, 1.0, 2.0] {
        let g = fraud_instance(&mut rng, &bench_cfg(camo));
        let unw = f_measure(&fraudar(&g, Weighting::Unweighted), &g);
        let log = f_measure(&fraudar(&g, Weighting::LogDegree), &g);
        println!("   {camo:>4.1}              {unw:>5.2}          {log:>5.2}");
    }
    println!();

    // Throughput: near-linear peeling on a bigger graph.
    let mut rng = seeded_rng(11);
    let big = FraudConfig {
        n_users: 100_000,
        n_objects: 50_000,
        background_edges: 1_000_000,
        block_users: 50,
        block_objects: 200,
        block_density: 1.0,
        camo_ratio: 1.0,
    };
    let g = fraud_instance(&mut rng, &big);
    let t = Instant::now();
    let det = fraudar(&g, Weighting::LogDegree);
    let dt = t.elapsed().as_secs_f64();
    println!(
        "   100k x 50k nodes, {} edges: peel everything in {:.2} s (F = {:.2})",
        g.edges.len(),
        dt,
        f_measure(&det, &g)
    );
    println!();
    println!("   the fraud block's columns never receive camouflage, so their");
    println!("   1/log(deg+5) weights never change — Theorem 3's resistance.");
    println!();
}

/// Lane 3 (needs er.rs): Fellegi-Sunter linkage — blocking savings, EM,
/// and the linked identity clusters.
fn lane3_er() {
    println!("== lane 3: entity resolution — Fellegi-Sunter with blocking + EM ==");
    let mut rng = seeded_rng(13);
    let records = generate_records(&mut rng, 5_000, 3);
    let naive = naive_pair_count(records.len());
    let t = Instant::now();
    let pairs = candidate_pairs(&records);
    let (clusters, fs) = link(&mut rng, &records, 12.0);
    let dt = t.elapsed().as_secs_f64();
    let (precision, recall) = pair_precision_recall(&records, &clusters);
    println!(
        "   {} records (5000 entities x 3): naive {} pairs -> blocked {} ({}x fewer)",
        records.len(),
        naive,
        pairs.len(),
        naive / pairs.len().max(1)
    );
    println!(
        "   EM fit (unlabeled): p = {:.3}, m = [{:.2} {:.2} {:.2} {:.2} {:.2}]",
        fs.p, fs.m[0], fs.m[1], fs.m[2], fs.m[3], fs.m[4]
    );
    println!(
        "   sampled u          = [{:.4} {:.4} {:.4} {:.4} {:.4}]",
        fs.u[0], fs.u[1], fs.u[2], fs.u[3], fs.u[4]
    );
    println!(
        "   link at 12.0 bits: pair precision {precision:.3}, recall {recall:.3}, end-to-end {:.0} ms",
        dt * 1e3
    );
    println!();
    println!("   two blocking passes (last name OR dob) so one typo cannot hide");
    println!("   a duplicate; u comes from random full-space pairs (splink's");
    println!("   estimate_u_using_random_sampling), EM fits p and m on the");
    println!("   unlabeled blocked pairs; union-find merges above threshold.");
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce()) {
    // Silence the default panic hook for the duration of the lane: an
    // unimplemented exercise is an expected state, not a crash to report.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    if r.is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_camouflage();
    stub_lane("lane 2", lane2_fraudar);
    stub_lane("lane 3", lane3_er);
}
