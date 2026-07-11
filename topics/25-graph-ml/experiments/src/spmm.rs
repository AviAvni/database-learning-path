//! The aggregation kernel. PyG's GCNConv.message_and_aggregate
//! (gcn_conv.py:273) is literally `spmm(adj_t, x)` — message passing over a
//! (+, *) semiring IS this function. Provided, because it's topic 20's SpMM
//! specialized to a dense right-hand side (the feature matrix).

use crate::dense::Mat;
use crate::graph::Csr;

pub struct CsrF32 {
    pub n: usize,
    pub offsets: Vec<usize>,
    pub targets: Vec<u32>,
    pub vals: Vec<f32>,
}

/// out[i,:] = sum_j A[i,j] * x[j,:] — row-wise gather (pull), the dense-RHS
/// SpMM. FLOPs = 2 * nnz * x.cols.
pub fn spmm(a: &CsrF32, x: &Mat) -> Mat {
    assert_eq!(a.n, x.rows);
    let d = x.cols;
    let mut out = Mat::zeros(a.n, d);
    for i in 0..a.n {
        let orow = &mut out.data[i * d..(i + 1) * d];
        for e in a.offsets[i]..a.offsets[i + 1] {
            let j = a.targets[e] as usize;
            let w = a.vals[e];
            let xrow = &x.data[j * d..(j + 1) * d];
            for c in 0..d {
                orow[c] += w * xrow[c];
            }
        }
    }
    out
}

/// Random-walk normalization D^{-1} A (each row sums to 1). The PageRank
/// pull matrix from topic 24; provided as the non-stub normalization.
pub fn row_norm_adj(g: &Csr) -> CsrF32 {
    let mut vals = Vec::with_capacity(g.m());
    for v in 0..g.n as u32 {
        let d = g.degree(v).max(1) as f32;
        for _ in g.neigh(v) {
            vals.push(1.0 / d);
        }
    }
    CsrF32 { n: g.n, offsets: g.offsets.clone(), targets: g.targets.clone(), vals }
}

pub fn to_dense(a: &CsrF32) -> Mat {
    let mut m = Mat::zeros(a.n, a.n);
    for i in 0..a.n {
        for e in a.offsets[i]..a.offsets[i + 1] {
            *m.at_mut(i, a.targets[e] as usize) += a.vals[e];
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dense::{glorot, matmul, max_abs_diff};
    use crate::graph::gen_sbm;

    #[test]
    fn spmm_matches_dense_matmul() {
        let (g, _) = gen_sbm(2, 20, 0.3, 0.05, 5);
        let a = row_norm_adj(&g);
        let x = glorot(g.n, 8, 9);
        let sparse = spmm(&a, &x);
        let dense = matmul(&to_dense(&a), &x);
        assert!(max_abs_diff(&sparse, &dense) < 1e-5);
    }

    #[test]
    fn row_norm_rows_sum_to_one() {
        let (g, _) = gen_sbm(2, 16, 0.4, 0.1, 6);
        let a = row_norm_adj(&g);
        for i in 0..a.n {
            if a.offsets[i] == a.offsets[i + 1] {
                continue;
            }
            let s: f32 = a.vals[a.offsets[i]..a.offsets[i + 1]].iter().sum();
            assert!((s - 1.0).abs() < 1e-5);
        }
    }
}
