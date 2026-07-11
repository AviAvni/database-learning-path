//! YOU implement: three-valued (Kleene) predicate evaluation and the
//! TLP check — SQLancer's TLPWhereOracle over a mini row engine.
//!
//! PROVIDED: the predicate AST, seeded generators, and a BUGGY
//! "optimized" filter that is NULL-blind: it evaluates two-valued
//! (NULL coerced to FALSE at every leaf), so `Not` over an unknown
//! becomes TRUE — the classic NULL-blind pushdown bug that TLP was
//! invented to catch.
//!
//! Contract:
//! - `eval3`: Kleene logic. AND: false dominates (NULL AND FALSE =
//!   FALSE!); OR: true dominates; NOT NULL = NULL; comparisons with
//!   a NULL operand are NULL; IsNull(p) is ALWAYS two-valued.
//! - `filter_correct`: rows where eval3 == Some(true).
//! - `tlp_check(rows, p, filter)`: |filter(p)| + |filter(Not p)| +
//!   |filter(IsNull p)| == |rows| (the WHERE partition identity —
//!   counts suffice because the three sets are disjoint).

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub type Row = Vec<Option<i64>>; // nullable columns

#[derive(Debug, Clone)]
pub enum Expr {
    Col(usize),
    Const(Option<i64>),
}

#[derive(Debug, Clone)]
pub enum Pred {
    Eq(Expr, Expr),
    Lt(Expr, Expr),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
    /// SQL's `p IS NULL` lifted to predicates — two-valued by definition
    IsNull(Box<Pred>),
}

fn eval_expr(e: &Expr, row: &Row) -> Option<i64> {
    match e {
        Expr::Col(i) => row[*i],
        Expr::Const(v) => *v,
    }
}

/// Kleene three-valued evaluation: None = UNKNOWN.
pub fn eval3(p: &Pred, row: &Row) -> Option<bool> {
    let _ = (p, row);
    todo!()
}

pub fn filter_correct<'r>(rows: &'r [Row], p: &Pred) -> Vec<&'r Row> {
    let _ = (rows, p);
    todo!()
}

/// The WHERE partition identity. Must hold for EVERY p under a
/// correct filter; a NULL-blind filter double-counts UNKNOWN rows.
pub fn tlp_check(rows: &[Row], p: &Pred, filter: impl Fn(&[Row], &Pred) -> usize) -> bool {
    let _ = (rows, p, &filter);
    todo!()
}

// ---------- PROVIDED: the buggy SUT + generators ----------

/// NULL-blind two-valued eval: every UNKNOWN collapses to false at
/// the point it appears — so Not(unknown) = true. This is the bug.
pub fn eval2_nullblind(p: &Pred, row: &Row) -> bool {
    match p {
        Pred::Eq(a, b) => match (eval_expr(a, row), eval_expr(b, row)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
        Pred::Lt(a, b) => match (eval_expr(a, row), eval_expr(b, row)) {
            (Some(x), Some(y)) => x < y,
            _ => false,
        },
        Pred::And(a, b) => eval2_nullblind(a, row) && eval2_nullblind(b, row),
        Pred::Or(a, b) => eval2_nullblind(a, row) || eval2_nullblind(b, row),
        Pred::Not(a) => !eval2_nullblind(a, row),
        Pred::IsNull(a) => eval3_is_unknown(a, row),
    }
}

/// Even the buggy engine implements IS NULL correctly (it must —
/// it's the only way users test for NULL). Uses YOUR eval3.
fn eval3_is_unknown(p: &Pred, row: &Row) -> bool {
    eval3(p, row).is_none()
}

pub fn filter_buggy(rows: &[Row], p: &Pred) -> usize {
    rows.iter().filter(|r| eval2_nullblind(p, r)).count()
}

pub fn gen_rows(n: usize, cols: usize, null_pct: f64, seed: u64) -> Vec<Row> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n)
        .map(|_| {
            (0..cols)
                .map(|_| if rng.gen_bool(null_pct) { None } else { Some(rng.gen_range(-4..4)) })
                .collect()
        })
        .collect()
}

pub fn gen_pred(cols: usize, depth: usize, rng: &mut StdRng) -> Pred {
    let leaf = |rng: &mut StdRng| {
        let e1 = Expr::Col(rng.gen_range(0..cols));
        let e2 = if rng.gen_bool(0.7) {
            Expr::Const(if rng.gen_bool(0.15) { None } else { Some(rng.gen_range(-4..4)) })
        } else {
            Expr::Col(rng.gen_range(0..cols))
        };
        if rng.gen_bool(0.5) {
            Pred::Eq(e1, e2)
        } else {
            Pred::Lt(e1, e2)
        }
    };
    if depth == 0 {
        return leaf(rng);
    }
    match rng.gen_range(0..4) {
        0 => Pred::And(Box::new(gen_pred(cols, depth - 1, rng)), Box::new(gen_pred(cols, depth - 1, rng))),
        1 => Pred::Or(Box::new(gen_pred(cols, depth - 1, rng)), Box::new(gen_pred(cols, depth - 1, rng))),
        2 => Pred::Not(Box::new(gen_pred(cols, depth - 1, rng))),
        _ => leaf(rng),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(vals: &[Option<i64>]) -> Row {
        vals.to_vec()
    }

    #[test]
    fn kleene_truth_table_corners() {
        let null = || Pred::Eq(Expr::Const(None), Expr::Const(Some(0)));
        let f = || Pred::Eq(Expr::Const(Some(0)), Expr::Const(Some(1)));
        let t = || Pred::Eq(Expr::Const(Some(1)), Expr::Const(Some(1)));
        let r = row(&[]);
        assert_eq!(eval3(&null(), &r), None);
        // false dominates AND even against UNKNOWN
        assert_eq!(eval3(&Pred::And(Box::new(null()), Box::new(f())), &r), Some(false));
        // true dominates OR even against UNKNOWN
        assert_eq!(eval3(&Pred::Or(Box::new(null()), Box::new(t())), &r), Some(true));
        assert_eq!(eval3(&Pred::Not(Box::new(null())), &r), None);
        assert_eq!(eval3(&Pred::IsNull(Box::new(null())), &r), Some(true));
        assert_eq!(eval3(&Pred::IsNull(Box::new(t())), &r), Some(false));
    }

    #[test]
    fn tlp_holds_for_correct_filter_all_seeds() {
        let rows = gen_rows(500, 3, 0.25, 1);
        let mut rng = StdRng::seed_from_u64(2);
        for _ in 0..100 {
            let p = gen_pred(3, 3, &mut rng);
            let correct = |rows: &[Row], p: &Pred| filter_correct(rows, p).len();
            assert!(tlp_check(&rows, &p, correct), "TLP violated by the CORRECT engine: {p:?}");
        }
    }

    #[test]
    fn tlp_catches_the_nullblind_engine() {
        let rows = gen_rows(500, 3, 0.25, 1);
        let mut rng = StdRng::seed_from_u64(2);
        let caught = (0..100).any(|_| {
            let p = gen_pred(3, 3, &mut rng);
            !tlp_check(&rows, &p, filter_buggy)
        });
        assert!(caught, "100 predicates and the NULL-blind bug never surfaced?");
    }

    #[test]
    fn null_comparison_is_unknown_not_false() {
        let r = row(&[None, Some(3)]);
        let p = Pred::Eq(Expr::Col(0), Expr::Col(1));
        assert_eq!(eval3(&p, &r), None);
        // and therefore NOT p is also UNKNOWN — the row lands in the
        // IS NULL partition, not the NOT partition
        assert_eq!(eval3(&Pred::Not(Box::new(p)), &r), None);
    }
}
