//! A minimal e-graph — union-find, hashcons, e-class map, deferred rebuild.
//!
//! Small on purpose: topic 21 reads egg's real one (`~/repos/egg/src/egraph.rs`)
//! and this crate is not trying to replace it. What we need here is an e-graph
//! whose *internal representation is visible*, so the same structure can be
//! walked as a graph (`backtrack.rs`) and read as a set of tables
//! (`relational.rs`) with nothing hidden between the two.
//!
//! egg anchors for the same three pieces, at the pinned commit:
//!   unionfind.rs:30   UnionFind::find      — no path compression on the & path
//!   egraph.rs:970     EGraph::add          — hashcons lookup-or-insert
//!   egraph.rs:1147    EGraph::union        — merge now, repair later
//!   egraph.rs:1416    EGraph::rebuild      — the deferred congruence repair

use std::collections::HashMap;

/// An e-class id. Not necessarily canonical — call [`EGraph::find`].
pub type Id = u32;
/// An interned function symbol.
pub type Sym = u32;

/// `(f, [child ids])`. In the relational view this is one tuple of `R_f`, with
/// the containing e-class id prepended.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ENode {
    pub op: Sym,
    pub children: Vec<Id>,
}

#[derive(Default)]
pub struct EGraph {
    parents: Vec<Id>,
    classes: HashMap<Id, Vec<ENode>>,
    memo: HashMap<ENode, Id>,
    names: Vec<String>,
    syms: HashMap<String, Sym>,
}

impl EGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern a function symbol. `sym("f") == sym("f")`, always.
    pub fn sym(&mut self, name: &str) -> Sym {
        if let Some(&s) = self.syms.get(name) {
            return s;
        }
        let s = self.names.len() as Sym;
        self.names.push(name.to_string());
        self.syms.insert(name.to_string(), s);
        s
    }

    pub fn sym_name(&self, s: Sym) -> &str {
        &self.names[s as usize]
    }

    pub fn lookup_sym(&self, name: &str) -> Option<Sym> {
        self.syms.get(name).copied()
    }

    /// Canonical id. Like egg's `find(&self)`, this walks without compressing.
    pub fn find(&self, mut id: Id) -> Id {
        while self.parents[id as usize] != id {
            id = self.parents[id as usize];
        }
        id
    }

    fn canon(&self, n: &ENode) -> ENode {
        ENode {
            op: n.op,
            children: n.children.iter().map(|&c| self.find(c)).collect(),
        }
    }

    /// Hashcons: an e-node with the same symbol and canonical children is the
    /// same e-node, and lands in the same e-class.
    pub fn add(&mut self, op: Sym, children: &[Id]) -> Id {
        let node = self.canon(&ENode {
            op,
            children: children.to_vec(),
        });
        if let Some(&id) = self.memo.get(&node) {
            return self.find(id);
        }
        let id = self.parents.len() as Id;
        self.parents.push(id);
        self.classes.insert(id, vec![node.clone()]);
        self.memo.insert(node, id);
        id
    }

    /// Merge two e-classes. Congruence is left broken until [`Self::rebuild`].
    pub fn union(&mut self, a: Id, b: Id) -> bool {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return false;
        }
        self.parents[b as usize] = a;
        if let Some(nodes) = self.classes.remove(&b) {
            self.classes.entry(a).or_default().extend(nodes);
        }
        true
    }

    /// Restore both invariants: every e-node's children are canonical ids, and
    /// no two e-classes contain the same e-node (congruence). Runs to fixpoint,
    /// because merging two classes can make two more e-nodes congruent.
    pub fn rebuild(&mut self) {
        loop {
            let old: Vec<(Id, Vec<ENode>)> = self.classes.drain().collect();
            let mut fresh: HashMap<Id, Vec<ENode>> = HashMap::new();
            for (id, nodes) in old {
                let c = self.find(id);
                let slot = fresh.entry(c).or_default();
                for n in nodes {
                    slot.push(self.canon(&n));
                }
            }
            for v in fresh.values_mut() {
                v.sort();
                v.dedup();
            }
            self.classes = fresh;

            let mut memo: HashMap<ENode, Id> = HashMap::new();
            let mut merges: Vec<(Id, Id)> = Vec::new();
            for (&c, nodes) in &self.classes {
                for n in nodes {
                    match memo.get(n) {
                        Some(&prev) if prev != c => merges.push((prev, c)),
                        Some(_) => {}
                        None => {
                            memo.insert(n.clone(), c);
                        }
                    }
                }
            }
            if merges.is_empty() {
                self.memo = memo;
                return;
            }
            for (a, b) in merges {
                self.union(a, b);
            }
        }
    }

    pub fn class_ids(&self) -> impl Iterator<Item = Id> + '_ {
        self.classes.keys().copied()
    }

    pub fn nodes(&self, class: Id) -> &[ENode] {
        static EMPTY: &[ENode] = &[];
        self.classes.get(&self.find(class)).map_or(EMPTY, |v| v)
    }

    pub fn total_nodes(&self) -> usize {
        self.classes.values().map(|v| v.len()).sum()
    }

    pub fn n_classes(&self) -> usize {
        self.classes.len()
    }

    /// Every e-class holding at least one `op` e-node — egg's `classes_by_op`
    /// index (`egraph.rs:81`), which keeps a pattern's root from scanning the
    /// whole e-graph. Both matchers get it, so the comparison is about the
    /// inner loop rather than about the root scan.
    pub fn classes_with_op(&self, op: Sym) -> Vec<Id> {
        let mut v: Vec<Id> = self
            .classes
            .iter()
            .filter(|(_, ns)| ns.iter().any(|n| n.op == op))
            .map(|(&c, _)| c)
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashcons_returns_the_same_class() {
        let mut g = EGraph::new();
        let f = g.sym("f");
        let a = g.sym("a");
        let x = g.add(a, &[]);
        let n1 = g.add(f, &[x]);
        let n2 = g.add(f, &[x]);
        assert_eq!(n1, n2);
        assert_eq!(g.total_nodes(), 2);
    }

    #[test]
    fn rebuild_closes_congruence() {
        // f(a) and f(b) are distinct until a and b are merged; then congruence
        // says they are the same e-node, so their classes must merge too.
        let mut g = EGraph::new();
        let (f, a, b) = (g.sym("f"), g.sym("a"), g.sym("b"));
        let (ia, ib) = (g.add(a, &[]), g.add(b, &[]));
        let (fa, fb) = (g.add(f, &[ia]), g.add(f, &[ib]));
        assert_ne!(g.find(fa), g.find(fb));
        g.union(ia, ib);
        g.rebuild();
        assert_eq!(g.find(fa), g.find(fb), "congruence not restored");
        assert_eq!(g.nodes(fa).len(), 1, "duplicate e-node survived rebuild");
    }
}
