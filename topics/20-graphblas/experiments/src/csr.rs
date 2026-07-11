//! CSR — the sparse-by-row format under everything here.
//! Square n×n graphs, u32 column indices (the v10 32-bit-index
//! lesson: half the index bytes for graphs under 4B edges).

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Debug, Clone)]
pub struct Csr {
    pub n: usize,
    pub rowptr: Vec<usize>, // len n+1
    pub colidx: Vec<u32>,   // len nnz, sorted within each row
    pub vals: Vec<f64>,     // len nnz
}

impl Csr {
    /// Build from edges; duplicates are summed (like GrB_build with PLUS dup).
    pub fn from_edges(n: usize, edges: &[(u32, u32, f64)]) -> Csr {
        let mut sorted: Vec<(u32, u32, f64)> = edges.to_vec();
        sorted.sort_unstable_by_key(|&(r, c, _)| (r, c));
        let mut dedup: Vec<(u32, u32, f64)> = Vec::with_capacity(sorted.len());
        for (r, c, v) in sorted {
            match dedup.last_mut() {
                Some(last) if last.0 == r && last.1 == c => last.2 += v,
                _ => dedup.push((r, c, v)),
            }
        }
        let mut rowptr = vec![0usize; n + 1];
        for &(r, _, _) in &dedup {
            rowptr[r as usize + 1] += 1;
        }
        for i in 0..n {
            rowptr[i + 1] += rowptr[i];
        }
        let colidx = dedup.iter().map(|&(_, c, _)| c).collect();
        let vals = dedup.iter().map(|&(_, _, v)| v).collect();
        Csr { n, rowptr, colidx, vals }
    }

    pub fn nnz(&self) -> usize {
        self.colidx.len()
    }

    pub fn row(&self, i: usize) -> (&[u32], &[f64]) {
        let (s, e) = (self.rowptr[i], self.rowptr[i + 1]);
        (&self.colidx[s..e], &self.vals[s..e])
    }

    pub fn transpose(&self) -> Csr {
        let mut cnt = vec![0usize; self.n + 1];
        for &c in &self.colidx {
            cnt[c as usize + 1] += 1;
        }
        for i in 0..self.n {
            cnt[i + 1] += cnt[i];
        }
        let rowptr = cnt.clone();
        let mut cur = cnt;
        let mut colidx = vec![0u32; self.nnz()];
        let mut vals = vec![0f64; self.nnz()];
        for r in 0..self.n {
            let (cols, vs) = self.row(r);
            for (&c, &v) in cols.iter().zip(vs) {
                let p = cur[c as usize];
                colidx[p] = r as u32;
                vals[p] = v;
                cur[c as usize] += 1;
            }
        }
        Csr { n: self.n, rowptr, colidx, vals }
    }

    pub fn out_degrees(&self) -> Vec<usize> {
        (0..self.n).map(|i| self.rowptr[i + 1] - self.rowptr[i]).collect()
    }

    /// Index bytes only (the format-comparison number).
    pub fn index_bytes(&self) -> usize {
        self.rowptr.len() * 8 + self.colidx.len() * 4
    }
}

/// RMAT power-law generator (Graph500 a,b,c,d = .57,.19,.19,.05).
/// n = 2^scale, m ≈ n*edge_factor after dedup-by-sum.
pub fn rmat(scale: u32, edge_factor: usize, seed: u64) -> Csr {
    let n = 1usize << scale;
    let m = n * edge_factor;
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut edges = Vec::with_capacity(m);
    for _ in 0..m {
        let (mut r, mut c) = (0usize, 0usize);
        for _ in 0..scale {
            let p: f64 = rng.gen();
            let (dr, dc) = if p < 0.57 {
                (0, 0)
            } else if p < 0.76 {
                (0, 1)
            } else if p < 0.95 {
                (1, 0)
            } else {
                (1, 1)
            };
            r = (r << 1) | dr;
            c = (c << 1) | dc;
        }
        edges.push((r as u32, c as u32, 1.0));
    }
    Csr::from_edges(n, &edges)
}

/// Uniform-random graph (flat degrees — the anti-RMAT control).
pub fn uniform(n: usize, m: usize, seed: u64) -> Csr {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let edges: Vec<(u32, u32, f64)> = (0..m)
        .map(|_| (rng.gen_range(0..n) as u32, rng.gen_range(0..n) as u32, 1.0))
        .collect();
    Csr::from_edges(n, &edges)
}

/// Path graph 0→1→…→n-1: diameter n, pull should NEVER trigger.
pub fn path(n: usize) -> Csr {
    let edges: Vec<(u32, u32, f64)> = (0..n - 1).map(|i| (i as u32, i as u32 + 1, 1.0)).collect();
    Csr::from_edges(n, &edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_transpose_roundtrip() {
        let a = Csr::from_edges(4, &[(0, 1, 1.0), (0, 3, 2.0), (2, 0, 3.0), (0, 1, 4.0)]);
        assert_eq!(a.nnz(), 3); // dup (0,1) summed
        assert_eq!(a.row(0), (&[1u32, 3][..], &[5.0f64, 2.0][..]));
        assert_eq!(a.row(1).0.len(), 0);
        let at = a.transpose();
        assert_eq!(at.row(1), (&[0u32][..], &[5.0f64][..]));
        let att = at.transpose();
        assert_eq!(att.colidx, a.colidx);
        assert_eq!(att.vals, a.vals);
    }

    #[test]
    fn rmat_is_power_law_ish() {
        let a = rmat(10, 8, 1);
        let d = a.out_degrees();
        let max = *d.iter().max().unwrap();
        let avg = a.nnz() / a.n;
        assert!(max > 10 * avg, "hub degree {max} vs avg {avg}");
    }
}
