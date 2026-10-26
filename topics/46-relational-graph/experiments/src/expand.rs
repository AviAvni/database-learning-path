//! LANE 1 (provided) — the graph-engine plan: the same path pattern as nested
//! expansion over CSR adjacency (a Cypher `Expand(All)` chain).
//!
//! It enumerates exactly the tuples the flat plan does — CSR does not shrink
//! the blow-up, it only makes each step cheaper (an array slice instead of a
//! hash probe). That distinction is the point of the lane: *representation*
//! (flat vs factorized) decides how many tuples exist; *access path* (hash vs
//! adjacency) decides what each one costs.

use crate::graph::Graph;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExpandStats {
    /// `out[i]` = bindings produced by the i-th expansion after the first edge.
    pub out: Vec<u64>,
    /// Adjacency slices fetched (one per binding that is expanded).
    pub lookups: u64,
}

impl ExpandStats {
    pub fn tuples(&self) -> u64 {
        self.out.iter().sum()
    }
    pub fn result(&self) -> u64 {
        *self.out.last().unwrap_or(&0)
    }
}

/// `k`-hop paths: for every edge (a, b), expand b's out-neighbours `k - 1`
/// more times.
pub fn k_hop(g: &Graph, k: usize, sink: &mut u64) -> ExpandStats {
    assert!(k >= 2);
    let mut st = ExpandStats { out: vec![0; k - 1], lookups: 0 };
    for a in 0..g.n() as u32 {
        for &b in g.out.neighbours(a) {
            go(g, b, 0, k - 1, &mut st, sink);
        }
    }
    st
}

fn go(g: &Graph, v: u32, step: usize, steps: usize, st: &mut ExpandStats, sink: &mut u64) {
    st.lookups += 1;
    let nb = g.out.neighbours(v);
    st.out[step] += nb.len() as u64;
    if step + 1 == steps {
        for &w in nb {
            *sink = sink.wrapping_add(w as u64);
        }
    } else {
        for &w in nb {
            go(g, w, step + 1, steps, st, sink);
        }
    }
}
