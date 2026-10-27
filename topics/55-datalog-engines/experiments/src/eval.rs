//! Lane 1 (provided): naive vs semi-naive evaluation over hash-indexed
//! relations.
//!
//! Programs (relation `arc` is the input graph):
//!
//! ```text
//! TC-linear     tc(x,y) :- arc(x,y).      tc(x,y) :- tc(x,z), arc(z,y).
//! TC-nonlinear  tc(x,y) :- arc(x,y).      tc(x,y) :- tc(x,z), tc(z,y).
//! SG            sg(x,y) :- arc(p,x), arc(p,y), x != y.
//!               sg(x,y) :- arc(a,x), sg(a,b), arc(b,y).
//! ```
//!
//! Every evaluator works in rounds with snapshot semantics: a round derives
//! against the relations as they stood when the round began, then merges. A
//! **derivation** is one successful match of a rule body, whether or not its
//! head tuple is already known; **new** counts the distinct head tuples that
//! were not yet in the relation; **duplicates** = derivations - new.

use crate::gen::{Csr, Graph};
use crate::hash::{mix, pack, FxMap, FxSet};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Re-evaluate every rule on the full relations each round.
    Naive,
    /// Evaluate each rule once per recursive atom with that atom restricted
    /// to the previous round's new tuples (the delta).
    SemiNaive,
}

/// How the nonlinear rule `tc(x,z), tc(z,y)` is split into delta versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nonlinear {
    Naive,
    /// `delta(x,z) join full(z,y)` union `full(x,z) join delta(z,y)`: a tuple
    /// pair with both halves in the delta is derived twice.
    TextbookDelta,
    /// A disjoint version split: `delta(x,z) join full(z,y)` union
    /// `old(x,z) join delta(z,y)`, the second version filtering out `(x,z)`
    /// that are in the delta, so every pair is derived once. Soufflé's split
    /// is the mirror image (atoms left of the delta atom read the full
    /// relation, atoms right of it read the full relation minus the delta;
    /// `ClauseTranslator.cpp:529-531`); the two partition the same pairs, so
    /// the derivation counts are identical.
    OldDelta,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Rounds executed, including the final round that derives nothing new.
    pub iterations: u32,
    pub derivations: u64,
    pub new_tuples: u64,
    pub ms: f64,
}

impl Stats {
    pub fn duplicates(&self) -> u64 {
        self.derivations - self.new_tuples
    }
}

#[derive(Clone, Debug)]
pub struct Run {
    pub stats: Stats,
    /// Final relation size.
    pub size: u64,
    /// Order-independent checksum of the final relation.
    pub checksum: u64,
}

/// A binary relation with hash-set membership and hash-indexed columns.
#[derive(Default)]
struct HashRel {
    set: FxSet<u64>,
    by0: FxMap<u32, Vec<u32>>,
    by1: FxMap<u32, Vec<u32>>,
    all: Vec<(u32, u32)>,
}

impl HashRel {
    fn contains(&self, x: u32, y: u32) -> bool {
        self.set.contains(&pack(x, y))
    }
    fn insert(&mut self, x: u32, y: u32, index_cols: bool) -> bool {
        if !self.set.insert(pack(x, y)) {
            return false;
        }
        self.all.push((x, y));
        if index_cols {
            self.by0.entry(x).or_default().push(y);
            self.by1.entry(y).or_default().push(x);
        }
        true
    }
    fn checksum(&self) -> u64 {
        self.all.iter().fold(0u64, |a, &(x, y)| a.wrapping_add(mix(pack(x, y))))
    }
}

fn finish(rel: &HashRel, mut stats: Stats, t: Instant) -> Run {
    stats.ms = t.elapsed().as_secs_f64() * 1e3;
    Run { size: rel.all.len() as u64, checksum: rel.checksum(), stats }
}

/// Merge a round's derived tuples into `rel`; returns the delta.
fn merge(rel: &mut HashRel, fresh: Vec<(u32, u32)>, index_cols: bool) -> Vec<(u32, u32)> {
    for &(x, y) in &fresh {
        rel.insert(x, y, index_cols);
    }
    fresh
}

/// tc(x,y) :- arc(x,y).  tc(x,y) :- tc(x,z), arc(z,y).
pub fn tc_linear(g: &Graph, mode: Mode) -> Run {
    let t = Instant::now();
    let out = g.out_csr();
    let mut st = Stats::default();
    let mut tc = HashRel::default();
    let mut delta: Vec<(u32, u32)> = Vec::new();
    for &(x, y) in &g.edges {
        st.derivations += 1;
        if tc.insert(x, y, false) {
            st.new_tuples += 1;
            delta.push((x, y));
        }
    }
    loop {
        st.iterations += 1;
        let driver: &[(u32, u32)] = match mode {
            Mode::Naive => &tc.all,
            Mode::SemiNaive => &delta,
        };
        let mut seen: FxSet<u64> = FxSet::default();
        let mut fresh = Vec::new();
        for &(x, z) in driver {
            for &y in out.row(z as usize) {
                st.derivations += 1;
                if !tc.contains(x, y) && seen.insert(pack(x, y)) {
                    fresh.push((x, y));
                }
            }
        }
        st.new_tuples += fresh.len() as u64;
        if fresh.is_empty() {
            break;
        }
        delta = merge(&mut tc, fresh, false);
    }
    finish(&tc, st, t)
}

/// The goal-directed version of linear TC, for the query `?- tc(src, y)`.
/// A magic-set rewrite specializes the recursion to the query's bound
/// argument; for linear TC that leaves a unary relation seeded by `src`:
///
/// ```text
/// reach(y) :- arc(src,y).      reach(y) :- reach(z), arc(z,y).
/// ```
///
/// Semi-naive, same accounting as `tc_linear` (derivations, new, rounds).
pub fn reach_from(g: &Graph, src: u32) -> Run {
    let t = Instant::now();
    let out = g.out_csr();
    let mut st = Stats::default();
    let mut known: FxSet<u32> = FxSet::default();
    let mut delta: Vec<u32> = Vec::new();
    for &y in out.row(src as usize) {
        st.derivations += 1;
        if known.insert(y) {
            st.new_tuples += 1;
            delta.push(y);
        }
    }
    loop {
        st.iterations += 1;
        let mut fresh: Vec<u32> = Vec::new();
        let mut seen: FxSet<u32> = FxSet::default();
        for &z in &delta {
            for &y in out.row(z as usize) {
                st.derivations += 1;
                if !known.contains(&y) && seen.insert(y) {
                    fresh.push(y);
                }
            }
        }
        st.new_tuples += fresh.len() as u64;
        if fresh.is_empty() {
            break;
        }
        for &y in &fresh {
            known.insert(y);
        }
        delta = fresh;
    }
    st.ms = t.elapsed().as_secs_f64() * 1e3;
    let checksum = known.iter().fold(0u64, |a, &y| a.wrapping_add(mix(pack(src, y))));
    Run { size: known.len() as u64, checksum, stats: st }
}

/// tc(x,y) :- arc(x,y).  tc(x,y) :- tc(x,z), tc(z,y).
pub fn tc_nonlinear(g: &Graph, how: Nonlinear) -> Run {
    let t = Instant::now();
    let mut st = Stats::default();
    let mut tc = HashRel::default();
    let mut delta: Vec<(u32, u32)> = Vec::new();
    for &(x, y) in &g.edges {
        st.derivations += 1;
        if tc.insert(x, y, true) {
            st.new_tuples += 1;
            delta.push((x, y));
        }
    }
    loop {
        st.iterations += 1;
        let mut seen: FxSet<u64> = FxSet::default();
        let mut fresh = Vec::new();
        {
            let mut emit = |x: u32, y: u32, st: &mut Stats| {
                st.derivations += 1;
                if !tc.contains(x, y) && seen.insert(pack(x, y)) {
                    fresh.push((x, y));
                }
            };
            match how {
                Nonlinear::Naive => {
                    for &(x, z) in &tc.all {
                        if let Some(ys) = tc.by0.get(&z) {
                            for &y in ys {
                                emit(x, y, &mut st);
                            }
                        }
                    }
                }
                Nonlinear::TextbookDelta | Nonlinear::OldDelta => {
                    // version 1: delta(x,z) join full(z,y)
                    for &(x, z) in &delta {
                        if let Some(ys) = tc.by0.get(&z) {
                            for &y in ys {
                                emit(x, y, &mut st);
                            }
                        }
                    }
                    // version 2: full-or-old(x,z) join delta(z,y)
                    let in_delta: FxSet<u64> = if how == Nonlinear::OldDelta {
                        delta.iter().map(|&(a, b)| pack(a, b)).collect()
                    } else {
                        FxSet::default()
                    };
                    for &(z, y) in &delta {
                        if let Some(xs) = tc.by1.get(&z) {
                            for &x in xs {
                                if how == Nonlinear::OldDelta && in_delta.contains(&pack(x, z)) {
                                    continue;
                                }
                                emit(x, y, &mut st);
                            }
                        }
                    }
                }
            }
        }
        st.new_tuples += fresh.len() as u64;
        if fresh.is_empty() {
            break;
        }
        delta = merge(&mut tc, fresh, true);
    }
    finish(&tc, st, t)
}

/// sg(x,y) :- arc(p,x), arc(p,y), x != y.
/// sg(x,y) :- arc(a,x), sg(a,b), arc(b,y).
pub fn sg(g: &Graph, mode: Mode) -> Run {
    let t = Instant::now();
    let out: Csr = g.out_csr();
    let mut st = Stats::default();
    let mut rel = HashRel::default();
    let mut delta: Vec<(u32, u32)> = Vec::new();
    for p in 0..g.n {
        let kids = out.row(p);
        for &x in kids {
            for &y in kids {
                if x != y {
                    st.derivations += 1;
                    if rel.insert(x, y, false) {
                        st.new_tuples += 1;
                        delta.push((x, y));
                    }
                }
            }
        }
    }
    loop {
        st.iterations += 1;
        let driver: &[(u32, u32)] = match mode {
            Mode::Naive => &rel.all,
            Mode::SemiNaive => &delta,
        };
        let mut seen: FxSet<u64> = FxSet::default();
        let mut fresh = Vec::new();
        for &(a, b) in driver {
            for &x in out.row(a as usize) {
                for &y in out.row(b as usize) {
                    st.derivations += 1;
                    if !rel.contains(x, y) && seen.insert(pack(x, y)) {
                        fresh.push((x, y));
                    }
                }
            }
        }
        st.new_tuples += fresh.len() as u64;
        if fresh.is_empty() {
            break;
        }
        delta = merge(&mut rel, fresh, false);
    }
    finish(&rel, st, t)
}

/// Exact derivation count semi-naive linear TC must report:
/// |arc| for the base rule, then every tc tuple (x,z) is in exactly one
/// delta and fires once per out-edge of z.
pub fn expected_linear_semi_naive_derivations(g: &Graph, tc_edges: &[(u32, u32)]) -> u64 {
    let out = g.out_csr();
    g.edges.len() as u64 + tc_edges.iter().map(|&(_, z)| out.deg(z as usize) as u64).sum::<u64>()
}

/// The closure itself as sorted pairs (reference for tests and sanity).
pub fn tc_pairs(g: &Graph) -> Vec<(u32, u32)> {
    let out = g.out_csr();
    let mut stamp = vec![u32::MAX; g.n];
    let mut res = Vec::new();
    let mut queue: Vec<u32> = Vec::new();
    for s in 0..g.n {
        queue.clear();
        for &v in out.row(s) {
            if stamp[v as usize] != s as u32 {
                stamp[v as usize] = s as u32;
                queue.push(v);
            }
        }
        let mut head = 0;
        while head < queue.len() {
            let u = queue[head] as usize;
            head += 1;
            for &v in out.row(u) {
                if stamp[v as usize] != s as u32 {
                    stamp[v as usize] = s as u32;
                    queue.push(v);
                }
            }
        }
        let mut r: Vec<u32> = queue.clone();
        r.sort_unstable();
        res.extend(r.into_iter().map(|v| (s as u32, v)));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{chain, grid, power_law, tc_size_bfs};

    #[test]
    fn semi_naive_equals_naive_equals_oracle() {
        for g in [power_law(120, 360, 1), grid(6, 6), chain(30)] {
            let n = tc_size_bfs(&g);
            let a = tc_linear(&g, Mode::Naive);
            let b = tc_linear(&g, Mode::SemiNaive);
            assert_eq!(a.size, n);
            assert_eq!(b.size, n);
            assert_eq!(a.checksum, b.checksum);
            assert_eq!(a.stats.iterations, b.stats.iterations);
            assert!(b.stats.derivations <= a.stats.derivations);
            let pairs = tc_pairs(&g);
            assert_eq!(b.stats.derivations, expected_linear_semi_naive_derivations(&g, &pairs));
            for how in [Nonlinear::Naive, Nonlinear::TextbookDelta, Nonlinear::OldDelta] {
                let r = tc_nonlinear(&g, how);
                assert_eq!(r.size, n, "{how:?}");
                assert_eq!(r.checksum, a.checksum, "{how:?}");
            }
        }
    }

    #[test]
    fn reach_from_is_one_row_of_the_closure() {
        for g in [power_law(120, 360, 1), grid(6, 6), chain(30)] {
            let pairs = tc_pairs(&g);
            for src in [0u32, 5, 17] {
                let want = pairs.iter().filter(|&&(x, _)| x == src).count() as u64;
                let r = reach_from(&g, src);
                assert_eq!(r.size, want);
                let full = tc_linear(&g, Mode::SemiNaive);
                assert!(r.stats.derivations <= full.stats.derivations);
            }
        }
    }

    #[test]
    fn old_delta_never_derives_more_than_textbook() {
        let g = power_law(120, 360, 1);
        let a = tc_nonlinear(&g, Nonlinear::TextbookDelta);
        let b = tc_nonlinear(&g, Nonlinear::OldDelta);
        assert!(b.stats.derivations <= a.stats.derivations);
    }

    #[test]
    fn sg_modes_agree() {
        for g in [power_law(80, 160, 2), grid(6, 6)] {
            let a = sg(&g, Mode::Naive);
            let b = sg(&g, Mode::SemiNaive);
            assert_eq!(a.size, b.size);
            assert_eq!(a.checksum, b.checksum);
            assert!(b.stats.derivations <= a.stats.derivations);
        }
    }
}
