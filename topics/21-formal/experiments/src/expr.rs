//! Tiny arithmetic IR standing in for a query-expression tree —
//! the same shape as topic 10's planner Expr, small enough that a
//! hand-ordered rewriter and an e-graph can both chew on it.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Num(i64),
    Var(String),
    Add(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Shl(Box<Expr>, Box<Expr>),
}

pub fn var(s: &str) -> Expr {
    Expr::Var(s.to_string())
}
pub fn num(n: i64) -> Expr {
    Expr::Num(n)
}
pub fn add(a: Expr, b: Expr) -> Expr {
    Expr::Add(Box::new(a), Box::new(b))
}
pub fn mul(a: Expr, b: Expr) -> Expr {
    Expr::Mul(Box::new(a), Box::new(b))
}
pub fn div(a: Expr, b: Expr) -> Expr {
    Expr::Div(Box::new(a), Box::new(b))
}

/// Cost = node count (egg's AstSize) — smaller tree, cheaper plan.
pub fn cost(e: &Expr) -> usize {
    match e {
        Expr::Num(_) | Expr::Var(_) => 1,
        Expr::Add(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) | Expr::Shl(a, b) => {
            1 + cost(a) + cost(b)
        }
    }
}

/// Random expression tree: full binary down to depth 0, leaves are
/// vars (70%) or small constants; ops weighted Add/Mul/Div = 3/3/1.
pub fn gen_expr(depth: usize, seed: u64) -> Expr {
    fn go(depth: usize, rng: &mut ChaCha8Rng) -> Expr {
        if depth == 0 {
            return if rng.gen_range(0..10) < 7 {
                var(["a", "b", "c", "d"][rng.gen_range(0..4)])
            } else {
                num(rng.gen_range(0..=3))
            };
        }
        let (a, b) = (go(depth - 1, rng), go(depth - 1, rng));
        match rng.gen_range(0..7) {
            0..=2 => add(a, b),
            3..=5 => mul(a, b),
            _ => div(a, b),
        }
    }
    go(depth, &mut ChaCha8Rng::seed_from_u64(seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_counts_nodes() {
        assert_eq!(cost(&div(mul(var("a"), num(2)), num(2))), 5);
    }

    #[test]
    fn gen_is_deterministic() {
        assert_eq!(gen_expr(6, 1), gen_expr(6, 1));
        assert_ne!(gen_expr(6, 1), gen_expr(6, 2));
    }
}
