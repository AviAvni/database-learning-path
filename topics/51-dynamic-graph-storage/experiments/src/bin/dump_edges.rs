//! dump_edges — write lane 1's seeded graph and operation streams as CSV, so
//! the same graph can be loaded into a real FalkorDB (scripts/falkordb_datapoint.py).
//!
//! cargo run --release --bin dump_edges -- <out dir> [n_starts] [n_inserts]
//!
//! Files written:
//!   V.csv        `id` column, 0..|V|−1 (the bulk loader's node key; named without an underscore it is
//!              also kept as a property, which is what makes the node exist)
//!   E.csv        `src,dst`, the graph's edges, sorted
//!   starts.csv   `start,first_hop,second_hop,distinct_second`: query start vertices with
//!                the out-degree of the start, the sum of its neighbours' out-degrees (the
//!                two-hop *path* count the in-process stores touch) and the number of
//!                distinct vertices reached in two hops (what FalkorDB's `count(c)` returns:
//!                its algebraic traversal keeps one row per reached vertex, not per path)
//!   inserts.csv  `src,dst`: candidate inserts (some already exist)

use dyngraph_experiments::csr::Csr;
use dyngraph_experiments::gen::{rmat, workload, GraphParams};
use dyngraph_experiments::store::{two_hop, GraphRead};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(1).expect("usage: dump_edges <out dir> [n_starts] [n_inserts]"));
    let n_starts: usize = args.get(2).map_or(200, |s| s.parse().unwrap());
    let n_inserts: usize = args.get(3).map_or(20_000, |s| s.parse().unwrap());
    std::fs::create_dir_all(&dir).unwrap();

    let p = GraphParams::LANE1;
    let el = rmat(p);
    let wl = workload(&el, p.scale, n_starts.max(1), n_inserts.max(1));
    let g = Csr::from_edges(el.n, &el.edges);

    let mut f = BufWriter::new(File::create(dir.join("V.csv")).unwrap());
    writeln!(f, "id").unwrap();
    for i in 0..el.n {
        writeln!(f, "{i}").unwrap();
    }
    let mut f = BufWriter::new(File::create(dir.join("E.csv")).unwrap());
    writeln!(f, "src,dst").unwrap();
    for &(u, v) in &el.edges {
        writeln!(f, "{u},{v}").unwrap();
    }
    let mut f = BufWriter::new(File::create(dir.join("starts.csv")).unwrap());
    writeln!(f, "start,first_hop,second_hop,distinct_second").unwrap();
    for &s in wl.starts.iter().take(n_starts) {
        let (touched, _) = two_hop(&g, s);
        let first = g.row(s).len() as u64;
        let mut reached: Vec<u32> = g.row(s).iter().flat_map(|&u| g.row(u).iter().copied()).collect();
        reached.sort_unstable();
        reached.dedup();
        writeln!(f, "{},{},{},{}", s, first, touched - first, reached.len()).unwrap();
    }
    let mut f = BufWriter::new(File::create(dir.join("inserts.csv")).unwrap());
    writeln!(f, "src,dst").unwrap();
    for &(u, v) in wl.inserts.iter().take(n_inserts) {
        writeln!(f, "{u},{v}").unwrap();
    }
    println!("wrote V.csv ({} vertices), E.csv ({} edges), starts.csv ({}), inserts.csv ({}) to {}", el.n, el.edges.len(), n_starts, n_inserts, dir.display());
    println!("graph: {} vertices, {} edges ({} B/edge CSR)", g.num_vertices(), g.num_edges(), g.bytes() as f64 / g.num_edges() as f64);
}
