//! ivm_bench — the recompute-vs-incremental gap, measured. Full
//! recompute lanes are PROVIDED (the enemy, priced); incremental lanes
//! run the stubs and degrade gracefully until implemented.

use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;
use streaming_experiments::djoin::{join_oracle, IncrementalJoin};
use streaming_experiments::graph::{bfs_reachable, count_triangles, gen_edges, ChurnGen, Edge};
use streaming_experiments::reach::SemiNaiveReach;
use streaming_experiments::tri::IncrementalTriangles;
use streaming_experiments::zset::ZSet;

const N: u32 = 50_000;
const M: usize = 500_000;
const BATCHES: usize = 10;
const BATCH_INS: usize = 90;
const BATCH_DEL: usize = 10;

fn main() {
    let base = gen_edges(N, M, 42);
    println!("graph: {} nodes, {} edges; {} batches of +{}/-{}", N, M, BATCHES, BATCH_INS, BATCH_DEL);

    // pre-generate the churn stream so every lane sees identical batches
    let mut gen = ChurnGen::new(&base, N, 7);
    let batches: Vec<ZSet<Edge>> = (0..BATCHES).map(|_| gen.next_batch(BATCH_INS, BATCH_DEL)).collect();

    // --- provided: full recompute per batch (triangles) ---
    let mut g = base.clone();
    let t0 = Instant::now();
    let mut counts = Vec::new();
    for d in &batches {
        g = g.merge(d);
        counts.push(count_triangles(&g));
    }
    let full_tri = t0.elapsed();
    println!(
        "triangles, full recompute: {:>10.1} ms/batch   (count after last batch: {})",
        full_tri.as_secs_f64() * 1000.0 / BATCHES as f64,
        counts.last().unwrap()
    );

    // --- stub: incremental triangles ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut inc = IncrementalTriangles::new();
        let t = Instant::now();
        inc.apply(&base);
        let init = t.elapsed();
        let t = Instant::now();
        for d in &batches {
            inc.apply(d);
        }
        let per = t.elapsed().as_secs_f64() * 1e6 / BATCHES as f64;
        println!(
            "triangles, incremental:    {:>10.1} us/batch   ({}x speedup; init {:?}; count {} {}; probes {})",
            per,
            (full_tri.as_secs_f64() * 1e6 / BATCHES as f64 / per) as u64,
            init,
            inc.count,
            if inc.count == *counts.last().unwrap() { "== oracle" } else { "!= ORACLE" },
            inc.probes
        );
    }));
    if r.is_err() {
        println!("triangles, incremental:    [stub — implement tri::IncrementalTriangles]");
    }

    // --- provided: full recompute per batch (2-hop wedge join) ---
    let keyed = |edges: &ZSet<Edge>| -> ZSet<(u32, u32)> {
        ZSet::from_updates(
            edges.iter().flat_map(|&((u, v), w)| [((u, v), w), ((v, u), w)]).collect(),
        )
    };
    let mut g = base.clone();
    let t0 = Instant::now();
    let mut wedge_w = 0i64;
    for d in &batches {
        g = g.merge(d);
        let a = keyed(&g);
        wedge_w = join_oracle(&a, &a).total_weight();
    }
    let full_join = t0.elapsed();
    println!(
        "wedge join, full recompute:{:>10.1} ms/batch   (wedges incl. trivial: {})",
        full_join.as_secs_f64() * 1000.0 / BATCHES as f64,
        wedge_w
    );

    // --- stub: incremental join ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut ij = IncrementalJoin::new();
        let da0 = keyed(&base);
        let t = Instant::now();
        let mut total = ij.step(&da0, &da0).total_weight();
        let init = t.elapsed();
        let t = Instant::now();
        for d in &batches {
            let dk = keyed(d);
            total += ij.step(&dk, &dk).total_weight();
        }
        let per = t.elapsed().as_secs_f64() * 1e6 / BATCHES as f64;
        println!(
            "wedge join, incremental:   {:>10.1} us/batch   ({}x speedup; init {:?}; wedges {} {})",
            per,
            (full_join.as_secs_f64() * 1e6 / BATCHES as f64 / per) as u64,
            init,
            total,
            if total == wedge_w { "== oracle" } else { "!= ORACLE" }
        );
    }));
    if r.is_err() {
        println!("wedge join, incremental:   [stub — implement djoin::IncrementalJoin]");
    }

    // --- reachability: naive re-BFS vs semi-naive, growing graph ---
    let mut edges: Vec<Edge> = base.iter().map(|(e, _)| *e).collect();
    edges.shuffle(&mut ChaCha8Rng::seed_from_u64(3));
    let chunks: Vec<&[Edge]> = edges.chunks(M / 50).collect();

    let t0 = Instant::now();
    let mut so_far: Vec<(Edge, i64)> = Vec::new();
    let mut reached = 0usize;
    for c in &chunks {
        so_far.extend(c.iter().map(|e| (*e, 1)));
        reached = bfs_reachable(&ZSet::from_updates(so_far.clone()), 0).len();
    }
    let full_bfs = t0.elapsed();
    println!(
        "reach, re-BFS per batch:   {:>10.1} ms/batch   (|reached| {})",
        full_bfs.as_secs_f64() * 1000.0 / chunks.len() as f64,
        reached
    );

    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut sn = SemiNaiveReach::new(0);
        let t = Instant::now();
        for c in &chunks {
            sn.insert_edges(c);
        }
        let per = t.elapsed().as_secs_f64() * 1e6 / chunks.len() as f64;
        println!(
            "reach, semi-naive:         {:>10.1} us/batch   ({}x speedup; |reached| {} {}; {} relaxations for {} edges)",
            per,
            (full_bfs.as_secs_f64() * 1e6 / chunks.len() as f64 / per) as u64,
            sn.reached.len(),
            if sn.reached.len() == reached { "== oracle" } else { "!= ORACLE" },
            sn.relaxations,
            edges.len()
        );
    }));
    if r.is_err() {
        println!("reach, semi-naive:         [stub — implement reach::SemiNaiveReach]");
    }
}
