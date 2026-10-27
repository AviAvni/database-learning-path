//! Estimators and the q-error (PROVIDED, lane 1).
//!
//! Each function is the arithmetic of a real engine or paper, restricted to
//! the label-typed walk patterns of [`crate::oracle`]:
//!
//! * [`indep_prefix`] — the independence / average-degree product. Kùzu
//!   (`CardinalityEstimator::getExtensionRate`, `cardinality_estimator.cpp:
//!   214-244`) multiplies the running cardinality by `|rel table| / |bound
//!   node table|` per `Extend`; Neo4j's `AssumeIndependenceQueryGraph
//!   CardinalityModel` multiplies the node cardinalities by
//!   `patternStepCardinality / (|left| · |right|)` per relationship. For a
//!   chain of typed edges both reduce to the same product, below.
//! * [`memgraph_const`] — Memgraph's `Expand` multiplies by the constant
//!   `CardParam::kExpand = 3.0` unless symbol statistics are supplied
//!   (`cost_estimator.hpp:368-380`, `cost_constants.hpp:47`).
//! * [`maxdeg_bound`] — the max-degree chain bound (the pessimistic family's
//!   BoundSketch / chain bound, Abo Khamis et al. Eq. 12).
//! * [`dsb_two_hop`], [`l2_two_hop`] — the degree-sequence bound (Eq. 22)
//!   and the ℓ2 bound (Eq. 16) for one join.

use crate::gen::{Graph, TYPES};

/// `|E_t| / |source label of t|` — the average out-degree of t.
pub fn avg_out(g: &Graph, t: usize) -> f64 {
    g.edges[t] as f64 / g.label_sizes[TYPES[t].0] as f64
}

/// `|E_t| / |destination label of t|` — the average in-degree of t.
pub fn avg_in(g: &Graph, t: usize) -> f64 {
    g.edges[t] as f64 / g.label_sizes[TYPES[t].1] as f64
}

/// Estimated number of walks of the first `j` edges of `types`:
/// `|E_t0| · Π_{i=1}^{j-1} avg_out(t_i)`.
pub fn indep_prefix(g: &Graph, types: &[usize], j: usize) -> f64 {
    let mut e = g.edges[types[0]] as f64;
    for &t in &types[1..j] {
        e *= avg_out(g, t);
    }
    e
}

/// Estimated number of walks of the last `j` edges, expanding from the far
/// end: `|E_t_last| · Π avg_in(t_i)` over the j−1 edges before it.
pub fn indep_suffix(g: &Graph, types: &[usize], j: usize) -> f64 {
    let k = types.len();
    let mut e = g.edges[types[k - 1]] as f64;
    for i in 1..j {
        e *= avg_in(g, types[k - 1 - i]);
    }
    e
}

pub fn indep_path(g: &Graph, types: &[usize]) -> f64 {
    indep_prefix(g, types, types.len())
}

/// Triangle `a -t0-> b -t1-> c -t2-> a`: the open 2-path, times the closing
/// edge's selectivity `|E_t2| / (|L_c| · |L_a|)` (Neo4j divides the
/// relationship cardinality by both endpoint cardinalities; the independence
/// assumption is that c → a is present with that probability).
pub fn indep_triangle(g: &Graph, t0: usize, t1: usize, t2: usize) -> f64 {
    let (lc, la) = TYPES[t2];
    indep_path(g, &[t0, t1])
        * g.edges[t2] as f64
        / (g.label_sizes[lc] as f64 * g.label_sizes[la] as f64)
}

/// `|L_start| · 3^k`: Memgraph's default when no symbol statistics exist.
pub fn memgraph_const(g: &Graph, types: &[usize]) -> f64 {
    g.label_sizes[TYPES[types[0]].0] as f64 * 3f64.powi(types.len() as i32)
}

/// `|E_t0| · Π max-out-degree(t_i)`.
pub fn maxdeg_bound(g: &Graph, types: &[usize]) -> f64 {
    let mut e = g.edges[types[0]] as f64;
    for &t in &types[1..] {
        let m = (0..g.n).map(|v| g.out[t].deg(v)).max().unwrap_or(0);
        e *= m as f64;
    }
    e
}

/// Degrees of the nodes with at least one edge, sorted descending: the
/// degree sequence `deg_R(*|X)` of a relation on its join column.
pub fn degree_sequence(csr: &crate::gen::Csr, n: usize) -> Vec<u64> {
    let mut d: Vec<u64> = (0..n).map(|v| csr.deg(v) as u64).filter(|&x| x > 0).collect();
    d.sort_unstable_by(|a, b| b.cmp(a));
    d
}

/// Degree-sequence bound for `R(X,Y) ⋈ S(Y,Z)`, R = t0 joined on its
/// destination and S = t1 on its source: `Σ_i a_i · b_i` with both
/// sequences sorted descending (Abo Khamis et al., Eq. 22).
pub fn dsb_two_hop(g: &Graph, t0: usize, t1: usize) -> f64 {
    let a = degree_sequence(&g.inn[t0], g.n);
    let b = degree_sequence(&g.out[t1], g.n);
    a.iter().zip(&b).map(|(&x, &y)| (x * y) as f64).sum()
}

/// `‖deg_R(X|Y)‖₂ · ‖deg_S(Z|Y)‖₂` (Eq. 16, Cauchy–Schwarz).
pub fn l2_two_hop(g: &Graph, t0: usize, t1: usize) -> f64 {
    let a = degree_sequence(&g.inn[t0], g.n);
    let b = degree_sequence(&g.out[t1], g.n);
    let na: f64 = a.iter().map(|&x| (x * x) as f64).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|&x| (x * x) as f64).sum::<f64>().sqrt();
    na * nb
}

/// The AGM bound for the triangle `a -t0-> b -t1-> c -t2-> a`:
/// `(|E_t0| · |E_t1| · |E_t2|)^(1/2)` (fractional edge cover (½, ½, ½);
/// Abo Khamis et al., Eq. 18).
pub fn agm_triangle(g: &Graph, t0: usize, t1: usize, t2: usize) -> f64 {
    (g.edges[t0] as f64 * g.edges[t1] as f64 * g.edges[t2] as f64).sqrt()
}

/// The ℓ2 bound for the same triangle (Eq. 19):
/// `(‖deg_t0(b|a)‖₂² · ‖deg_t1(c|b)‖₂² · ‖deg_t2(a|c)‖₂²)^(1/3)`, where
/// `deg_t(y|x)` is the out-degree of x in type t, so each squared norm is a
/// sum of squared out-degrees.
pub fn l2_triangle(g: &Graph, t0: usize, t1: usize, t2: usize) -> f64 {
    let sq = |t: usize| -> f64 { (0..g.n).map(|v| (g.out[t].deg(v) as f64).powi(2)).sum() };
    (sq(t0) * sq(t1) * sq(t2)).powf(1.0 / 3.0)
}

/// G-CARE's q-error (Park et al. §5.1): `max(max(1,c)/max(1,ĉ),
/// max(1,ĉ)/max(1,c))` — both counts floored at 1, so an estimate of 0 for a
/// non-empty answer is scored as an estimate of 1.
pub fn q_error(est: f64, truth: f64) -> f64 {
    let (e, c) = (est.max(1.0), truth.max(1.0));
    (c / e).max(e / c)
}

/// Median / 90th percentile / max of a sample (nearest-rank).
pub fn summarize(mut v: Vec<f64>) -> (f64, f64, f64) {
    assert!(!v.is_empty());
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rank = |p: f64| v[(((v.len() as f64) * p).ceil() as usize).clamp(1, v.len()) - 1];
    (rank(0.5), rank(0.9), *v.last().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{generate, Params, Popularity};
    use crate::oracle::{path_count, walks};

    #[test]
    fn q_error_is_symmetric_and_floored() {
        assert_eq!(q_error(10.0, 100.0), 10.0);
        assert_eq!(q_error(100.0, 10.0), 10.0);
        assert_eq!(q_error(0.0, 50.0), 50.0);
        assert_eq!(q_error(0.2, 0.0), 1.0);
    }

    #[test]
    fn one_edge_is_exact_and_bounds_are_upper_bounds() {
        let g = generate(&Params::small(1, Popularity::Shared));
        for t in 0..5 {
            assert_eq!(indep_path(&g, &[t]), g.edges[t] as f64);
        }
        for w in walks(2).into_iter().chain(walks(3)) {
            let truth = path_count(&g, &w) as f64;
            assert!(maxdeg_bound(&g, &w) >= truth, "{w:?}");
        }
        for w in walks(2) {
            let truth = path_count(&g, &w) as f64;
            let (d, l) = (dsb_two_hop(&g, w[0], w[1]), l2_two_hop(&g, w[0], w[1]));
            assert!(d >= truth - 0.5 && l >= d - 0.5, "{w:?}: {truth} {d} {l}");
        }
    }

    #[test]
    fn dsb_is_exact_when_ranks_align_and_the_maxdeg_bound_is_not() {
        // Shared popularity: the most popular r0-destination is the most
        // popular r1-source, so the sorted sequences pair the same nodes
        // wherever both are non-zero.
        let g = generate(&Params::small(4, Popularity::Shared));
        let truth = path_count(&g, &[0, 1]) as f64;
        let dsb = dsb_two_hop(&g, 0, 1);
        assert!(dsb / truth < 1.5, "dsb {dsb} truth {truth}");
        assert!(maxdeg_bound(&g, &[0, 1]) > 2.0 * dsb);
    }

    #[test]
    fn the_worked_example_in_the_pessimistic_guide() {
        use crate::gen::Graph;
        let r0: Vec<(u32, u32)> = vec![(0, 4), (1, 4), (2, 4), (3, 5)];
        let mk = |r1: Vec<(u32, u32)>| {
            Graph::from_edges([4, 4, 4, 0], vec![r0.clone(), r1, vec![], vec![], vec![]])
        };
        // aligned: the node with 3 in-edges also has 3 out-edges
        let a = mk(vec![(4, 8), (4, 9), (4, 10), (5, 11)]);
        assert_eq!(path_count(&a, &[0, 1]), 10);
        assert_eq!(dsb_two_hop(&a, 0, 1), 10.0);
        assert!((l2_two_hop(&a, 0, 1) - 10.0).abs() < 1e-9);
        assert_eq!(maxdeg_bound(&a, &[0, 1]), 12.0);
        assert_eq!(indep_path(&a, &[0, 1]), 4.0);
        // anti-aligned: the in-hub has 1 out-edge, the other node 3
        let b = mk(vec![(4, 8), (5, 9), (5, 10), (5, 11)]);
        assert_eq!(path_count(&b, &[0, 1]), 6);
        assert_eq!(dsb_two_hop(&b, 0, 1), 10.0);
        assert!((l2_two_hop(&b, 0, 1) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn triangle_bounds_hold() {
        use crate::oracle::triangle_count;
        for pop in [Popularity::Shared, Popularity::Independent] {
            let g = generate(&Params::small(2, pop));
            let t = triangle_count(&g, 0, 1, 4) as f64;
            assert!(agm_triangle(&g, 0, 1, 4) >= t);
            assert!(l2_triangle(&g, 0, 1, 4) >= t);
        }
    }

    #[test]
    fn independence_is_good_when_popularity_is_independent() {
        let g = generate(&Params::small(4, Popularity::Independent));
        let q: Vec<f64> = walks(2)
            .iter()
            .map(|w| q_error(indep_path(&g, w), path_count(&g, w) as f64))
            .collect();
        assert!(q.iter().all(|&x| x < 3.0), "{q:?}");
    }

    #[test]
    fn independence_collapses_when_popularity_is_shared() {
        let g = generate(&Params::small(4, Popularity::Shared));
        let q: Vec<f64> = walks(2)
            .iter()
            .map(|w| q_error(indep_path(&g, w), path_count(&g, w) as f64))
            .collect();
        assert!(q.iter().all(|&x| x > 2.0), "{q:?}");
    }
}
