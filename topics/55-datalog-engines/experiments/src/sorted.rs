//! Lane 2 (exercise): semi-naive evaluation over sorted relations, with a
//! join-order choice.
//!
//! Lane 1 kept every relation as a hash set plus hash-indexed columns.
//! Soufflé keeps them as B-trees and Cozo as `BTreeMap`s: ordered, so a join
//! is a range seek on a prefix and the delta can be merged in by a linear
//! pass. Here the relations are sorted vectors, which is the same idea with
//! the tree removed:
//!
//! * `arc` as two CSR adjacencies (out and in), sorted rows, probed by index;
//! * the closure as a sorted run of `(x, y)` pairs; each round's delta is
//!   sorted and deduplicated, anti-joined against the closure by a merge, and
//!   merged in (no hash table anywhere);
//! * a **join-order choice**: the same closure can be written two ways,
//!   `Left`:  `tc(x,y) :- tc(x,z), arc(z,y)`   (delta drives, probe `out[z]`)
//!   `Right`: `tc(x,y) :- arc(x,z), tc(z,y)`   (delta drives, probe `in[z]`)
//!   Both compute the same set. They derive different numbers of tuples:
//!   `Left` fires once per out-edge of each closure tuple's *middle* vertex,
//!   `Right` once per in-edge. A static choice needs the closure's shape
//!   (degree statistics give the same total for both). `Adaptive` instead
//!   chooses **per round**, from the delta in hand: it sums `out(z)` over the
//!   delta's tuples `(x, z)` and `in(x)` over the same tuples — an
//!   O(|delta|) pass — and extends on whichever side is cheaper. Mixing the
//!   two forms across rounds is sound because round r's delta is exactly the
//!   pairs at shortest-path distance r, and a pair at distance r + 1 is one
//!   arc beyond a distance-r pair on *either* end.
//!
//! The tests are the specification. `expected_derivations` is the exact
//! count each form must report, derived from per-source BFS levels.

use crate::gen::Graph;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Left,
    Right,
    /// Per round, whichever of Left/Right is cheaper for that round's delta.
    Adaptive,
}

#[derive(Clone, Debug, Default)]
pub struct SortedStats {
    /// Rounds, including the final round that derives nothing new.
    pub iterations: u32,
    /// Successful rule-body matches, including the base rule's `|arc|`.
    pub derivations: u64,
    pub new_tuples: u64,
    /// Index seeks (row lookups and merge-cursor probes) performed.
    pub probes: u64,
    pub ms: f64,
}

#[derive(Clone, Debug)]
pub struct SortedRun {
    pub stats: SortedStats,
    /// The closure, sorted, no duplicates.
    pub pairs: Vec<(u32, u32)>,
}

/// Semi-naive transitive closure over sorted relations in the given form.
pub fn tc_sorted(_g: &Graph, _form: Form) -> SortedRun {
    todo!("lane 2: sorted delta, merge anti-join against the closure, merge-in; no hash sets")
}

/// Exact derivations `form` must report: `|arc|` for the base rule, plus one
/// per (closure tuple, matching arc) pair — each closure tuple is in exactly
/// one delta. For `Adaptive` the sum is taken per round and each round pays
/// the cheaper side: round r's delta is the set of pairs at shortest-path
/// distance r, so both forms' per-round costs are fixed by the graph.
/// Computed from per-source BFS levels, so it is the oracle, not an estimate.
pub fn expected_derivations(g: &Graph, form: Form) -> u64 {
    let out = g.out_csr();
    let inn = g.in_csr();
    let mut left: Vec<u64> = Vec::new();
    let mut right: Vec<u64> = Vec::new();
    let mut stamp = vec![u32::MAX; g.n];
    for s in 0..g.n {
        let mut level: Vec<u32> = out.row(s).to_vec();
        for &v in &level {
            stamp[v as usize] = s as u32;
        }
        let mut r = 0;
        while !level.is_empty() {
            if left.len() <= r {
                left.push(0);
                right.push(0);
            }
            let mut next = Vec::new();
            for &z in &level {
                // closure tuple (s, z), at shortest-path distance r + 1
                left[r] += out.deg(z as usize) as u64; // tc(s,z) ⋈ arc(z,·)
                right[r] += inn.deg(s) as u64; // arc(·,s) ⋈ tc(s,z)
                for &y in out.row(z as usize) {
                    if stamp[y as usize] != s as u32 {
                        stamp[y as usize] = s as u32;
                        next.push(y);
                    }
                }
            }
            level = next;
            r += 1;
        }
    }
    g.edges.len() as u64
        + match form {
            Form::Left => left.iter().sum::<u64>(),
            Form::Right => right.iter().sum::<u64>(),
            Form::Adaptive => left.iter().zip(&right).map(|(l, r)| *l.min(r)).sum::<u64>(),
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::tc_pairs;
    use crate::gen::{chain, grid, power_law};

    const FORMS: [Form; 3] = [Form::Left, Form::Right, Form::Adaptive];

    #[test]
    fn every_form_computes_the_closure() {
        for g in [power_law(100, 300, 3), grid(5, 5), chain(25)] {
            let want = tc_pairs(&g);
            for form in FORMS {
                let r = tc_sorted(&g, form);
                assert_eq!(r.pairs, want, "{form:?}");
                assert!(r.pairs.windows(2).all(|w| w[0] < w[1]), "sorted, unique");
                assert_eq!(r.stats.new_tuples, want.len() as u64);
            }
        }
    }

    #[test]
    fn derivation_counts_are_exact() {
        for g in [power_law(100, 300, 3), grid(5, 5), chain(25)] {
            for form in FORMS {
                let r = tc_sorted(&g, form);
                assert_eq!(r.stats.derivations, expected_derivations(&g, form), "{form:?}");
            }
        }
    }

    #[test]
    fn forms_differ_on_a_skewed_graph() {
        // The two static forms must not be interchangeable on the lane's
        // skewed graph, or the join-order choice would be vacuous.
        let g = power_law(300, 900, 55);
        assert_ne!(
            expected_derivations(&g, Form::Left),
            expected_derivations(&g, Form::Right)
        );
    }

    #[test]
    fn adaptive_never_derives_more_than_either_static_form() {
        for seed in [55u64, 7, 8, 9] {
            let g = power_law(300, 900, seed);
            let l = expected_derivations(&g, Form::Left);
            let r = expected_derivations(&g, Form::Right);
            let a = expected_derivations(&g, Form::Adaptive);
            assert!(a <= l.min(r), "oracle seed {seed}: left {l}, right {r}, adaptive {a}");
            assert_eq!(tc_sorted(&g, Form::Adaptive).stats.derivations, a);
        }
    }
}
