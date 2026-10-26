//! The two workloads the orderings are timed on (PROVIDED).
//!
//! * `scan_csr` — a gather: for every vertex, sum `vals[t]` over its list.
//!   One pass is `m` random-ish reads of an `n`-element array, the memory
//!   pattern of a PageRank pull step or a SpMV over a boolean matrix.
//! * `bfs` — top-down breadth-first search over out-edges from one source.

use crate::csr::Csr;

pub fn scan_csr(g: &Csr, vals: &[u64]) -> u64 {
    let mut total = 0u64;
    for v in 0..g.n {
        let mut s = 0u64;
        for &t in g.neighbors(v) {
            s = s.wrapping_add(vals[t as usize]);
        }
        total = total.wrapping_add(s);
    }
    total
}

/// Returns (vertices reached, edges examined, levels). All three are
/// invariant under relabelling, which is the bench's sanity check.
pub fn bfs(g: &Csr, src: usize) -> (usize, usize, usize) {
    let mut dist = vec![u32::MAX; g.n];
    let mut frontier = vec![src as u32];
    let mut next = Vec::new();
    dist[src] = 0;
    let (mut reached, mut edges, mut levels) = (1usize, 0usize, 0usize);
    while !frontier.is_empty() {
        for &v in &frontier {
            for &t in g.neighbors(v as usize) {
                edges += 1;
                if dist[t as usize] == u32::MAX {
                    dist[t as usize] = levels as u32 + 1;
                    reached += 1;
                    next.push(t);
                }
            }
        }
        levels += 1;
        std::mem::swap(&mut frontier, &mut next);
        next.clear();
    }
    (reached, edges, levels - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bfs_on_a_path() {
        let g = Csr::from_edges(4, vec![(0, 1), (1, 2), (2, 3)]);
        assert_eq!(bfs(&g, 0), (4, 3, 3));
        assert_eq!(bfs(&g, 2), (2, 1, 1));
    }

    #[test]
    fn scan_is_invariant_under_relabelling() {
        let p = crate::gen::community_graph(crate::gen::GenParams::SMALL);
        let perm = crate::gen::shuffle_perm(p.g.n, 5);
        let vals: Vec<u64> = (0..p.g.n as u64).map(|x| x.wrapping_mul(0x9E3779B97F4A7C15)).collect();
        let mut vals2 = vec![0u64; p.g.n];
        for (old, &nw) in perm.iter().enumerate() {
            vals2[nw as usize] = vals[old];
        }
        assert_eq!(scan_csr(&p.g, &vals), scan_csr(&p.g.permute(&perm), &vals2));
    }
}
