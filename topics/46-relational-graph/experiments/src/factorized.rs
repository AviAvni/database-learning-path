//! LANE 2 (exercise) — a factorized join operator over packed vectors.
//!
//! Lane 1 computed the size of the factorized result from degrees. This lane
//! builds it, in the layout of Yasser, Dorbani & Mhedhbi's packed factorized
//! vectors (SIGMOD'26 §4.1): every f-tree node is one contiguous vector of
//! values, and the grouping under its parent is an offset array — the values
//! belonging to parent position `i` are `vals[off[i]..off[i+1]]`. No pointers,
//! no per-group allocation, and a scan over one level is a tight loop.
//!
//! PROVIDED: [`Level`], [`TwoHop`] and [`two_hop`] — the 2-hop path
//! `R(a,b) ⋈ R(b,c)` over the f-tree `b → {a, c}`, built from the relation
//! with two hash tables, exactly as a relational engine would.
//!
//! YOUR JOB (the `todo!()`s):
//!
//! 1. [`extend`] — join a third copy of `R` onto the `c` level, producing the
//!    3-hop path over `b → a`, `b → c → d`. This is Lehner & Neumann's
//!    *bottom insert* in miniature (VLDB'26 §4.2): the new relation goes under
//!    the node it joins on, so `a` is never repeated per `d`. A `c` with no
//!    out-edge must disappear, and a `b` whose every `c` disappeared must
//!    disappear with its whole `a` slice — empty slices are not allowed to
//!    survive (see [`ThreeHop::check`]).
//! 2. [`ThreeHop::values`] and [`ThreeHop::count`] — the size of the
//!    representation and the number of flat tuples it stands for, the latter
//!    *without* flattening (Lehner & Neumann §4.5: products of subtree sizes).
//! 3. [`ThreeHop::flatten`] — enumerate the flat `(a, b, c, d)` tuples.
//! 4. [`ThreeHop::retain_d`] — a filter on `d`, with the reduction cascaded up
//!    the hierarchy: an emptied `c` slice kills its `c`, an emptied `b` kills
//!    its `b` and its `a` slice. This is FFX's Cascade Update (§5.2), done
//!    eagerly rather than with a selection bitmap.
//!
//! The tests below are the specification. `values()` must equal lane 1's
//! closed form `fsize::three_hop`, and `count()` the flat join's tuple count.

use crate::flat::FxBuild;
use crate::gen::Edges;
use std::collections::HashMap;

/// One f-tree node: packed values, grouped under the parent node's positions.
/// For a root level `off` is empty; otherwise `off.len() == parent.len() + 1`
/// and `vals[off[i] as usize..off[i + 1] as usize]` belong to parent position `i`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Level {
    pub vals: Vec<u32>,
    pub off: Vec<u64>,
}

impl Level {
    pub fn slice(&self, parent: usize) -> &[u32] {
        &self.vals[self.off[parent] as usize..self.off[parent + 1] as usize]
    }
    pub fn len(&self) -> usize {
        self.vals.len()
    }
    pub fn is_empty(&self) -> bool {
        self.vals.is_empty()
    }
}

/// `R(a,b) ⋈ R(b,c)` over the f-tree `b → {a, c}`.
#[derive(Debug, Clone)]
pub struct TwoHop {
    pub b: Vec<u32>,
    pub a: Level,
    pub c: Level,
}

impl TwoHop {
    pub fn values(&self) -> u64 {
        (self.b.len() + self.a.len() + self.c.len()) as u64
    }
    pub fn count(&self) -> u64 {
        (0..self.b.len()).map(|i| self.a.slice(i).len() as u64 * self.c.slice(i).len() as u64).sum()
    }
}

/// Hash tables a relational engine would build for the join: `R` keyed by
/// `src` and by `dst`. Probed only, never iterated.
pub struct Indexes {
    pub by_src: HashMap<u32, Vec<u32>, FxBuild>,
    pub by_dst: HashMap<u32, Vec<u32>, FxBuild>,
}

impl Indexes {
    pub fn new(edges: &Edges) -> Indexes {
        let mut by_src: HashMap<u32, Vec<u32>, FxBuild> = HashMap::default();
        let mut by_dst: HashMap<u32, Vec<u32>, FxBuild> = HashMap::default();
        for &(s, d) in edges {
            by_src.entry(s).or_default().push(d);
            by_dst.entry(d).or_default().push(s);
        }
        Indexes { by_src, by_dst }
    }
}

/// PROVIDED. Build the 2-hop f-representation. `b` runs over the distinct
/// destinations of `R` in increasing order (a sort, not a hash iteration), and
/// is kept only if it also has an out-edge.
pub fn two_hop(edges: &Edges, ix: &Indexes) -> TwoHop {
    let mut keys: Vec<u32> = edges.iter().map(|e| e.1).collect();
    keys.sort_unstable();
    keys.dedup();
    let mut t = TwoHop { b: Vec::new(), a: Level { vals: Vec::new(), off: vec![0] }, c: Level { vals: Vec::new(), off: vec![0] } };
    for b in keys {
        let Some(cs) = ix.by_src.get(&b) else { continue };
        let as_ = &ix.by_dst[&b];
        t.b.push(b);
        t.a.vals.extend_from_slice(as_);
        t.a.off.push(t.a.vals.len() as u64);
        t.c.vals.extend_from_slice(cs);
        t.c.off.push(t.c.vals.len() as u64);
    }
    t
}

/// `R(a,b) ⋈ R(b,c) ⋈ R(c,d)` over `b → a`, `b → c → d`. `d` is grouped
/// under positions of the `c` level.
#[derive(Debug, Clone)]
pub struct ThreeHop {
    pub b: Vec<u32>,
    pub a: Level,
    pub c: Level,
    pub d: Level,
}

impl ThreeHop {
    /// PROVIDED. Structural invariants: offsets are well-formed and no group is
    /// empty (an empty group would mean a binding with no completions).
    pub fn check(&self) -> Result<(), String> {
        let lvl = |name: &str, l: &Level, parents: usize| -> Result<(), String> {
            if l.off.len() != parents + 1 {
                return Err(format!("{name}: off.len() = {}, want {}", l.off.len(), parents + 1));
            }
            if l.off[0] != 0 || *l.off.last().unwrap() as usize != l.vals.len() {
                return Err(format!("{name}: offsets do not span the values"));
            }
            if l.off.windows(2).any(|w| w[1] <= w[0]) {
                return Err(format!("{name}: an empty (or inverted) group survived"));
            }
            Ok(())
        };
        lvl("a", &self.a, self.b.len())?;
        lvl("c", &self.c, self.b.len())?;
        lvl("d", &self.d, self.c.len())
    }

    /// Values stored: one per f-tree node occurrence.
    pub fn values(&self) -> u64 {
        let _ = self;
        todo!("lane 2: the representation's size in values")
    }

    /// Flat tuples represented, computed without enumerating them.
    pub fn count(&self) -> u64 {
        let _ = self;
        todo!("lane 2: Σ_b |a_b| · Σ_(c under b) |d_c|")
    }

    /// Every flat `(a, b, c, d)` tuple.
    pub fn flatten(&self) -> Vec<[u32; 4]> {
        let _ = self;
        todo!("lane 2: enumerate the flat tuples")
    }

    /// Keep only the `d` values for which `keep(d)` holds, cascading the
    /// reduction: no empty group may survive.
    pub fn retain_d(&mut self, keep: impl Fn(u32) -> bool) {
        let _ = keep;
        todo!("lane 2: filter d and cascade the emptied groups upward")
    }
}

/// Extend the 2-hop representation by `R(c, d)`, putting `d` under `c`.
pub fn extend(two: &TwoHop, ix: &Indexes) -> ThreeHop {
    let _ = (two, ix);
    todo!("lane 2: bottom-insert R(c,d) under the c level")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flat::{build_by_src, k_hop};
    use crate::fsize;
    use crate::gen::{ffx_fig1, power_law, PowerLaw};
    use crate::graph::Graph;

    fn small() -> (Edges, Graph) {
        let n = 500;
        let e = power_law(PowerLaw { vertices: n, edges: 3000, pref_percent: 70, seed: 11 });
        let g = Graph::new(n, &e);
        (e, g)
    }

    fn brute3(g: &Graph) -> Vec<[u32; 4]> {
        let mut v = Vec::new();
        for a in 0..g.n() as u32 {
            for &b in g.out.neighbours(a) {
                for &c in g.out.neighbours(b) {
                    for &d in g.out.neighbours(c) {
                        v.push([a, b, c, d]);
                    }
                }
            }
        }
        v.sort_unstable();
        v
    }

    #[test]
    fn provided_two_hop_matches_lane_1_and_the_paper() {
        let e = ffx_fig1();
        let t = two_hop(&e, &Indexes::new(&e));
        assert_eq!(t.values(), 15, "SIGMOD'26 §2.2.1: 15 values over T2");
        assert_eq!(t.count(), 13);
        let (e, g) = small();
        let t = two_hop(&e, &Indexes::new(&e));
        assert_eq!(t.values(), fsize::two_hop(&g).fact_values);
        assert_eq!(t.count(), fsize::two_hop(&g).flat_tuples);
    }

    #[test]
    fn extend_is_the_three_hop_join_and_has_no_empty_groups() {
        let (e, g) = small();
        let ix = Indexes::new(&e);
        let t = extend(&two_hop(&e, &ix), &ix);
        t.check().unwrap();
        assert_eq!(t.values(), fsize::three_hop(&g).fact_values, "size = lane 1's closed form");
        let mut sink = 0;
        assert_eq!(t.count(), k_hop(&e, &build_by_src(&e), 3, &mut sink).result(), "count = the flat join");
    }

    #[test]
    fn flatten_is_exactly_the_flat_result() {
        let (e, g) = small();
        let ix = Indexes::new(&e);
        let t = extend(&two_hop(&e, &ix), &ix);
        let mut f = t.flatten();
        f.sort_unstable();
        assert_eq!(f, brute3(&g));
        assert_eq!(f.len() as u64, t.count());
    }

    #[test]
    fn retain_d_cascades_and_matches_a_filtered_flat_join() {
        let (e, g) = small();
        let ix = Indexes::new(&e);
        let mut t = extend(&two_hop(&e, &ix), &ix);
        let keep = |d: u32| d % 7 == 0;
        t.retain_d(keep);
        t.check().expect("emptied groups must be cascaded away");
        let want: Vec<[u32; 4]> = brute3(&g).into_iter().filter(|r| keep(r[3])).collect();
        assert_eq!(t.count(), want.len() as u64);
        let mut f = t.flatten();
        f.sort_unstable();
        assert_eq!(f, want);
        // Size after the filter = distinct prefixes of the surviving tuples.
        let mut b: Vec<u32> = want.iter().map(|r| r[1]).collect();
        let mut ba: Vec<(u32, u32)> = want.iter().map(|r| (r[1], r[0])).collect();
        let mut bc: Vec<(u32, u32)> = want.iter().map(|r| (r[1], r[2])).collect();
        let mut bcd: Vec<(u32, u32, u32)> = want.iter().map(|r| (r[1], r[2], r[3])).collect();
        for v in [&mut b] {
            v.sort_unstable();
            v.dedup();
        }
        ba.sort_unstable();
        ba.dedup();
        bc.sort_unstable();
        bc.dedup();
        bcd.sort_unstable();
        bcd.dedup();
        assert_eq!(t.values(), (b.len() + ba.len() + bc.len() + bcd.len()) as u64);
    }
}
