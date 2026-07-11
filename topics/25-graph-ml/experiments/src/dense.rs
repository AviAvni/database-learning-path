//! Minimal dense f32 matrix — just enough linear algebra for a 2-layer GCN
//! and skip-gram embeddings. No BLAS, no SIMD intrinsics: the naive triple
//! loop with the k-loop innermost (row-major friendly) is the baseline every
//! ML framework kernel is measured against.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Clone)]
pub struct Mat {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f32>, // row-major
}

impl Mat {
    pub fn zeros(rows: usize, cols: usize) -> Mat {
        Mat { rows, cols, data: vec![0.0; rows * cols] }
    }
    #[inline]
    pub fn at(&self, i: usize, j: usize) -> f32 {
        self.data[i * self.cols + j]
    }
    #[inline]
    pub fn at_mut(&mut self, i: usize, j: usize) -> &mut f32 {
        &mut self.data[i * self.cols + j]
    }
    pub fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.cols..(i + 1) * self.cols]
    }
}

/// Glorot/Xavier-uniform init: U(-s, s) with s = sqrt(6 / (rows + cols)).
pub fn glorot(rows: usize, cols: usize, seed: u64) -> Mat {
    let s = (6.0 / (rows + cols) as f64).sqrt() as f32;
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let data = (0..rows * cols).map(|_| rng.gen_range(-s..s)).collect();
    Mat { rows, cols, data }
}

/// C = A x B, ikj loop order (streams B's rows, accumulates into C's row).
pub fn matmul(a: &Mat, b: &Mat) -> Mat {
    assert_eq!(a.cols, b.rows);
    let mut c = Mat::zeros(a.rows, b.cols);
    for i in 0..a.rows {
        for k in 0..a.cols {
            let aik = a.at(i, k);
            if aik == 0.0 {
                continue;
            }
            let brow = b.row(k);
            let crow = &mut c.data[i * b.cols..(i + 1) * b.cols];
            for j in 0..b.cols {
                crow[j] += aik * brow[j];
            }
        }
    }
    c
}

pub fn relu_inplace(m: &mut Mat) {
    for x in &mut m.data {
        if *x < 0.0 {
            *x = 0.0;
        }
    }
}

pub fn row_softmax(m: &Mat) -> Mat {
    let mut out = Mat::zeros(m.rows, m.cols);
    for i in 0..m.rows {
        let row = m.row(i);
        let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let mut sum = 0.0f32;
        for j in 0..m.cols {
            let e = (row[j] - max).exp();
            *out.at_mut(i, j) = e;
            sum += e;
        }
        for j in 0..m.cols {
            *out.at_mut(i, j) /= sum;
        }
    }
    out
}

pub fn max_abs_diff(a: &Mat, b: &Mat) -> f32 {
    assert_eq!((a.rows, a.cols), (b.rows, b.cols));
    a.data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matmul_known() {
        let a = Mat { rows: 2, cols: 2, data: vec![1.0, 2.0, 3.0, 4.0] };
        let b = Mat { rows: 2, cols: 2, data: vec![5.0, 6.0, 7.0, 8.0] };
        let c = matmul(&a, &b);
        assert_eq!(c.data, vec![19.0, 22.0, 43.0, 50.0]);
    }

    #[test]
    fn softmax_rows_sum_to_one() {
        let m = Mat { rows: 2, cols: 3, data: vec![1.0, 2.0, 3.0, -1.0, 0.0, 1.0] };
        let s = row_softmax(&m);
        for i in 0..2 {
            let sum: f32 = s.row(i).iter().sum();
            assert!((sum - 1.0).abs() < 1e-6);
        }
        assert!(s.at(0, 2) > s.at(0, 1) && s.at(0, 1) > s.at(0, 0));
    }
}
