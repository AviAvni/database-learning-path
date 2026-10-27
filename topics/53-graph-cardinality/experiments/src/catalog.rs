//! Lane 3 — a GLogue-style catalog of two-edge paths (EXERCISE).
//!
//! GOpt (Lyu et al., arXiv:2401.17786 §6.3.1) estimates a pattern from the
//! stored frequencies of its sub-patterns:
//!
//! ```text
//!   F(P_t) = F(P_s1) · F(P_s2) / F(P_s1 ∩ P_s2)                      (Eq. 1)
//! ```
//!
//! where `P_s1` and `P_s2` are two sub-patterns whose union is `P_t` and
//! `F(·)` is the number of matches, under the assumption that the two
//! sub-patterns occur independently *given* their common part. For a walk
//! `t0 t1 … t_{k-1}`, take `P_s1` = the first k−1 edges, `P_s2` = the last
//! k−1 edges, common part = the middle k−2 edges; applied until every factor
//! is a stored pattern this is the order-2 Markov rule
//!
//! ```text
//!   F(t0 … t_{k-1}) = Π_{i=0}^{k-2} F2[t_i][t_{i+1}]  /  Π_{i=1}^{k-2} F1[t_i]
//! ```
//!
//! with `F1[t] = |E_t|` and `F2[a][b]` the *exact* number of `a`-then-`b`
//! walks. For k = 2 this is just `F2`, and for k = 1 `F1`: the catalog
//! answers its own patterns exactly. Beyond that only the independence of
//! the third edge from the first, given the second, is assumed — not the
//! independence of every edge from every other that lane 1's estimator
//! assumes.
//!
//! Contract (see the tests): `build` computes `F1` and `F2` exactly — for
//! every pair of label-consistent types and `0` for the others — with the
//! oracle's counting or its own; `estimate_path` is exact for one- and
//! two-edge paths and follows the formula above beyond; a zero in a
//! denominator gives `0.0`, not NaN.

use crate::gen::{Graph, NTYPES};
#[derive(Clone, Debug)]
pub struct PathCatalog {
    pub f1: [f64; NTYPES],
    pub f2: [[f64; NTYPES]; NTYPES],
}

pub fn build(g: &Graph) -> PathCatalog {
    let _ = g;
    todo!("lane 3: F1[t] = |E_t|; F2[a][b] = exact number of a-then-b walks (0 when the types do not connect)")
}

impl PathCatalog {
    /// Number of stored non-zero patterns (edges plus two-edge paths).
    pub fn size(&self) -> usize {
        todo!("lane 3: count the non-zero entries of F1 and F2")
    }

    pub fn estimate_path(&self, types: &[usize]) -> f64 {
        let _ = types;
        todo!("lane 3: product of F2 over consecutive pairs divided by product of F1 over the interior edges")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::est::{indep_path, q_error, summarize};
    use crate::gen::{generate, Params, Popularity};
    use crate::oracle::{path_count, walks};

    #[test]
    fn exact_for_one_and_two_edge_paths() {
        let g = generate(&Params::small(3, Popularity::Shared));
        let c = build(&g);
        for t in 0..NTYPES {
            assert_eq!(c.estimate_path(&[t]), g.edges[t] as f64);
        }
        for w in walks(2) {
            assert_eq!(c.estimate_path(&w), path_count(&g, &w) as f64, "{w:?}");
        }
        assert_eq!(c.f2[1][0], 0.0, "r1 does not continue into r0");
    }

    #[test]
    fn three_edge_estimate_is_f2_f2_over_f1() {
        let g = generate(&Params::small(3, Popularity::Shared));
        let c = build(&g);
        for w in walks(3) {
            let want = c.f2[w[0]][w[1]] * c.f2[w[1]][w[2]] / c.f1[w[1]];
            assert!((c.estimate_path(&w) - want).abs() <= 1e-9 * want.max(1.0), "{w:?}");
        }
    }

    #[test]
    fn exact_when_the_third_edge_does_not_depend_on_the_first() {
        use crate::gen::Graph;
        // L1 hub 4 has 3 in and 3 out; every r1 edge continues into exactly one r2 edge.
        let l: Vec<Vec<(u32, u32)>> = vec![
            vec![(0, 4), (1, 4), (2, 4), (3, 5)],
            vec![(4, 8), (4, 9), (4, 10), (5, 11)],
            vec![(8, 12), (9, 12), (10, 13), (11, 13)],
            vec![],
            vec![],
        ];
        let g = Graph::from_edges([4, 4, 4, 4], l);
        let c = build(&g);
        assert_eq!(path_count(&g, &[0, 1, 2]), 10);
        assert_eq!(c.estimate_path(&[0, 1, 2]), 10.0);
        assert_eq!(indep_path(&g, &[0, 1, 2]), 4.0);
        assert_eq!(c.size(), 3 + 2);
    }

    #[test]
    fn order_two_misses_a_dependence_on_the_first_edge() {
        use crate::gen::Graph;
        // Hub 4 (3 in-edges) feeds r2-rich nodes, node 5 (1 in-edge) feeds one with none:
        // the extension of an r1 edge depends on how many r0 edges entered its source.
        let l: Vec<Vec<(u32, u32)>> = vec![
            vec![(0, 4), (1, 4), (2, 4), (3, 5)],
            vec![(4, 8), (4, 9), (4, 10), (5, 11)],
            vec![(8, 12), (8, 13), (9, 12), (10, 12)],
            vec![],
            vec![],
        ];
        let g = Graph::from_edges([4, 4, 4, 4], l);
        assert_eq!(path_count(&g, &[0, 1, 2]), 12);
        assert_eq!(build(&g).estimate_path(&[0, 1, 2]), 10.0);
    }

    #[test]
    fn far_closer_than_independence_at_three_hops_on_shared_popularity() {
        let (mut qc, mut qi) = (Vec::new(), Vec::new());
        for seed in 1..=3 {
            let g = generate(&Params::small(seed, Popularity::Shared));
            let c = build(&g);
            for w in walks(3) {
                let truth = path_count(&g, &w) as f64;
                qc.push(q_error(c.estimate_path(&w), truth));
                qi.push(q_error(indep_path(&g, &w), truth));
            }
        }
        let (mc, mi) = (summarize(qc).0, summarize(qi).0);
        assert!(mc * 5.0 < mi, "catalog median {mc} vs independence {mi}");
    }

    #[test]
    fn missing_denominator_is_zero_not_nan() {
        let c = PathCatalog { f1: [0.0; NTYPES], f2: [[1.0; NTYPES]; NTYPES] };
        assert_eq!(c.estimate_path(&[0, 1, 2]), 0.0);
    }
}
