# Z3: SAT plus theories, with an e-graph at the core

SMT is what turns "is this rewrite rule sound?" into a solver
query. This chapter reads de Moura & Bjørner's 4-page TACAS 2008
tool paper — the architecture is the point — alongside Z3's modern
e-graph in `src/ast/euf/`, which turns out to be egg's data
structure ([reading-egg-popl21.md](reading-egg-popl21.md)) built
for search instead of rewriting. Before either, this chapter builds
the stack from the bottom: SAT, theories, the DPLL(T) loop, then
the e-graph's role in it.

## The problem in one sentence

Decide whether a formula mixing booleans, integer arithmetic,
arrays, and uninterpreted functions has a satisfying assignment —
"does any input make this rewrite change the result?" is one such
formula, and Z3 answers it in milliseconds where enumeration would
take longer than the universe.

## The concepts, step by step

### Step 1 — SAT and CDCL: the boolean engine

**SAT** is the problem of finding a true/false assignment to
boolean variables that satisfies a formula (conventionally a
conjunction of **clauses**, each a disjunction of literals like
`p ∨ ¬q`). It's NP-complete, yet modern solvers routinely handle
millions of clauses, because of **CDCL** (conflict-driven clause
learning): guess a variable (*decide*), *propagate* forced
consequences, and when a contradiction appears, analyze it into a
new **learned clause** — a compact "never go down this road again"
— then backtrack and keep it forever. Each conflict permanently
prunes an exponential slice of the search space, which is the whole
reason SAT solving works in practice.

### Step 2 — atoms that mean something: SMT = SAT + theories

**SMT** (satisfiability modulo theories) lifts SAT to formulas
whose atomic propositions have *meaning*: `x + y ≤ 3` is not an
opaque boolean p — it's a claim in the **theory** of linear
arithmetic. A theory solver is a decision procedure for
conjunctions of such atoms: simplex for linear arithmetic, a
congruence engine for **EUF** (equality with uninterpreted
functions — you know nothing about f except x = y ⇒ f(x) = f(y)),
plus arrays, bit-vectors. The SAT core sees only the **boolean
skeleton** — each theory atom replaced by a fresh boolean — so it
can happily assert `x ≤ 3` and `x ≥ 7` together; only the theory
solver knows those conflict.

### Step 3 — DPLL(T): the SAT core proposes, the theories dispose

**DPLL(T)** is the loop that couples them: the SAT core proposes
an assignment to the skeleton; the theory solvers check whether
the implied conjunction of atoms is consistent; if not, they hand
back a **theory lemma** — a clause like `¬(x≤3) ∨ ¬(x≥7)` that
encodes the inconsistency in the SAT core's language, pruning the
search exactly like a learned clause:

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

(Real solvers interleave theory checks *during* propagation rather
than waiting for full assignments — but the contract is this loop.)
The division of labor is the design's genius: boolean case
splitting is CDCL's specialty, theory reasoning stays inside
specialized procedures, and clauses are the only currency between
them.

### Step 4 — theories must also talk to each other: Nelson-Oppen

A formula like `f(x) = f(y) ∧ x + 1 ≤ y ∧ y ≤ x + 1` splits atoms
between EUF and arithmetic — but arithmetic knows x = y and only
EUF can conclude f(x) = f(y). The **Nelson-Oppen** combination
scheme has theories cooperate by exchanging exactly one kind of
fact: *equalities between shared terms*. Each theory propagates
the equalities it can derive; the others consume them. Equalities
are the narrow-waist interface — the analogy to operators
exchanging join keys (topic 11) is question 4.

### Step 5 — the e-graph, again: EUF's engine, with two extra duties

EUF's decision procedure is congruence closure over an e-graph —
the very structure from the egg chapter (e-classes of equal terms,
hashcons, congruence: equal children ⇒ equal parents). Z3's
modern rewrite of it lives in `src/ast/euf/`:

| anchor | what |
|---|---|
| `euf_egraph.h:23` | comment: "same effect as delayed congruence table reconstruction **from egg**" — the 2021 paper flowing back into the 2008 solver |
| `euf_egraph.h:85` | `class egraph` |
| `euf_egraph.h:91-96` | `to_merge` queue (plain / commutativity / justified) — the pending-unions worklist, egg's `pending` |
| `euf_enode.h` | e-node: term + parents + root pointer |
| `euf_etable.h` | the congruence table (hashcons keyed on canonicalized children) |
| `euf_justification.h` | proof-producing unions — egg's `explain.rs` counterpart; Z3 needs it for conflict lemmas |

Key difference from egg: Z3's e-graph must support **backtracking**
(the SAT core undoes decisions, so unions must be undoable via a
trail — a log of mutations replayed in reverse) and
**justifications** (every merge must be explainable, because a
theory conflict must be handed back as a *specific* lemma naming
the guilty atoms). egg only needs monotone growth + optional
explanations. Same structure, different contract — and the
deferred-repair idea still transferred (the :23 comment), 13 years
from solver to library and back.

### Step 6 — quantifiers: e-matching, heuristic by necessity

A quantified axiom like `∀x. f(g(x)) = x` can't be handed to CDCL
— there are infinitely many instances. Z3 picks a **trigger** (a
subterm pattern, here `f(g(x))`) and instantiates the axiom for
every term in the e-graph matching the trigger *modulo the known
equalities* — that matching is **e-matching**, implemented as an
abstract machine (`euf_mam.h` — egg's `machine.rs`, industrial
strength). This is why quantified SMT is incomplete-but-useful:
instantiation is heuristic — too general a trigger floods the
solver with instances, too specific misses the needed one
(question 5 calls this the "index choice" problem of SMT).

### Step 7 — where a database meets Z3

- **Query equivalence** (Cosette, topic 16): compile two SQL plans
  to formulas, ask Z3 if outputs can differ. UNSAT = equivalent.
- **Constraint-based test generation**: "give me a row that makes
  this WHERE clause true" is a SAT query.
- **Optimizer rule soundness**: our `x/x → 1` caveat is checkable —
  `assert x=0 ∧ rewrite-changes-result`, SAT means unsound rule.

The usage pattern is always the same inversion: encode "a
counterexample exists" and hope for UNSAT — the solver's failure
to satisfy is your proof.

## How to read the paper (with the concepts in hand)

It's 4 pages — read all of it. The architecture diagram is the
payload: it's step 3's picture with Z3's actual component names.
Map each named component to a step as you read (SAT core → step 1,
theory solvers and their combination → steps 2-4, congruence
closure → step 5, e-matching/quantifiers → step 6). Then read the
`src/ast/euf/` headers in the order of step 5's anchor table —
starting with the comment at `euf_egraph.h:23`, the 2021 idea
cited inside the 2008 solver.

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
