//! Undirected graph as CSR (both directions stored), plus a stochastic
//! block model generator — the standard synthetic for graph ML because it
//! comes with GROUND-TRUTH community labels to test embeddings against.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub struct Csr {
    pub n: usize,
    pub offsets: Vec<usize>,
    pub targets: Vec<u32>, // sorted per row
}

impl Csr {
    pub fn neigh(&self, v: u32) -> &[u32] {
        &self.targets[self.offsets[v as usize]..self.offsets[v as usize + 1]]
    }
    pub fn degree(&self, v: u32) -> usize {
        self.offsets[v as usize + 1] - self.offsets[v as usize]
    }
    /// Directed edge count (2x the undirected count).
    pub fn m(&self) -> usize {
        self.targets.len()
    }
    pub fn has_edge(&self, u: u32, v: u32) -> bool {
        self.neigh(u).binary_search(&v).is_ok()
    }
}

/// Symmetrizes, drops self-loops, dedups, sorts each row.
pub fn from_edges(n: usize, edges: &[(u32, u32)]) -> Csr {
    let mut both: Vec<(u32, u32)> = Vec::with_capacity(edges.len() * 2);
    for &(u, v) in edges {
        if u == v {
            continue;
        }
        both.push((u, v));
        both.push((v, u));
    }
    both.sort_unstable();
    both.dedup();
    let mut offsets = vec![0usize; n + 1];
    for &(u, _) in &both {
        offsets[u as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let targets = both.into_iter().map(|(_, v)| v).collect();
    Csr { n, offsets, targets }
}

/// Stochastic block model: `blocks` communities of `per_block` vertices.
/// Intra-block pairs get an edge with prob `p_in` (exact Bernoulli sweep);
/// inter-block edges are sampled by expected COUNT (p_out * inter_pairs)
/// with random cross-block endpoints — same expectation, O(m) not O(n^2).
/// Returns (graph, labels) where labels[v] = block id.
pub fn gen_sbm(
    blocks: usize,
    per_block: usize,
    p_in: f64,
    p_out: f64,
    seed: u64,
) -> (Csr, Vec<u32>) {
    let n = blocks * per_block;
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut edges: Vec<(u32, u32)> = Vec::new();
    for b in 0..blocks {
        let base = (b * per_block) as u32;
        for i in 0..per_block as u32 {
            for j in (i + 1)..per_block as u32 {
                if rng.gen::<f64>() < p_in {
                    edges.push((base + i, base + j));
                }
            }
        }
    }
    let total_pairs = n * (n - 1) / 2;
    let intra_pairs = blocks * (per_block * (per_block - 1) / 2);
    let inter_edges = (p_out * (total_pairs - intra_pairs) as f64).round() as usize;
    for _ in 0..inter_edges {
        loop {
            let u = rng.gen_range(0..n as u32);
            let v = rng.gen_range(0..n as u32);
            if u as usize / per_block != v as usize / per_block {
                edges.push((u, v));
                break;
            }
        }
    }
    let labels = (0..n).map(|v| (v / per_block) as u32).collect();
    (from_edges(n, &edges), labels)
}

/// `cliques` cliques of size `k`, adjacent cliques bridged by one edge in a
/// ring. Used by the node2vec tests: exploration must cross bridges.
pub fn gen_ring_of_cliques(cliques: usize, k: usize) -> Csr {
    let n = cliques * k;
    let mut edges = Vec::new();
    for c in 0..cliques {
        let base = (c * k) as u32;
        for i in 0..k as u32 {
            for j in (i + 1)..k as u32 {
                edges.push((base + i, base + j));
            }
        }
        let next_base = (((c + 1) % cliques) * k) as u32;
        edges.push((base + (k as u32 - 1), next_base));
    }
    from_edges(n, &edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csr_symmetric_sorted() {
        let g = from_edges(4, &[(0, 1), (1, 2), (2, 2), (1, 0)]);
        assert_eq!(g.neigh(0), &[1]);
        assert_eq!(g.neigh(1), &[0, 2]);
        assert_eq!(g.neigh(2), &[1]); // self-loop dropped
        assert_eq!(g.m(), 4);
        assert!(g.has_edge(2, 1) && !g.has_edge(0, 2));
    }

    #[test]
    fn sbm_is_assortative() {
        let (g, labels) = gen_sbm(4, 64, 0.2, 0.005, 42);
        assert_eq!(g.n, 256);
        let (mut intra, mut inter) = (0usize, 0usize);
        for u in 0..g.n as u32 {
            for &v in g.neigh(u) {
                if labels[u as usize] == labels[v as usize] {
                    intra += 1;
                } else {
                    inter += 1;
                }
            }
        }
        // ~12.6 intra vs ~1 inter neighbors per vertex at these params
        assert!(intra > inter * 5, "intra {} inter {}", intra, inter);
    }

    #[test]
    fn ring_of_cliques_shape() {
        let g = gen_ring_of_cliques(4, 8);
        assert_eq!(g.n, 32);
        // clique-internal vertex: degree 7; bridge endpoints: 8
        assert_eq!(g.degree(1), 7);
        assert_eq!(g.degree(7), 8);
    }
}
