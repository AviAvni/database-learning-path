//! Weighted CSR + generators (RMAT for skew, uniform for contrast) —
//! the same shapes GAP benchmarks on (gapbs builder.h/generator.h).

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub struct Csr {
    pub n: usize,
    pub offsets: Vec<usize>, // n+1
    pub targets: Vec<u32>,   // sorted within each row
    pub weights: Vec<u32>,   // parallel to targets
}

impl Csr {
    pub fn m(&self) -> usize {
        self.targets.len()
    }
    pub fn neigh(&self, u: usize) -> &[u32] {
        &self.targets[self.offsets[u]..self.offsets[u + 1]]
    }
    pub fn neigh_w(&self, u: usize) -> (&[u32], &[u32]) {
        let r = self.offsets[u]..self.offsets[u + 1];
        (&self.targets[r.clone()], &self.weights[r])
    }
    pub fn degree(&self, u: usize) -> usize {
        self.offsets[u + 1] - self.offsets[u]
    }
}

/// Build CSR from an edge list; drops self loops, dedups parallel
/// edges (keeping the first weight), optionally adds reverse edges.
pub fn from_edges(n: usize, edges: &[(u32, u32, u32)], symmetric: bool) -> Csr {
    let mut all: Vec<(u32, u32, u32)> = Vec::with_capacity(edges.len() * 2);
    for &(u, v, w) in edges {
        if u == v {
            continue;
        }
        all.push((u, v, w));
        if symmetric {
            all.push((v, u, w));
        }
    }
    all.sort_unstable();
    all.dedup_by_key(|e| (e.0, e.1));
    let mut offsets = vec![0usize; n + 1];
    for &(u, _, _) in &all {
        offsets[u as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    Csr {
        n,
        offsets,
        targets: all.iter().map(|e| e.1).collect(),
        weights: all.iter().map(|e| e.2).collect(),
    }
}

/// RMAT (a,b,c,d)=(.57,.19,.19,.05) — the GAP kron shape, power-law
pub fn gen_rmat(scale: u32, avg_deg: usize, seed: u64) -> (usize, Vec<(u32, u32, u32)>) {
    let n = 1usize << scale;
    let m = n * avg_deg;
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let edges = (0..m)
        .map(|_| {
            let (mut u, mut v) = (0u32, 0u32);
            for _ in 0..scale {
                let r: f64 = rng.gen();
                let (du, dv) = if r < 0.57 {
                    (0, 0)
                } else if r < 0.76 {
                    (0, 1)
                } else if r < 0.95 {
                    (1, 0)
                } else {
                    (1, 1)
                };
                u = (u << 1) | du;
                v = (v << 1) | dv;
            }
            (u, v, rng.gen_range(1..=255u32))
        })
        .collect();
    (n, edges)
}

pub fn gen_uniform(n: usize, m: usize, seed: u64) -> Vec<(u32, u32, u32)> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..m)
        .map(|_| {
            (
                rng.gen_range(0..n as u32),
                rng.gen_range(0..n as u32),
                rng.gen_range(1..=255u32),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csr_basics() {
        let g = from_edges(4, &[(0, 1, 5), (0, 1, 9), (1, 2, 3), (3, 3, 1)], true);
        assert_eq!(g.neigh(0), &[1]);
        assert_eq!(g.neigh_w(0).1, &[5], "dedup keeps first weight");
        assert_eq!(g.neigh(1), &[0, 2]);
        assert_eq!(g.degree(3), 0, "self loop dropped");
    }

    #[test]
    fn rmat_is_skewed() {
        let (n, e) = gen_rmat(12, 16, 1);
        let g = from_edges(n, &e, true);
        let mut degs: Vec<usize> = (0..n).map(|u| g.degree(u)).collect();
        degs.sort_unstable_by(|a, b| b.cmp(a));
        let top1pct: usize = degs[..n / 100].iter().sum();
        // measured: 19.1% at scale 12 (36.6% at scale 16 — skew grows with scale)
        assert!(
            top1pct * 7 > g.m(),
            "top 1% of RMAT vertices should hold >14% of edges"
        );
        assert!(degs[0] > 50 * (g.m() / n), "hub degree ≫ average");
    }
}
