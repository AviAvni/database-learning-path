//! gnn_bench — graph ML kernels over an SBM graph.
//! Provided lanes: SBM build, uniform walks, SpMM (aggregation), dense
//! matmul (transform). Stub lanes (node2vec, skip-gram, GCN) print a
//! placeholder until implemented.

use graph_ml_experiments::dense::{glorot, matmul};
use graph_ml_experiments::embed::{mean_pair_cosine, train_skipgram};
use graph_ml_experiments::gcn::{gcn_forward, gcn_norm};
use graph_ml_experiments::graph::gen_sbm;
use graph_ml_experiments::spmm::{row_norm_adj, spmm};
use graph_ml_experiments::walks::{node2vec_walks, uniform_walks};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    // 64 blocks x 256 = 16,384 vertices; ~30 intra + ~4 inter deg
    let t = Instant::now();
    let (g, labels) = gen_sbm(64, 256, 0.12, 0.00025, 42);
    println!(
        "sbm: n={} m={} (directed) avg_deg={:.1} build={:?}",
        g.n,
        g.m(),
        g.m() as f64 / g.n as f64,
        t.elapsed()
    );

    // --- provided: uniform walks (DeepWalk corpus) ---
    let t = Instant::now();
    let walks = uniform_walks(&g, 40, 4, 7);
    let steps: usize = walks.iter().map(|w| w.len() - 1).sum();
    let el = t.elapsed();
    println!(
        "uniform walks: {} walks, {} steps in {:?} ({:.1} Msteps/s)",
        walks.len(),
        steps,
        el,
        steps as f64 / el.as_secs_f64() / 1e6
    );

    // --- provided: SpMM aggregation (the message-passing kernel) ---
    let a = row_norm_adj(&g);
    let x = glorot(g.n, 64, 100);
    let t = Instant::now();
    let mut agg = spmm(&a, &x);
    for _ in 0..9 {
        agg = spmm(&a, &agg);
    }
    let el = t.elapsed() / 10;
    let flops = 2.0 * g.m() as f64 * 64.0;
    println!(
        "spmm (D^-1 A) x X[{}x64]: {:?}/iter ({:.2} GFLOP/s)  checksum {:.4}",
        g.n,
        el,
        flops / el.as_secs_f64() / 1e9,
        agg.data.iter().map(|v| v.abs() as f64).sum::<f64>()
    );

    // --- provided: dense transform X x W ---
    let w = glorot(64, 64, 101);
    let t = Instant::now();
    let mut h = matmul(&x, &w);
    for _ in 0..9 {
        h = matmul(&h, &w);
    }
    let el = t.elapsed() / 10;
    let flops = 2.0 * g.n as f64 * 64.0 * 64.0;
    println!(
        "dense matmul [{}x64]x[64x64]: {:?}/iter ({:.2} GFLOP/s)",
        g.n,
        el,
        flops / el.as_secs_f64() / 1e9
    );

    // --- stub: node2vec biased walks ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        let t = Instant::now();
        let w = node2vec_walks(&g, 1.0, 0.5, 40, 4, 7);
        let steps: usize = w.iter().map(|x| x.len() - 1).sum();
        let el = t.elapsed();
        println!(
            "node2vec walks (p=1 q=0.5): {} steps in {:?} ({:.1} Msteps/s)",
            steps,
            el,
            steps as f64 / el.as_secs_f64() / 1e6
        );
    }));
    if r.is_err() {
        println!("node2vec walks: [stub — implement walks::node2vec_walks]");
    }

    // --- stub: skip-gram training + block separation ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        let t = Instant::now();
        let z = train_skipgram(&walks, g.n, 64, 5, 5, 1, 0.025, 17);
        let el = t.elapsed();
        let intra = mean_pair_cosine(&z, |i, j| labels[i] == labels[j], 2000);
        let inter = mean_pair_cosine(&z, |i, j| labels[i] != labels[j], 2000);
        println!(
            "skipgram d=64 1 epoch: {:?}  intra-cos {:.3} vs inter-cos {:.3}",
            el, intra, inter
        );
    }));
    if r.is_err() {
        println!("skipgram: [stub — implement embed::train_skipgram]");
    }

    // --- stub: GCN 2-layer forward ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        let t = Instant::now();
        let a_hat = gcn_norm(&g);
        let norm_el = t.elapsed();
        let w1 = glorot(64, 64, 200);
        let w2 = glorot(64, 16, 201);
        let t = Instant::now();
        let z = gcn_forward(&a_hat, &x, &w1, &w2);
        println!(
            "gcn forward 64->64->16: norm {:?} + forward {:?}  checksum {:.4}",
            norm_el,
            t.elapsed(),
            z.data.iter().map(|v| *v as f64).sum::<f64>()
        );
    }));
    if r.is_err() {
        println!("gcn: [stub — implement gcn::gcn_norm + gcn::gcn_forward]");
    }
}
