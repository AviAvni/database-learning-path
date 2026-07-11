//! SpGEMM: C = A*B. Two accumulators for Gustavson's row-wise
//! formulation — the saxpy3 coarse-task choice (GB_AxB_saxpy3.c:29-60).
//!
//! PROVIDED: hash accumulator (obviously correct, allocator-heavy).
//! STUB:     dense SPA (Gustavson '78) — symbolic + numeric phases.

use crate::csr::Csr;
use std::collections::HashMap;

/// Reference: per-row HashMap accumulation — the "hash task".
pub fn spgemm_hash(a: &Csr, b: &Csr) -> Csr {
    assert_eq!(a.n, b.n);
    let mut rowptr = vec![0usize; a.n + 1];
    let mut colidx = Vec::new();
    let mut vals = Vec::new();
    let mut acc: HashMap<u32, f64> = HashMap::new();
    for i in 0..a.n {
        acc.clear();
        let (acols, avals) = a.row(i);
        for (&k, &av) in acols.iter().zip(avals) {
            let (bcols, bvals) = b.row(k as usize);
            for (&j, &bv) in bcols.iter().zip(bvals) {
                *acc.entry(j).or_insert(0.0) += av * bv;
            }
        }
        let mut row: Vec<(u32, f64)> = acc.iter().map(|(&j, &v)| (j, v)).collect();
        row.sort_unstable_by_key(|&(j, _)| j);
        for (j, v) in row {
            colidx.push(j);
            vals.push(v);
        }
        rowptr[i + 1] = colidx.len();
    }
    Csr { n: a.n, rowptr, colidx, vals }
}

/// Total multiply count — Gustavson's lower bound, saxpy3's flopcount
/// pre-pass (GB_AxB_saxpy3_flopcount.c).
pub fn flopcount(a: &Csr, b: &Csr) -> usize {
    (0..a.n)
        .flat_map(|i| a.row(i).0.iter())
        .map(|&k| b.row(k as usize).0.len())
        .sum()
}

/// STUB — Gustavson with a dense SPA (sparse accumulator):
///
///   workspace (alloc ONCE, reuse across rows):
///     spa_val:  Vec<f64>  len n
///     spa_mark: Vec<u32>  len n   (stamp = row id + 1; ≠ stamp ⇒ stale
///                                  — no O(n) clear per row)
///     occupied: Vec<u32>          (cols hit this row, for the gather)
///   symbolic option: run pattern-only first to size colidx exactly,
///   or push-and-grow (measure both if curious — Gustavson vs cudf).
///   numeric per row i:
///     for (k, av) in A.row(i): for (j, bv) in B.row(k):
///       if spa_mark[j] != stamp { mark, spa_val[j] = av*bv, occupied.push(j) }
///       else { spa_val[j] += av*bv }
///     sort occupied, gather into colidx/vals  (rows must be sorted —
///     the tests compare against spgemm_hash exactly)
pub fn spgemm_spa(a: &Csr, b: &Csr) -> Csr {
    let _ = (a, b);
    todo!("Gustavson dense-SPA SpGEMM (see module docs)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csr::rmat;

    #[test]
    fn hash_hand_checked() {
        // A = [[1,1],[0,1]]; A*A = [[1,2],[0,1]]
        let a = Csr::from_edges(2, &[(0, 0, 1.0), (0, 1, 1.0), (1, 1, 1.0)]);
        let c = spgemm_hash(&a, &a);
        assert_eq!(c.row(0), (&[0u32, 1][..], &[1.0f64, 2.0][..]));
        assert_eq!(c.row(1), (&[1u32][..], &[1.0f64][..]));
    }

    #[test]
    fn spa_matches_hash_on_rmat() {
        let a = rmat(8, 4, 5);
        let want = spgemm_hash(&a, &a);
        let got = spgemm_spa(&a, &a);
        assert_eq!(got.rowptr, want.rowptr);
        assert_eq!(got.colidx, want.colidx);
        for (g, w) in got.vals.iter().zip(&want.vals) {
            assert!((g - w).abs() < 1e-9);
        }
    }

    #[test]
    fn flopcount_bounds_output() {
        let a = rmat(8, 4, 6);
        let c = spgemm_hash(&a, &a);
        assert!(flopcount(&a, &a) >= c.nnz());
    }
}
