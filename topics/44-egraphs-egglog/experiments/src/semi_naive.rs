//! LANE 2 (exercise) — semi-naive evaluation.
//!
//! An equality-saturation loop runs the same queries against an e-graph that
//! only ever grows. Naive evaluation re-derives, on iteration i+1, every match
//! it already derived on iteration i. Semi-naive evaluation derives only the
//! matches that *use at least one new tuple*, which is the difference between
//! egglog and egglogNI in the PLDI'23 paper — the two curves of Figure 7, and
//! the 9.27x vs 3.34x speedups in §5.3.
//!
//! The rule (PLDI'23 §4.3): a rule with m body atoms
//!
//!     A :- A_1, ..., A_m
//!
//! expands into m *delta rules*, the j-th of which ranges atom j over the new
//! tuples only and every other atom over the whole database:
//!
//!     A :- A_1, ..., A_{j-1}, dA_j, A_{j+1}, ..., A_m
//!
//! Their union is exactly the set of derivations that touch something new.
//! Note the word union: a substitution using two new tuples is produced twice,
//! by two different delta rules, so the results must be deduplicated. That
//! duplication is the price of the incrementalisation and it is worth
//! measuring, not just avoiding.
//!
//! Implement [`delta_matches`]:
//!
//!   1. for each atom index j, build a database that is `db` everywhere except
//!      relation `q.atoms[j].rel`, which is `delta`'s tuples for that relation
//!      (careful: two atoms may name the same relation, as the triangle query
//!      does — the substitution must be for atom j, not for every atom over
//!      that relation);
//!   2. run [`crate::relational::matches`] on each, sharing `probes`;
//!   3. concatenate, then dedup.
//!
//! The test below is the specification: the result must equal
//! `matches(after) - matches(before)`, as sets.

use crate::egraph::Id;
use crate::pattern::Query;
use crate::relational::Database;
use std::cell::Cell;

pub fn delta_matches(
    q: &Query,
    db: &Database,
    delta: &Database,
    probes: &Cell<u64>,
) -> Vec<Vec<Id>> {
    let _ = (q, db, delta, probes);
    todo!("semi-naive evaluation (see module docs)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{db_delta, Fig2};
    use crate::pattern::{compile, papp, pvar};
    use crate::relational::{matches, to_database};
    use std::collections::HashSet;

    fn setup(n: usize, k: usize) -> (Query, Database, Database, Database) {
        let mut fig = Fig2::new(n);
        let before = to_database(&fig.g);
        let (f, gs) = (fig.f, fig.gs);
        fig.grow(k);
        let after = to_database(&fig.g);
        let delta = db_delta(&after, &before);
        let q = compile(&[papp(f, vec![pvar("a"), papp(gs, vec![pvar("a")])])]);
        (q, before, after, delta)
    }

    #[test]
    fn semi_naive_derives_exactly_the_new_matches() {
        let (q, before, after, delta) = setup(200, 8);
        let p = Cell::new(0);
        let old: HashSet<Vec<Id>> = matches(&q, &before, &p).into_iter().collect();
        let new: HashSet<Vec<Id>> = matches(&q, &after, &p).into_iter().collect();
        let expected: HashSet<Vec<Id>> = new.difference(&old).cloned().collect();
        assert_eq!(expected.len(), 8, "the delta should be exactly the 8 new f-terms");

        let got: HashSet<Vec<Id>> = delta_matches(&q, &after, &delta, &p).into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn semi_naive_is_cheaper_than_re_deriving_everything() {
        let (q, _before, after, delta) = setup(2000, 8);
        let full = Cell::new(0);
        let n_full = matches(&q, &after, &full).len();
        let inc = Cell::new(0);
        let n_inc = delta_matches(&q, &after, &delta, &inc).len();
        assert_eq!(n_full, 2008);
        assert_eq!(n_inc, 8);
        assert!(
            inc.get() * 10 < full.get(),
            "semi-naive did {} probes against naive's {} — that is not incremental",
            inc.get(),
            full.get()
        );
    }
}
