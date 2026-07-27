use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use opsgraph_experiments::rca::{random_walk_rca, sherlock_single_fault, top_k_hit, Ranking};
use opsgraph_experiments::sampling::{
    edge_recall, mean_latency_us, p99_latency_us, rare_path_recall, sample,
};
use opsgraph_experiments::services::{
    all_edges, configured_edges, distinct_paths, failure_correlation, rank_by_error_rate,
    rank_by_failures, rank_of, run_workload, seeded_rng, topology, Topology, TopologyConfig,
    Workload,
};

fn setup(seed: u64) -> (Topology, Workload, TopologyConfig) {
    let cfg = TopologyConfig::default();
    let mut rng = seeded_rng(seed);
    let t = topology(&mut rng, &cfg);
    let w = run_workload(&mut rng, &t, &cfg);
    (t, w, cfg)
}

/// Lane 1 (PROVIDED): the alert storm, and why the dashboard lies.
fn lane1_storm() {
    println!("== lane 1: one broken service, thirty-four alerts ==");
    let (t, w, cfg) = setup(1);
    println!(
        "   {} services ({} frontends, {} infra), {} requests, {} configured edges",
        t.n_services,
        t.frontends.len(),
        t.infra.len(),
        cfg.n_requests,
        configured_edges(&t).len()
    );
    println!(
        "   the broken service is {} — SLOW on {:.0}% of calls, not failing\n",
        t.name(t.root_cause),
        cfg.fault_severity * 100.0
    );

    let alerting = w.alerting(0.05);
    println!(
        "   services alerting above a 5% error rate: {} of {}",
        alerting.len(),
        t.n_services
    );
    println!(
        "   is the broken service among them? {}",
        if alerting.contains(&t.root_cause) { "yes" } else { "NO" }
    );
    println!(
        "   its own error rate: {:.4}  (baseline is {:.4})\n",
        w.error_rate(t.root_cause),
        cfg.baseline_error
    );

    let by_count = rank_by_failures(&w);
    let by_rate = rank_by_error_rate(&w);
    println!("   ranking                top 3                                  rank of the cause");
    println!(
        "   by failure count       {:<38} {} of {}",
        by_count
            .iter()
            .take(3)
            .map(|&(s, v)| format!("{}({})", t.name(s), v as u64))
            .collect::<Vec<_>>()
            .join(" "),
        rank_of(&by_count, t.root_cause),
        t.n_services
    );
    println!(
        "   by error rate          {:<38} {} of {}",
        by_rate
            .iter()
            .take(3)
            .map(|&(s, v)| format!("{}({:.2})", t.name(s), v))
            .collect::<Vec<_>>()
            .join(" "),
        rank_of(&by_rate, t.root_cause),
        t.n_services
    );
    println!();
    println!("   and every infra leaf looks identical from per-node statistics:");
    for &i in &t.infra {
        println!(
            "     {:<9} error rate {:.4}, {} callers{}",
            t.name(i),
            w.error_rate(i),
            t.rdeps[i as usize].len(),
            if i == t.root_cause { "   <-- the broken one" } else { "" }
        );
    }
    println!();
    println!("   This is a gray failure: the component's own health check is");
    println!("   green because it is slow rather than wrong, and the errors are");
    println!("   generated one hop ABOVE it by callers that time out. No sorting");
    println!("   of a per-service dashboard can find it — the five infra leaves");
    println!("   are statistically indistinguishable. Only the graph separates");
    println!("   them, which is what lane 2 is for.");
    println!();
}

/// Lane 2 (needs rca.rs): localization on the graph.
fn lane2_rca() {
    println!("== lane 2: root-cause localization ==");
    println!("   seed   method                     rank of cause   top-1   top-3");
    let mut agg: Vec<(String, usize, usize, usize)> = vec![
        ("failure count".into(), 0, 0, 0),
        ("error rate".into(), 0, 0, 0),
        ("random walk".into(), 0, 0, 0),
        ("Sherlock k=1".into(), 0, 0, 0),
    ];
    let seeds = [1u64, 7, 13, 21, 33];
    for &seed in &seeds {
        let (t, w, cfg) = setup(seed);
        let corr = failure_correlation(&t, &w);
        let mut rng = seeded_rng(99);
        let rankings: Vec<(usize, Ranking)> = vec![
            (0, rank_by_failures(&w)),
            (1, rank_by_error_rate(&w)),
            (2, random_walk_rca(&mut rng, &t, &corr, 200_000, 0.15, false)),
            (3, sherlock_single_fault(&t, &w, cfg.propagation)),
        ];
        for (i, r) in &rankings {
            let rk = rank_of(r, t.root_cause);
            agg[*i].1 += rk;
            if top_k_hit(r, t.root_cause, 1) {
                agg[*i].2 += 1;
            }
            if top_k_hit(r, t.root_cause, 3) {
                agg[*i].3 += 1;
            }
        }
    }
    for (name, sum, t1, t3) in &agg {
        println!(
            "   mean   {name:<24}   {:>13.1}   {}/{}   {}/{}",
            *sum as f64 / seeds.len() as f64,
            t1,
            seeds.len(),
            t3,
            seeds.len()
        );
    }
    println!();

    // Cost, and the ablation.
    let (t, w, cfg) = setup(1);
    let corr = failure_correlation(&t, &w);
    let mut rng = seeded_rng(99);
    let s = Instant::now();
    let full = random_walk_rca(&mut rng, &t, &corr, 200_000, 0.15, false);
    let walk_ms = s.elapsed().as_secs_f64() * 1e3;
    let mut rng = seeded_rng(99);
    let back = random_walk_rca(&mut rng, &t, &corr, 200_000, 0.15, true);
    let s = Instant::now();
    let sher = sherlock_single_fault(&t, &w, cfg.propagation);
    let sher_ms = s.elapsed().as_secs_f64() * 1e3;
    println!(
        "   200k-step walk {walk_ms:.1} ms (rank {}), backward-only variant rank {}",
        rank_of(&full, t.root_cause),
        rank_of(&back, t.root_cause)
    );
    println!(
        "   Sherlock k=1 over {} candidates x {} frontends: {sher_ms:.1} ms (rank {})",
        t.n_services,
        t.frontends.len(),
        rank_of(&sher, t.root_cause)
    );
    println!();
    println!("   Both graph methods find what no per-node ranking can, and they");
    println!("   do it differently: the walk needs no model of how failure");
    println!("   propagates, only a correlation signal and the topology;");
    println!("   Sherlock's Ferret needs the model but then explains the whole");
    println!("   observation vector at once. Ferret's real trick is Observation");
    println!("   3.1 — 'it is very likely that at any point in time only a few");
    println!("   root-cause nodes are troubled or down' — which cuts 3^r");
    println!("   assignment vectors to at most (2r)^k, vanishingly lossy by k=4.");
    println!();
}

/// Lane 3 (needs sampling.rs): what a sampling rate buys and costs.
fn lane3_sampling() {
    println!("== lane 3: Dapper sampling — the aggregate question vs the rare one ==");
    let (t, w, _) = setup(1);
    let truth: HashSet<(u32, u32)> = w
        .traces
        .iter()
        .flat_map(|tr| tr.edges.iter().copied())
        .collect();
    let paths = distinct_paths(&w.traces);
    let rare = paths.values().filter(|&&c| c <= 2).count();
    println!(
        "   {} traces, {} reachable edges ({} configured), {} distinct paths of which {} are rare",
        w.traces.len(),
        all_edges(&t).len(),
        configured_edges(&t).len(),
        paths.len(),
        rare
    );
    println!(
        "   full-trace mean latency {:.0} us, p99 {} us\n",
        mean_latency_us(&w.traces),
        p99_latency_us(&w.traces)
    );
    println!("   rate      traces   edge recall   rare-path recall   mean-latency err   p99 err");
    for denom in [1u32, 4, 16, 64, 256, 1024] {
        let rate = 1.0 / denom as f64;
        let mut rng = seeded_rng(11);
        let s = sample(&mut rng, &w.traces, rate);
        let er = edge_recall(&s, &truth);
        let rr = rare_path_recall(&w.traces, &s, 2);
        let me = (mean_latency_us(&s) - mean_latency_us(&w.traces)).abs()
            / mean_latency_us(&w.traces);
        let pe = if p99_latency_us(&w.traces) > 0 {
            (p99_latency_us(&s) as f64 - p99_latency_us(&w.traces) as f64).abs()
                / p99_latency_us(&w.traces) as f64
        } else {
            0.0
        };
        println!(
            "   1/{denom:<6} {:>7}   {er:>11.3}   {rr:>16.3}   {:>15.1}%   {:>5.1}%",
            s.len(),
            me * 100.0,
            pe * 100.0
        );
    }
    println!();
    println!("   Two questions, two answers. \"What is the dependency graph?\" is");
    println!("   an aggregate question and it saturates almost immediately —");
    println!("   every edge is exercised constantly, so a tiny sample finds them");
    println!("   all. \"What happened on that one weird path?\" is a rare-event");
    println!("   question and its recall falls roughly linearly with the rate.");
    println!("   Dapper shipped 1/1024 and justified it exactly this way: \"if a");
    println!("   notable execution pattern surfaces once in such systems, it");
    println!("   will surface thousands of times\" — while noting in the same");
    println!("   breath that low-volume services must trace every request.");
    println!("   Their measured cost of NOT sampling on a web-search cluster:");
    println!("   +16.3% latency at 1/1, +2.12% at 1/16, -0.20% (inside error)");
    println!("   at 1/1024.");
    println!();
    println!("   One caveat on the first column, stated rather than glossed:");
    println!("   edge recall hits 1.000 even at 39 traces because in THIS");
    println!("   topology almost every request touches almost every edge. A real");
    println!("   service graph has far more path diversity, so the curve bends");
    println!("   sooner — exercise 5 adds skew and finds where. The shape of the");
    println!("   result survives; the exact rate does not.");
    println!();
    println!("   Note also which metrics survive. The mean is unbiased under");
    println!("   sampling; the p99 is made of the tail, and at 1/1024 the tail");
    println!("   is a handful of traces.");
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_storm();
    stub_lane("lane 2", lane2_rca);
    stub_lane("lane 3", lane3_sampling);
}
