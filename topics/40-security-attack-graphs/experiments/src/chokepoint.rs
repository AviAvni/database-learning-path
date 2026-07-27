//! Choke points: the defender's half of the attack graph.
//!
//! Lane 1 says 90% of the directory can become Domain Admin. That is a
//! finding, not a work order. The work order is: *which few nodes, if
//! removed, delete the most attack paths?* Ammann et al. (CCS'02 §2.3)
//! name this exactly — "what set of exploits (edges) or attributes
//! (nodes) in our graph must be removed to disconnect the goal state
//! from the initial state" — and then say "standard graph analysis
//! algorithms can be applied". This module is that sentence, made
//! precise and measured.
//!
//! The naive answer costs |V| traversals: delete a node, re-run
//! reachability, count who lost their path. The exact answer costs one
//! pass, and it is a classic:
//!
//! > In the **reverse** graph rooted at tier zero, node `d` **dominates**
//! > node `u` iff every path from tier zero to `u` passes through `d` —
//! > which is to say, every attack path from `u` to tier zero passes
//! > through `d`. So deleting `d` disconnects *exactly* the nodes in
//! > `d`'s dominator-tree subtree.
//!
//! One dominator tree therefore prices every single-node remediation at
//! once. That is the same trick compilers use for control-flow analysis
//! (Lengauer–Tarjan, or the iterative Cooper–Harvey–Kennedy formulation
//! used here), pointed at an identity graph.

use crate::ad_graph::{reaches, AdGraph};

/// Number of users with an attack path to tier zero.
pub fn exposure(g: &AdGraph, removed: Option<usize>) -> usize {
    let mut rev = g.reverse_adj();
    if let Some(d) = removed {
        // Deleting a node deletes its incident edges.
        rev[d].clear();
        for list in rev.iter_mut() {
            list.retain(|&x| x != d);
        }
    }
    let seen = reaches(&rev, g.tier_zero);
    (0..g.n_users)
        .filter(|&u| seen[u] && Some(u) != removed)
        .count()
}

#[allow(dead_code)]
/// Reverse-postorder over the reverse graph, rooted at `root`.
/// Returns `(order, index_in_order)`; unreachable nodes get `usize::MAX`.
fn reverse_postorder(rev: &[Vec<usize>], root: usize) -> (Vec<usize>, Vec<usize>) {
    let n = rev.len();
    let mut visited = vec![false; n];
    let mut post = Vec::new();
    // Iterative DFS so a 10M-node directory does not blow the stack.
    let mut stack = vec![(root, 0usize)];
    visited[root] = true;
    while let Some(&mut (v, ref mut i)) = stack.last_mut() {
        if *i < rev[v].len() {
            let w = rev[v][*i];
            *i += 1;
            if !visited[w] {
                visited[w] = true;
                stack.push((w, 0));
            }
        } else {
            post.push(v);
            stack.pop();
        }
    }
    post.reverse();
    let mut idx = vec![usize::MAX; n];
    for (i, &v) in post.iter().enumerate() {
        idx[v] = i;
    }
    (post, idx)
}

/// Immediate dominators of the reverse graph rooted at tier zero.
///
/// Cooper, Harvey & Kennedy's iterative algorithm: walk nodes in reverse
/// postorder, set each node's idom to the "intersection" (walk both up
/// the tree until the RPO numbers meet) of its already-processed
/// predecessors, repeat until nothing changes. Simple, and fast in
/// practice on the shallow, wide graphs directories produce.
pub fn immediate_dominators(g: &AdGraph) -> Vec<Option<usize>> {
    let _ = g;
    todo!(
        "iterative dominators over the reverse graph rooted at tier zero: \n\
         walk `reverse_postorder` skipping the root, set each node's idom \n\
         to the pairwise `intersect` of its already-processed predecessors \n\
         (predecessors in the reverse graph are `g.adj[v]`), and repeat \n\
         until a full sweep changes nothing. `intersect(a, b)` walks both \n\
         up the current idom chain until their RPO numbers meet. Return \n\
         None for the root and for unreachable nodes."
    )
}

/// For every node, how many exposed users lose *all* attack paths if
/// that node is removed — computed for all nodes in one pass.
///
/// This is the dominator-tree subtree user count. `blast[tier_zero]` is
/// the total exposure (removing tier zero disconnects everyone).
pub fn blast_radius(g: &AdGraph) -> Vec<usize> {
    let _ = g;
    todo!(
        "seed every reachable *user* node with 1, then accumulate up the \n\
         dominator tree. Reverse postorder puts a node before its idom, so \n\
         one backwards sweep over that order pushes every count to its \n\
         parent exactly once."
    )
}

/// Rank actionable choke points (groups and computers — you cannot
/// delete your users) by blast radius, highest first.
pub fn rank_chokepoints(g: &AdGraph) -> Vec<(usize, usize)> {
    let blast = blast_radius(g);
    let mut v: Vec<(usize, usize)> = (0..g.n_nodes())
        .filter(|&d| d != g.tier_zero && !g.is_user(d) && blast[d] > 0)
        .map(|d| (d, blast[d]))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// The honest baseline: remove one node, recompute reachability, repeat.
/// O(|V| · (|V| + |E|)). Used by the tests to prove the dominator
/// attribution is not an approximation.
pub fn blast_radius_naive(g: &AdGraph) -> Vec<usize> {
    let total = exposure(g, None);
    (0..g.n_nodes())
        .map(|d| {
            if d == g.tier_zero {
                total
            } else {
                total - exposure(g, Some(d))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ad_graph::{ad_instance, seeded_rng, AdConfig};

    fn small(cfg: AdConfig) -> AdGraph {
        let mut rng = seeded_rng(5);
        ad_instance(
            &mut rng,
            &AdConfig {
                n_users: 120,
                n_groups: 40,
                n_computers: 60,
                group_nesting: 50,
                admin_edges: 80,
                privileged_users: 10,
                privileged_sessions: 40,
                ordinary_sessions: 100,
                acl_edges: 12,
                ..cfg
            },
        )
    }

    #[test]
    fn dominators_price_every_removal_exactly() {
        // The claim that makes the one-pass version legitimate: the
        // dominator subtree count equals what you get by deleting the
        // node and re-running reachability. Not an approximation.
        let g = small(AdConfig::tiered());
        let fast = blast_radius(&g);
        let slow = blast_radius_naive(&g);
        for d in 0..g.n_nodes() {
            assert_eq!(fast[d], slow[d], "node {d}");
        }
    }

    #[test]
    fn removing_the_top_chokepoint_actually_reduces_exposure() {
        let g = small(AdConfig::tiered());
        let ranked = rank_chokepoints(&g);
        assert!(!ranked.is_empty(), "no actionable choke point found");
        let (top, predicted) = ranked[0];
        let before = exposure(&g, None);
        let after = exposure(&g, Some(top));
        assert_eq!(before - after, predicted);
        assert!(predicted > 0);
    }

    #[test]
    fn a_flat_directory_has_no_single_node_choke_point() {
        // Same exposure, no remediation. Two unmanaged service-account
        // groups and one Domain Admin token on a workstation are enough
        // to route around every gateway, so no node dominates anyone
        // and the blast radius of every single cut is zero. That is the
        // finding — not a bug in the analysis.
        let flat = small(AdConfig::default());
        let tiered = small(AdConfig::tiered());
        assert_eq!(exposure(&flat, None), exposure(&tiered, None));
        assert!(rank_chokepoints(&flat).is_empty());
        assert!(!rank_chokepoints(&tiered).is_empty());
    }

    #[test]
    fn the_dominator_pass_also_agrees_on_the_flat_directory() {
        let g = small(AdConfig::default());
        let fast = blast_radius(&g);
        let slow = blast_radius_naive(&g);
        for d in 0..g.n_nodes() {
            assert_eq!(fast[d], slow[d], "node {d}");
        }
    }

    #[test]
    fn tier_zero_dominates_everyone() {
        let g = small(AdConfig::tiered());
        let blast = blast_radius(&g);
        assert_eq!(blast[g.tier_zero], exposure(&g, None));
    }
}
