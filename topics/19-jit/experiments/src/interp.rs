//! The strawman: an AST-walking interpreter, one row at a time.
//!
//! Every node costs a match dispatch + two recursive calls — the
//! per-tuple overhead Neumann VLDB'11 and topic 11 both target.

use crate::expr::Expr;

pub fn eval(e: &Expr, row: &[f64]) -> f64 {
    match e {
        Expr::Col(i) => row[*i],
        Expr::Const(c) => *c,
        Expr::Add(a, b) => eval(a, row) + eval(b, row),
        Expr::Mul(a, b) => eval(a, row) * eval(b, row),
        Expr::Lt(a, b) => (eval(a, row) < eval(b, row)) as u8 as f64,
        Expr::And(a, b) => ((eval(a, row) != 0.0) & (eval(b, row) != 0.0)) as u8 as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Expr::*;

    #[test]
    fn hand_checked() {
        // (c0 + 2.0) * (c1 < c0)
        let e = Mul(
            Box::new(Add(Box::new(Col(0)), Box::new(Const(2.0)))),
            Box::new(Lt(Box::new(Col(1)), Box::new(Col(0)))),
        );
        assert_eq!(eval(&e, &[1.0, 0.5]), 3.0);
        assert_eq!(eval(&e, &[1.0, 1.5]), 0.0);
    }
}
