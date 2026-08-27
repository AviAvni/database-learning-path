//! Backtracking e-matching: the algorithm every e-graph library shipped before
//! relational e-matching, reproduced faithfully enough to be worth timing.
//!
//! It is de Moura and Bjorner's declarative algorithm (POPL'22 Figure 3,
//! reproduced from their 2007 paper) compiled to egg's four-instruction virtual
//! machine so that the comparison in lane 1 is against a real implementation
//! strategy rather than against a set-of-substitutions strawman:
//!
//!   egg src/machine.rs:24-29   enum Instruction { Bind, Compare, Lookup, Scan }
//!   egg src/machine.rs:66-74   Scan iterates every e-class
//!   egg src/pattern.rs:300-304 classes_for_op short-circuits the scan
//!
//! `Lookup` (a whole ground subterm resolved in one hashcons probe) is the one
//! instruction we leave out: none of this topic's patterns contain a ground
//! subterm, so it would never be emitted.
//!
//! The shape of the cost is the point. `Bind` walks *every* e-node of the right
//! symbol in an e-class and pushes its children into registers; `Compare` — the
//! equality constraint — can only run once both registers are filled. So a
//! pattern like `f(a, g(a))` binds all N g-e-nodes under each of the N f-e-nodes
//! before rejecting N^2 - N of the pairs it built.

use crate::egraph::{EGraph, Id, Sym};
use crate::pattern::{PatV, VarId};
use std::cell::Cell;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum Ins {
    /// Enumerate candidate e-classes for a root register, through the op index.
    Scan { out: usize, op: Option<Sym> },
    /// For each `op` e-node in the e-class in register `class`, write its
    /// children into registers `out..out+arity`.
    Bind {
        class: usize,
        op: Sym,
        out: usize,
        arity: usize,
    },
    /// The equality constraint, checked when the second occurrence is reached.
    Compare { a: usize, b: usize },
}

pub struct Program {
    pub ins: Vec<Ins>,
    pub n_regs: usize,
    /// Register holding each query variable (usize::MAX if the variable is not
    /// bound by these patterns).
    pub var_reg: Vec<usize>,
    pub root_regs: Vec<usize>,
}

/// Compile numbered patterns into a straight-line program. Variables are bound
/// at their first occurrence and compared at every later one — the earliest a
/// backtracking matcher can check an equality constraint.
pub fn compile(pats: &[PatV], n_vars: usize, root_vars: &[VarId]) -> Program {
    let mut p = Program {
        ins: vec![],
        n_regs: 0,
        var_reg: vec![usize::MAX; n_vars],
        root_regs: vec![],
    };
    for (i, pat) in pats.iter().enumerate() {
        let root = p.n_regs;
        p.n_regs += 1;
        p.root_regs.push(root);
        // The root's auxiliary variable is answered by the root register.
        p.var_reg[root_vars[i]] = root;
        let op = match pat {
            PatV::App(op, _) => Some(*op),
            PatV::Var(_) => None,
        };
        p.ins.push(Ins::Scan { out: root, op });
        emit(&mut p, pat, root);
    }
    p
}

fn emit(p: &mut Program, pat: &PatV, reg: usize) {
    match pat {
        PatV::Var(v) => {
            if p.var_reg[*v] == usize::MAX {
                p.var_reg[*v] = reg;
            } else {
                p.ins.push(Ins::Compare {
                    a: p.var_reg[*v],
                    b: reg,
                });
            }
        }
        PatV::App(op, args) => {
            let base = p.n_regs;
            p.n_regs += args.len();
            p.ins.push(Ins::Bind {
                class: reg,
                op: *op,
                out: base,
                arity: args.len(),
            });
            for (i, a) in args.iter().enumerate() {
                emit(p, a, base + i);
            }
        }
    }
}

/// Run the program. `visits` counts units of work: one per e-node a `Bind`
/// steps over and one per e-class a `Scan` steps over — the same accounting
/// [`crate::relational`] uses for generic join, so the two are comparable.
pub fn search(
    g: &EGraph,
    prog: &Program,
    visits: &Cell<u64>,
    out: &mut dyn FnMut(&[Id]),
) {
    // The op index, built once — this is egg's `classes_by_op`, and both
    // matchers are entitled to it.
    let mut roots: HashMap<Option<Sym>, Vec<Id>> = HashMap::new();
    for ins in &prog.ins {
        if let Ins::Scan { op, .. } = ins {
            roots.entry(*op).or_insert_with(|| match op {
                Some(s) => g.classes_with_op(*s),
                None => g.class_ids().collect(),
            });
        }
    }
    let mut regs = vec![0 as Id; prog.n_regs];
    exec(g, &prog.ins, &roots, &mut regs, visits, out);
}

fn exec(
    g: &EGraph,
    ins: &[Ins],
    roots: &HashMap<Option<Sym>, Vec<Id>>,
    regs: &mut Vec<Id>,
    visits: &Cell<u64>,
    out: &mut dyn FnMut(&[Id]),
) {
    let Some((head, rest)) = ins.split_first() else {
        out(regs);
        return;
    };
    match head {
        Ins::Scan { out: o, op } => {
            for &c in &roots[op] {
                visits.set(visits.get() + 1);
                regs[*o] = c;
                exec(g, rest, roots, regs, visits, out);
            }
        }
        Ins::Bind {
            class,
            op,
            out: base,
            arity,
        } => {
            let c = regs[*class];
            for n in g.nodes(c) {
                if n.op != *op || n.children.len() != *arity {
                    continue;
                }
                visits.set(visits.get() + 1);
                for (i, &ch) in n.children.iter().enumerate() {
                    regs[base + i] = ch;
                }
                exec(g, rest, roots, regs, visits, out);
            }
        }
        Ins::Compare { a, b } => {
            if g.find(regs[*a]) == g.find(regs[*b]) {
                exec(g, rest, roots, regs, visits, out);
            }
        }
    }
}

/// Substitutions for the query's head variables, in head order.
pub fn matches(g: &EGraph, prog: &Program, head: &[VarId], visits: &Cell<u64>) -> Vec<Vec<Id>> {
    let mut found = vec![];
    search(g, prog, visits, &mut |regs| {
        found.push(head.iter().map(|&v| regs[prog.var_reg[v]]).collect());
    });
    found
}
