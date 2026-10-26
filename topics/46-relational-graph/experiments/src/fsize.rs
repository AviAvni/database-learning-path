//! LANE 1 (provided) — how big the *factorized* result is, computed from
//! degrees alone, without enumerating a single path.
//!
//! An **f-representation** stores a join result as nested unions of values
//! under products (Olteanu & Závodný; Lehner & Neumann §2; Yasser et al.
//! §2.2.1). Its shape is an **f-tree**: one node per attribute, a child stored
//! once per value of its parent, siblings independent given their parent. Its
//! size is the number of values it stores — the number of trie nodes, f-tree
//! level by level.
//!
//! For a path pattern rooted at a middle vertex, the per-level sums collapse to
//! degree expressions:
//!
//! * 2-hop, f-tree `b → {a, c}`: each joining `b` once, its in-neighbours and
//!   its out-neighbours once each: `|B| + Σ_b (in(b) + out(b))`, linear in |E|.
//!   The flat result has `Σ_b in(b)·out(b)` tuples — the product, not the sum.
//! * 3-hop, f-tree `b → a`, `b → c → d`: the `d` level is stored once per
//!   `(b, c)` edge, so it costs `Σ_(b,c) out(c)`, which is the 2-hop count
//!   again. Factorization removes one level of blow-up, not all of them —
//!   Yasser et al. §2.2.3 give the same answer as worst-case sizes (N² for the
//!   best f-tree of a 4-path, N³ for the trees list-based processing allows).
//!
//! The counts come out of the same sums with products in place of sums — the
//! "degree product" DuckPGQ's `FactorizedJoinOptimizer` rewrites `count(*)`
//! into (`docs/FACTORIZED_JOINS.md:36`).

use crate::graph::Graph;

/// Sizes of one pattern's result, in values (atomic attribute values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FSize {
    pub flat_tuples: u64,
    pub flat_values: u64,
    pub fact_values: u64,
}

/// 2-hop path `R(a,b) ⋈ R(b,c)`, f-tree rooted at `b`.
pub fn two_hop(g: &Graph) -> FSize {
    let (mut tuples, mut fact) = (0u64, 0u64);
    for b in 0..g.n() as u32 {
        let (i, o) = (g.inc.degree(b), g.out.degree(b));
        if i > 0 && o > 0 {
            tuples += i * o;
            fact += 1 + i + o;
        }
    }
    FSize { flat_tuples: tuples, flat_values: 3 * tuples, fact_values: fact }
}

/// 2-hop path over the deep f-tree `a → b → c` (the flat nesting order, with
/// only repeated prefixes removed — Yasser et al.'s T1).
pub fn two_hop_deep(g: &Graph) -> u64 {
    let mut vals = 0u64;
    for a in 0..g.n() as u32 {
        let mut level_b = 0u64;
        let mut level_c = 0u64;
        for &b in g.out.neighbours(a) {
            let o = g.out.degree(b);
            if o > 0 {
                level_b += 1;
                level_c += o;
            }
        }
        if level_b > 0 {
            vals += 1 + level_b + level_c;
        }
    }
    vals
}

/// 3-hop path `R(a,b) ⋈ R(b,c) ⋈ R(c,d)`, f-tree `b → a`, `b → c → d`.
pub fn three_hop(g: &Graph) -> FSize {
    let (mut tuples, mut fact) = (0u64, 0u64);
    for b in 0..g.n() as u32 {
        let i = g.inc.degree(b);
        if i == 0 {
            continue;
        }
        let (mut cs, mut ds) = (0u64, 0u64);
        for &c in g.out.neighbours(b) {
            let o = g.out.degree(c);
            if o > 0 {
                cs += 1;
                ds += o;
            }
        }
        if cs > 0 {
            tuples += i * ds;
            fact += 1 + i + cs + ds;
        }
    }
    FSize { flat_tuples: tuples, flat_values: 4 * tuples, fact_values: fact }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{ffx_fig1, power_law, PowerLaw};
    use std::collections::BTreeSet;

    /// Brute force, independent of the formulas: enumerate every flat path and
    /// count distinct prefixes along each root-to-leaf path of the f-tree.
    fn brute(g: &Graph) -> (u64, u64, u64, u64, u64) {
        let (mut b1, mut ba, mut bc) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        let (mut a1, mut ab, mut abc) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        let (mut t3b, mut t3ba, mut t3bc, mut t3bcd) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        let (mut p2, mut p3) = (0u64, 0u64);
        for a in 0..g.n() as u32 {
            for &b in g.out.neighbours(a) {
                for &c in g.out.neighbours(b) {
                    p2 += 1;
                    b1.insert(b);
                    ba.insert((b, a));
                    bc.insert((b, c));
                    a1.insert(a);
                    ab.insert((a, b));
                    abc.insert((a, b, c));
                    for &d in g.out.neighbours(c) {
                        p3 += 1;
                        t3b.insert(b);
                        t3ba.insert((b, a));
                        t3bc.insert((b, c));
                        t3bcd.insert((b, c, d));
                    }
                }
            }
        }
        (
            p2,
            (b1.len() + ba.len() + bc.len()) as u64,
            (a1.len() + ab.len() + abc.len()) as u64,
            p3,
            (t3b.len() + t3ba.len() + t3bc.len() + t3bcd.len()) as u64,
        )
    }

    #[test]
    fn ffx_fig1_sizes_reproduce_the_paper() {
        let g = Graph::new(9, &ffx_fig1());
        let s = two_hop(&g);
        assert_eq!(s.flat_tuples, 13);
        assert_eq!(s.flat_values, 39);
        assert_eq!(two_hop_deep(&g), 22, "SIGMOD'26 §2.2.1: T1 holds 22 values");
        assert_eq!(s.fact_values, 15, "SIGMOD'26 §2.2.1: T2 holds 15 values");
    }

    #[test]
    fn formulas_match_brute_force_on_a_power_law_graph() {
        let n = 400;
        let g = Graph::new(n, &power_law(PowerLaw { vertices: n, edges: 2400, pref_percent: 70, seed: 3 }));
        let (p2, f2, f2deep, p3, f3) = brute(&g);
        assert_eq!(two_hop(&g).flat_tuples, p2);
        assert_eq!(two_hop(&g).fact_values, f2);
        assert_eq!(two_hop_deep(&g), f2deep);
        assert_eq!(three_hop(&g).flat_tuples, p3);
        assert_eq!(three_hop(&g).fact_values, f3);
    }
}
