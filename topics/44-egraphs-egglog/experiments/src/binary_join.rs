//! LANE 3 (exercise) — the binary-join baseline, on a cyclic pattern.
//!
//! Generic join is only interesting if the plan it replaces is worse. The
//! multi-pattern `{ e(x,y), e(y,z), e(z,x) }` compiles to the triangle query
//!
//!     Q(x, y, z) <- R_e(r1, x, y), R_e(r2, y, z), R_e(r3, z, x)
//!
//! which is *cyclic*: no join tree covers it (POPL'22 §2.3). Any binary plan
//! must pick two atoms to join first, and that intermediate is the whole
//! problem — for M edges it is O(M^2) in the worst case even when the answer is
//! only O(M^1.5), the AGM bound of the triangle.
//!
//! Implement [`binary_join`] as a left-deep hash-join plan:
//!
//!   1. order the atoms (any order; a fixed one is fine, but say which);
//!   2. join atom 1 with atom 2 on their shared variables by building a hash
//!      table on the smaller side and probing with the larger — the textbook
//!      build/probe of topic 11;
//!   3. materialise the intermediate, then join it with atom 3, and so on;
//!   4. count every hash-table insert and every probe into `probes`, and record
//!      the largest intermediate you materialised.
//!
//! The numbers to compare in `ematch_bench` lane 3 are `max_intermediate`
//! against the generic-join column's `probes`, as M grows. If the binary plan's
//! intermediate grows quadratically while the output grows as M^1.5, you have
//! reproduced the reason worst-case optimal joins exist — the same reason
//! topic 13's two-hop query is a join-order problem.

use crate::egraph::Id;
use crate::pattern::Query;
use crate::relational::Database;
use std::cell::Cell;

#[derive(Debug, Default)]
pub struct BinaryJoinReport {
    pub matches: Vec<Vec<Id>>,
    /// Tuples in the largest intermediate relation the plan materialised.
    pub max_intermediate: usize,
}

pub fn binary_join(q: &Query, db: &Database, probes: &Cell<u64>) -> BinaryJoinReport {
    let _ = (q, db, probes);
    todo!("left-deep hash-join plan (see module docs)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::edge_graph;
    use crate::pattern::{compile, papp, pvar};
    use crate::relational::{matches, to_database};
    use std::collections::HashSet;

    fn triangle_query(e: crate::egraph::Sym) -> Query {
        compile(&[
            papp(e, vec![pvar("x"), pvar("y")]),
            papp(e, vec![pvar("y"), pvar("z")]),
            papp(e, vec![pvar("z"), pvar("x")]),
        ])
    }

    #[test]
    fn binary_join_agrees_with_generic_join() {
        let (g, e) = edge_graph(120, 900, 7);
        let db = to_database(&g);
        let q = triangle_query(e);
        let p = Cell::new(0);
        let want: HashSet<Vec<Id>> = matches(&q, &db, &p).into_iter().collect();
        assert!(!want.is_empty(), "the generator should produce triangles");
        let got: HashSet<Vec<Id>> = binary_join(&q, &db, &p).matches.into_iter().collect();
        assert_eq!(got, want);
    }

    #[test]
    fn the_intermediate_is_the_problem() {
        let (g, e) = edge_graph(400, 4000, 11);
        let db = to_database(&g);
        let q = triangle_query(e);
        let p = Cell::new(0);
        let r = binary_join(&q, &db, &p);
        assert!(
            r.max_intermediate > 4 * r.matches.len(),
            "intermediate {} vs output {} — a binary plan on a cyclic query is \
             supposed to materialise far more than it returns",
            r.max_intermediate,
            r.matches.len()
        );
    }
}
