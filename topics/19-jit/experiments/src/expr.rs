//! The expression language all three executors share.
//!
//! Everything is f64. Comparisons/booleans use 1.0 / 0.0 — the
//! branch-free convention from topic 17, which also keeps the JIT
//! to a single CLIF type.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Debug, Clone)]
pub enum Expr {
    Col(usize),
    Const(f64),
    Add(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    /// 1.0 if lhs < rhs else 0.0
    Lt(Box<Expr>, Box<Expr>),
    /// 1.0 if both sides are nonzero else 0.0
    And(Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn node_count(&self) -> usize {
        match self {
            Expr::Col(_) | Expr::Const(_) => 1,
            Expr::Add(a, b) | Expr::Mul(a, b) | Expr::Lt(a, b) | Expr::And(a, b) => {
                1 + a.node_count() + b.node_count()
            }
        }
    }
}

/// Random full binary expression tree of the given depth (depth 0 =
/// leaf). Deterministic per seed. Top level is arithmetic-flavored;
/// Lt/And appear with the same probability everywhere, so deep trees
/// mix filters and math like a real WHERE clause.
pub fn gen_expr(depth: usize, n_cols: usize, seed: u64) -> Expr {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    gen_node(depth, n_cols, &mut rng)
}

fn gen_node(depth: usize, n_cols: usize, rng: &mut ChaCha8Rng) -> Expr {
    if depth == 0 {
        return if rng.gen_bool(0.7) {
            Expr::Col(rng.gen_range(0..n_cols))
        } else {
            Expr::Const(rng.gen_range(0.0..1.0))
        };
    }
    let a = Box::new(gen_node(depth - 1, n_cols, rng));
    let b = Box::new(gen_node(depth - 1, n_cols, rng));
    match rng.gen_range(0..10) {
        0..=3 => Expr::Add(a, b),
        4..=6 => Expr::Mul(a, b),
        7..=8 => Expr::Lt(a, b),
        _ => Expr::And(a, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gen_is_deterministic_and_sized() {
        let e1 = gen_expr(6, 4, 7);
        let e2 = gen_expr(6, 4, 7);
        assert_eq!(format!("{e1:?}"), format!("{e2:?}"));
        assert_eq!(e1.node_count(), (1 << 7) - 1); // full tree, depth 6
    }
}
