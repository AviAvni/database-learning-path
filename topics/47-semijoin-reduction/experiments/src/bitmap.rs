//! Lane 3 — bitmap filters pushed through hash joins (EXERCISE).
//!
//! No reduction phase. Each hash join in a left-deep plan builds a *bitmap*
//! (here: an exact set — SQL Server's "exact bit-vector", zero false
//! positives) over its build side's join-key values and pushes it into its
//! probe subtree, where a scan drops rows whose value of that variable is
//! not in it. The question is *which* scans a bitmap may reach.
//!
//! Execution is pull-based, as in SQL Server's Figure 3: the top join opens
//! its build side first, so builds happen top-down — join n−1's build, then
//! join n−2's, …, join 1's — and the probe relation `order[0]` is scanned
//! last. A bitmap created at join j can therefore filter any relation that is
//! scanned after it: the builds of the joins below j, and the probe.
//!
//! * [`Pushdown::IntoLowerBuilds`] — a bitmap from join j filters every
//!   relation in `order[0..j]` that binds the bitmap's variable. This is the
//!   cascade the CIDR'26 paper proves instance-optimal (its Algorithm 1).
//! * [`Pushdown::ProbeSpineOnly`] — a bitmap may only filter `order[0]`, the
//!   relation at the bottom of the probe spine. This models DuckDB's
//!   join-filter pushdown at the pinned commit, which follows only a join's
//!   probe child (`join_filter_pushdown_optimizer.cpp:153-161`) and bails out
//!   at a scan that does not bind the filtered column (`:216-219`). It is a
//!   model of that code path, not a measurement of DuckDB.
//!
//! Filters do not chain within one relation's scan in a surprising way: a
//! build relation is filtered by every bitmap that reached it *before* its own
//! bitmap is built, so a bitmap built from a filtered build side carries the
//! filtering forward. That is the whole mechanism.

use crate::rel::Relation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushdown {
    ProbeSpineOnly,
    IntoLowerBuilds,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CascadeReport {
    /// Rows of `order[i]` that survived the bitmaps applied to its scan,
    /// indexed by plan position i (0 = the probe relation).
    pub scanned: Vec<usize>,
    /// Tuples emitted by join 1 … n−1, as in `join::PlanRun`.
    pub join_sizes: Vec<usize>,
    pub out: usize,
    pub checksum: u64,
    /// Rows inserted into the join hash tables (sum of `scanned[1..]`).
    pub build_rows: usize,
    /// Bitmap lookups performed by scans.
    pub bitmap_probes: usize,
}

impl CascadeReport {
    pub fn intermediates(&self) -> usize {
        self.join_sizes.iter().sum()
    }
}

/// Run the left-deep plan `order` with bitmap pushdown in mode `mode`.
///
/// Contract (see the tests):
/// 1. same `out` and `checksum` as `join::left_deep` on the same order;
/// 2. each join j (1-based, build side `order[j]`) builds one bitmap on the
///    variable it joins on — the first variable of `order[j]` already bound
///    by `order[0..j]`;
/// 3. builds are filtered top-down (j = n−1 down to 1), the probe last;
/// 4. after filtering, run the ordinary left-deep join on the filtered
///    relations (`join::left_deep`) to get `join_sizes`, `out`, `checksum`.
pub fn bitmap_cascade(rels: &[Relation], nvars: usize, order: &[usize], mode: Pushdown) -> CascadeReport {
    let _ = (rels, nvars, order, mode);
    todo!("lane 3: bitmap filters pushed from build sides into the probe subtree")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{social_graph, GraphParams, BAD, GOOD};
    use crate::join::left_deep;

    #[test]
    fn same_answer_in_every_mode_and_order() {
        let q = social_graph(GraphParams::SMALL);
        for order in [GOOD, BAD] {
            let plain = left_deep(&q.rels, q.nvars(), &order);
            for mode in [Pushdown::ProbeSpineOnly, Pushdown::IntoLowerBuilds] {
                let c = bitmap_cascade(&q.rels, q.nvars(), &order, mode);
                assert_eq!((c.out, c.checksum), (plain.out, plain.checksum), "{order:?} {mode:?}");
                assert_eq!(c.build_rows, c.scanned[1..].iter().sum::<usize>());
            }
        }
    }

    #[test]
    fn cascade_into_lower_builds_makes_the_bad_order_instance_optimal() {
        // The paper's §3.3 argument: once every build side has been filtered by
        // the bitmaps from above, every probe hits, so no join emits more than OUT.
        let q = social_graph(GraphParams::SMALL);
        let c = bitmap_cascade(&q.rels, q.nvars(), &BAD, Pushdown::IntoLowerBuilds);
        assert!(c.join_sizes.iter().all(|&s| s <= c.out), "{:?} out {}", c.join_sizes, c.out);
        // and the probe relation was cut to the rows that participate
        assert!(c.scanned[0] < q.rels[0].len() / 5);
    }

    #[test]
    fn probe_spine_only_barely_helps_the_bad_order() {
        // rare_tag's bitmap is on t, which only has_tag binds — a build side.
        let q = social_graph(GraphParams::SMALL);
        let plain = left_deep(&q.rels, q.nvars(), &BAD);
        let c = bitmap_cascade(&q.rels, q.nvars(), &BAD, Pushdown::ProbeSpineOnly);
        assert_eq!(c.scanned[1..], [q.rels[1].len(), q.rels[2].len(), q.rels[3].len()]);
        assert!(c.intermediates() * 10 >= plain.intermediates() * 9);
    }

    #[test]
    fn the_good_order_builds_the_biggest_relation_unfiltered() {
        // Bitmaps only flow from build to probe. In GOOD the top build is knows,
        // built first, so nothing has filtered it yet.
        let q = social_graph(GraphParams::SMALL);
        let c = bitmap_cascade(&q.rels, q.nvars(), &GOOD, Pushdown::IntoLowerBuilds);
        assert_eq!(c.scanned[3], q.rels[0].len());
    }
}
