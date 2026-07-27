//! Root-cause localization: turning an alert storm into a ranked list.
//!
//! Two families, and this module implements one of each.
//!
//! **Random-walk localization** (MonitorRank, SIGMETRICS'13, and the
//! shape most modern RCA tools use). Build the call graph, then walk it
//! *backwards* from the symptom: from a failing front end, step toward
//! the things it called, preferring edges whose failure pattern
//! correlates with the symptom. Nodes visited often are candidates. It
//! is personalized PageRank — the same primitive as topic 38's HippoRAG
//! and topic 42's Pixie — pointed at a dependency graph, and the reason
//! it beats a per-node score is that it uses the *shape* of the graph
//! rather than only the values on the nodes.
//!
//! The walk needs three edge types, and getting them right is most of
//! the work:
//!
//! ```text
//!   backward  caller → callee    "did the thing I called break?"
//!   forward   callee → caller    escape hatch, so a walk that entered a
//!                                dead end can climb back out
//!   self      s → s              stay put when no neighbour correlates
//!                                better than you do
//! ```
//!
//! **Probabilistic inference** (Sherlock, SIGCOMM'07). Model each node's
//! state as `(P_up, P_troubled, P_down)`, propagate through noisy-max
//! meta-nodes, and score every *assignment vector* — an assignment of
//! state to every root-cause node — by how well it explains the
//! observations. There are 3^r assignment vectors, so Ferret uses
//! Observation 3.1: "it is very likely that at any point in time only a
//! few root-cause nodes are troubled or down", and evaluates only
//! vectors with at most `k` abnormal nodes — at most `(2r)^k` of them,
//! with approximation error that "becomes vanishingly small for k = 4".
//! This module implements the k = 1 case, which is already enough to
//! beat both per-node baselines.

use crate::services::{Topology, Workload};
use std::collections::HashMap;

/// Result of a localization run: services ranked by suspicion.
pub type Ranking = Vec<(u32, f64)>;

fn sort_desc(mut v: Ranking) -> Ranking {
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
    v
}

/// Normalize a score map into a sorted ranking over all services.
pub fn rank(n: usize, scores: &HashMap<u32, f64>) -> Ranking {
    sort_desc(
        (0..n as u32)
            .map(|s| (s, *scores.get(&s).unwrap_or(&0.0)))
            .collect(),
    )
}

/// **Random-walk localization (STUB).**
///
/// Start at the alerting front ends. At each step, move to a neighbour
/// chosen in proportion to that neighbour's failure correlation with the
/// symptom, over the three edge types above; restart at a front end with
/// probability `alpha`. Return visit counts as scores.
///
/// `corr` is the per-service symptom correlation from
/// `services::failure_correlation`.
pub fn random_walk_rca(
    rng: &mut rand_chacha::ChaCha8Rng,
    t: &Topology,
    corr: &[f64],
    steps: usize,
    alpha: f64,
    backward_only: bool,
) -> Ranking {
    let _ = (rng, t, corr, steps, alpha, backward_only);
    todo!(
        "seed the walk at a random alerting frontend. At service s, build the candidate set: backward neighbours t.deps[s] (things s called), plus - unless backward_only - forward neighbours t.rdeps[s] and s itself. Weight each candidate by max(corr, 0), damping the forward edges (they exist so the walk can escape a dead end, not so it can drift back to the frontend). Pick proportionally, count the visit, restart at a frontend with probability alpha, and return rank(n, visits)."
    )
}

/// **Sherlock-style single-fault scoring (STUB).**
///
/// Ferret's `k = 1` case: for each candidate root cause `c`, assume `c`
/// is the only abnormal node, propagate that assumption to the
/// observation nodes (the front ends) through the dependency graph, and
/// score how well the predicted front-end failure rates match the
/// observed ones. Return candidates ranked by score.
///
/// Propagation is noisy-max: a caller is affected if any callee it
/// depends on is affected, damped by `propagation` per hop — which is
/// the same attenuation `services::run_workload` applies when it
/// generates the data, so a correct implementation is inverting the
/// generative model rather than guessing.
pub fn sherlock_single_fault(t: &Topology, w: &Workload, propagation: f64) -> Ranking {
    let _ = (t, w, propagation);
    todo!(
        "Ferret with k=1. Use services::participation to get P(service on path | entry frontend). For each candidate c, predict each frontend's failure rate as severity * participation * propagation, fit `severity` by least squares - then CLAMP it to [0,1], because a severity is a probability. The clamp is what separates the candidates: a service that is simply not on enough requests would need severity > 1 to explain the observed rates. Score by negative squared residual."
    )
}

/// Was the true root cause ranked in the top `k`?
pub fn top_k_hit(r: &Ranking, root_cause: u32, k: usize) -> bool {
    r.iter().take(k).any(|&(s, _)| s == root_cause)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{
        failure_correlation, rank_by_error_rate, rank_by_failures, rank_of, run_workload,
        seeded_rng, topology, TopologyConfig,
    };

    fn setup(seed: u64) -> (Topology, Workload, TopologyConfig) {
        let cfg = TopologyConfig::default();
        let mut rng = seeded_rng(seed);
        let t = topology(&mut rng, &cfg);
        let w = run_workload(&mut rng, &t, &cfg);
        (t, w, cfg)
    }

    #[test]
    fn the_walk_beats_both_per_node_baselines() {
        let (t, w, _) = setup(1);
        let corr = failure_correlation(&t, &w);
        let mut rng = seeded_rng(99);
        let r = random_walk_rca(&mut rng, &t, &corr, 200_000, 0.15, false);
        assert!(
            top_k_hit(&r, t.root_cause, 3),
            "root cause {} not in top 3: {:?}",
            t.name(t.root_cause),
            &r[..5]
        );
        let by_count = rank_of(&rank_by_failures(&w), t.root_cause);
        let by_rate = rank_of(&rank_by_error_rate(&w), t.root_cause);
        let by_walk = rank_of(&r, t.root_cause);
        assert!(
            by_walk < by_count,
            "walk ranked it {by_walk}, failure count {by_count}"
        );
        assert!(
            by_walk <= by_rate,
            "walk ranked it {by_walk}, error rate {by_rate}"
        );
    }

    #[test]
    fn walking_only_backwards_is_not_enough() {
        // A backward-only walk drains into the leaves and cannot climb
        // back out of a dead end, so it spreads its mass over every
        // infra node instead of concentrating on the broken one. The
        // forward and self edges are what make the correlation weights
        // bite.
        let (t, w, _) = setup(1);
        let corr = failure_correlation(&t, &w);
        let mut rng = seeded_rng(99);
        let full = random_walk_rca(&mut rng, &t, &corr, 200_000, 0.15, false);
        let mut rng = seeded_rng(99);
        let back = random_walk_rca(&mut rng, &t, &corr, 200_000, 0.15, true);
        assert!(
            rank_of(&full, t.root_cause) <= rank_of(&back, t.root_cause),
            "backward-only ranked {} vs full {}",
            rank_of(&back, t.root_cause),
            rank_of(&full, t.root_cause)
        );
    }

    #[test]
    fn the_walk_is_stable_across_seeds() {
        // An operator cannot act on a ranking that changes every time
        // they refresh the page.
        let (t, w, _) = setup(1);
        let corr = failure_correlation(&t, &w);
        let mut hits = 0;
        for seed in 0..5u64 {
            let mut rng = seeded_rng(seed);
            let r = random_walk_rca(&mut rng, &t, &corr, 200_000, 0.15, false);
            if top_k_hit(&r, t.root_cause, 3) {
                hits += 1;
            }
        }
        assert_eq!(hits, 5, "only {hits}/5 seeds put the cause in the top 3");
    }

    #[test]
    fn inference_localizes_the_single_fault() {
        // Sherlock's k=1 Ferret case. It has an advantage the walk does
        // not — it knows the propagation model — and should use it.
        let (t, w, cfg) = setup(1);
        let r = sherlock_single_fault(&t, &w, cfg.propagation);
        assert!(
            top_k_hit(&r, t.root_cause, 3),
            "root cause {} not in top 3: {:?}",
            t.name(t.root_cause),
            &r[..5]
        );
    }

    #[test]
    fn localization_survives_a_different_topology() {
        let (t, w, cfg) = setup(7);
        let corr = failure_correlation(&t, &w);
        let mut rng = seeded_rng(3);
        let r = random_walk_rca(&mut rng, &t, &corr, 200_000, 0.15, false);
        assert!(top_k_hit(&r, t.root_cause, 5));
        assert!(top_k_hit(&sherlock_single_fault(&t, &w, cfg.propagation), t.root_cause, 5));
    }
}
