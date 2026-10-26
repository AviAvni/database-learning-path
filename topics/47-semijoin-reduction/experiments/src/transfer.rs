//! Lane 2 — predicate transfer with Bloom filters (EXERCISE).
//!
//! Run Yannakakis' two passes over the same join tree, but replace each exact
//! semi-join `r ⋉ s` with "build a Bloom filter over s's join-key values,
//! drop the rows of r whose key it rejects". That is predicate transfer
//! (Yang et al., CIDR'24) on a tree — RPT's schedule (Zhao et al., SIGMOD'25).
//!
//! A Bloom filter has no false negatives, so every row the exact reduction
//! keeps is still kept. What it adds are rows that should have gone, and this
//! lane makes you count them in two kinds:
//!
//! * **filter false positives** — a row whose key was *not* among the keys
//!   inserted into some filter applied to it, and which that filter passed
//!   anyway;
//! * **inherited** — a row that passed every filter honestly (its key really
//!   was inserted), but was inserted only because an *upstream* filter let a
//!   false positive through. This is the multi-hop accumulation RPT+ §4.2
//!   warns about: errors made at hop 1 widen the filter built at hop 2.
//!
//! For every relation: `survived − exact = filter_fp + inherited`.

use crate::bloom::Bloom;
use crate::rel::Relation;
use crate::yannakakis::JoinTree;

/// What one relation looks like after the transfer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelTransfer {
    pub name: &'static str,
    pub rows_before: usize,
    /// Rows the exact (Yannakakis) reduction keeps.
    pub exact: usize,
    /// Rows the Bloom transfer keeps.
    pub survived: usize,
    /// Survivors admitted as a false positive by at least one filter.
    pub filter_fp: usize,
    /// Survivors that are not exact and were never a filter false positive.
    pub inherited: usize,
}

/// One Bloom filter built and applied during the transfer.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterStat {
    /// Relation the filter was built from (`s` in `r ⋉ s`).
    pub from: &'static str,
    /// Relation it was applied to (`r`).
    pub to: &'static str,
    /// 'F' for the forward pass, 'B' for the backward pass.
    pub pass: char,
    /// Distinct keys inserted.
    pub keys: usize,
    pub bits: u64,
    pub k: u32,
    /// Rows of `r` probed whose key was not inserted.
    pub negatives: usize,
    /// …of which the filter passed.
    pub false_positives: usize,
    /// Distinct key values among those negative rows.
    pub negative_keys: usize,
    /// …of which the filter passed. A Bloom filter's false-positive rate is
    /// a property of *keys*: a row inherits its key's verdict, so a key that
    /// appears 100 times turns one false positive into 100 rows.
    pub fp_keys: usize,
    /// The textbook rate for this filter's (k, n, m).
    pub theory_fpr: f64,
}

impl FilterStat {
    /// False positives per distinct negative key — the quantity the
    /// textbook formula predicts.
    pub fn measured_fpr(&self) -> f64 {
        if self.negative_keys == 0 {
            0.0
        } else {
            self.fp_keys as f64 / self.negative_keys as f64
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransferReport {
    /// The reduced relations, same positions as the input.
    pub rels: Vec<Relation>,
    pub per_rel: Vec<RelTransfer>,
    /// In the order they were built: forward pass, then backward.
    pub filters: Vec<FilterStat>,
}

/// Bloom-filter predicate transfer over `tree`, `bits_per_key` bits per
/// distinct inserted key.
///
/// `exact` is the fully reduced relation set from `yannakakis::full_reduce`
/// on the same input, which the accounting compares against (use the row
/// `ids` — a base row is exact iff its id survives there).
///
/// Contract (see the tests):
/// 1. no false negatives — every exact row survives, and the reduced
///    relations still produce the same OUT and checksum under any plan;
/// 2. `survived − exact == filter_fp + inherited` for every relation;
/// 3. filters are applied in Yannakakis' order: for each u in post-order, one
///    filter per child c (built from c's *current* rows, applied to u); then
///    for each u in reverse post-order with a parent p, one filter built from
///    p's current rows applied to u. Size each filter for the number of
///    *distinct* keys it receives.
pub fn bloom_transfer(
    rels: &[Relation],
    tree: &JoinTree,
    bits_per_key: f64,
    exact: &[Relation],
) -> TransferReport {
    let _ = (rels, tree, bits_per_key, exact, Bloom::with_bits_per_key);
    todo!("lane 2: Bloom-filter predicate transfer with false-positive accounting")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{social_graph, GraphParams, BAD, GOOD};
    use crate::join::left_deep;
    use crate::yannakakis::{full_reduce, JoinTree};

    fn setup() -> (crate::rel::Query, JoinTree, Vec<Relation>) {
        let q = social_graph(GraphParams::SMALL);
        let tree = JoinTree::largest_root(&q.rels);
        let exact = full_reduce(&q.rels, &tree).rels;
        (q, tree, exact)
    }

    #[test]
    fn no_false_negatives_same_answer() {
        let (q, tree, exact) = setup();
        let rep = bloom_transfer(&q.rels, &tree, 4.0, &exact);
        for (i, r) in rep.per_rel.iter().enumerate() {
            assert!(r.survived >= r.exact, "{}: {} < {}", r.name, r.survived, r.exact);
            assert_eq!(r.exact, exact[i].len());
            assert_eq!(r.survived, rep.rels[i].len());
        }
        for order in [GOOD, BAD] {
            let a = left_deep(&q.rels, q.nvars(), &order);
            let b = left_deep(&rep.rels, q.nvars(), &order);
            assert_eq!((a.out, a.checksum), (b.out, b.checksum));
        }
    }

    #[test]
    fn accounting_identity_holds() {
        let (q, tree, exact) = setup();
        for bpk in [2.0, 4.0, 8.0] {
            let rep = bloom_transfer(&q.rels, &tree, bpk, &exact);
            for r in &rep.per_rel {
                assert_eq!(r.survived - r.exact, r.filter_fp + r.inherited, "{} at {bpk}", r.name);
            }
        }
    }

    #[test]
    fn six_filters_in_yannakakis_order() {
        let (q, tree, exact) = setup();
        let rep = bloom_transfer(&q.rels, &tree, 8.0, &exact);
        let route: Vec<(&str, &str, char)> = rep.filters.iter().map(|f| (f.from, f.to, f.pass)).collect();
        assert_eq!(
            route,
            vec![
                ("rare_tag", "has_tag", 'F'),
                ("has_tag", "likes", 'F'),
                ("likes", "knows", 'F'),
                ("knows", "likes", 'B'),
                ("likes", "has_tag", 'B'),
                ("has_tag", "rare_tag", 'B'),
            ]
        );
    }

    #[test]
    fn measured_rate_tracks_the_formula() {
        let (q, tree, exact) = setup();
        let rep = bloom_transfer(&q.rels, &tree, 8.0, &exact);
        assert!(rep.filters.iter().any(|f| f.negative_keys >= 1_000), "no filter saw enough negatives to test");
        for f in rep.filters.iter().filter(|f| f.negative_keys >= 1_000) {
            let (m, t) = (f.measured_fpr(), f.theory_fpr);
            assert!(m <= 2.0 * t + 0.002, "{}→{}: measured {m} vs theory {t}", f.from, f.to);
        }
    }

    #[test]
    fn more_bits_never_more_survivors_and_plenty_is_exact() {
        let (q, tree, exact) = setup();
        let total = |bpk| -> usize { bloom_transfer(&q.rels, &tree, bpk, &exact).per_rel.iter().map(|r| r.survived).sum() };
        let (t2, t8, t32) = (total(2.0), total(8.0), total(32.0));
        assert!(t2 >= t8 && t8 >= t32, "{t2} {t8} {t32}");
        let exact_total: usize = exact.iter().map(|r| r.len()).sum();
        assert!(t32 - exact_total <= exact_total / 1000, "{t32} vs {exact_total}");
    }
}
