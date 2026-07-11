# Z3: SAT plus theories, with an e-graph at the core

SMT is what turns "is this rewrite rule sound?" into a solver
query. This chapter reads de Moura & Bjørner's 4-page TACAS 2008
tool paper — the architecture is the point — alongside Z3's modern
e-graph in `src/ast/euf/`, which turns out to be egg's data
structure ([reading-egg-popl21.md](reading-egg-popl21.md)) built
for search instead of rewriting.

## SMT in one diagram

```
        formula (QF or quantified)
              │ simplify / tactics
              ▼
   ┌──────── CDCL SAT core ────────┐   boolean skeleton:
   │  decide / propagate / learn   │   p ∨ ¬q, p ≡ "x+y ≤ 3" …
   └──────┬─────────────▲──────────┘
          │ partial      │ theory lemma
          │ assignment   │ (conflict clause)
          ▼             │
   theory solvers: EUF (congruence closure e-graph),
   linear arith (simplex), arrays, bit-vectors …
```

DPLL(T): SAT core proposes a boolean assignment; theory solvers
check its conjunction of atoms; on conflict they hand back a lemma
that prunes the SAT search. Theories *cooperate* by exchanging
equalities over shared terms (Nelson-Oppen).

```rust
// DPLL(T): the SAT core proposes, the theory solvers dispose
fn smt_solve(mut clauses: Vec<Clause>, theories: &Theories) -> Result {
    loop {
        match sat_cdcl(&clauses) {
            Unsat => return Unsat,               // even the skeleton is out
            Sat(assignment) => {
                // the boolean skeleton says: these theory atoms hold
                match theories.check(assignment.atoms()) {
                    Consistent(model) => return Sat(model),
                    Conflict(lemma) => clauses.push(lemma),
                    // the lemma ("¬(x≤3) ∨ ¬(x≥7)") prunes the SAT
                    // search — theory knowledge flows back as clauses
                }
            }
        }
    }
}
```

## The e-graph, again — `src/ast/euf/`

| anchor | what |
|---|---|
| `euf_egraph.h:23` | comment: "same effect as delayed congruence table reconstruction **from egg**" — the 2021 paper flowing back into the 2008 solver |
| `euf_egraph.h:85` | `class egraph` |
| `euf_egraph.h:91-96` | `to_merge` queue (plain / commutativity / justified) — the pending-unions worklist, egg's `pending` |
| `euf_enode.h` | e-node: term + parents + root pointer |
| `euf_etable.h` | the congruence table (hashcons keyed on canonicalized children) |
| `euf_justification.h` | proof-producing unions — egg's `explain.rs` counterpart; Z3 needs it for conflict lemmas |

Key difference from egg: Z3's e-graph must support **backtracking**
(SAT core undoes decisions ⇒ undo unions via a trail) and
**justifications** (every merge must be explainable to build
conflict clauses). egg only needs monotone growth + optional
explanations. Same structure, different contract.

## E-matching (quantifiers)

`∀x. f(g(x)) = x` becomes a *trigger* `f(g(x))`; e-matching finds
instantiations by matching the trigger against the e-graph modulo
equivalence (`euf_mam.h` — matching abstract machine ≈ egg's
`machine.rs`, industrial strength). This is why quantified SMT is
incomplete-but-useful: instantiation is heuristic.

## Where a database meets Z3

- **Query equivalence** (Cosette, topic 16): compile two SQL plans
  to formulas, ask Z3 if outputs can differ. UNSAT = equivalent.
- **Constraint-based test generation**: "give me a row that makes
  this WHERE clause true" is a SAT query.
- **Optimizer rule soundness**: our `x/x → 1` caveat is checkable —
  `assert x=0 ∧ rewrite-changes-result`, SAT means unsound rule.

## Questions (answer in notes.md)

1. Why must Z3's e-graph carry justifications while egg's can skip
   them? What would proof-producing unions cost egg's rebuild?
2. The trail/backtracking requirement: why does deferred rebuilding
   interact badly with undo, and how does `to_merge_t` (:91) hint at
   the resolution?
3. Encode the `x/x → 1` soundness check as an SMT query (ints, then
   reals). Which theory answers each?
4. Nelson-Oppen needs theories to agree on equalities of shared
   terms — spot the analogy to exchanging join keys between operators
   (topic 11).
5. E-matching triggers: why is trigger selection the "index choice"
   problem of SMT (too general = blowup, too specific = incomplete)?

## References

**Papers**
- de Moura, Bjørner — "Z3: An Efficient SMT Solver" (TACAS 2008) —
  4 pages; read all of it for the architecture diagram

**Code**
- [z3](https://github.com/Z3Prover/z3) `src/ast/euf/` —
  `euf_egraph.h` (:23 cites egg's deferred repair, :91-96 the
  `to_merge` worklist), `euf_enode.h`, `euf_etable.h`,
  `euf_justification.h`, `euf_mam.h` (e-matching abstract machine)
