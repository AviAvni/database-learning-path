//! A plain CSR graph over `u32` vertex ids (PROVIDED).
//!
//! `off[v]..off[v+1]` indexes `tgt`; every list is sorted ascending with no
//! duplicates and no self-loops. This is the "uncompressed" baseline: 32 bits
//! per edge plus one offset per vertex.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Csr {
    pub n: usize,
    pub off: Vec<usize>,
    pub tgt: Vec<u32>,
}

impl Csr {
    pub fn m(&self) -> usize {
        self.tgt.len()
    }

    pub fn neighbors(&self, v: usize) -> &[u32] {
        &self.tgt[self.off[v]..self.off[v + 1]]
    }

    pub fn degree(&self, v: usize) -> usize {
        self.off[v + 1] - self.off[v]
    }

    /// Build from an edge list: sorts, drops duplicates and self-loops.
    pub fn from_edges(n: usize, mut edges: Vec<(u32, u32)>) -> Csr {
        edges.retain(|&(u, v)| u != v);
        let mut keyed: Vec<u64> = edges.iter().map(|&(u, v)| ((u as u64) << 32) | v as u64).collect();
        keyed.sort_unstable();
        keyed.dedup();
        let mut off = vec![0usize; n + 1];
        let mut tgt = Vec::with_capacity(keyed.len());
        for &k in &keyed {
            off[(k >> 32) as usize + 1] += 1;
            tgt.push(k as u32);
        }
        for v in 0..n {
            off[v + 1] += off[v];
        }
        Csr { n, off, tgt }
    }

    /// Relabel: vertex `old` becomes `new_id[old]`. Lists are re-sorted.
    pub fn permute(&self, new_id: &[u32]) -> Csr {
        assert_eq!(new_id.len(), self.n);
        let mut deg = vec![0usize; self.n];
        for v in 0..self.n {
            deg[new_id[v] as usize] = self.degree(v);
        }
        let mut off = vec![0usize; self.n + 1];
        for v in 0..self.n {
            off[v + 1] = off[v] + deg[v];
        }
        let mut tgt = vec![0u32; self.m()];
        for v in 0..self.n {
            let s = off[new_id[v] as usize];
            let l = self.degree(v);
            for (k, &t) in self.neighbors(v).iter().enumerate() {
                tgt[s + k] = new_id[t as usize];
            }
            tgt[s..s + l].sort_unstable();
        }
        Csr { n: self.n, off, tgt }
    }

    /// The reverse graph (in-neighbour lists), each list sorted.
    pub fn transpose(&self) -> Csr {
        let mut off = vec![0usize; self.n + 1];
        for &t in &self.tgt {
            off[t as usize + 1] += 1;
        }
        for v in 0..self.n {
            off[v + 1] += off[v];
        }
        let mut cur = off.clone();
        let mut tgt = vec![0u32; self.m()];
        for v in 0..self.n {
            for &t in self.neighbors(v) {
                tgt[cur[t as usize]] = v as u32;
                cur[t as usize] += 1;
            }
        }
        Csr { n: self.n, off, tgt }
    }

    /// Bits per edge of the plain layout: 32-bit targets, and (second value)
    /// with one 32-bit offset per vertex amortised over the edges.
    pub fn plain_bits_per_edge(&self) -> (f64, f64) {
        (32.0, 32.0 + 32.0 * (self.n + 1) as f64 / self.m() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_edges_sorts_dedups_and_drops_loops() {
        let g = Csr::from_edges(4, vec![(0, 3), (0, 1), (0, 1), (2, 2), (3, 0)]);
        assert_eq!(g.neighbors(0), &[1, 3]);
        assert_eq!(g.neighbors(2), &[] as &[u32]);
        assert_eq!(g.neighbors(3), &[0]);
        assert_eq!(g.m(), 3);
    }

    #[test]
    fn permute_then_inverse_is_identity() {
        let g = Csr::from_edges(5, vec![(0, 1), (0, 4), (1, 2), (3, 0), (4, 3), (4, 1)]);
        let p = vec![3u32, 0, 4, 1, 2];
        let mut inv = vec![0u32; 5];
        for (o, &nw) in p.iter().enumerate() {
            inv[nw as usize] = o as u32;
        }
        assert_eq!(g.permute(&p).permute(&inv), g);
    }

    #[test]
    fn transpose_twice_is_identity() {
        let g = Csr::from_edges(5, vec![(0, 1), (0, 4), (1, 2), (3, 0), (4, 3), (4, 1)]);
        assert_eq!(g.transpose().transpose(), g);
    }
}
