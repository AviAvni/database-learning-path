//! Provided: 2-hop shootout over three representations.
//!
//!   cargo run --release --bin hop_bench
//!
//! Panics on the stubs after the adj_list baseline runs. Predict in
//! notes.md first: adj_list vs csr vs matrix — order and ×, for
//! random sources AND for the 100 highest-degree supernodes.

use std::time::Instant;

use graph_experiments::adj_list::AdjList;
use graph_experiments::csr::Csr;
use graph_experiments::{data, matrix, new_seen};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

const N: u32 = 1_000_000;
const M: u32 = 8; // symmetrized => ~16M directed edges
const QUERIES: usize = 10_000;

fn report(name: &str, label: &str, secs: f64, n: usize, checksum: u64) {
    println!(
        "  {name:<10} {label:<12} {:>10.0} ns/query   total {:.3} s   checksum={checksum}",
        secs / n as f64 * 1e9,
        secs
    );
}

fn main() {
    let t = Instant::now();
    let g = data::preferential_attachment(N, M, 42);
    println!(
        "graph: {} nodes, {} directed edges (gen {:.1} s)",
        g.num_nodes,
        g.edges.len(),
        t.elapsed().as_secs_f64()
    );

    let t = Instant::now();
    let adj = AdjList::build(g.num_nodes, &g.edges);
    println!("adj_list build: {:.2} s", t.elapsed().as_secs_f64());

    // sources: random + the 100 highest-degree (the graph-shaped tail)
    let mut rng = StdRng::seed_from_u64(1);
    let random: Vec<u32> = (0..QUERIES).map(|_| rng.gen_range(0..N)).collect();
    let mut by_degree: Vec<u32> = (0..N).collect();
    by_degree.sort_unstable_by_key(|&v| std::cmp::Reverse(adj.adj[v as usize].len()));
    let supernodes: Vec<u32> = by_degree[..100].to_vec();
    println!(
        "max degree {} | p50 degree {}",
        adj.adj[by_degree[0] as usize].len(),
        adj.adj[by_degree[(N / 2) as usize] as usize].len()
    );

    let mut seen = new_seen(N);
    let mut stamp = 0u32;

    println!("\n== adj_list (oracle)");
    for (label, srcs) in [("random", &random), ("supernodes", &supernodes)] {
        let mut checksum = 0u64;
        let t = Instant::now();
        for &s in srcs {
            stamp += 1;
            checksum += adj.two_hop(s, &mut seen, stamp);
        }
        report("adj_list", label, t.elapsed().as_secs_f64(), srcs.len(), checksum);
    }

    println!("\n== csr");
    let t = Instant::now();
    let csr = Csr::build(g.num_nodes, &g.edges);
    println!("  build: {:.2} s", t.elapsed().as_secs_f64());
    for (label, srcs) in [("random", &random), ("supernodes", &supernodes)] {
        let mut checksum = 0u64;
        let t = Instant::now();
        for &s in srcs {
            stamp += 1;
            checksum += csr.two_hop(s, &mut seen, stamp);
        }
        report("csr", label, t.elapsed().as_secs_f64(), srcs.len(), checksum);
    }

    println!("\n== matrix (masked SpMV over the same CSR)");
    let (mut f1, mut f2) = (Vec::new(), Vec::new());
    for (label, srcs) in [("random", &random), ("supernodes", &supernodes)] {
        let mut checksum = 0u64;
        let t = Instant::now();
        for &s in srcs {
            stamp += 1;
            checksum += matrix::two_hop(&csr, s, &mut seen, stamp, &mut f1, &mut f2);
        }
        report("matrix", label, t.elapsed().as_secs_f64(), srcs.len(), checksum);
    }

    println!("\nnotes:");
    println!("- checksums must MATCH across implementations per source set");
    println!("- compare with FalkorDB: GRAPH.QUERY g \"MATCH (a)-[*1..2]->(b)");
    println!("  WHERE id(a)=<src> RETURN count(DISTINCT b)\" — record in notes.md");
}
