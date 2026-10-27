//! The topic-10 way: an ORDERED rule list applied bottom-up to
//! fixpoint. Deliberately greedy and destructive — once a rule
//! rewrites a node, the old form is gone. The rule order below
//! contains a trap: strength reduction (x*2 → x<<1) fires before
//! division reassociation ((x*y)/z → x*(y/z)), so on (a*2)/2 the
//! Mul is destroyed before the Div rule can see it and the rewriter
//! parks at (a<<1)/2 forever. egg keeps BOTH forms and finds `a`.

use crate::expr::Expr;

/// Bottom-up fixpoint. Returns (result, rule firings).
pub fn hand_optimize(e: &Expr) -> (Expr, usize) {
    hand_optimize_ordered(e, false)
}

/// Same rules, same fixpoint loop; `div_first` moves R4 (division
/// reassociation) ahead of R2 (strength reduction). This is the control for
/// "just reorder the rules": it fixes the `(a*2)/2` trap and nothing else
/// about the approach (see `normalise_bench`).
pub fn hand_optimize_ordered(e: &Expr, div_first: bool) -> (Expr, usize) {
    let mut cur = e.clone();
    let mut steps = 0;
    loop {
        let before = steps;
        cur = pass(&cur, &mut steps, div_first);
        if steps == before {
            return (cur, steps);
        }
    }
}

fn pass(e: &Expr, steps: &mut usize, div_first: bool) -> Expr {
    use Expr::*;
    let rebuilt = match e {
        Add(a, b) => Add(Box::new(pass(a, steps, div_first)), Box::new(pass(b, steps, div_first))),
        Mul(a, b) => Mul(Box::new(pass(a, steps, div_first)), Box::new(pass(b, steps, div_first))),
        Div(a, b) => Div(Box::new(pass(a, steps, div_first)), Box::new(pass(b, steps, div_first))),
        Shl(a, b) => Shl(Box::new(pass(a, steps, div_first)), Box::new(pass(b, steps, div_first))),
        leaf => leaf.clone(),
    };
    match try_rules(&rebuilt, div_first) {
        Some(next) => {
            *steps += 1;
            next
        }
        None => rebuilt,
    }
}

/// First matching rule wins — THE defining property (and flaw) of
/// the hand-ordered approach.
fn try_rules(e: &Expr, div_first: bool) -> Option<Expr> {
    use Expr::*;
    // R1: constant folding
    if let Add(a, b) = e {
        if let (Num(x), Num(y)) = (&**a, &**b) {
            return Some(Num(x + y));
        }
    }
    if let Mul(a, b) = e {
        if let (Num(x), Num(y)) = (&**a, &**b) {
            return Some(Num(x * y));
        }
    }
    if let Div(a, b) = e {
        if let (Num(x), Num(y)) = (&**a, &**b) {
            if *y != 0 {
                return Some(Num(x / y));
            }
        }
    }
    if div_first {
        if let Some(r) = div_rules(e) {
            return Some(r);
        }
    }
    // R2: strength reduction — ordered too early ON PURPOSE
    if let Mul(a, b) = e {
        if **b == Num(2) {
            return Some(Shl(a.clone(), Box::new(Num(1))));
        }
        if **a == Num(2) {
            return Some(Shl(b.clone(), Box::new(Num(1))));
        }
    }
    // R3: identities
    if let Mul(a, b) = e {
        if **b == Num(1) {
            return Some((**a).clone());
        }
        if **a == Num(1) {
            return Some((**b).clone());
        }
        if **a == Num(0) || **b == Num(0) {
            return Some(Num(0));
        }
    }
    if let Add(a, b) = e {
        if **b == Num(0) {
            return Some((**a).clone());
        }
        if **a == Num(0) {
            return Some((**b).clone());
        }
    }
    // R4: division reassociation — too late once R2 fired
    if !div_first {
        if let Some(r) = div_rules(e) {
            return Some(r);
        }
    }
    // R5: x/x → 1 (assumes x ≠ 0 — the soundness caveat, see README)
    if let Div(a, b) = e {
        if a == b {
            return Some(Num(1));
        }
    }
    None
}

/// R4: division reassociation and x/1.
fn div_rules(e: &Expr) -> Option<Expr> {
    use Expr::*;
    if let Div(m, z) = e {
        if let Mul(x, y) = &**m {
            return Some(Mul(x.clone(), Box::new(Div(y.clone(), z.clone()))));
        }
        if **z == Num(1) {
            return Some((**m).clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::{add, cost, div, mul, num, var};

    #[test]
    fn folds_and_identities() {
        assert_eq!(hand_optimize(&mul(add(var("a"), num(0)), num(1))).0, var("a"));
        assert_eq!(hand_optimize(&add(num(2), num(3))).0, num(5));
        assert_eq!(hand_optimize(&div(var("b"), var("b"))).0, num(1));
    }

    #[test]
    fn the_ordering_trap() {
        // (a*2)/2 should be a (cost 1) — but R2 destroys the Mul
        // before R4 can reassociate, and the rewriter parks here:
        let (got, _) = hand_optimize(&div(mul(var("a"), num(2)), num(2)));
        assert_eq!(
            got,
            Expr::Div(
                Box::new(Expr::Shl(Box::new(var("a")), Box::new(num(1)))),
                Box::new(num(2))
            )
        );
        assert_eq!(cost(&got), 5); // egg gets cost 1 — see eqsat.rs
    }
}
