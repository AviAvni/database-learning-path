//! Seeded e-graph generators. Every figure in this topic reproduces exactly.

use crate::egraph::{EGraph, Id, Sym};
use crate::relational::{Database, Relation};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::HashSet;

/// The e-graph of POPL'22 Figure 2: N constants, one e-class holding
/// `g(1)..g(N)`, one e-class holding `f(1, i_g)..f(N, i_g)`.
///
/// 3N e-nodes, N+2 e-classes — and it represents N + N + N^2 terms, which is
/// the whole point: the e-graph is polynomial and the term set it stands for is
/// quadratic, so an algorithm that enumerates terms has already lost.
pub struct Fig2 {
    pub g: EGraph,
    pub f: Sym,
    pub gs: Sym,
    pub ig: Id,
    pub iff: Id,
    pub n: usize,
}

impl Fig2 {
    pub fn new(n: usize) -> Self {
        let mut g = EGraph::new();
        let f = g.sym("f");
        let gs = g.sym("g");
        let mut me = Fig2 {
            g,
            f,
            gs,
            ig: 0,
            iff: 0,
            n: 0,
        };
        me.extend(n, true);
        me
    }

    /// Add `k` more constants, with their `g` and `f` e-nodes, into the same
    /// two e-classes. Used by the semi-naive lane: the e-graph after the delta
    /// keeps every id it had before it.
    pub fn grow(&mut self, k: usize) {
        self.extend(k, false);
    }

    fn extend(&mut self, k: usize, first: bool) {
        let (f, gs) = (self.f, self.gs);
        let base = self.n;
        let leaves: Vec<Id> = (base..base + k)
            .map(|i| {
                let s = self.g.sym(&format!("c{i}"));
                self.g.add(s, &[])
            })
            .collect();

        for (j, &l) in leaves.iter().enumerate() {
            let x = self.g.add(gs, &[l]);
            if first && j == 0 {
                self.ig = x;
            } else {
                self.g.union(self.ig, x);
            }
        }
        self.g.rebuild();
        self.ig = self.g.find(self.ig);

        for (j, &l) in leaves.iter().enumerate() {
            let x = self.g.add(f, &[l, self.ig]);
            if first && j == 0 {
                self.iff = x;
            } else {
                self.g.union(self.iff, x);
            }
        }
        self.g.rebuild();
        self.iff = self.g.find(self.iff);
        self.n += k;
    }
}

/// A seeded directed graph as an e-graph: one nullary e-node per vertex, one
/// binary `e(x, y)` e-node per edge, no unions. `R_e` is then an edge list, and
/// the triangle multi-pattern is the database triangle query on the nose.
pub fn edge_graph(vertices: usize, edges: usize, seed: u64) -> (EGraph, Sym) {
    let mut g = EGraph::new();
    let e = g.sym("e");
    let vs: Vec<Id> = (0..vertices)
        .map(|i| {
            let s = g.sym(&format!("v{i}"));
            g.add(s, &[])
        })
        .collect();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut seen = HashSet::new();
    let mut made = 0;
    while made < edges {
        let (a, b) = (rng.gen_range(0..vertices), rng.gen_range(0..vertices));
        if a == b || !seen.insert((a, b)) {
            continue;
        }
        g.add(e, &[vs[a], vs[b]]);
        made += 1;
    }
    g.rebuild();
    (g, e)
}

/// Tuples present in `new` and not in `old` — the delta database a semi-naive
/// iteration is allowed to look at.
pub fn db_delta(new: &Database, old: &Database) -> Database {
    let mut d: Database = Database::new();
    for (&rel, r) in new {
        let before: HashSet<&Vec<Id>> = old.get(&rel).map(|o| o.tuples.iter().collect()).unwrap_or_default();
        let tuples: Vec<Vec<Id>> = r
            .tuples
            .iter()
            .filter(|t| !before.contains(*t))
            .cloned()
            .collect();
        if !tuples.is_empty() {
            d.insert(
                rel,
                Relation {
                    arity: r.arity,
                    tuples,
                },
            );
        }
    }
    d
}
