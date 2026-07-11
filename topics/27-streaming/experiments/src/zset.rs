//! Z-sets — the algebra under all incremental view maintenance.
//! A Z-set is a collection where every element carries an i64 weight;
//! +1 = insert, -1 = delete, and CHANGES to a collection are themselves
//! Z-sets (possibly with negative weights). This is DBSP's Z-set
//! (feldera crates/dbsp/src/algebra/zset/) and differential-dataflow's
//! Collection with isize diffs — DD keeps (data, time, diff) triples and
//! consolidates them exactly like `from_updates` below
//! (differential-dataflow consolidation.rs:24 `consolidate`).
//!
//! The load-bearing fact: LINEAR operators (filter, map, keyed join per
//! the bilinearity trick) commute with taking deltas — op(ΔA) = Δop(A) —
//! so they need NO state to incrementalize. Nonlinear ones (distinct,
//! aggregates) don't, and that's where arrangements/state come in.

/// Invariant: `entries` sorted by T, no zero weights, no duplicate T.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ZSet<T: Ord + Clone> {
    pub entries: Vec<(T, i64)>,
}

fn consolidate<T: Ord>(vec: &mut Vec<(T, i64)>) {
    vec.sort_by(|a, b| a.0.cmp(&b.0));
    let mut w = 0;
    for i in 0..vec.len() {
        if i + 1 < vec.len() && vec[i].0 == vec[i + 1].0 {
            let (lo, hi) = vec.split_at_mut(i + 1);
            hi[0].1 += lo[i].1;
            lo[i].1 = 0;
        } else if vec[i].1 != 0 {
            vec.swap(w, i);
            w += 1;
        }
    }
    vec.truncate(w);
}

impl<T: Ord + Clone> ZSet<T> {
    pub fn new() -> Self {
        ZSet { entries: Vec::new() }
    }

    pub fn from_updates(mut updates: Vec<(T, i64)>) -> Self {
        consolidate(&mut updates);
        ZSet { entries: updates }
    }

    pub fn singleton(t: T, w: i64) -> Self {
        Self::from_updates(vec![(t, w)])
    }

    /// Union with weight addition — the group operation. Deltas compose
    /// by merge; delete-then-reinsert cancels to nothing.
    pub fn merge(&self, other: &Self) -> Self {
        let mut v = self.entries.clone();
        v.extend_from_slice(&other.entries);
        Self::from_updates(v)
    }

    pub fn negate(&self) -> Self {
        ZSet { entries: self.entries.iter().map(|(t, w)| (t.clone(), -w)).collect() }
    }

    pub fn weight(&self, t: &T) -> i64 {
        match self.entries.binary_search_by(|(e, _)| e.cmp(t)) {
            Ok(i) => self.entries[i].1,
            Err(_) => 0,
        }
    }

    /// Number of elements with nonzero weight.
    pub fn support(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_weight(&self) -> i64 {
        self.entries.iter().map(|(_, w)| w).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = &(T, i64)> {
        self.entries.iter()
    }

    /// The canonical NONLINEAR operator: weight > 0 becomes exactly 1.
    /// distinct(a).merge(distinct(b)) ≠ distinct(a.merge(b)) in general —
    /// which is precisely why DBSP/DD must keep integrated state (an
    /// arrangement) behind every distinct/reduce, while map/filter/join
    /// deltas stream through statelessly.
    pub fn distinct(&self) -> Self {
        ZSet {
            entries: self
                .entries
                .iter()
                .filter(|(_, w)| *w > 0)
                .map(|(t, _)| (t.clone(), 1))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_cancels_insert_delete() {
        let a = ZSet::from_updates(vec![("x", 1), ("y", 1)]);
        let d = ZSet::from_updates(vec![("x", -1), ("z", 1)]);
        let out = a.merge(&d);
        assert_eq!(out.weight(&"x"), 0);
        assert_eq!(out.support(), 2);
    }

    #[test]
    fn from_updates_consolidates() {
        let z = ZSet::from_updates(vec![(3, 1), (1, 2), (3, -1), (2, 0)]);
        assert_eq!(z.entries, vec![(1, 2)]);
    }

    #[test]
    fn distinct_is_not_linear() {
        // a has x twice; b deletes one copy. Linearity would demand
        // distinct(a+b) == distinct(a) + distinct(b). It doesn't hold.
        let a = ZSet::from_updates(vec![("x", 2)]);
        let b = ZSet::from_updates(vec![("x", -1)]);
        let lhs = a.merge(&b).distinct();
        let rhs = a.distinct().merge(&b.distinct());
        assert_eq!(lhs.weight(&"x"), 1);
        assert_eq!(rhs.weight(&"x"), 1); // happens to match in weight...
        // ...but feed the delta through distinct directly and the output
        // delta is WRONG: distinct sees ("x",-1) -> emits nothing, yet the
        // true change to distinct(a) is zero as well here; the real
        // counterexample is deleting the LAST copy:
        let a = ZSet::from_updates(vec![("x", 1)]);
        let b = ZSet::from_updates(vec![("x", -1)]);
        let true_delta = a.merge(&b).distinct().merge(&a.distinct().negate());
        assert_eq!(true_delta.weight(&"x"), -1); // must retract
        assert_eq!(b.distinct().weight(&"x"), 0); // stateless op says "no change" — wrong
    }
}
