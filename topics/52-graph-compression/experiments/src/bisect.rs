//! Lane 2 (EXERCISE, stub): recursive graph bisection as a vertex ordering.
//!
//! Implement Dhulipala et al., "Compressing Graphs and Indexes with
//! Recursive Graph Bisection" (KDD'16; see reading-recursive-bisection.md).
//!
//! The model (their Section 3.1): *query* vertices are the adjacency lists —
//! here every vertex `q` with its out-list — and *data* vertices are the ids
//! to be relabelled — here every vertex. Find the permutation of the data
//! vertices that minimises `sum over q, over consecutive neighbours of
//! log(pi(u_{i+1}) - pi(u_i))`.
//!
//! The algorithm (their Algorithms 1 and 2):
//!
//! 1. Split the data vertices into two halves V1 and V2 of equal size
//!    (initialise with a seeded random split).
//! 2. Repeat up to `iters` times: for every data vertex compute its *move
//!    gain*, the change of the cost
//!    `sum_q  deg1(q)·log2(n1/(deg1(q)+1)) + deg2(q)·log2(n2/(deg2(q)+1))`
//!    when it moves to the other half (`deg_i(q)` = neighbours of `q` in
//!    `V_i`, `n_i = |V_i|`); sort each half by gain descending; swap pairs
//!    while `gain(v)+gain(u) > 0`. Stop when nothing swaps.
//! 3. Recurse on V1 and V2 (the id range `[lo, mid)` and `[mid, hi)`) until a
//!    range is smaller than `leaf` vertices; leaves keep their current order.
//!
//! Contract (the tests below):
//! * the result is a permutation of `0..n` with `new_id[old]` semantics;
//! * the same `seed` gives the same permutation;
//! * on `GenParams::SMALL` with random ids it reaches a `log_gap` at least
//!   20% below the random order's and below BFS's.
//!
//! You need the in-neighbour lists to find each data vertex's queries:
//! `Csr::transpose` is provided.

use crate::csr::Csr;

/// `new_id[old]`. `leaf`: stop recursing below this many vertices (the paper
/// stops at depth log2(n) - 5, i.e. leaves of ~32). `iters`: iterations per
/// bisection (the paper uses 20).
pub fn bisection_order(g: &Csr, leaf: usize, iters: usize, seed: u64) -> Vec<u32> {
    let _ = (g, leaf, iters, seed);
    todo!("lane 2: recursive graph bisection (Dhulipala et al., KDD'16, Algorithms 1-2)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gapcode::log_gap;
    use crate::gen::{community_graph, shuffle_perm, GenParams};
    use crate::order::{bfs_order, is_permutation};

    fn scrambled() -> Csr {
        let p = community_graph(GenParams::SMALL);
        p.g.permute(&shuffle_perm(p.g.n, 11))
    }

    #[test]
    fn bisection_returns_a_permutation() {
        let g = scrambled();
        assert!(is_permutation(&bisection_order(&g, 32, 8, 1)));
    }

    #[test]
    fn bisection_is_deterministic() {
        let g = scrambled();
        assert_eq!(bisection_order(&g, 32, 8, 1), bisection_order(&g, 32, 8, 1));
    }

    #[test]
    fn bisection_beats_random_and_bfs_on_log_gap() {
        let g = scrambled();
        let rand = log_gap(&g);
        let bfs = log_gap(&g.permute(&bfs_order(&g)));
        let bp = log_gap(&g.permute(&bisection_order(&g, 32, 8, 1)));
        assert!(bp < 0.8 * rand, "bp {bp} vs random {rand}");
        assert!(bp < bfs, "bp {bp} vs bfs {bfs}");
    }
}
