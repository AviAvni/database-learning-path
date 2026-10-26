//! gnn_bench — graph ML kernels over an SBM graph.
//! Provided lanes: SBM build, uniform walks, SpMM (aggregation), dense
//! matmul (transform). Stub lanes (node2vec, skip-gram, GCN) print a
//! placeholder until implemented.

use graph_ml_experiments::dense::{glorot, matmul};
use graph_ml_experiments::embed::{mean_pair_cosine, train_skipgram};
use graph_ml_experiments::gcn::{gcn_forward, gcn_norm};
use graph_ml_experiments::graph::gen_sbm;
use graph_ml_experiments::sampling;
use graph_ml_experiments::spmm::{row_norm_adj, spmm};
use graph_ml_experiments::walks::{node2vec_walks, uniform_walks};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    // An unimplemented exercise lane is an expected state, not a crash: every
    // stub lane below is wrapped in catch_unwind, so drop the default panic
    // hook to keep their todo!() traces out of the measured output.
    std::panic::set_hook(Box::new(|_| {}));
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

    // --- provided, counted: neighbour explosion and sampling (GraphSAGE §3.1) ---
    println!("\n== neighbour explosion: receptive field of a 256-vertex batch (counted, seeded) ==");
    println!("   (two graphs of 262,144 vertices and about the same average degree, one with hubs and one without,");
    println!("    large enough that a small batch's field is a fraction of the graph rather than all of it)");
    let (big_sbm, _) = gen_sbm(256, 1024, 0.01, 0.00002, 43);
    let big_pa = sampling::gen_preferential(262_144, 8, 43);
    let max_deg = |g: &graph_ml_experiments::graph::Csr| (0..g.n as u32).map(|v| g.degree(v)).max().unwrap();
    println!(
        "   SBM: n={} avg_deg {:.1} max_deg {};  preferential attachment: n={} avg_deg {:.1} max_deg {}",
        big_sbm.n, big_sbm.m() as f64 / big_sbm.n as f64, max_deg(&big_sbm),
        big_pa.n, big_pa.m() as f64 / big_pa.n as f64, max_deg(&big_pa)
    );
    println!("   {:<13} {:<24} {:>10} {:>11} {:>12}", "graph", "receptive field", "vertices", "% of graph", "edges");
    for (name, gr) in [("SBM", &big_sbm), ("preferential", &big_pa)] {
        let seeds = sampling::pick_seeds(gr.n, 256, 5);
        let rows = [
            ("full 1 layer", sampling::full_field(gr, &seeds, 1)),
            ("full 2 layers", sampling::full_field(gr, &seeds, 2)),
            ("full 3 layers", sampling::full_field(gr, &seeds, 3)),
            ("sampled 2 layers (25,10)", sampling::sampled_field(gr, &seeds, &[25, 10], 6)),
            ("sampled 3 layers (15,10,5)", sampling::sampled_field(gr, &seeds, &[15, 10, 5], 6)),
        ];
        for (label, f) in rows {
            println!(
                "   {name:<13} {label:<26} {:>8} {:>10.2}% {:>12}",
                f.vertices,
                100.0 * f.vertices as f64 / gr.n as f64,
                f.edges
            );
        }
        println!(
            "   {:<13} sampling cuts 2-layer vertices {:.1}x, edges {:.1}x; 3-layer vertices {:.1}x, edges {:.1}x",
            "",
            rows[1].1.vertices as f64 / rows[3].1.vertices as f64,
            rows[1].1.edges as f64 / rows[3].1.edges as f64,
            rows[2].1.vertices as f64 / rows[4].1.vertices as f64,
            rows[2].1.edges as f64 / rows[4].1.edges as f64
        );
    }

    // --- provided, measured: aggregate-first vs transform-first ---
    println!("\n== associativity: A(XW) vs (AX)W, one layer, Cora-shaped widths (d_in 1433 -> 16) on the SBM ==");
    let (d_in, d_out) = (1433usize, 16usize);
    let xin = glorot(g.n, d_in, 300);
    let wl = glorot(d_in, d_out, 301);
    let nnz = g.m() as f64;
    let n = g.n as f64;
    let sp_tf = 2.0 * nnz * d_out as f64; // A (XW): spmm over d_out columns
    let sp_af = 2.0 * nnz * d_in as f64; //  (A X) W: spmm over d_in columns
    let dn = 2.0 * n * d_in as f64 * d_out as f64; // the dense XW or (AX)W product, same either way
    println!(
        "   counted FLOPs: transform-first {:.3} G (sparse {:.3} + dense {:.3});  aggregate-first {:.3} G (sparse {:.3} + dense {:.3})",
        (sp_tf + dn) / 1e9, sp_tf / 1e9, dn / 1e9, (sp_af + dn) / 1e9, sp_af / 1e9, dn / 1e9
    );
    println!(
        "   sparse side alone: {:.1}x fewer FLOPs transform-first; whole layer: {:.2}x",
        sp_af / sp_tf,
        (sp_af + dn) / (sp_tf + dn)
    );
    let med = |f: &dyn Fn() -> f64| {
        let mut ts: Vec<f64> = (0..5).map(|_| f()).collect();
        ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ts[2]
    };
    let out_tf = std::cell::RefCell::new(None);
    let out_af = std::cell::RefCell::new(None);
    let t_tf = med(&|| {
        let t = Instant::now();
        let h = matmul(std::hint::black_box(&xin), &wl);
        *out_tf.borrow_mut() = Some(spmm(&a, &h));
        t.elapsed().as_secs_f64() * 1e3
    });
    let t_af = med(&|| {
        let t = Instant::now();
        let ax = spmm(&a, std::hint::black_box(&xin));
        *out_af.borrow_mut() = Some(matmul(&ax, &wl));
        t.elapsed().as_secs_f64() * 1e3
    });
    let diff = graph_ml_experiments::dense::max_abs_diff(out_tf.borrow().as_ref().unwrap(), out_af.borrow().as_ref().unwrap());
    println!(
        "   measured (median of 5): transform-first {t_tf:.1} ms, aggregate-first {t_af:.1} ms -> {:.2}x; outputs agree to {diff:.1e}",
        t_af / t_tf
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
