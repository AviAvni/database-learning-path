//! The seeded graph (PROVIDED): power-law out-degrees, planted communities.
//!
//! Vertices are laid out community by community (the *planted* order: ids of
//! one community are contiguous, like pages of one host under URL order).
//! Community sizes are uniform in `[comm_min, comm_max]`. Each vertex draws
//! an out-degree from a Pareto tail (`xm`, `alpha`, capped); each edge stays
//! inside the vertex's community with probability `p_intra` (target offset
//! `size·u²`, so low offsets are local hubs) and otherwise goes anywhere:
//! half to a global hub drawn as `n·u³`, half uniform. A ChaCha8 stream
//! drives everything, so every count reproduces exactly.
//!
//! `shuffle_perm` then hides the planted order: lane 1 loads the graph with
//! random ids, which is what a graph looks like when ids were handed out by
//! arrival order or by hashing.

use crate::csr::Csr;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Clone, Copy, Debug)]
pub struct GenParams {
    pub n: usize,
    pub comm_min: usize,
    pub comm_max: usize,
    pub p_intra: f64,
    pub deg_xm: f64,
    pub deg_alpha: f64,
    pub deg_cap: usize,
    /// Probability that a vertex (other than a community's first) is a noisy
    /// copy of the previous vertex's list: each target kept with probability
    /// 0.9, plus a few fresh edges. 0.0 draws no extra random numbers, so the
    /// stream is the plain generator's.
    pub p_copy: f64,
    pub seed: u64,
}

impl GenParams {
    /// The lane-1 instance: 6,000,000 vertices, ~65 M edges (a 48 MB value
    /// array, so the gather does not fit the M3 Pro's 16 MB P-cluster L2).
    pub const LANE1: GenParams = GenParams {
        n: 6_000_000,
        comm_min: 32,
        comm_max: 512,
        p_intra: 0.75,
        deg_xm: 5.0,
        deg_alpha: 1.7,
        deg_cap: 5000,
        p_copy: 0.0,
        seed: 52,
    };
    /// Lane 3's second graph: lane 1's parameters plus list similarity.
    pub const WEBLIKE: GenParams = GenParams { p_copy: 0.4, ..GenParams::LANE1 };
    /// A small instance for tests.
    pub const SMALL: GenParams = GenParams {
        n: 20_000,
        comm_min: 32,
        comm_max: 256,
        p_intra: 0.75,
        deg_xm: 5.0,
        deg_alpha: 1.7,
        deg_cap: 500,
        p_copy: 0.0,
        seed: 52,
    };
    /// A small instance with list similarity, for tests.
    pub const SMALL_WEB: GenParams = GenParams { p_copy: 0.4, ..GenParams::SMALL };
}

/// The graph in planted order (communities contiguous) and each vertex's
/// community id.
pub struct Planted {
    pub g: Csr,
    pub community: Vec<u32>,
}

pub fn community_graph(p: GenParams) -> Planted {
    let mut rng = ChaCha8Rng::seed_from_u64(p.seed);
    let mut start = Vec::new();
    let mut community = vec![0u32; p.n];
    let mut v = 0usize;
    while v < p.n {
        let sz = rng.gen_range(p.comm_min..=p.comm_max).min(p.n - v);
        let c = start.len() as u32;
        start.push((v, sz));
        for x in v..v + sz {
            community[x] = c;
        }
        v += sz;
    }
    let mut edges: Vec<(u32, u32)> = Vec::with_capacity(p.n * 13);
    let mut prev: Vec<u32> = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    for v in 0..p.n {
        let u: f64 = rng.gen();
        let d = ((p.deg_xm * (1.0 - u).powf(-1.0 / p.deg_alpha)) as usize).min(p.deg_cap);
        let (cs, csz) = start[community[v] as usize];
        cur.clear();
        let first_of_comm = start[community[v] as usize].0 == v;
        let copy = p.p_copy > 0.0 && !first_of_comm && rng.gen::<f64>() < p.p_copy;
        let fresh = if copy {
            for &t in &prev {
                if rng.gen::<f64>() < 0.9 {
                    cur.push(t);
                }
            }
            (d / 10).max(1)
        } else {
            d
        };
        for _ in 0..fresh {
            let t = if rng.gen::<f64>() < p.p_intra {
                let x: f64 = rng.gen();
                cs + ((csz as f64) * x * x) as usize
            } else if rng.gen::<bool>() {
                let x: f64 = rng.gen();
                ((p.n as f64) * x * x * x) as usize
            } else {
                rng.gen_range(0..p.n)
            };
            cur.push(t.min(p.n - 1) as u32);
        }
        for &t in &cur {
            edges.push((v as u32, t));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    Planted { g: Csr::from_edges(p.n, edges), community }
}

/// A seeded random permutation: `perm[old] = new`.
pub fn shuffle_perm(n: usize, seed: u64) -> Vec<u32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut p: Vec<u32> = (0..n as u32).collect();
    for i in (1..n).rev() {
        let j = rng.gen_range(0..=i);
        p.swap(i, j);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_is_deterministic() {
        let a = community_graph(GenParams::SMALL);
        let b = community_graph(GenParams::SMALL);
        assert_eq!(a.g, b.g);
    }

    #[test]
    fn small_graph_has_a_heavy_tail_and_mostly_local_edges() {
        let p = community_graph(GenParams::SMALL);
        let g = &p.g;
        let maxd = (0..g.n).map(|v| g.degree(v)).max().unwrap();
        let mean = g.m() as f64 / g.n as f64;
        assert!(maxd as f64 > 10.0 * mean, "max degree {maxd} vs mean {mean}");
        let intra = (0..g.n)
            .flat_map(|v| g.neighbors(v).iter().map(move |&t| (v, t as usize)))
            .filter(|&(v, t)| p.community[v] == p.community[t])
            .count();
        assert!(intra as f64 > 0.5 * g.m() as f64);
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let p = shuffle_perm(1000, 7);
        let mut s = p.clone();
        s.sort_unstable();
        assert!(s.iter().enumerate().all(|(i, &x)| x as usize == i));
    }
}
