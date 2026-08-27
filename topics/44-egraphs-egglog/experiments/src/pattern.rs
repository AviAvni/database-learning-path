//! Patterns, and the compilation of a pattern into a conjunctive query.
//!
//! This is Figure 8 of "Relational E-matching" (POPL'22), which unnests a
//! pattern by giving every non-variable subpattern a fresh auxiliary variable:
//!
//!   Aux(f(p1..pk)) = v ~ R_f(v, v1..vk), A1..Ak   where Aux(pi) = vi ~ Ai
//!   Aux(x)         = x ~ []
//!
//! So `f(a, g(a))` becomes  `Q(root, a) <- R_f(root, a, x), R_g(x, a)`, and the
//! repeated `a` — the *equality constraint* backtracking checks last — is now
//! just a join variable, indistinguishable from the structural one (`x`).

use crate::egraph::Sym;
use std::collections::HashMap;

pub type VarId = usize;

#[derive(Clone, Debug)]
pub enum Pat {
    Var(String),
    App(Sym, Vec<Pat>),
}

pub fn pvar(name: &str) -> Pat {
    Pat::Var(name.to_string())
}

pub fn papp(op: Sym, args: Vec<Pat>) -> Pat {
    Pat::App(op, args)
}

/// `R_rel(vars[0], vars[1..])` — `vars[0]` is always the e-class id column.
#[derive(Clone, Debug)]
pub struct Atom {
    pub rel: Sym,
    pub vars: Vec<VarId>,
}

#[derive(Clone, Debug)]
pub struct Query {
    pub atoms: Vec<Atom>,
    pub n_vars: usize,
    /// Display name per variable; auxiliaries are named `?0`, `?1`, …
    pub names: Vec<String>,
    /// The variables the caller wants back: the roots, then the pattern vars.
    pub head: Vec<VarId>,
    /// One root per input pattern, in order.
    pub roots: Vec<VarId>,
}

struct Builder {
    atoms: Vec<Atom>,
    names: Vec<String>,
    by_name: HashMap<String, VarId>,
    aux: usize,
}

impl Builder {
    fn named(&mut self, name: &str) -> VarId {
        if let Some(&v) = self.by_name.get(name) {
            return v;
        }
        let v = self.names.len();
        self.names.push(name.to_string());
        self.by_name.insert(name.to_string(), v);
        v
    }

    fn fresh(&mut self) -> VarId {
        let v = self.names.len();
        self.names.push(format!("?{}", self.aux));
        self.aux += 1;
        v
    }

    /// Aux from Figure 8: returns the variable standing for this subpattern.
    fn aux_of(&mut self, p: &Pat) -> VarId {
        match p {
            Pat::Var(name) => self.named(name),
            Pat::App(op, args) => {
                let v = self.fresh();
                // Reserve this atom's slot before recursing, so the body reads
                // outside-in the way the paper writes it.
                let slot = self.atoms.len();
                self.atoms.push(Atom { rel: *op, vars: vec![v] });
                let child_vars: Vec<VarId> = args.iter().map(|a| self.aux_of(a)).collect();
                self.atoms[slot].vars.extend(child_vars);
                v
            }
        }
    }
}

/// Compile one or more patterns into a single conjunctive query. Several
/// patterns sharing variables is a *multi-pattern*; the relational view gets it
/// for free, since it is just more atoms in the same query body (POPL'22 §1).
pub fn compile(pats: &[Pat]) -> Query {
    let mut b = Builder {
        atoms: vec![],
        names: vec![],
        by_name: HashMap::new(),
        aux: 0,
    };
    let roots: Vec<VarId> = pats.iter().map(|p| b.aux_of(p)).collect();
    let mut head = roots.clone();
    // The pattern's own variables, in first-appearance order.
    let mut named: Vec<VarId> = b.by_name.values().copied().collect();
    named.sort();
    head.extend(named);
    Query {
        n_vars: b.names.len(),
        atoms: b.atoms,
        names: b.names,
        head,
        roots,
    }
}

impl Query {
    /// Human-readable, in the paper's notation.
    pub fn render(&self, name_of: &dyn Fn(Sym) -> String) -> String {
        let head: Vec<String> = self.head.iter().map(|&v| self.names[v].clone()).collect();
        let body: Vec<String> = self
            .atoms
            .iter()
            .map(|a| {
                let vs: Vec<String> = a.vars.iter().map(|&v| self.names[v].clone()).collect();
                format!("R_{}({})", name_of(a.rel), vs.join(", "))
            })
            .collect();
        format!("Q({}) <- {}", head.join(", "), body.join(", "))
    }

    pub fn atoms_with(&self, v: VarId) -> Vec<usize> {
        (0..self.atoms.len())
            .filter(|&i| self.atoms[i].vars.contains(&v))
            .collect()
    }
}

/// A pattern whose variables have been numbered against a [`Query`], so both
/// matchers report substitutions in the same variable space.
#[derive(Clone, Debug)]
pub enum PatV {
    Var(VarId),
    App(Sym, Vec<PatV>),
}

pub fn number(p: &Pat, q: &Query) -> PatV {
    match p {
        Pat::Var(name) => PatV::Var(
            q.names
                .iter()
                .position(|n| n == name)
                .expect("pattern variable not in the compiled query"),
        ),
        Pat::App(op, args) => PatV::App(*op, args.iter().map(|a| number(a, q)).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egraph::EGraph;

    #[test]
    fn unnesting_matches_figure_8() {
        let mut g = EGraph::new();
        let (f, gg) = (g.sym("f"), g.sym("g"));
        // f(a, g(a))
        let q = compile(&[papp(f, vec![pvar("a"), papp(gg, vec![pvar("a")])])]);
        assert_eq!(q.atoms.len(), 2, "one atom per non-variable subpattern");
        let rendered = q.render(&|s| g.sym_name(s).to_string());
        assert_eq!(rendered, "Q(?0, a) <- R_f(?0, a, ?1), R_g(?1, a)");
        // `a` occurs in both atoms: the equality constraint became a join.
        assert_eq!(q.atoms_with(q.head[1]).len(), 2);
    }
}
