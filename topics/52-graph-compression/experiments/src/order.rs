//! Cheap vertex orderings (PROVIDED): random, degree-sorted, BFS.
//!
//! Every function returns `new_id[old]`, a permutation of `0..n`.

use crate::csr::Csr;
use crate::gen::shuffle_perm;

pub fn random_order(n: usize, seed: u64) -> Vec<u32> {
    shuffle_perm(n, seed)
}

/// Descending in-degree (the vertices most often *gathered from* get the
/// smallest ids), ties by old id. Gorder's paper calls this InDegSort.
pub fn degree_order(g: &Csr) -> Vec<u32> {
    let mut indeg = vec![0u32; g.n];
    for &t in &g.tgt {
        indeg[t as usize] += 1;
    }
    let mut by: Vec<u32> = (0..g.n as u32).collect();
    by.sort_by_key(|&v| (std::cmp::Reverse(indeg[v as usize]), v));
    invert(&by)
}

/// Breadth-first over out-edges. Roots are taken in descending out-degree
/// (ties by id), so every vertex gets visited; ids are handed out in visit
/// order.
pub fn bfs_order(g: &Csr) -> Vec<u32> {
    let mut roots: Vec<u32> = (0..g.n as u32).collect();
    roots.sort_by_key(|&v| (std::cmp::Reverse(g.degree(v as usize)), v));
    let mut new_id = vec![u32::MAX; g.n];
    let mut next = 0u32;
    let mut queue: Vec<u32> = Vec::new();
    for &r in &roots {
        if new_id[r as usize] != u32::MAX {
            continue;
        }
        new_id[r as usize] = next;
        next += 1;
        queue.clear();
        queue.push(r);
        let mut head = 0;
        while head < queue.len() {
            let v = queue[head] as usize;
            head += 1;
            for &t in g.neighbors(v) {
                if new_id[t as usize] == u32::MAX {
                    new_id[t as usize] = next;
                    next += 1;
                    queue.push(t);
                }
            }
        }
    }
    new_id
}

/// `order[new] = old` -> `new_id[old] = new`.
pub fn invert(order: &[u32]) -> Vec<u32> {
    let mut inv = vec![0u32; order.len()];
    for (new, &old) in order.iter().enumerate() {
        inv[old as usize] = new as u32;
    }
    inv
}

pub fn is_permutation(p: &[u32]) -> bool {
    let mut seen = vec![false; p.len()];
    for &x in p {
        if x as usize >= p.len() || seen[x as usize] {
            return false;
        }
        seen[x as usize] = true;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{community_graph, GenParams};

    #[test]
    fn all_provided_orders_are_permutations() {
        let p = community_graph(GenParams::SMALL);
        assert!(is_permutation(&random_order(p.g.n, 1)));
        assert!(is_permutation(&degree_order(&p.g)));
        assert!(is_permutation(&bfs_order(&p.g)));
    }

    #[test]
    fn degree_order_puts_the_biggest_in_degree_first() {
        let g = Csr::from_edges(4, vec![(0, 2), (1, 2), (3, 2), (0, 1)]);
        assert_eq!(degree_order(&g)[2], 0);
    }
}
