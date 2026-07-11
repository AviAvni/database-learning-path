pub mod expr;
pub mod interp;
pub mod jit;
pub mod vectorized;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// n_cols columns of n_rows f64s in [0, 1). Columnar layout.
pub fn gen_cols(n_cols: usize, n_rows: usize, seed: u64) -> Vec<Vec<f64>> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n_cols)
        .map(|_| (0..n_rows).map(|_| rng.gen::<f64>()).collect())
        .collect()
}

/// Row-major copy of the same data: rows[i] = [cols[0][i], cols[1][i], ...].
/// The interpreter and the JIT both take one row at a time.
pub fn to_rows(cols: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let n_rows = cols[0].len();
    (0..n_rows)
        .map(|i| cols.iter().map(|c| c[i]).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_agree() {
        let cols = gen_cols(3, 5, 42);
        let rows = to_rows(&cols);
        assert_eq!(rows[4][2], cols[2][4]);
    }
}
