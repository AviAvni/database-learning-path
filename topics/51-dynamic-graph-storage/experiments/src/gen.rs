//! Seeded power-law graph (R-MAT, the Graph500 generator) and the seeded
//! operation streams that drive every lane.
//!
//! Everything here is a pure function of the seeds, so every count printed by
//! the bench reproduces exactly on any machine.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Clone, Copy, Debug)]
pub struct GraphParams {
    /// |V| = 2^scale.
    pub scale: u32,
    /// Edges drawn per vertex before de-duplication.
    pub edge_factor: u64,
    pub seed: u64,
}

impl GraphParams {
    /// 1,048,576 vertices, 16 draws per vertex → ~16.8 M draws.
    pub const LANE1: GraphParams = GraphParams { scale: 20, edge_factor: 16, seed: 51 };
    /// A small graph for unit tests.
    pub const TINY: GraphParams = GraphParams { scale: 10, edge_factor: 8, seed: 51 };
}

/// A simple directed graph: sorted by (src, dst), no duplicates, no loops.
#[derive(Clone, Debug)]
pub struct EdgeList {
    pub n: u32,
    pub edges: Vec<(u32, u32)>,
    /// The random relabelling applied to the raw R-MAT ids, so that hubs are
    /// not clustered at low ids (Graph500 does the same). The insert stream
    /// reuses it.
    pub perm: Vec<u32>,
}

// R-MAT quadrant probabilities (Graph500: a = .57, b = .19, c = .19, d = .05),
// as thresholds on a uniform u32.
const A: u32 = (0.57 * u32::MAX as f64) as u32;
const AB: u32 = (0.76 * u32::MAX as f64) as u32;
const ABC: u32 = (0.95 * u32::MAX as f64) as u32;

fn rmat_pair(rng: &mut ChaCha8Rng, scale: u32) -> (u32, u32) {
    let (mut u, mut v) = (0u32, 0u32);
    for _ in 0..scale {
        let r: u32 = rng.gen();
        u <<= 1;
        v <<= 1;
        if r < A {
        } else if r < AB {
            v |= 1;
        } else if r < ABC {
            u |= 1;
        } else {
            u |= 1;
            v |= 1;
        }
    }
    (u, v)
}

fn permutation(rng: &mut ChaCha8Rng, n: u32) -> Vec<u32> {
    let mut p: Vec<u32> = (0..n).collect();
    for i in (1..n as usize).rev() {
        let j = rng.gen_range(0..=i);
        p.swap(i, j);
    }
    p
}

pub fn rmat(p: GraphParams) -> EdgeList {
    let n = 1u32 << p.scale;
    let mut rng = ChaCha8Rng::seed_from_u64(p.seed);
    let perm = permutation(&mut rng, n);
    let draws = p.edge_factor * n as u64;
    let mut edges: Vec<(u32, u32)> = Vec::with_capacity(draws as usize);
    for _ in 0..draws {
        let (u, v) = rmat_pair(&mut rng, p.scale);
        if u != v {
            edges.push((perm[u as usize], perm[v as usize]));
        }
    }
    edges.sort_unstable();
    edges.dedup();
    EdgeList { n, edges, perm }
}

/// Out-degree histogram facts used in the bench header.
pub struct DegreeStats {
    pub max: usize,
    pub mean: f64,
    /// Fraction of all edges owned by the top 1% of vertices by out-degree.
    pub top1pct_share: f64,
    pub isolated: usize,
}

pub fn degree_stats(el: &EdgeList) -> DegreeStats {
    let mut deg = vec![0usize; el.n as usize];
    for &(u, _) in &el.edges {
        deg[u as usize] += 1;
    }
    let isolated = deg.iter().filter(|&&d| d == 0).count();
    let max = *deg.iter().max().unwrap();
    deg.sort_unstable_by(|a, b| b.cmp(a));
    let top = (el.n as usize) / 100;
    let share = deg[..top].iter().sum::<usize>() as f64 / el.edges.len() as f64;
    DegreeStats { max, mean: el.edges.len() as f64 / el.n as f64, top1pct_share: share, isolated }
}

/// The operation streams. All three are pre-drawn so every store sees the
/// same ones.
pub struct Workload {
    /// Query start vertices (uniform over all vertices).
    pub starts: Vec<u32>,
    /// Candidate inserts: fresh R-MAT draws. Some already exist in the graph;
    /// a store's `insert` returns `false` for those, identically everywhere.
    pub inserts: Vec<(u32, u32)>,
    /// Candidate deletes: uniform picks from the original edge list.
    pub deletes: Vec<(u32, u32)>,
}

pub fn workload(el: &EdgeList, scale: u32, n_starts: usize, n_writes: usize) -> Workload {
    let mut r1 = ChaCha8Rng::seed_from_u64(5101);
    let starts = (0..n_starts).map(|_| r1.gen_range(0..el.n)).collect();
    let mut r2 = ChaCha8Rng::seed_from_u64(5102);
    let mut inserts = Vec::with_capacity(n_writes);
    while inserts.len() < n_writes {
        let (u, v) = rmat_pair(&mut r2, scale);
        if u != v {
            inserts.push((el.perm[u as usize], el.perm[v as usize]));
        }
    }
    let mut r3 = ChaCha8Rng::seed_from_u64(5103);
    let deletes = (0..n_writes).map(|_| el.edges[r3.gen_range(0..el.edges.len())]).collect();
    Workload { starts, inserts, deletes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_is_sorted_unique_loopless_and_reproducible() {
        let a = rmat(GraphParams::TINY);
        let b = rmat(GraphParams::TINY);
        assert_eq!(a.edges, b.edges);
        assert!(a.edges.windows(2).all(|w| w[0] < w[1]));
        assert!(a.edges.iter().all(|&(u, v)| u != v && u < a.n && v < a.n));
    }
}
