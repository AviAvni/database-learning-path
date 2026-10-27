//! The baseline: CSR. `Csr` is the immutable form (and the settled matrix `M`
//! inside the delta design); `EagerCsr` is the same arrays with a real splice
//! on every write — what "just make CSR dynamic" costs.

use crate::store::{GraphRead, GraphWrite};

#[derive(Clone, Debug)]
pub struct Csr {
    /// `offsets[u]..offsets[u+1]` indexes `edges`; length n + 1.
    pub offsets: Vec<u32>,
    /// Destinations, sorted within each row.
    pub edges: Vec<u32>,
}

impl Csr {
    /// `edges` must be sorted by (src, dst) with no duplicates.
    pub fn from_edges(n: u32, edges: &[(u32, u32)]) -> Csr {
        let mut offsets = vec![0u32; n as usize + 1];
        for &(u, _) in edges {
            offsets[u as usize + 1] += 1;
        }
        for i in 0..n as usize {
            offsets[i + 1] += offsets[i];
        }
        Csr { offsets, edges: edges.iter().map(|&(_, v)| v).collect() }
    }

    pub fn n(&self) -> usize {
        self.offsets.len() - 1
    }

    #[inline]
    pub fn row(&self, u: u32) -> &[u32] {
        let u = u as usize;
        &self.edges[self.offsets[u] as usize..self.offsets[u + 1] as usize]
    }

    pub fn contains_edge(&self, u: u32, v: u32) -> bool {
        self.row(u).binary_search(&v).is_ok()
    }

    pub fn bytes_used(&self) -> usize {
        4 * self.offsets.capacity() + 4 * self.edges.capacity()
    }
}

impl GraphRead for Csr {
    fn name(&self) -> String {
        "static CSR".into()
    }
    fn num_vertices(&self) -> usize {
        self.n()
    }
    fn num_edges(&self) -> usize {
        self.edges.len()
    }
    #[inline]
    fn for_each<F: FnMut(u32)>(&self, u: u32, mut f: F) {
        for &x in self.row(u) {
            f(x);
        }
    }
    fn contains(&self, u: u32, v: u32) -> bool {
        self.contains_edge(u, v)
    }
    fn bytes(&self) -> usize {
        self.bytes_used()
    }
}

/// CSR that splices on every write: `Vec::insert` into the edge array (a
/// memmove of everything after the position) and a `+1` over every later
/// offset. Correct, and O(|V| + |E|) per write.
pub struct EagerCsr(pub Csr);

impl GraphRead for EagerCsr {
    fn name(&self) -> String {
        "CSR, splice per write".into()
    }
    fn num_vertices(&self) -> usize {
        self.0.n()
    }
    fn num_edges(&self) -> usize {
        self.0.edges.len()
    }
    #[inline]
    fn for_each<F: FnMut(u32)>(&self, u: u32, f: F) {
        self.0.for_each(u, f)
    }
    fn contains(&self, u: u32, v: u32) -> bool {
        self.0.contains_edge(u, v)
    }
    fn bytes(&self) -> usize {
        self.0.bytes_used()
    }
}

impl GraphWrite for EagerCsr {
    fn insert(&mut self, u: u32, v: u32) -> bool {
        let lo = self.0.offsets[u as usize] as usize;
        match self.0.row(u).binary_search(&v) {
            Ok(_) => false,
            Err(i) => {
                self.0.edges.insert(lo + i, v);
                for o in &mut self.0.offsets[u as usize + 1..] {
                    *o += 1;
                }
                true
            }
        }
    }
    fn delete(&mut self, u: u32, v: u32) -> bool {
        let lo = self.0.offsets[u as usize] as usize;
        match self.0.row(u).binary_search(&v) {
            Err(_) => false,
            Ok(i) => {
                self.0.edges.remove(lo + i);
                for o in &mut self.0.offsets[u as usize + 1..] {
                    *o -= 1;
                }
                true
            }
        }
    }
}
