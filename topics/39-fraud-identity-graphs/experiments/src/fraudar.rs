//! STUB — FRAUDAR's camouflage-resistant dense-block detection (KDD'16).
//!
//! The metric family: g(S) = f(S) / |S| where f(S) sums the weights of
//! edges with both endpoints inside S. Unweighted (every edge = 1) this
//! is average degree / 2 — and on a skewed background the densest thing
//! by that measure is the *popular core* (power users x hit products),
//! not the fraud block. FRAUDAR's fix is column weighting: an edge into
//! object j is worth 1/log(d_j + 5), so edges into popular columns are
//! nearly free. Theorem 3 of the paper: column weights are camouflage-
//! resistant, because camouflage edges land on *honest* columns and
//! never change the fraud block's own edges or column degrees.
//!
//! The algorithm: greedy peeling. Repeatedly delete the node whose
//! removal costs the least weighted degree ("exonerate the least
//! suspicious"), tracking g over the shrinking set; return the best set
//! seen. A priority structure makes it O(|E| log |V|), and Theorem 2
//! guarantees g(returned) >= g_optimal / 2.
//!
//! Contracts (the tests): peeling with log weights recovers the planted
//! block (F >= 0.9) with and without camouflage; camouflage drags
//! unweighted peeling into the popular core (F < 0.7 — the returned set
//! swallows the power users x hit products community); the returned
//! block's g is at least half the planted block's g.

use crate::review_graph::ReviewGraph;
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Weighting {
    /// c_ij = 1 — plain average degree. Not camouflage-resistant:
    /// camouflage edges glue the fraud block to the popular core and the
    /// densest unweighted set swallows both.
    Unweighted,
    /// c_ij = 1 / log(d_j + 5), d_j = global column degree (the paper's
    /// recommended h; the tf-idf analogy).
    LogDegree,
}

pub fn column_weight(obj_deg: usize, w: Weighting) -> f64 {
    match w {
        Weighting::Unweighted => 1.0,
        Weighting::LogDegree => 1.0 / ((obj_deg as f64 + 5.0).ln()),
    }
}

pub struct Detection {
    pub users: Vec<usize>,
    pub objects: Vec<usize>,
    /// g of the returned set.
    pub g: f64,
}

/// g(S) = f(S)/|S| for an explicit candidate set (used by tests to
/// score the planted block).
pub fn g_value(g: &ReviewGraph, users: &[usize], objects: &[usize], w: Weighting) -> f64 {
    let us: HashSet<usize> = users.iter().copied().collect();
    let os: HashSet<usize> = objects.iter().copied().collect();
    let f: f64 = g
        .edges
        .iter()
        .filter(|&&(u, o)| us.contains(&u) && os.contains(&o))
        .map(|&(_, o)| column_weight(g.obj_deg[o], w))
        .sum();
    f / (users.len() + objects.len()) as f64
}

/// Precision/recall F-measure of a detection against the planted block
/// (users and objects pooled).
pub fn f_measure(det: &Detection, g: &ReviewGraph) -> f64 {
    let truth: HashSet<usize> = g
        .fraud_users
        .iter()
        .map(|&u| u)
        .chain(g.fraud_objects.iter().map(|&o| g.n_users + o))
        .collect();
    let found: HashSet<usize> = det
        .users
        .iter()
        .map(|&u| u)
        .chain(det.objects.iter().map(|&o| g.n_users + o))
        .collect();
    let hit = truth.intersection(&found).count() as f64;
    if hit == 0.0 {
        return 0.0;
    }
    let precision = hit / found.len() as f64;
    let recall = hit / truth.len() as f64;
    2.0 * precision * recall / (precision + recall)
}

/// Greedy peeling: delete the minimum-weighted-degree node, track the
/// best g(S) over the shrinking set, return the best set seen.
///
/// Nodes are 0..n_users (users) then n_users..n_users+n_objects
/// (objects). A lazy min-heap of (weighted degree, node) makes this
/// O(|E| log |V|): pop, skip entries whose stored weight is stale
/// (compare against a live weight array), decrement neighbors on
/// removal and push their fresh weights.
pub fn fraudar(g: &ReviewGraph, w: Weighting) -> Detection {
    let _ = (g, w);
    todo!("greedy peeling with a lazy heap, tracking the best g(S)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_graph::{fraud_instance, seeded_rng, FraudConfig};

    #[test]
    fn log_weighted_peeling_recovers_the_block() {
        let mut rng = seeded_rng(10);
        let g = fraud_instance(&mut rng, &FraudConfig::default());
        let det = fraudar(&g, Weighting::LogDegree);
        let f = f_measure(&det, &g);
        assert!(f >= 0.9, "no camouflage: F = {f}");
        // Theorem 2 gives g(returned) >= g_OPT / 2, and the planted
        // block's g is a lower bound on g_OPT.
        let planted = g_value(&g, &g.fraud_users, &g.fraud_objects, Weighting::LogDegree);
        assert!(
            det.g >= planted / 2.0,
            "g(returned) = {} < half of g(planted) = {planted}",
            det.g
        );
    }

    #[test]
    fn camouflage_drags_unweighted_peeling_into_the_popular_core() {
        // Camouflage edges glue the block to the power users x hit
        // products community; without column weights the densest set is
        // their union — precision collapses (the paper's "normal
        // hyperbolic community" swallowing the detection).
        let mut rng = seeded_rng(11);
        let cfg = FraudConfig {
            camo_ratio: 2.0,
            ..FraudConfig::default()
        };
        let g = fraud_instance(&mut rng, &cfg);
        let unw_f = f_measure(&fraudar(&g, Weighting::Unweighted), &g);
        assert!(unw_f < 0.7, "camo 2x: unweighted F = {unw_f}");
    }

    #[test]
    fn camouflage_does_not_move_log_weighted_detection() {
        // Camouflage lands on honest columns: the fraud block's own
        // edges and column degrees never change (Theorem 3).
        let mut rng = seeded_rng(12);
        let cfg = FraudConfig {
            camo_ratio: 2.0,
            ..FraudConfig::default()
        };
        let g = fraud_instance(&mut rng, &cfg);
        let f = f_measure(&fraudar(&g, Weighting::LogDegree), &g);
        assert!(f >= 0.9, "camo 2x: log-weighted F = {f}");
    }
}
