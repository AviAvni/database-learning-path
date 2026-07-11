//! SpMV — the bandwidth benchmark (PageRank's inner loop).

use crate::csr::Csr;

/// y = A*x over the (PLUS, TIMES) semiring. Row-wise: each output
/// is a dot of A's row with x — gathers from x are the random
/// accesses (topic 13's locality problem when colidx is shuffled).
pub fn spmv(a: &Csr, x: &[f64]) -> Vec<f64> {
    let mut y = vec![0.0; a.n];
    for i in 0..a.n {
        let (cols, vals) = a.row(i);
        let mut acc = 0.0;
        for (&c, &v) in cols.iter().zip(vals) {
            acc += v * x[c as usize];
        }
        y[i] = acc;
    }
    y
}

/// Bytes moved per spmv call (index + value + gather traffic), for GB/s.
pub fn spmv_bytes(a: &Csr) -> usize {
    a.nnz() * (4 + 8 + 8) + a.n * 8 * 2 + a.rowptr.len() * 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_checked() {
        // [[0,2],[3,0]] * [1,10] = [20, 3]
        let a = Csr::from_edges(2, &[(0, 1, 2.0), (1, 0, 3.0)]);
        assert_eq!(spmv(&a, &[1.0, 10.0]), vec![20.0, 3.0]);
    }
}
