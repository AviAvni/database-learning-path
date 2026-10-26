//! Print lane 1b's seeded edge list as `src dst` lines, one per edge, so another
//! engine can be loaded with exactly the graph `pgq_bench` measures.
//!
//!   cargo run --release --bin pgq_edges -- 10000 70 > edges-10000.txt
//!
//! Arguments: vertices, preferential-attachment percent (70 for the skewed rows,
//! 0 for the uniform control). Seed 46 and average out-degree 8, as in pgq_bench.
//! `cengine.py` consumes this output.

use relational_graph_experiments::gen::{power_law, PowerLaw};
use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let n: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10_000);
    let pref: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(70);
    let e = power_law(PowerLaw { vertices: n, edges: 8 * n as u64, pref_percent: pref, seed: 46 });
    let out = std::io::stdout();
    let mut w = std::io::BufWriter::new(out.lock());
    for (s, d) in e {
        writeln!(w, "{s} {d}").unwrap();
    }
}
