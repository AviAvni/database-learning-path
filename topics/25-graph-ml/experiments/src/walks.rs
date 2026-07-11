//! Random walks: the corpus generator for skip-gram embeddings.
//! DeepWalk = uniform first-order walks (provided). node2vec = biased
//! SECOND-order walks (stub): the transition depends on where you came
//! from, which is what lets p/q interpolate between BFS-ish and DFS-ish
//! neighborhoods (Grover & Leskovec, KDD'16 §3.2).

use crate::graph::Csr;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// `walks_per_node` uniform walks of `walk_len` steps from every non-isolated
/// vertex. Walk vector includes the start, so length is walk_len + 1.
pub fn uniform_walks(g: &Csr, walk_len: usize, walks_per_node: usize, seed: u64) -> Vec<Vec<u32>> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut walks = Vec::with_capacity(g.n * walks_per_node);
    for start in 0..g.n as u32 {
        if g.degree(start) == 0 {
            continue;
        }
        for _ in 0..walks_per_node {
            let mut walk = Vec::with_capacity(walk_len + 1);
            walk.push(start);
            let mut cur = start;
            for _ in 0..walk_len {
                let nb = g.neigh(cur);
                cur = nb[rng.gen_range(0..nb.len())];
                walk.push(cur);
            }
            walks.push(walk);
        }
    }
    walks
}

/// STUB — node2vec biased second-order walks.
///
/// At (prev = t, cur = v), candidate x in N(v) has unnormalized weight:
///   1/p  if x == t            (return)
///   1    if g.has_edge(t, x)  (stay in t's neighborhood — distance 1)
///   1/q  otherwise            (move away — distance 2)
/// First step from each start is uniform (no prev yet).
///
/// Two implementation routes:
///  - alias tables per (edge) pair: O(1) sampling but O(sum deg(v) over
///    edges) = O(m * avg_deg) memory — the original C++ does this and it's
///    why node2vec preprocessing blows up on big graphs;
///  - rejection sampling (KnightKing-style): draw x uniform from N(v),
///    accept with prob w(x)/w_max where w_max = max(1, 1/p, 1/q) — O(1)
///    memory, expected O(w_max / w_avg) draws. DO THIS ONE; count
///    rejections if you want to see the cost of extreme p/q.
/// has_edge is a binary search over sorted CSR rows — O(log deg(t)).
///
/// Semantics must match uniform_walks: walks_per_node walks from every
/// non-isolated vertex, in vertex order, each of length walk_len + 1
/// including the start.
pub fn node2vec_walks(
    _g: &Csr,
    _p: f64,
    _q: f64,
    _walk_len: usize,
    _walks_per_node: usize,
    _seed: u64,
) -> Vec<Vec<u32>> {
    todo!("biased second-order walks with rejection sampling")
}

/// Normalized visit-frequency histogram over all positions in all walks.
pub fn visit_dist(walks: &[Vec<u32>], n: usize) -> Vec<f64> {
    let mut counts = vec![0f64; n];
    let mut total = 0f64;
    for w in walks {
        for &v in w {
            counts[v as usize] += 1.0;
            total += 1.0;
        }
    }
    for c in &mut counts {
        *c /= total;
    }
    counts
}

fn degree_dist(g: &Csr) -> Vec<f64> {
    let m = g.m() as f64;
    (0..g.n as u32).map(|v| g.degree(v) as f64 / m).collect()
}

fn l1(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

fn avg_distinct(walks: &[Vec<u32>]) -> f64 {
    let mut sum = 0.0;
    for w in walks {
        let mut s: Vec<u32> = w.clone();
        s.sort_unstable();
        s.dedup();
        sum += s.len() as f64;
    }
    sum / walks.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{gen_ring_of_cliques, gen_sbm};

    // Stationary distribution of a random walk on an undirected graph is
    // proportional to degree — the yardstick both walkers must hit at p=q=1.
    #[test]
    fn uniform_walks_hit_degree_stationary() {
        let (g, _) = gen_sbm(4, 32, 0.3, 0.02, 21);
        let walks = uniform_walks(&g, 80, 20, 3);
        assert!(l1(&visit_dist(&walks, g.n), &degree_dist(&g)) < 0.08);
    }

    #[test]
    fn node2vec_p1_q1_is_uniform() {
        let (g, _) = gen_sbm(4, 32, 0.3, 0.02, 21);
        let walks = node2vec_walks(&g, 1.0, 1.0, 80, 20, 3);
        assert_eq!(walks.len(), g.n * 20);
        for w in &walks {
            assert_eq!(w.len(), 81);
        }
        assert!(l1(&visit_dist(&walks, g.n), &degree_dist(&g)) < 0.08);
    }

    // Low q pushes outward (DFS-ish), high q keeps walks local (BFS-ish):
    // on a ring of cliques, distinct vertices per walk must order that way.
    #[test]
    fn q_controls_exploration() {
        let g = gen_ring_of_cliques(16, 16);
        let explore = avg_distinct(&node2vec_walks(&g, 1.0, 0.25, 40, 10, 7));
        let local = avg_distinct(&node2vec_walks(&g, 1.0, 4.0, 40, 10, 7));
        assert!(
            explore > local * 1.15,
            "explore {:.2} local {:.2}",
            explore,
            local
        );
    }

    // High p discourages returning to the previous vertex.
    #[test]
    fn p_controls_backtracking() {
        let g = gen_ring_of_cliques(16, 16);
        let backtracks = |walks: &[Vec<u32>]| -> f64 {
            let mut b = 0usize;
            let mut steps = 0usize;
            for w in walks {
                for i in 2..w.len() {
                    if w[i] == w[i - 2] {
                        b += 1;
                    }
                    steps += 1;
                }
            }
            b as f64 / steps as f64
        };
        let hi_p = backtracks(&node2vec_walks(&g, 8.0, 1.0, 40, 10, 9));
        let lo_p = backtracks(&node2vec_walks(&g, 0.125, 1.0, 40, 10, 9));
        assert!(lo_p > hi_p * 2.0, "lo_p {:.4} hi_p {:.4}", lo_p, hi_p);
    }
}
