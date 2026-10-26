//! Neighbour explosion and neighbour sampling (GraphSAGE's actual
//! contribution, Hamilton et al. NeurIPS'17 §3.1) — PROVIDED, counted, not
//! timed. Everything here is seeded and reproduces exactly.
//!
//! A k-layer GNN computing the embedding of a batch of seed vertices needs the
//! k-hop neighbourhood of the batch: the *receptive field*. Its size is the
//! cost of one mini-batch, in vertices gathered and edges aggregated.
//! `full_field` counts it exactly; `sampled_field` counts it when each vertex
//! keeps at most `fanout[h]` of its neighbours at hop h, chosen uniformly
//! without replacement (PyG's NeighborLoader default; GraphSAGE samples with
//! replacement to a fixed size, which touches the same or fewer distinct
//! vertices).

use crate::graph::{from_edges, Csr};
use rand::{seq::index::sample, Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// distinct vertices whose features must be gathered (seeds included)
    pub vertices: usize,
    /// edges aggregated over, summed across layers (a vertex reached at two
    /// hops is aggregated once per layer that needs it, as a layer-wise GNN does)
    pub edges: usize,
}

/// Exact k-hop receptive field of `seeds` (k = `hops`).
pub fn full_field(g: &Csr, seeds: &[u32], hops: usize) -> Field {
    expand(g, seeds, &vec![usize::MAX; hops], 0)
}

/// Receptive field when hop h keeps at most `fanout[h]` neighbours per vertex.
pub fn sampled_field(g: &Csr, seeds: &[u32], fanout: &[usize], seed: u64) -> Field {
    expand(g, seeds, fanout, seed)
}

fn expand(g: &Csr, seeds: &[u32], fanout: &[usize], seed: u64) -> Field {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut seen = vec![false; g.n];
    let mut frontier: Vec<u32> = Vec::new();
    for &s in seeds {
        if !seen[s as usize] {
            seen[s as usize] = true;
            frontier.push(s);
        }
    }
    let mut vertices = frontier.len();
    let mut edges = 0usize;
    for &f in fanout {
        let mut next = Vec::new();
        for &v in &frontier {
            let nb = g.neigh(v);
            let picked: Vec<u32> = if nb.len() <= f {
                nb.to_vec()
            } else {
                // index::sample returns the chosen positions; sort for a
                // deterministic traversal order independent of the sampler
                let mut idx = sample(&mut rng, nb.len(), f).into_vec();
                idx.sort_unstable();
                idx.into_iter().map(|i| nb[i]).collect()
            };
            edges += picked.len();
            for u in picked {
                if !seen[u as usize] {
                    seen[u as usize] = true;
                    next.push(u);
                }
            }
        }
        vertices += next.len();
        frontier = next;
    }
    Field { vertices, edges }
}

/// `count` distinct seed vertices, uniformly at random, seeded.
pub fn pick_seeds(n: usize, count: usize, seed: u64) -> Vec<u32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut v: Vec<u32> = sample(&mut rng, n, count).into_iter().map(|i| i as u32).collect();
    v.sort_unstable();
    v
}

/// Preferential-attachment graph (Barabasi-Albert): each new vertex attaches
/// `m_per` edges to earlier vertices chosen proportionally to degree, by
/// drawing from the list of all edge endpoints so far. Produces hubs; the SBM
/// has none. Seeded.
pub fn gen_preferential(n: usize, m_per: usize, seed: u64) -> Csr {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut endpoints: Vec<u32> = Vec::new();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    // seed clique of m_per + 1 vertices
    for i in 0..=m_per as u32 {
        for j in (i + 1)..=m_per as u32 {
            edges.push((i, j));
            endpoints.push(i);
            endpoints.push(j);
        }
    }
    for v in (m_per + 1)..n {
        let mut chosen: Vec<u32> = Vec::with_capacity(m_per);
        while chosen.len() < m_per {
            let u = endpoints[rng.gen_range(0..endpoints.len())];
            if !chosen.contains(&u) {
                chosen.push(u);
            }
        }
        for u in chosen {
            edges.push((v as u32, u));
            endpoints.push(v as u32);
            endpoints.push(u);
        }
    }
    from_edges(n, &edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::from_edges;

    #[test]
    fn one_hop_full_field_on_a_path() {
        // 0-1-2-3-4
        let g = from_edges(5, &[(0, 1), (1, 2), (2, 3), (3, 4)]);
        let f = full_field(&g, &[2], 1);
        assert_eq!(f, Field { vertices: 3, edges: 2 });
        let f2 = full_field(&g, &[2], 2);
        assert_eq!(f2.vertices, 5);
    }

    #[test]
    fn sampling_never_exceeds_the_full_field_and_is_deterministic() {
        let g = gen_preferential(2000, 8, 3);
        let seeds = pick_seeds(g.n, 64, 9);
        let full = full_field(&g, &seeds, 2);
        let a = sampled_field(&g, &seeds, &[5, 3], 1);
        let b = sampled_field(&g, &seeds, &[5, 3], 1);
        assert_eq!(a, b);
        assert!(a.vertices <= full.vertices && a.edges <= full.edges);
        // fanout bound: hop-1 edges <= 64*5, hop-2 edges <= (64*5)*3
        assert!(a.edges <= 64 * 5 + 64 * 5 * 3);
    }

    #[test]
    fn preferential_graph_has_hubs() {
        let g = gen_preferential(5000, 8, 3);
        let max = (0..g.n as u32).map(|v| g.degree(v)).max().unwrap();
        let avg = g.m() as f64 / g.n as f64;
        assert!(max as f64 > 10.0 * avg, "max {max} avg {avg}");
    }
}
