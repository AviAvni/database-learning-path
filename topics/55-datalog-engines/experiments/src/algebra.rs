//! Lane 3 (exercise): the GraphBLAS formulation of the same fixpoints.
//!
//! Over the boolean semiring (`or` as add, `and` as multiply) the closure's
//! recursive rule is a matrix product. With `A` the adjacency matrix, `T` the
//! closure so far and `D` the previous round's new entries:
//!
//! ```text
//! D' = (D · A) ⟨¬T⟩        // masked mxm with a COMPLEMENT mask: write only
//! T  = T ∨ D'              // where T is still empty. Fixpoint when D' = ∅.
//! ```
//!
//! That is semi-naive evaluation with the anti-join against `T` folded into
//! the product as a mask (GrB_mxm with `GrB_COMP` and `GrB_STRUCTURE`), and
//! `D` as the delta. Same-generation is `D' = (Aᵀ · D · A) ⟨¬SG⟩` — two
//! products per round.
//!
//! Write the kernel in pure Rust (row-wise Gustavson, as in topic 20), with
//! the mask held as a dense bit matrix, so the complement test is one bit
//! read. `BitMatrix` is provided; the kernel and the two fixpoints are yours.

use crate::gen::{Csr, Graph};
use crate::hash::{mix, pack};

/// An n x n boolean matrix, one bit per cell, row-major.
#[derive(Clone, Debug)]
pub struct BitMatrix {
    pub n: usize,
    words_per_row: usize,
    bits: Vec<u64>,
}

impl BitMatrix {
    pub fn new(n: usize) -> BitMatrix {
        let words_per_row = n.div_ceil(64);
        BitMatrix { n, words_per_row, bits: vec![0; n * words_per_row] }
    }
    #[inline]
    pub fn get(&self, r: usize, c: usize) -> bool {
        self.bits[r * self.words_per_row + c / 64] >> (c % 64) & 1 == 1
    }
    /// Sets the bit; returns true if it was clear.
    #[inline]
    pub fn set(&mut self, r: usize, c: usize) -> bool {
        let w = &mut self.bits[r * self.words_per_row + c / 64];
        let m = 1u64 << (c % 64);
        let was_clear = *w & m == 0;
        *w |= m;
        was_clear
    }
    pub fn count(&self) -> u64 {
        self.bits.iter().map(|w| w.count_ones() as u64).sum()
    }
    /// Sorted (row, col) pairs.
    pub fn pairs(&self) -> Vec<(u32, u32)> {
        let mut v = Vec::with_capacity(self.count() as usize);
        for r in 0..self.n {
            for c in 0..self.n {
                if self.get(r, c) {
                    v.push((r as u32, c as u32));
                }
            }
        }
        v
    }
    /// Same order-independent checksum lane 1 reports.
    pub fn checksum(&self) -> u64 {
        self.pairs().iter().fold(0u64, |a, &(x, y)| a.wrapping_add(mix(pack(x, y))))
    }
}

#[derive(Clone, Debug, Default)]
pub struct AlgebraStats {
    /// Rounds, including the final round whose product is empty.
    pub iterations: u32,
    /// Semiring multiply-accumulates attempted (each `D[i,k]` with each `A[k,j]`).
    pub flops: u64,
    /// Entries written by the masked products (new tuples), summed over rounds.
    pub new_entries: u64,
    pub ms: f64,
}

pub struct AlgebraRun {
    pub stats: AlgebraStats,
    pub result: BitMatrix,
}

/// `C = (D · A) ⟨¬mask⟩` over the boolean semiring: `C[i,j]` is set iff some
/// `k` has `D[i,k]` and `A[k,j]` and `mask[i,j]` is clear. Returns `C` as a
/// CSR (rows sorted, no duplicates) and the number of multiply-accumulates
/// attempted. `d` and `a` are n x n.
pub fn mxm_complement_mask(_d: &Csr, _a: &Csr, _mask: &BitMatrix) -> (Csr, u64) {
    todo!("lane 3: row-wise Gustavson with the complement mask tested per candidate")
}

/// Transitive closure as the fixpoint of `D' = (D · A) ⟨¬T⟩`.
pub fn tc_algebra(_g: &Graph) -> AlgebraRun {
    todo!("lane 3: start from T = D = A, then repeat D = mxm_complement_mask(D, A, T), T |= D until D is empty")
}

/// Same generation as the fixpoint of `D' = (Aᵀ · D · A) ⟨¬SG⟩`, seeded with
/// `(Aᵀ · A) ⟨x ≠ y⟩`. The mask applies to the second product only.
pub fn sg_algebra(_g: &Graph) -> AlgebraRun {
    todo!("lane 3: two chained products per round; transpose A once")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{sg, tc_linear, tc_pairs, Mode};
    use crate::gen::{chain, grid, power_law};

    #[test]
    fn mask_excludes_existing_entries() {
        // D = {(0,1)}, A = {(1,2),(1,3)}, mask has (0,2): only (0,3) survives.
        let d = Graph::from_raw(4, vec![(0, 1)]).out_csr();
        let a = Graph::from_raw(4, vec![(1, 2), (1, 3)]).out_csr();
        let mut m = BitMatrix::new(4);
        m.set(0, 2);
        let (c, flops) = mxm_complement_mask(&d, &a, &m);
        assert_eq!(c.row(0), &[3]);
        assert_eq!(c.nnz(), 1);
        assert_eq!(flops, 2);
    }

    #[test]
    fn tc_matches_lane_1_and_the_oracle() {
        for g in [power_law(120, 360, 1), grid(6, 6), chain(30)] {
            let r = tc_algebra(&g);
            assert_eq!(r.result.pairs(), tc_pairs(&g));
            assert_eq!(r.result.checksum(), tc_linear(&g, Mode::SemiNaive).checksum);
        }
    }

    #[test]
    fn tc_flops_equal_lane_1_semi_naive_derivations() {
        // The product visits exactly the (delta tuple, arc) pairs the hash
        // join does: the mask changes what is written, not what is multiplied.
        for g in [power_law(120, 360, 1), grid(6, 6), chain(30)] {
            let r = tc_algebra(&g);
            let sn = tc_linear(&g, Mode::SemiNaive);
            assert_eq!(r.stats.iterations, sn.stats.iterations);
            assert_eq!(r.stats.flops + g.edges.len() as u64, sn.stats.derivations);
        }
    }

    #[test]
    fn sg_matches_lane_1() {
        for g in [power_law(80, 160, 2), grid(6, 6)] {
            let r = sg_algebra(&g);
            let want = sg(&g, Mode::SemiNaive);
            assert_eq!(r.result.count(), want.size);
            assert_eq!(r.result.checksum(), want.checksum);
        }
    }
}
