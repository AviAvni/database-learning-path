//! Hypersparse: store only non-empty rows. The FalkorDB case:
//! 10M-node id space, 100K edges — CSR's rowptr alone is 80 MB;
//! hypersparse stores ~100K row entries instead.

use crate::csr::Csr;

pub struct HyperCsr {
    pub n: usize,
    pub rows: Vec<u32>,     // non-empty rows, sorted
    pub rowptr: Vec<usize>, // len rows.len()+1
    pub colidx: Vec<u32>,
    pub vals: Vec<f64>,
}

impl HyperCsr {
    pub fn from_csr(a: &Csr) -> HyperCsr {
        let mut rows = Vec::new();
        let mut rowptr = vec![0usize];
        let mut colidx = Vec::with_capacity(a.nnz());
        let mut vals = Vec::with_capacity(a.nnz());
        for i in 0..a.n {
            let (cols, vs) = a.row(i);
            if !cols.is_empty() {
                rows.push(i as u32);
                colidx.extend_from_slice(cols);
                vals.extend_from_slice(vs);
                rowptr.push(colidx.len());
            }
        }
        HyperCsr { n: a.n, rows, rowptr, colidx, vals }
    }

    /// Row lookup costs a binary search — the price of the compression.
    pub fn row(&self, i: usize) -> (&[u32], &[f64]) {
        match self.rows.binary_search(&(i as u32)) {
            Ok(k) => {
                let (s, e) = (self.rowptr[k], self.rowptr[k + 1]);
                (&self.colidx[s..e], &self.vals[s..e])
            }
            Err(_) => (&[], &[]),
        }
    }

    pub fn index_bytes(&self) -> usize {
        self.rows.len() * 4 + self.rowptr.len() * 8 + self.colidx.len() * 4
    }

    /// Iterate all entries WITHOUT lookups — the fast path any kernel
    /// should use (iterate non-empty rows, not the id space).
    pub fn iter_rows(&self) -> impl Iterator<Item = (u32, &[u32], &[f64])> {
        self.rows.iter().enumerate().map(|(k, &r)| {
            let (s, e) = (self.rowptr[k], self.rowptr[k + 1]);
            (r, &self.colidx[s..e], &self.vals[s..e])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csr::uniform;

    #[test]
    fn matches_csr() {
        let a = uniform(10_000, 500, 9);
        let h = HyperCsr::from_csr(&a);
        for i in 0..a.n {
            assert_eq!(h.row(i), a.row(i));
        }
        assert!(h.index_bytes() < a.index_bytes() / 10);
        let total: usize = h.iter_rows().map(|(_, c, _)| c.len()).sum();
        assert_eq!(total, a.nnz());
    }
}
