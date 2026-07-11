//! Equality saturation with egg (POPL'21). Instead of picking ONE
//! rewrite per node like hand.rs, grow an e-graph holding ALL
//! equivalent forms simultaneously, then extract the cheapest.
//!
//! egg anchors (~/repos/egg/src):
//!   egraph.rs:970  EGraph::add   — hashcons (memo) lookup-or-insert
//!   egraph.rs:1147 EGraph::union — union-find merge, defers repair
//!   egraph.rs:1416 EGraph::rebuild / :1346 process_unions —
//!                  deferred congruence closure, THE egg contribution
//!   run.rs:138     Runner (iter/node/time limits, StopReason :237)
//!   extract.rs:41  Extractor, :116 CostFunction, :157 AstSize
//!   machine.rs:8   pattern-matching VM (Bind/Scan/Compare)

use crate::expr::Expr;
use egg::*;

define_language! {
    /// Same operators as expr::Expr, in egg's flat RecExpr form.
    pub enum Math {
        "+" = Add([Id; 2]),
        "*" = Mul([Id; 2]),
        "/" = Div([Id; 2]),
        "<<" = Shl([Id; 2]),
        Num(i64),
        Var(Symbol),
    }
}

pub fn to_rec(e: &Expr) -> RecExpr<Math> {
    fn go(e: &Expr, out: &mut RecExpr<Math>) -> Id {
        match e {
            Expr::Num(n) => out.add(Math::Num(*n)),
            Expr::Var(s) => out.add(Math::Var(Symbol::from(s.as_str()))),
            Expr::Add(a, b) => {
                let (x, y) = (go(a, out), go(b, out));
                out.add(Math::Add([x, y]))
            }
            Expr::Mul(a, b) => {
                let (x, y) = (go(a, out), go(b, out));
                out.add(Math::Mul([x, y]))
            }
            Expr::Div(a, b) => {
                let (x, y) = (go(a, out), go(b, out));
                out.add(Math::Div([x, y]))
            }
            Expr::Shl(a, b) => {
                let (x, y) = (go(a, out), go(b, out));
                out.add(Math::Shl([x, y]))
            }
        }
    }
    let mut r = RecExpr::default();
    go(e, &mut r);
    r
}

pub fn from_rec(r: &RecExpr<Math>) -> Expr {
    fn go(r: &RecExpr<Math>, id: Id) -> Expr {
        let b = |x: &Id| Box::new(go(r, *x));
        match &r[id] {
            Math::Num(n) => Expr::Num(*n),
            Math::Var(s) => Expr::Var(s.to_string()),
            Math::Add([a, x]) => Expr::Add(b(a), b(x)),
            Math::Mul([a, x]) => Expr::Mul(b(a), b(x)),
            Math::Div([a, x]) => Expr::Div(b(a), b(x)),
            Math::Shl([a, x]) => Expr::Shl(b(a), b(x)),
        }
    }
    go(r, Id::from(r.as_ref().len() - 1))
}

#[derive(Debug)]
pub struct EqsatReport {
    pub best: Expr,
    pub cost: usize,
    pub enodes: usize,
    pub eclasses: usize,
    pub iters: usize,
    pub stop: String,
}

/// STUB — equality saturation:
///   1. rules: the SAME set as hand.rs but UNORDERED, as rewrite!
///      patterns — e.g.
///        rewrite!("mul2-shl";    "(* ?x 2)"       => "(<< ?x 1)")
///        rewrite!("div-reassoc"; "(/ (* ?x ?y) ?z)" => "(* ?x (/ ?y ?z))")
///        rewrite!("div-same";    "(/ ?x ?x)"      => "1")
///        rewrite!("mul-one";     "(* ?x 1)"       => "?x")
///        rewrite!("add-zero";    "(+ ?x 0)"       => "?x")
///        rewrite!("mul-comm";    "(* ?x ?y)"      => "(* ?y ?x)")
///        rewrite!("add-comm";    "(+ ?x ?y)"      => "(+ ?y ?x)")
///      (div-same on (/ 2 2) already folds the trap's constant —
///      no ConstantFold Analysis needed; that's the stretch goal.)
///   2. Runner::default().with_expr(&to_rec(e)).run(&rules)
///   3. Extractor::new(&runner.egraph, AstSize) then
///      find_best(runner.roots[0])
///   4. fill the report from runner.egraph.total_number_of_nodes(),
///      .number_of_classes(), runner.iterations.len(), stop_reason
pub fn egg_optimize(e: &Expr) -> EqsatReport {
    let _ = e;
    todo!("equality saturation (see module docs)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::{cost, div, gen_expr, mul, num, var};
    use crate::hand::hand_optimize;

    #[test]
    fn roundtrip() {
        for seed in 0..8 {
            let e = gen_expr(6, seed);
            assert_eq!(from_rec(&to_rec(&e)), e);
        }
    }

    #[test]
    fn egg_finds_what_hand_misses() {
        let trap = div(mul(var("a"), num(2)), num(2));
        let r = egg_optimize(&trap);
        assert_eq!(r.best, var("a"));
        assert_eq!(r.cost, 1);
    }

    #[test]
    fn egg_never_worse_than_hand() {
        for seed in 0..12 {
            let e = gen_expr(6, seed);
            let (h, _) = hand_optimize(&e);
            let r = egg_optimize(&e);
            assert!(r.cost <= cost(&h), "seed {seed}: egg {} > hand {}", r.cost, cost(&h));
        }
    }
}
