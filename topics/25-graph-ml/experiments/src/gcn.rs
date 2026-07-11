//! Two-layer GCN forward pass (Kipf & Welling, ICLR'17):
//!   Z = softmax( A_hat * relu(A_hat * X * W1) * W2 )
//! with A_hat = D^{-1/2} (A + I) D^{-1/2}.
//! The dense oracle is provided; the sparse path (what an engine would
//! actually run: two SpMMs + two small dense matmuls) is the stub.

use crate::dense::{matmul, relu_inplace, row_softmax, Mat};
use crate::graph::Csr;
use crate::spmm::CsrF32;

/// Dense n x n A_hat — the definitional oracle. deg counts A + I.
pub fn gcn_norm_dense(g: &Csr) -> Mat {
    let n = g.n;
    let dinv: Vec<f32> = (0..n as u32)
        .map(|v| 1.0 / ((g.degree(v) + 1) as f32).sqrt())
        .collect();
    let mut a = Mat::zeros(n, n);
    for v in 0..n as u32 {
        *a.at_mut(v as usize, v as usize) = dinv[v as usize] * dinv[v as usize];
        for &u in g.neigh(v) {
            *a.at_mut(v as usize, u as usize) = dinv[v as usize] * dinv[u as usize];
        }
    }
    a
}

/// Dense oracle forward: softmax(A(relu(A X W1)) W2).
pub fn gcn_forward_dense(a_hat: &Mat, x: &Mat, w1: &Mat, w2: &Mat) -> Mat {
    let mut h = matmul(a_hat, &matmul(x, w1));
    relu_inplace(&mut h);
    row_softmax(&matmul(a_hat, &matmul(&h, w2)))
}

/// STUB — A_hat as CSR: A + I with symmetric normalization.
///
/// Rows must stay sorted by target (the self-loop entry v goes in target
/// order, not appended). vals[e] for edge (v, u) = dinv[v] * dinv[u] with
/// dinv[v] = 1 / sqrt(degree(v) + 1). PyG: gcn_conv.py:45-71 (gcn_norm),
/// same fill_value=1 self-loop convention.
pub fn gcn_norm(_g: &Csr) -> CsrF32 {
    todo!("A + I, symmetric normalization, sorted rows")
}

/// STUB — sparse forward pass, must match gcn_forward_dense to 1e-4.
///
/// Do the dense transform FIRST, then aggregate: spmm(a_hat, X W1) does
/// 2 * nnz * hidden FLOPs; aggregating first then transforming would do
/// 2 * nnz * in_dim — associativity is a query plan (in_dim vs hidden
/// decides, exactly like join ordering). PyG fuses this as
/// message_and_aggregate = spmm(adj_t, x) (gcn_conv.py:273).
pub fn gcn_forward(_a_hat: &CsrF32, _x: &Mat, _w1: &Mat, _w2: &Mat) -> Mat {
    todo!("spmm -> relu -> spmm -> softmax, transform before aggregate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dense::{glorot, max_abs_diff};
    use crate::graph::gen_sbm;
    use crate::spmm::to_dense;

    #[test]
    fn dense_norm_is_symmetric_with_correct_diagonal() {
        let (g, _) = gen_sbm(2, 20, 0.3, 0.05, 3);
        let a = gcn_norm_dense(&g);
        for v in 0..g.n {
            let expect = 1.0 / (g.degree(v as u32) + 1) as f32;
            assert!((a.at(v, v) - expect).abs() < 1e-6);
            for u in 0..g.n {
                assert!((a.at(v, u) - a.at(u, v)).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn sparse_norm_matches_dense() {
        let (g, _) = gen_sbm(2, 24, 0.25, 0.04, 8);
        let sparse = gcn_norm(&g);
        assert_eq!(sparse.n, g.n);
        // rows sorted (contract for has_edge-style lookups downstream)
        for i in 0..sparse.n {
            let row = &sparse.targets[sparse.offsets[i]..sparse.offsets[i + 1]];
            assert!(row.windows(2).all(|w| w[0] < w[1]));
        }
        assert!(max_abs_diff(&to_dense(&sparse), &gcn_norm_dense(&g)) < 1e-6);
    }

    #[test]
    fn sparse_forward_matches_dense_oracle() {
        let (g, _) = gen_sbm(4, 25, 0.2, 0.02, 12);
        let x = glorot(g.n, 16, 100);
        let w1 = glorot(16, 8, 101);
        let w2 = glorot(8, 4, 102);
        let dense = gcn_forward_dense(&gcn_norm_dense(&g), &x, &w1, &w2);
        let sparse = gcn_forward(&gcn_norm(&g), &x, &w1, &w2);
        assert!(max_abs_diff(&dense, &sparse) < 1e-4);
        // sanity: output rows are distributions
        for i in 0..g.n {
            let s: f32 = sparse.row(i).iter().sum();
            assert!((s - 1.0).abs() < 1e-4);
        }
    }
}
