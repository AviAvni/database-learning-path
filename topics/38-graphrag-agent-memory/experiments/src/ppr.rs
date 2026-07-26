//! STUB — Personalized PageRank retrieval (HippoRAG §2.3).
//!
//! Standard PageRank spreads restart mass uniformly; the *personalized*
//! variant restarts only at the query's seed nodes, so the stationary
//! distribution measures "how reachable from what the query mentions".
//! HippoRAG runs it with damping 0.5 over an OpenIE graph and ranks
//! passages by the PPR mass of the nodes they mention — multi-hop
//! retrieval in a single step, no iterative LLM loop.
//!
//! Why it fixes kg.rs's collapse: mass flowing out of seed u and mass
//! flowing out of seed w SUM at the one candidate connected to both.
//! Every dead-end candidate collects from one seed; the answer collects
//! from two. Association becomes arithmetic.
//!
//! Contracts (the tests): the result is a probability distribution;
//! mass decays with hop distance from the seed; on 2-hop path-finding
//! instances the answer's PPR rank is 1 while mention ranking is chance.

use crate::kg::{Instance, Kg};

/// Personalized PageRank by power iteration.
///
/// pi = (1 - damping) * restart + damping * pi * W
///
/// where restart is uniform over `seeds` (zero elsewhere) and W is the
/// column-stochastic walk on `kg.adj` (each node splits its mass evenly
/// over its neighbors; a node with no neighbors returns its mass to the
/// restart vector). `iters` power iterations from the restart vector.
pub fn ppr(kg: &Kg, seeds: &[usize], damping: f64, iters: usize) -> Vec<f64> {
    let _ = (kg, seeds, damping, iters);
    todo!("power iteration: restart at seeds, walk the adjacency")
}

/// Rank of the true answer among an instance's candidates by PPR mass
/// (1 = best). HippoRAG's damping: 0.5.
pub fn ppr_rank(inst: &Instance, damping: f64, iters: usize) -> usize {
    let pi = ppr(&inst.kg, &inst.seeds, damping, iters);
    let mut order: Vec<usize> = inst.candidates.clone();
    order.sort_by(|&a, &b| pi[b].total_cmp(&pi[a]));
    1 + order.iter().position(|&c| c == inst.answer).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kg::{mean_rank, mention_rank, path_finding_instance, seeded_rng};

    #[test]
    fn ppr_is_a_distribution() {
        let inst = path_finding_instance(2, 8);
        let pi = ppr(&inst.kg, &inst.seeds, 0.5, 30);
        assert_eq!(pi.len(), inst.kg.n_nodes);
        assert!(pi.iter().all(|&p| p >= 0.0));
        let sum: f64 = pi.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "sums to {sum}");
    }

    #[test]
    fn mass_decays_with_distance_from_seed() {
        // A 6-node chain seeded at node 0: pi must be strictly
        // decreasing along the chain.
        let mut kg = Kg::new(6);
        for i in 0..5 {
            kg.add_fact(i, i + 1);
        }
        let pi = ppr(&kg, &[0], 0.5, 60);
        for i in 1..6 {
            assert!(
                pi[i - 1] > pi[i],
                "pi[{}]={} !> pi[{}]={}",
                i - 1,
                pi[i - 1],
                i,
                pi[i]
            );
        }
    }

    #[test]
    fn association_ranks_the_meet_node_first() {
        // The same 2-hop instances where mention ranking is chance (~9):
        // the answer must rank 1 by PPR on every instance.
        let inst = path_finding_instance(2, 8);
        assert_eq!(ppr_rank(&inst, 0.5, 30), 1);

        let mut rng = seeded_rng(3);
        let mention = mean_rank(&mut rng, 100, 2, 8, mention_rank);
        let ppr_mean = mean_rank(&mut rng, 100, 2, 8, |_, inst| ppr_rank(inst, 0.5, 30));
        assert!(mention > 7.0, "mention mean rank {mention} should be chance");
        assert!(ppr_mean < 1.05, "PPR mean rank {ppr_mean} should be ~1");
    }
}
