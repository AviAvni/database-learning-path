//! The e-graph read as a relational database, and generic join over it.
//!
//! POPL'22 §3.1: every e-node with symbol `f` and arity k is one tuple of a
//! relation `R_f` of arity k+1 — the containing e-class id, then the children,
//! all canonical. Nothing is copied out of the e-graph that was not already in
//! it; this is a *view*, and egglog's answer (topic 44's second half) is to
//! stop maintaining the other view at all.
//!
//! Generic join (POPL'22 Algorithm 1, from Ngo et al.) is variable-at-a-time
//! rather than relation-at-a-time: pick a variable, intersect the sets of
//! values every atom allows for it, recurse on each survivor. Its cost is
//! bounded by the AGM bound of the query, which is why it cannot blow up on a
//! cyclic query the way a binary-join plan can.

use crate::egraph::{EGraph, Id, Sym};
use crate::pattern::{Atom, Query, VarId};
use std::cell::Cell;
use std::collections::HashMap;

#[derive(Default, Clone, Debug)]
pub struct Relation {
    pub arity: usize,
    pub tuples: Vec<Vec<Id>>,
}

pub type Database = HashMap<Sym, Relation>;

/// One pass over the e-graph, one tuple per e-node.
pub fn to_database(g: &EGraph) -> Database {
    let mut db: Database = HashMap::new();
    for c in g.class_ids() {
        for n in g.nodes(c) {
            let r = db.entry(n.op).or_insert_with(|| Relation {
                arity: n.children.len() + 1,
                tuples: vec![],
            });
            let mut t = Vec::with_capacity(n.children.len() + 1);
            t.push(c);
            t.extend(n.children.iter().map(|&x| g.find(x)));
            r.tuples.push(t);
        }
    }
    db
}

pub fn db_tuples(db: &Database) -> usize {
    db.values().map(|r| r.tuples.len()).sum()
}

/// A trie is a tree of maps; a path from the root spells one tuple, with the
/// columns ordered to agree with the query's variable ordering (POPL'22 Fig 5).
/// This is what makes "the residual relation R(v, y)" a pointer chase rather
/// than a filter.
#[derive(Default, Debug)]
pub struct Trie {
    pub kids: HashMap<Id, Trie>,
}

impl Trie {
    fn insert(&mut self, path: &[Id]) {
        let Some((h, rest)) = path.split_first() else {
            return;
        };
        self.kids.entry(*h).or_default().insert(rest);
    }
}

/// An atom indexed for one variable ordering.
pub struct AtomIndex {
    /// The atom's distinct variables, in the global ordering.
    pub vars: Vec<VarId>,
    pub trie: Trie,
    pub tuples_indexed: usize,
}

/// Build the trie for one atom. A variable occurring twice in the same atom
/// (`f(x, x)`) indexes on its first column and filters on the rest.
pub fn index_atom(rel: &Relation, atom: &Atom, order: &[VarId]) -> AtomIndex {
    let pos = |v: VarId| order.iter().position(|&o| o == v).expect("var not in ordering");
    let mut first: Vec<(VarId, usize)> = vec![];
    let mut filters: Vec<(usize, usize)> = vec![];
    for (col, &v) in atom.vars.iter().enumerate() {
        match first.iter().find(|(fv, _)| *fv == v) {
            Some(&(_, c0)) => filters.push((c0, col)),
            None => first.push((v, col)),
        }
    }
    first.sort_by_key(|&(v, _)| pos(v));
    let cols: Vec<usize> = first.iter().map(|&(_, c)| c).collect();
    let vars: Vec<VarId> = first.iter().map(|&(v, _)| v).collect();

    let mut trie = Trie::default();
    let mut indexed = 0;
    let mut path = vec![0 as Id; cols.len()];
    for t in &rel.tuples {
        if filters.iter().any(|&(a, b)| t[a] != t[b]) {
            continue;
        }
        for (i, &c) in cols.iter().enumerate() {
            path[i] = t[c];
        }
        trie.insert(&path);
        indexed += 1;
    }
    AtomIndex {
        vars,
        trie,
        tuples_indexed: indexed,
    }
}

/// Variable ordering: most-constrained-first — the variable in the most atoms,
/// breaking ties toward the one whose smallest relation is smallest. Any order
/// is worst-case optimal; the order is what decides the constant (POPL'22 §2.3,
/// "different orderings can lead to dramatically different run time").
pub fn plan(q: &Query, db: &Database) -> Vec<VarId> {
    let mut vars: Vec<VarId> = (0..q.n_vars).collect();
    let key = |v: &VarId| {
        let atoms = q.atoms_with(*v);
        let smallest = atoms
            .iter()
            .map(|&i| db.get(&q.atoms[i].rel).map_or(usize::MAX, |r| r.tuples.len()))
            .min()
            .unwrap_or(usize::MAX);
        (usize::MAX - atoms.len(), smallest, *v)
    };
    vars.sort_by_key(key);
    vars
}

pub fn index_query(q: &Query, db: &Database, order: &[VarId]) -> Vec<AtomIndex> {
    static EMPTY: &[Vec<Id>] = &[];
    q.atoms
        .iter()
        .map(|a| match db.get(&a.rel) {
            Some(r) => index_atom(r, a, order),
            None => index_atom(
                &Relation {
                    arity: a.vars.len(),
                    tuples: EMPTY.to_vec(),
                },
                a,
                order,
            ),
        })
        .collect()
}

/// Generic join. `probes` counts the same unit backtracking counts: one per
/// key looked at during an intersection.
pub fn generic_join(
    order: &[VarId],
    idx: &[AtomIndex],
    n_vars: usize,
    probes: &Cell<u64>,
    out: &mut dyn FnMut(&[Id]),
) {
    let mut cur: Vec<&Trie> = idx.iter().map(|a| &a.trie).collect();
    let mut depth_of: Vec<usize> = vec![0; idx.len()];
    let mut subst = vec![0 as Id; n_vars];
    gj(0, order, idx, &mut cur, &mut depth_of, &mut subst, probes, out);
}

/// Atoms participating in one intersection. Patterns in this topic have at
/// most three atoms; the fixed array keeps the inner loop allocation-free, so
/// the numbers compare algorithms rather than allocators.
const MAX_ATOMS: usize = 8;

#[allow(clippy::too_many_arguments)]
fn gj<'a>(
    depth: usize,
    order: &[VarId],
    idx: &'a [AtomIndex],
    cur: &mut [&'a Trie],
    at: &mut [usize],
    subst: &mut [Id],
    probes: &Cell<u64>,
    out: &mut dyn FnMut(&[Id]),
) {
    if depth == order.len() {
        out(subst);
        return;
    }
    let x = order[depth];
    let mut part = [0usize; MAX_ATOMS];
    let mut n_part = 0;
    for i in 0..idx.len() {
        if idx[i].vars.get(at[i]) == Some(&x) {
            assert!(n_part < MAX_ATOMS, "raise MAX_ATOMS for this query");
            part[n_part] = i;
            n_part += 1;
        }
    }
    if n_part == 0 {
        // A variable no atom constrains: nothing to intersect, nothing to bind.
        gj(depth + 1, order, idx, cur, at, subst, probes, out);
        return;
    }
    // Intersect smallest-first, which is what buys the O(min |R_j.x|) bound.
    let lead = *part[..n_part]
        .iter()
        .min_by_key(|&&i| cur[i].kids.len())
        .expect("non-empty");
    let lead_trie: &'a Trie = cur[lead];
    let mut others = [(0usize, lead_trie); MAX_ATOMS];
    let mut n_others = 0;
    for &i in &part[..n_part] {
        if i != lead {
            others[n_others] = (i, cur[i]);
            n_others += 1;
        }
    }
    let mut next = [(0usize, lead_trie); MAX_ATOMS];

    for (&v, sub) in &lead_trie.kids {
        probes.set(probes.get() + 1);
        let mut ok = true;
        for k in 0..n_others {
            probes.set(probes.get() + 1);
            match others[k].1.kids.get(&v) {
                Some(child) => next[k] = (others[k].0, child),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        cur[lead] = sub;
        at[lead] += 1;
        for &(i, t) in &next[..n_others] {
            cur[i] = t;
            at[i] += 1;
        }
        subst[x] = v;
        gj(depth + 1, order, idx, cur, at, subst, probes, out);
        cur[lead] = lead_trie;
        at[lead] -= 1;
        for &(i, t) in &others[..n_others] {
            cur[i] = t;
            at[i] -= 1;
        }
    }
}

/// Substitutions for the head variables, in head order.
pub fn matches(q: &Query, db: &Database, probes: &Cell<u64>) -> Vec<Vec<Id>> {
    let order = plan(q, db);
    let idx = index_query(q, db, &order);
    let mut found = vec![];
    generic_join(&order, &idx, q.n_vars, probes, &mut |s| {
        found.push(q.head.iter().map(|&v| s[v]).collect());
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtrack;
    use crate::gen::{edge_graph, Fig2};
    use crate::pattern::{compile, number, papp, pvar, Pat};
    use std::collections::HashSet;

    /// The load-bearing test for lane 1: a graph walk and a join must return
    /// the same set of substitutions, or the timing table compares nothing.
    fn agree(g: &EGraph, pats: &[Pat], expect: usize) {
        let q = compile(pats);
        let pv: Vec<_> = pats.iter().map(|p| number(p, &q)).collect();
        let prog = backtrack::compile(&pv, q.n_vars, &q.roots);
        let bt = Cell::new(0);
        let walked: HashSet<Vec<Id>> = backtrack::matches(g, &prog, &q.head, &bt)
            .into_iter()
            .collect();
        let gj = Cell::new(0);
        let joined: HashSet<Vec<Id>> = matches(&q, &to_database(g), &gj).into_iter().collect();
        assert_eq!(walked.len(), expect, "unexpected match count");
        assert_eq!(walked, joined, "the two matchers disagree");
    }

    #[test]
    fn nonlinear_pattern_agrees() {
        let fig = Fig2::new(60);
        // f(a, g(a)) — N matches out of N^2 candidate terms.
        agree(
            &fig.g,
            &[papp(fig.f, vec![pvar("a"), papp(fig.gs, vec![pvar("a")])])],
            60,
        );
    }

    #[test]
    fn linear_pattern_agrees() {
        let fig = Fig2::new(40);
        // f(a, g(b)) — every candidate is a match: N^2 of them.
        agree(
            &fig.g,
            &[papp(fig.f, vec![pvar("a"), papp(fig.gs, vec![pvar("b")])])],
            1600,
        );
    }

    #[test]
    fn triangle_multipattern_agrees() {
        let (g, e) = edge_graph(60, 300, 3);
        let pats = vec![
            papp(e, vec![pvar("x"), pvar("y")]),
            papp(e, vec![pvar("y"), pvar("z")]),
            papp(e, vec![pvar("z"), pvar("x")]),
        ];
        let q = compile(&pats);
        let gj = Cell::new(0);
        let n = matches(&q, &to_database(&g), &gj).len();
        assert!(n > 0, "generator produced no triangles");
        agree(&g, &pats, n);
    }
}
