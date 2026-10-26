//! Lane 2 — a characteristic-set estimator (EXERCISE).
//!
//! Neumann & Moerkotte's **characteristic set** of a node is the set of edge
//! types it has. Here a node's set is `(label, in-types, out-types)`: which
//! of the five edge types arrive at it and which leave it. The summary keeps,
//! per distinct set,
//!
//! * `count` — how many nodes have exactly that set, and
//! * `in_edges[t]`, `out_edges[t]` — the total number of type-t edges
//!   arriving at / leaving those nodes.
//!
//! A two-edge path `a -ta-> b -tb-> c` is a *star* centred on b with one
//! incoming ta edge and one outgoing tb edge. G-CARE §3.2 gives the
//! estimate for a star over one characteristic set cs:
//! `cs.count · Π_el (cs.freq(el) / cs.count)`; the star is answered by
//! summing that over every set that contains all the required types:
//!
//! ```text
//!   est2(ta, tb) = Σ_{cs : ta ∈ cs.in, tb ∈ cs.out}
//!                     cs.count · (cs.in_edges[ta] / cs.count) · (cs.out_edges[tb] / cs.count)
//!                = Σ  cs.in_edges[ta] · cs.out_edges[tb] / cs.count
//! ```
//!
//! A longer path chains its stars with the independence assumption between
//! consecutive stars (G-CARE: "an aggregation based on the independence
//! assumption between subqueries"). Written as a chain of conditional
//! fan-outs, with `F1[t] = |E_t|`:
//!
//! ```text
//!   est(t0 … t_{k-1}) = F1[t0] · Π_{i=1}^{k-1}  est2(t_{i-1}, t_i) / F1[t_{i-1}]
//! ```
//!
//! Contract (see the tests): a one-edge path returns `|E_t|` exactly; the
//! summary has one entry per distinct (label, in-types, out-types); a
//! two-edge estimate is exact when every node of each set has the same
//! degrees; it falls back to the independence product when all nodes of a
//! label share one set; iteration order must not affect the result.

use crate::gen::{Graph, NTYPES};

#[derive(Clone, Debug)]
pub struct CsSummary {
    /// Edges per type, `F1`.
    pub f1: [f64; NTYPES],
    // Implementation-defined: the per-set tables.
    #[allow(dead_code)]
    sets: Vec<CsEntry>,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
struct CsEntry {
    in_mask: u8,
    out_mask: u8,
    count: f64,
    in_edges: [f64; NTYPES],
    out_edges: [f64; NTYPES],
}

/// Build the summary from the graph. Group nodes by
/// `(label, in-type mask, out-type mask)` using a sorted structure, never a
/// hash map iterated in arbitrary order.
pub fn build(g: &Graph) -> CsSummary {
    let _ = g;
    todo!("lane 2: group nodes by (label, in-type mask, out-type mask); sum counts and per-type edge totals")
}

impl CsSummary {
    /// Number of distinct characteristic sets.
    pub fn num_sets(&self) -> usize {
        todo!("lane 2: one entry per distinct (label, in-types, out-types)")
    }

    /// Estimated number of `a -ta-> b -tb-> c` walks.
    pub fn estimate_two_path(&self, ta: usize, tb: usize) -> f64 {
        let _ = (ta, tb);
        todo!("lane 2: sum in_edges[ta] * out_edges[tb] / count over the sets that contain both types")
    }

    /// Estimated number of walks of `types` (see the module doc).
    pub fn estimate_path(&self, types: &[usize]) -> f64 {
        let _ = types;
        todo!("lane 2: F1[t0] * product over consecutive pairs of est2(prev, next) / F1[prev]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::est::{indep_path, q_error, summarize};
    use crate::gen::{generate, Graph, Params, Popularity};
    use crate::oracle::{path_count, walks};

    fn lists(r: [&[(u32, u32)]; NTYPES]) -> Vec<Vec<(u32, u32)>> {
        r.iter().map(|s| s.to_vec()).collect()
    }

    /// L0 = 0..3, L1 = 4..7, L2 = 8..11, L3 = 12..15. In L1 the hub (node 4)
    /// has 3 in and 3 out; node 5 has 1 and 1; nodes 6, 7 have neither.
    fn hub_graph() -> Graph {
        Graph::from_edges(
            [4, 4, 4, 4],
            lists([
                &[(0, 4), (1, 4), (2, 4), (3, 5)],
                &[(4, 8), (4, 9), (4, 10), (5, 11)],
                &[(8, 12), (9, 12), (10, 13), (11, 13)],
                &[],
                &[],
            ]),
        )
    }

    #[test]
    fn a_set_per_distinct_label_and_type_combination() {
        // L0: {out r0}; L1: {in r0, out r1} and {}; L2: {in r1, out r2};
        // L3: {in r2} and {} (nodes 14 and 15 have no edges).
        assert_eq!(build(&hub_graph()).num_sets(), 6);
    }

    #[test]
    fn counts_only_the_nodes_that_have_both_edges() {
        let g = hub_graph();
        let cs = build(&g);
        assert_eq!(path_count(&g, &[0, 1]), 10);
        // independence spreads 4 x 4 edges over all four L1 nodes: 4
        assert_eq!(indep_path(&g, &[0, 1]), 4.0);
        // the set {in r0, out r1} holds nodes 4 and 5: 4 * 4 / 2
        assert_eq!(cs.estimate_two_path(0, 1), 8.0);
        assert!((cs.estimate_two_path(0, 1) - 10.0).abs() < (4.0f64 - 10.0).abs());
        assert_eq!(cs.estimate_two_path(1, 0), 0.0, "r1 does not continue into r0");
    }

    #[test]
    fn exact_when_the_nodes_of_a_set_are_alike() {
        let g = Graph::from_edges(
            [4, 4, 4, 0],
            lists([&[(0, 4), (1, 5)], &[(4, 8), (5, 9)], &[], &[], &[]]),
        );
        assert_eq!(path_count(&g, &[0, 1]), 2);
        assert_eq!(build(&g).estimate_two_path(0, 1), 2.0);
    }

    #[test]
    fn chains_stars_by_conditional_fanout() {
        let g = hub_graph();
        let cs = build(&g);
        assert_eq!(cs.estimate_path(&[0]), 4.0);
        assert_eq!(cs.estimate_path(&[0, 1]), 8.0);
        // 8 * (est2(r1, r2) / F1[r1]) = 8 * 4 / 4
        assert_eq!(cs.estimate_two_path(1, 2), 4.0);
        assert_eq!(cs.estimate_path(&[0, 1, 2]), 8.0);
    }

    #[test]
    fn one_set_per_label_means_the_independence_product() {
        // Every node of every label has every type its label can have, with one edge each.
        let g = Graph::from_edges(
            [2, 2, 2, 0],
            lists([&[(0, 2), (1, 3)], &[(2, 4), (3, 5)], &[], &[], &[]]),
        );
        let cs = build(&g);
        assert_eq!(cs.estimate_path(&[0, 1]), indep_path(&g, &[0, 1]));
    }

    #[test]
    fn deterministic_and_never_worse_than_independence_at_two_hops() {
        let mut qc = Vec::new();
        let mut qi = Vec::new();
        for seed in 1..=3 {
            let g = generate(&Params::small(seed, Popularity::Shared));
            let (a, b) = (build(&g), build(&g));
            for w in walks(2) {
                assert_eq!(a.estimate_path(&w), b.estimate_path(&w));
                let truth = path_count(&g, &w) as f64;
                qc.push(q_error(a.estimate_path(&w), truth));
                qi.push(q_error(indep_path(&g, &w), truth));
            }
        }
        assert!(summarize(qc).0 <= summarize(qi).0);
    }
}
