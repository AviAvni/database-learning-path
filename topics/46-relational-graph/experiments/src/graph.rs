//! Compressed sparse row (CSR) adjacency — the structure a graph engine keeps
//! and a relational engine does not (DuckPGQ builds one per query, on the fly,
//! in `src/core/functions/scalar/csr_creation.cpp`).
//!
//! `offsets[v]..offsets[v+1]` indexes `targets`, so the neighbours of `v` are a
//! contiguous slice found with two array reads and no hashing.

use crate::gen::Edges;

pub struct Csr {
    pub offsets: Vec<u64>,
    pub targets: Vec<u32>,
}

impl Csr {
    /// Build from an edge list, keyed by `key(e)` and storing `val(e)`.
    /// Counting sort: one pass for degrees, a prefix sum, one pass to place.
    /// Within a vertex, targets keep the edge list's order — deterministic.
    fn build(n: usize, edges: &Edges, key: impl Fn(&(u32, u32)) -> u32, val: impl Fn(&(u32, u32)) -> u32) -> Csr {
        let mut offsets = vec![0u64; n + 1];
        for e in edges {
            offsets[key(e) as usize + 1] += 1;
        }
        for i in 0..n {
            offsets[i + 1] += offsets[i];
        }
        let mut cursor = offsets.clone();
        let mut targets = vec![0u32; edges.len()];
        for e in edges {
            let k = key(e) as usize;
            targets[cursor[k] as usize] = val(e);
            cursor[k] += 1;
        }
        Csr { offsets, targets }
    }

    #[inline]
    pub fn neighbours(&self, v: u32) -> &[u32] {
        let v = v as usize;
        &self.targets[self.offsets[v] as usize..self.offsets[v + 1] as usize]
    }

    #[inline]
    pub fn degree(&self, v: u32) -> u64 {
        let v = v as usize;
        self.offsets[v + 1] - self.offsets[v]
    }

    pub fn vertices(&self) -> usize {
        self.offsets.len() - 1
    }
}

/// Out- and in-adjacency of one directed graph.
pub struct Graph {
    pub out: Csr,
    pub inc: Csr,
}

impl Graph {
    pub fn new(n: u32, edges: &Edges) -> Graph {
        let n = n as usize;
        Graph {
            out: Csr::build(n, edges, |e| e.0, |e| e.1),
            inc: Csr::build(n, edges, |e| e.1, |e| e.0),
        }
    }
    pub fn n(&self) -> usize {
        self.out.vertices()
    }
    pub fn m(&self) -> u64 {
        self.out.targets.len() as u64
    }
    pub fn max_out(&self) -> u64 {
        (0..self.n() as u32).map(|v| self.out.degree(v)).max().unwrap_or(0)
    }
    pub fn max_in(&self) -> u64 {
        (0..self.n() as u32).map(|v| self.inc.degree(v)).max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::ffx_fig1;

    #[test]
    fn csr_matches_edge_list() {
        let g = Graph::new(9, &ffx_fig1());
        assert_eq!(g.out.neighbours(2), &[5, 6, 7]);
        assert_eq!(g.inc.neighbours(2), &[0, 1, 3]);
        assert_eq!(g.m(), 10);
    }
}
