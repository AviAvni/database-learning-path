use graphalgo_experiments::{analytics, bc, cc, graph, sssp};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    println!("== graphs ==");
    let t = Instant::now();
    let (n, e) = graph::gen_rmat(16, 16, 42);
    let g = graph::from_edges(n, &e, true);
    println!(
        "rmat scale 16: n {}  m {} (directed)  build {:.0} ms",
        g.n,
        g.m(),
        t.elapsed().as_secs_f64() * 1e3
    );
    let eu = graph::gen_uniform(n, n * 16, 43);
    let gu = graph::from_edges(n, &eu, true);
    println!("uniform:       n {}  m {}", gu.n, gu.m());
    let mut degs: Vec<usize> = (0..g.n).map(|u| g.degree(u)).collect();
    degs.sort_unstable_by(|a, b| b.cmp(a));
    println!("rmat max deg {}  uniform max deg {}", degs[0], (0..gu.n).map(|u| gu.degree(u)).max().unwrap());

    println!("\n== PageRank (pull, damp .85, eps 1e-4/n-scaled) ==");
    for (name, gr) in [("rmat   ", &g), ("uniform", &gu)] {
        let t = Instant::now();
        let (_, iters) = analytics::pagerank(gr, 1e-4, 100);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "{name}  {iters} iters  {ms:7.1} ms  {:.2} GTEPS-ish",
            (gr.m() * iters) as f64 / (ms / 1e3) / 1e9
        );
    }

    println!("\n== Triangle count (degree-ordered intersection) ==");
    for (name, gr) in [("rmat   ", &g), ("uniform", &gu)] {
        let t = Instant::now();
        let tc = analytics::triangle_count(gr);
        println!("{name}  {tc:>12} triangles  {:7.1} ms", t.elapsed().as_secs_f64() * 1e3);
    }

    println!("\n== SSSP: Dijkstra oracle ==");
    let mut total_pops = 0usize;
    let t = Instant::now();
    for src in [0u32, 1000, 30000] {
        let (d, pops) = sssp::dijkstra(&g, src);
        total_pops += pops;
        std::hint::black_box(d);
    }
    println!(
        "3 sources  {:7.1} ms total  {} heap pops (n={})",
        t.elapsed().as_secs_f64() * 1e3,
        total_pops,
        g.n
    );

    println!("\n== delta-stepping (stub lane) ==");
    let r = catch_unwind(AssertUnwindSafe(|| {
        for delta in [16u64, 128, 1024, 1 << 40] {
            let t = Instant::now();
            let (d, stats) = sssp::delta_stepping(&g, 0, delta);
            std::hint::black_box(d);
            println!(
                "delta {delta:>13}  {:7.1} ms  relaxations {:>9}  buckets {}",
                t.elapsed().as_secs_f64() * 1e3,
                stats.relaxations,
                stats.buckets
            );
        }
    }));
    if r.is_err() {
        println!("[stub — implement sssp::delta_stepping]");
    }

    println!("\n== CC: union-find oracle ==");
    let t = Instant::now();
    let labels = cc::cc_unionfind(&g);
    let mut comp = cc::canonical(&labels);
    comp.sort_unstable();
    comp.dedup();
    println!(
        "components {}  {:7.1} ms (all {} edges inspected)",
        comp.len(),
        t.elapsed().as_secs_f64() * 1e3,
        g.m()
    );

    println!("\n== Afforest (stub lane) ==");
    let r = catch_unwind(AssertUnwindSafe(|| {
        let t = Instant::now();
        let (labels, inspected) = cc::afforest(&g, 2);
        let mut c = cc::canonical(&labels);
        c.sort_unstable();
        c.dedup();
        println!(
            "components {}  {:7.1} ms  edges inspected {inspected} of {} ({:.1}%)",
            c.len(),
            t.elapsed().as_secs_f64() * 1e3,
            g.m(),
            100.0 * inspected as f64 / g.m() as f64
        );
    }));
    if r.is_err() {
        println!("[stub — implement cc::afforest]");
    }

    println!("\n== Brandes BC (stub lane, 8 sampled sources on scale 13) ==");
    let (n13, e13) = graph::gen_rmat(13, 16, 44);
    let g13 = graph::from_edges(n13, &e13, true);
    let r = catch_unwind(AssertUnwindSafe(|| {
        let t = Instant::now();
        let scores = bc::brandes(&g13, Some(&[0, 1, 2, 3, 4, 5, 6, 7]));
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let top = scores
            .iter()
            .cloned()
            .fold(0.0f64, f64::max);
        println!("8 sources on n={}  {ms:7.1} ms  max bc {top:.1}", g13.n);
    }));
    if r.is_err() {
        println!("[stub — implement bc::brandes]");
    }
}
