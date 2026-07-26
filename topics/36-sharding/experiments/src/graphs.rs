//! PROVIDED — deterministic graph generators and the edge-cut metric.
//!
//! Two workloads for the partitioner: a planted-partition graph (real
//! community structure a partitioner can find) and a preferential-
//! attachment graph (the power-law degree tail of natural graphs —
//! PowerGraph's whole argument is that these have no good edge-cuts).

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub struct Graph {
    pub n: usize,
    pub edges: Vec<(u32, u32)>,
}

impl Graph {
    pub fn adjacency(&self) -> Vec<Vec<u32>> {
        let mut adj = vec![Vec::new(); self.n];
        for &(u, v) in &self.edges {
            adj[u as usize].push(v);
            adj[v as usize].push(u);
        }
        adj
    }
}

/// Planted partition: `k` communities of `per` vertices; each vertex
/// draws `d_in` random intra-community edges and `d_out` cross-community
/// edges. Cross-community edges are the cut floor: d_out/(d_in+d_out).
pub fn community(k: usize, per: usize, d_in: usize, d_out: usize, seed: u64) -> Graph {
    let n = k * per;
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut edges = Vec::with_capacity(n * (d_in + d_out));
    for v in 0..n {
        let c = v / per;
        for _ in 0..d_in {
            let u = c * per + rng.gen_range(0..per);
            if u != v {
                edges.push((v as u32, u as u32));
            }
        }
        for _ in 0..d_out {
            let u = rng.gen_range(0..n);
            if u / per != c {
                edges.push((v as u32, u as u32));
            }
        }
    }
    Graph { n, edges }
}

/// Preferential attachment (Barabási–Albert): each new vertex attaches
/// `m` edges to endpoints sampled degree-proportionally from the running
/// edge list. Power-law degree tail, like Twitter's follower graph.
pub fn power_law(n: usize, m: usize, seed: u64) -> Graph {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut edges: Vec<(u32, u32)> = Vec::with_capacity(n * m);
    let mut ends: Vec<u32> = Vec::with_capacity(2 * n * m);
    for v in 0..=m {
        for u in 0..v {
            edges.push((u as u32, v as u32));
            ends.push(u as u32);
            ends.push(v as u32);
        }
    }
    for v in (m + 1)..n {
        for _ in 0..m {
            let t = ends[rng.gen_range(0..ends.len())];
            edges.push((t, v as u32));
            ends.push(t);
            ends.push(v as u32);
        }
    }
    Graph { n, edges }
}

/// Fraction of edges whose endpoints land in different parts — the
/// communication a distributed traversal pays.
pub fn edge_cut(assign: &[u32], edges: &[(u32, u32)]) -> f64 {
    let cut = edges
        .iter()
        .filter(|&&(u, v)| assign[u as usize] != assign[v as usize])
        .count();
    cut as f64 / edges.len() as f64
}

/// Hash-random placement — the baseline every system starts from.
/// Expected edge-cut: (k−1)/k (PowerGraph Theorem 5.1 with p = k).
pub fn random_assignment(n: usize, k: u32, seed: u64) -> Vec<u32> {
    (0..n)
        .map(|v| (crate::placement::splitmix64(seed ^ v as u64) % k as u64) as u32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_cut_matches_theorem_5_1() {
        let g = community(8, 1_000, 8, 2, 3);
        let cut = edge_cut(&random_assignment(g.n, 8, 11), &g.edges);
        // (k-1)/k = 0.875
        assert!((cut - 0.875).abs() < 0.01, "got {cut}");
    }
}
