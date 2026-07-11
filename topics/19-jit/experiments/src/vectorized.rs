//! Topic 11's answer: column-at-a-time evaluation. One match
//! dispatch per NODE per BATCH instead of per node per row; the
//! inner loops are tight autovectorizable kernels over Vec<f64>.
//!
//! Cost it does NOT hide: every interior node materializes a
//! full-length temporary vector (memory traffic ∝ node count) —
//! the reason JIT can still win on deep expressions.

use crate::expr::Expr;

pub fn eval_batch(e: &Expr, cols: &[Vec<f64>]) -> Vec<f64> {
    match e {
        Expr::Col(i) => cols[*i].clone(),
        Expr::Const(c) => vec![*c; cols[0].len()],
        Expr::Add(a, b) => zip_map(&eval_batch(a, cols), &eval_batch(b, cols), |x, y| x + y),
        Expr::Mul(a, b) => zip_map(&eval_batch(a, cols), &eval_batch(b, cols), |x, y| x * y),
        Expr::Lt(a, b) => zip_map(&eval_batch(a, cols), &eval_batch(b, cols), |x, y| {
            (x < y) as u8 as f64
        }),
        Expr::And(a, b) => zip_map(&eval_batch(a, cols), &eval_batch(b, cols), |x, y| {
            ((x != 0.0) & (y != 0.0)) as u8 as f64
        }),
    }
}

fn zip_map(a: &[f64], b: &[f64], f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    a.iter().zip(b).map(|(&x, &y)| f(x, y)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{expr::gen_expr, gen_cols, interp, to_rows};

    #[test]
    fn matches_interpreter_on_random_exprs() {
        let cols = gen_cols(4, 257, 11);
        let rows = to_rows(&cols);
        for seed in 0..8 {
            let e = gen_expr(5, 4, seed);
            let batch = eval_batch(&e, &cols);
            for (i, row) in rows.iter().enumerate() {
                assert_eq!(batch[i], interp::eval(&e, row), "seed {seed} row {i}");
            }
        }
    }
}
