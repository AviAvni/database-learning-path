# Z3: SAT plus theories, with an e-graph at the core

SMT is what turns "is this rewrite rule sound?" into a solver query. This
chapter reads de Moura and Bjørner's TACAS 2008 tool paper alongside Z3's modern
e-graph in `src/ast/euf/` — which turns out to be egg's data structure
([reading-egg-popl21.md](reading-egg-popl21.md)) built for *search* rather than
rewriting, and which cites egg by name in a source comment.

**Read the paper knowing what it is.** *Z3: An Efficient SMT Solver* is a
**four-page tool paper** — LNCS 4963, pages 337–340 — announcing a first external
release. It has an architecture figure and one short paragraph per component. It
does **not** contain DPLL(T) internals, the theory-combination algorithm, the
e-matching algorithm, or the relevancy algorithm; each of those is a separate
cited paper. Attributing them to this paper is the standard mistake, and this
chapter flags each one at the point where the temptation arises.

Code anchors are `Z3Prover/z3` at the commit this repo pins, **`1d425e5`**
(`resources/codebases.md` pin table). Note the date gap before you read them: the
paper is 2008 and `src/ast/euf/euf_egraph.h` is headed "Copyright (c) 2020 …
Nikolaj Bjorner (nbjorner) 2020-08-23". You are reading a twelve-year-younger
rewrite next to the announcement of the original.

## The problem in one sentence

Decide whether a formula mixing booleans, integer arithmetic, arrays and
uninterpreted functions has a satisfying assignment — "does any input make this
rewrite change the result?" is one such formula, and the whole engineering
problem is that the boolean part wants exhaustive case splitting while the
arithmetic part wants a decision procedure, and the two must exchange
information without either giving up its specialised representation.

## The concepts, step by step

### Step 1 — SAT and CDCL, in the paper's own vocabulary

> **In:** a conjunction of clauses over boolean variables.
> **Out:** the four search-pruning techniques the paper names, and the one term
> it never uses.

**SAT** asks for a true/false assignment to boolean variables satisfying a
formula, conventionally a conjunction of **clauses**, each a disjunction of
literals such as `p ∨ ¬q`. It is NP-complete, and modern solvers handle millions
of clauses anyway.

The mechanism is usually called **CDCL** — conflict-driven clause learning:
*decide* a variable, *propagate* forced consequences, and on contradiction
analyse the conflict into a **learned clause** ("never go down this road again"),
then backtrack and keep the clause. Each conflict permanently prunes a slice of
the search space.

The paper never writes "CDCL". Its *SAT Solver* paragraph says:

> "Boolean case splits are controlled using a state-of-the art SAT solver. The
> SAT solver integrates standard search pruning methods, such as **two-watch
> literals** for efficient Boolean constraint propagation, **lemma learning using
> conflict clauses**, **phase caching** for guiding case splits, and performs
> **non-chronological backtracking**."

Four named techniques, no algorithm. That is the level the whole paper operates
at, and knowing it saves you looking for detail that is not there.

Two of those four matter downstream in this chapter. **Non-chronological
backtracking** means the solver may jump back several decision levels at once —
Step 6's e-graph must be able to undo that many merges. **Lemma learning** is the
currency Step 3 uses to get theory knowledge into the boolean search.

### Step 2 — SMT: atoms that mean something

> **In:** a formula whose atoms are not opaque.
> **Out:** the boolean skeleton, and why the SAT core cannot see the
> contradiction in `x ≤ 3 ∧ x ≥ 7`.

**SMT** (satisfiability modulo theories) lifts SAT to formulas whose atomic
propositions have meaning. `x + y ≤ 3` is not an opaque `p`; it is a claim in the
**theory** of linear arithmetic. A **theory solver** is a decision procedure for
*conjunctions* of such atoms.

The SAT core is handed only the **boolean skeleton** — each theory atom replaced
by a fresh boolean variable. That is why it will cheerfully assert `x ≤ 3` and
`x ≥ 7` together: at the skeleton level those are two unrelated variables, both
set true, no clause violated. Only the arithmetic solver knows they conflict.

The paper's abstract fixes the theory list: "arithmetic, bit-vectors, arrays, and
uninterpreted functions", and the introduction adds quantifiers. **EUF** —
equality with uninterpreted functions — is the theory where you know nothing
about `f` except the congruence axiom `x = y ⇒ f(x) = f(y)`, and it is the one
Step 5 is about.

### Step 3 — the architecture figure, read as a dataflow

> **In:** Step 1's boolean engine and Step 2's theory solvers.
> **Out:** the paper's actual figure, with its actual edge labels — this is the
> payload of the whole four pages.

The paper's one-sentence summary: "Z3 integrates a modern DPLL-based SAT solver,
a core theory solver that handles equalities and uninterpreted functions,
satellite solvers (for arithmetic, arrays, etc.), and an **E-matching abstract
machine** (for quantifiers). Z3 is implemented in C++."

The figure, redrawn with the edge labels the paper prints:

```
   SMT-LIB   Simplify   Native text        C   .NET   OCaml
        └────────┬──────────┘                └────┬────┘
                 ▼                                ▼
             Simplifier          ← contextual simplification, x=4 ∧ q(x) ↦ x=4 ∧ q(4)
                 ▼
              Compiler           ← AST becomes clauses + congruence-closure nodes
                 ▼
   ┌──── Congruence closure core ────┐  ◄── literal assignments ──  SAT solver
   │        (the E-graph)            │  ─── new atoms, clauses ──►
   └──┬───────────────────────▲──────┘
      │ equalities            │ equalities
      ▼                       │
   Theory Solvers: Linear arithmetic · Bit-vectors · Arrays · Tuples
                 ▲
                 └── E-matching engine  (quantifier instantiation)
```

Each box gets one paragraph in the paper, and each paragraph contains one
specific, quotable fact:

- **Simplifier** — "incomplete, but efficient". Does contextual simplification:
  `x = 4 ∧ q(x) ↦ x = 4 ∧ q(4)`. The trivially satisfiable conjunct `x = 4` is
  *not* compiled into the core, but "kept aside in the case the client requires a
  model to evaluate `x`".
- **Compiler** — converts the simplified AST into "a set of clauses and
  congruence-closure nodes". This is where the boolean skeleton of Step 2 is
  actually built.
- **Congruence closure core** — Step 5.
- **Deleting clauses** — quantifier instantiation produces new clauses and atoms;
  Z3 garbage collects the ones "that were useless in closing branches". But:
  "Conflict clauses, and literals used in them, are on the other hand not
  deleted, so quantifier instantiations that were useful in producing conflicts
  are retained as a side-effect." A cache-eviction policy in a theorem prover.
- **Relevancy propagation** — "**DPLL(T) based solvers** assign a Boolean value
  to potentially all atoms appearing in a goal. In practice, several of these
  atoms are don't cares. Z3 ignores these atoms for expensive theories, such as
  bit-vectors, and inference rules, such as quantifier instantiation." That
  sentence is the *only* occurrence of "DPLL(T)" in the paper, and the algorithm
  is in a separate technical report (MSR-TR-2007-140).
- **Theory Solvers** — linear arithmetic "based on the algorithm used in
  **Yices**"; arrays use "**lazy instantiation of array axioms**"; bit-vectors
  apply "**bit-blasting to all bit-vector operations, but equality**".
- **Model generation** — models assign values to constants and "generate partial
  function graphs for predicates and function symbols".

The **DPLL(T)** loop those pieces implement — SAT core proposes a partial
assignment, theory solvers check consistency, an inconsistency comes back as a
clause the SAT core can learn — is worth having in your head, but it is *not*
described in this paper. Here it is as pseudocode, so you can hold it while
reading the figure:

```rust
// ILLUSTRATION — not Z3 code. The contract the figure implies; the real loop
// interleaves theory checks with propagation. For the code, read the worklist
// drain at src/ast/euf/euf_egraph.cpp:654 and the merge at :511.
loop {
    match sat_core.next_assignment() {
        Unsat            => return Unsat,        // even the skeleton is out
        Sat(assignment)  => match theories.check(assignment.atoms()) {
            Consistent(model) => return Sat(model),
            Conflict(lemma)   => sat_core.learn(lemma),  // e.g. ¬(x≤3) ∨ ¬(x≥7)
        }
    }
}
```

The division of labour is the design: boolean case splitting stays in CDCL,
theory reasoning stays inside specialised procedures, and **clauses are the only
currency between them**.

### Step 4 — theory combination: what Z3 does *instead of* Nelson-Oppen

> **In:** two theory solvers that each know part of a formula.
> **Out:** the classical answer, and the paper's explicit statement that Z3 does
> something else — the correction that matters most in this chapter.

The problem is real. In `f(x) = f(y) ∧ x + 1 ≤ y ∧ y ≤ x + 1`, arithmetic can
derive `x = y` but knows nothing about `f`; EUF can conclude `f(x) = f(y)` but
only if someone tells it `x = y`. The classical solution is **Nelson–Oppen**
combination: theories cooperate by exchanging exactly one kind of fact —
*equalities between shared terms* — and each must be able to produce all the
equalities it implies.

**Z3 does not do this, and the paper says so in its own section:**

> "Traditional methods for combining theory solvers rely on capabilities of the
> solvers to produce all implied equalities or a pre-processing step that
> introduces additional literals into the search space. Z3 uses a new theory
> combination method that **incrementally reconciles models maintained by each
> theory** [5]."

Reference [5] is de Moura and Bjørner, *Model-based Theory Combination*, SMT
2007. The idea named there is different in kind: rather than deriving and
exchanging implied equalities, each theory keeps a candidate **model**, and the
combination procedure looks at those models for variables that happen to be
assigned equal values, guesses the corresponding equality, and repairs when the
guess fails. It is a search-with-backtracking strategy where Nelson–Oppen is a
deduction strategy — which is why the paper calls out the two costs it avoids
(producing *all* implied equalities; introducing extra literals).

Do not write "Z3 uses Nelson–Oppen" and cite this paper. The paper's only mention
of the traditional method is to say it is not what Z3 does.

The equality-exchange picture is still useful, because it is what the e-graph
actually implements at the interface: "Nodes in the E-graph may point to one or
more theory solvers. When two nodes are merged, the set of theory solver
references are merged, and the merge is propagated as an equality to the theory
solvers **in the intersection** of the two sets of solver references." That
sentence describes a real field on a real struct — Step 5.

### Step 5 — the e-graph: same structure, different contract

> **In:** the egg chapter's e-graph.
> **Out:** the fields Z3 adds, and the two requirements — backtracking and
> justification — that make them necessary.

The paper is explicit that the structure is borrowed and even that the name is:
"Equalities asserted by the SAT solver are propagated by the congruence closure
core using a data structure that we will call an **E-graph following [8]**" —
[8] being Detlefs, Nelson and Saxe's *Simplify* (JACM 52(3), 2005).

Open `euf_enode.h` and the paper's architecture paragraph turns into fields:

```cpp
// z3 src/ast/euf/euf_enode.h, lines 40-65 — the enode, boolean flags elided
    40      class enode {
    41          expr*         m_expr = nullptr;
    50          bool          m_is_relevant = false;
    51          lbool         m_is_shared = l_undef;
    52          lbool         m_value = l_undef;        // Assignment by SAT solver for Boolean node
    53          sat::bool_var m_bool_var = sat::null_bool_var;    // SAT solver variable associated with Boolean node
    54          unsigned      m_class_size = 1;         // Size of the equivalence class if the enode is the root.
    56          unsigned      m_generation = 0;         // Tracks how many quantifier instantiation rounds were needed to generate this enode.
    57          enode_vector  m_parents;
    58          enode*        m_next   = nullptr;
    59          enode*        m_root   = nullptr;
    62          th_var_list   m_th_vars;
    63          justification m_justification;
```

Read it against the paper: `m_th_vars` (62) is "nodes in the E-graph may point to
one or more theory solvers"; `m_bool_var` and `m_value` (52–53) are the wire from
the SAT solver; `m_is_relevant` (50) is the relevancy-propagation paragraph;
`m_generation` (56) is the e-matching paragraph's instantiation rounds;
`m_justification` (63) is what makes conflicts explainable. egg's `EClass` has
none of these — because egg answers a different question.

**Difference 1: Z3 has no union-find path to walk.** `m_root` (59) points
directly at the class root, always, and `merge` maintains that eagerly:

```cpp
// z3 src/ast/euf/euf_egraph.cpp, lines 536-551 — union by class size, eager roots
   536          if (!r2->interpreted() &&
   537               (r1->class_size() > r2->class_size() || r1->interpreted() || r1->value() != l_undef)) {
   538              std::swap(r1, r2);
   539              std::swap(n1, n2);
   540          }
   542          remove_parents(r1);
   543          push_eq(r1, n1, r2->num_parents());
   545          for (enode* c : enode_class(n1))
   546              c->m_root = r2;
   548          r2->inc_class_size(r1->class_size());
   551          reinsert_parents(r1, r2);
```

Line 537 is **union by class size** (smaller class becomes `r1`), and 545–546
rewrites `m_root` for **every node in the smaller class**. The header comment
says as much: `euf_egraph.h:20`, "it still uses eager path compression."

**Work the cost.** Union by size means a node's root is rewritten only when the
class containing it at least doubles, so each node is rewritten at most `log₂ n`
times. Merging `n = 1000` singleton nodes into one class therefore costs at most
`1000 × log₂ 1000 ≈ 1000 × 9.97 ≈ 9,966` root writes **in total** — and every
`get_root()` (`euf_enode.h:203`) is a single load, forever, with no pointer chain
to walk.

Compare egg (`unionfind.rs:47-50`): `union` writes **one** pointer, and `find`
(`:30-35`) pays by walking a chain at read time. The two libraries chose opposite
sides of the same trade because their access patterns are opposite: Z3's SAT core
canonicalizes constantly during propagation and merges relatively rarely per
query, while egg merges in enormous batches and then reads in bulk at rebuild.

**Difference 2: everything must be undoable.** Step 1's non-chronological
backtracking means merges get retracted, in bulk. `push()` and `pop(unsigned)`
(`euf_egraph.h:277-278`) bracket scopes, `update_record` (`euf_egraph.h:112`) is
the trail entry, and `push_eq` at line 543 above records the pre-merge parent
count. Undo is the mirror image of merge:

```cpp
// z3 src/ast/euf/euf_egraph.cpp, lines 627-650 — undo_eq, traces elided
   627      void egraph::undo_eq(enode* r1, enode* n1, unsigned r2_num_parents) {
   628          enode* r2 = r1->get_root();
   630          r2->dec_class_size(r1->class_size());
   632          std::swap(r1->m_next, r2->m_next);
   633          auto begin = r2->begin_parents() + r2_num_parents, end = r2->end_parents();
   634          for (auto it = begin; it != end; ++it) {
   639              if (p->cgc_enabled())
   640                  erase_from_table(p);
   641          }
   643          for (enode* c : enode_class(r1))
   644              c->m_root = r1;
   649          r2->m_parents.shrink(r2_num_parents);
   650          unmerge_justification(n1);
   651      }
```

Line 643–644 is line 545–546 run backwards; line 649 truncates the parent vector
to the length recorded at merge time. **This is why Z3 cannot use egg's
union-find.** `find_mut`'s path halving (`unionfind.rs:37-44`) rewrites parent
pointers as a side effect of *reading*, and undoing those would require logging
every compressed pointer. Eager roots make undo a matter of re-walking one class
list; lazy roots with compression would make it a general-purpose trail of every
read. The backtracking requirement chose the data structure.

**Difference 3: justifications.** Every merge records *why*:

```cpp
// z3 src/ast/euf/euf_justification.h, lines 41-47 — the five reasons to merge
    41          enum class kind_t {
    42              axiom_t,
    43              congruence_t,
    44              external_t,
    45              dependent_t,
    46              equality_t
    47          };
```

A theory conflict has to be handed back as a *specific* clause naming the guilty
atoms — a lemma over `x ≤ 3` and `x ≥ 7`, not "something is wrong". So the
e-graph must be able to explain any derived equality in terms of asserted ones,
which is what `push_congruence` (`euf_egraph.cpp:765`) does by walking to the
least common ancestor of each argument pair. egg's equivalent, `explain.rs`, is
**optional** and off by default; in Z3 it is load-bearing.

### Step 6 — congruence repair: eager table, deferred merges

> **In:** Step 5's merge.
> **Out:** what Z3's worklist actually defers, and an honest reading of the
> source comment that cites egg.

The comment is real, and it is the first thing in the file:

```cpp
// z3 src/ast/euf/euf_egraph.h, lines 16-24 — the header's Notes block, verbatim
    16  Notes:
    17
    18      It relies on
    19      - data structures form the (legacy) SMT solver.
    20        - it still uses eager path compression.
    21
    22      NB. The worklist is in reality inherited from the legacy SMT solver.
    23      It is claimed to have the same effect as delayed congruence table reconstruction from egg.
    24      Similar to the legacy solver, parents are partially deduplicated.
```

Read line 22 before line 23. The worklist is **inherited from the legacy SMT
solver** — it predates egg — and line 23 says it "is **claimed** to have the same
effect", which is a careful hedge, not an adoption notice. (egg's own paper,
footnote 5, makes the matching claim from the other side: Z3's e-graph separates
read and write phases "as an implementation detail", and egg is "the first
algorithm to take advantage of this by deferring invariant maintenance.")

And in the code, what is deferred is narrower than egg's rebuilding.
`egraph::merge` repairs the congruence table **inline, in the same call** —
`remove_parents(r1)` at line 542 pulls the parents out of the hash table and
`reinsert_parents(r1, r2)` at 551 puts them back canonicalized. What gets queued
is the *consequences*:

```cpp
// z3 src/ast/euf/euf_egraph.cpp, lines 592-599 — a table collision becomes a queued merge
   592              if (p->cgc_enabled()) {
   593                  auto [p_other, comm] = insert_table(p);
   596                  if (p_other != p)
   597                      m_to_merge.push_back(to_merge(p_other, p, comm));
   598                  else
   599                      r2->m_parents.push_back(p);
   600                  if (p->is_equality())
```

Line 596–597 is the same discovery egg makes at `egraph.rs:1353` — a hash
collision *is* a congruence — but instead of recursing it appends to
`m_to_merge`, drained by a fixpoint loop:

```cpp
// z3 src/ast/euf/euf_egraph.cpp, lines 654-677 — the propagate fixpoint
   654      bool egraph::propagate() {
   656          unsigned i = 0;
   657          bool change = true;
   658          while (change) {
   659              change = false;
   660              propagate_plugins();
   661              for (; i < m_to_merge.size() && m.limit().inc() && !inconsistent(); ++i) {
   662                  auto const& w = m_to_merge[i];
   666                      merge(w.a, w.b, justification::congruence(w.commutativity(), m_congruence_timestamp++));
   675              }
   676          }
   677          m_to_merge.reset();
```

So the accurate statement is: **Z3 defers the cascading merges, egg defers the
table repair as well.** Both replace recursion with a worklist; egg additionally
lets the hashcons hold non-canonical keys between rebuilds
(`egraph.rs:63-64`), which Z3 does not do — its table is canonical at the end of
every `merge`, because a solver that must answer `get_root()` and produce a
conflict at any moment cannot afford a window in which its index is wrong.

### Step 7 — quantifiers: e-matching, and where triggers actually come from

> **In:** an axiom like `∀x. f(g(x)) = x`.
> **Out:** what the tool paper claims, what it does not, and the mechanism's
> real fragility.

A quantified axiom cannot be handed to CDCL: there are infinitely many instances.
The standard approach instantiates the axiom only for terms already present,
matched **modulo the equalities the e-graph currently knows** — that is
**e-matching**. The subterm pattern used to find candidates is a **trigger**
(here `f(g(x))`).

The paper's entire quantifier paragraph is three sentences:

> "Z3 uses a well known approach for quantifier reasoning that works over an
> E-graph to instantiate quantified variables. Z3 uses new algorithms that
> identify matches on E-graphs incrementally and efficiently. Experimental
> results show substantial performance improvements over existing
> state-of-the-art SMT solvers [4]."

Note what is absent. **The word "trigger" never appears in this paper.** Neither
does model-based quantifier instantiation, nor any description of how patterns
are selected or how the matching machine works. The "well known approach" is
Simplify's [8]; the "new algorithms" are reference [4], Bjørner and de Moura,
*Efficient E-Matching for SMT Solvers*, CADE 2007. Cite those, not this.

What you *can* verify is that the machine exists and predates the paper:

```cpp
// z3 src/ast/euf/euf_mam.h, lines 8-15 and 50 — the Matching Abstract Machine
     8  Abstract:
     9
    10      Matching Abstract Machine
    12  Author:
    14      Leonardo de Moura (leonardo) 2007-02-13.
    15      Nikolaj Bjorner (nbjorner) 2021-01-22.
    50      class mam {
```

The 2007 date on line 14 matches the CADE'07 paper; the 2021 date on line 15 is
the port into the new `euf` layer. This is egg's `machine.rs`, at industrial
scale and fourteen years older.

The fragility is worth stating plainly because it is the practical face of
"incomplete but useful": instantiation is heuristic. Too general a trigger floods
the solver with useless instances — and by the *Deleting clauses* paragraph of
Step 3, the useless ones get garbage-collected while the ones that produced
conflicts are kept, which is a mitigation, not a cure. Too specific a trigger
never fires and the needed fact is never derived, so the solver returns `unknown`
on a valid formula. Question 5 calls this the index-choice problem of SMT: the
same shape as picking which index to build, with the same failure modes at both
extremes.

### Step 8 — where a database meets Z3

> **In:** a solver that decides formulas.
> **Out:** three concrete uses, and the one inversion they all share.

- **Query equivalence** (Cosette, topic 16): compile two SQL plans to formulas
  and ask whether their outputs can differ. `unsat` means equivalent.
- **Constraint-based test generation**: "give me a row that makes this `WHERE`
  clause true" is literally a satisfiability query, and the paper's *Pex* client
  (§2) is exactly this pattern for unit tests — "Z3 is used to produce new test
  cases with different behavior."
- **Optimizer rule soundness**: the `div-same` rewrite `(/ ?x ?x) => 1`, which
  this topic's stub suggests you add to the saturating lane
  (`experiments/src/eqsat.rs:87`, in the doc comment on `egg_optimize`), is
  checkable. Assert `x = 0 ∧ (x/x ≠ 1)` and ask whether it is satisfiable. Over
  the integers with SMT-LIB's *total* `div`, `(div 0 0)` is an
  arbitrary-but-fixed value rather than an error, so the query is satisfiable and
  the rule is unsound as written; over the reals with the same totalisation
  convention, likewise. The rule is still fine for the trap expression, where it
  only ever fires on the literal `(/ 2 2)` — but that is a fact about the input,
  not about the rule, and a solver is how you find out which one you have.

The usage pattern is always the same inversion: encode "a counterexample exists"
and hope for `unsat`. **The solver's failure to satisfy is your proof** — which
is also why an `unknown` from a quantified query (Step 7) is not a proof of
anything.

## How to read the paper (with the concepts in hand)

Four pages, LNCS 4963 pp. 337–340. Read all of it in twenty minutes, then spend
the afternoon in `src/ast/euf/`.

- **§1 Introduction** — the adoption facts, which are the point of a tool paper:
  a prototype won **4 first places and 7 second places at SMT-COMP'07**; first
  external release **September 2007**; in use at Microsoft since **February 2007**
  in Spec#/Boogie, Pex, HAVOC, Vigilante, VCC and Yogi. Note the sentence "Z3
  uses novel algorithms for quantifier instantiation [4] and theory combination
  [5]" — that is the paper telling you where its own content is not.
- **§2 Clients** — Spec#/Boogie and Pex. Three textual input formats (SMT-LIB,
  Simplify, a native DIMACS-like one) and three APIs (ANSI C, .NET, OCaml).
- **§3 System Architecture** — the figure and the per-component paragraphs of
  Step 3. Read *Theory Combination* twice; it is the paragraph most often
  misremembered (Step 4).
- **§4 Conclusion** — four sentences.

Then read the code in this order: the `Notes:` block at `euf_egraph.h:16-24`
(Step 6), `euf_enode.h:40-69` (Step 5's field-by-field map to the paper),
`egraph::merge` at `euf_egraph.cpp:511`, `undo_eq` at `:627`, and
`propagate` at `:654`.

## Questions (answer in notes.md)

1. Why must Z3's e-graph carry justifications while egg's `explain.rs` is
   optional and off by default? Name the specific output that needs them, and
   estimate what always-on proof production would cost egg's `rebuild`.
2. Path compression versus eager roots: state the read/write cost of each
   (`unionfind.rs:30-50` against `euf_egraph.cpp:536-551`), then explain in one
   sentence why `undo_eq` (`:627-651`) would be impractical if Z3 used egg's
   `find_mut`.
3. Recompute Step 5's arithmetic for `n = 10⁶` nodes. How many root writes, worst
   case, and how many pointer dereferences does a `get_root()` cost after them?
   Do the same for egg's `find` on a maximally unbalanced forest.
4. Encode the `x/x → 1` soundness check as an SMT query, first over `Int` and
   then over `Real`. Which theory answers each, and what does SMT-LIB's
   totalisation of division do to your answer?
5. Trigger selection is the index-choice problem of SMT. Write out both failure
   modes with a concrete axiom, and say which one the *Deleting clauses*
   paragraph partially mitigates and which it does not.
6. The paper says Z3 "incrementally reconciles models maintained by each theory"
   rather than exchanging all implied equalities. Name one cost Nelson–Oppen pays
   that model-based combination avoids, and one risk model-based combination
   takes that Nelson–Oppen does not.

## Done when

Answer each before unfolding it.

- [ ] You can state what the TACAS'08 paper does and does not contain, and name three things commonly misattributed to it.

  <details><summary>Answer</summary>

  It is a **four-page tool paper** (LNCS 4963, 337–340) announcing Z3's first
  external release: clients, an architecture figure, one paragraph per component,
  adoption facts. It contains no algorithms.

  Commonly misattributed: **DPLL(T) internals** (the term appears once, inside
  the *Relevancy propagation* paragraph, describing a class of solvers);
  **e-matching / quantifier instantiation** (reference [4], Bjørner and de Moura,
  CADE 2007 — and the word "trigger" never appears); **theory combination**
  (reference [5], *Model-based Theory Combination*, SMT 2007). Also frequently
  mis-said: "CDCL", which the paper never writes — it names two-watch literals,
  lemma learning using conflict clauses, phase caching and non-chronological
  backtracking.

  </details>

- [ ] You can explain what Z3 does instead of Nelson–Oppen, and quote the paper on it.

  <details><summary>Answer</summary>

  Nelson–Oppen has theories exchange **equalities between shared terms**, and
  requires each solver to produce all the equalities it implies. The paper's
  *Theory Combination* paragraph rejects that: "Traditional methods for combining
  theory solvers rely on capabilities of the solvers to produce all implied
  equalities or a pre-processing step that introduces additional literals into
  the search space. Z3 uses a new theory combination method that **incrementally
  reconciles models maintained by each theory** [5]."

  Reference [5] is *Model-based Theory Combination* (SMT 2007): each theory keeps
  a candidate model, equalities are *guessed* from variables that happen to be
  assigned equal values, and wrong guesses are repaired. Search where
  Nelson–Oppen deduces. The two costs it names as avoided are producing all
  implied equalities and introducing extra literals.

  </details>

- [ ] You can map at least four fields of `euf::enode` onto sentences of the paper's architecture section.

  <details><summary>Answer</summary>

  From `euf_enode.h:40-65`: `m_th_vars` (62) ↔ "Nodes in the E-graph may point to
  one or more theory solvers … the merge is propagated as an equality to the
  theory solvers in the intersection of the two sets"; `m_bool_var` (53) and
  `m_value` (52) ↔ "The congruence closure core receives truth assignments to
  atoms from the SAT solver"; `m_is_relevant` (50) ↔ the *Relevancy propagation*
  paragraph; `m_generation` (56, "how many quantifier instantiation rounds were
  needed to generate this enode") ↔ the E-matching paragraph; `m_justification`
  (63) ↔ the conflict clauses that flow back to the SAT solver.

  egg's `EClass` has none of these, which is the compact statement of "same data
  structure, different contract".

  </details>

- [ ] You can compute the cost of Z3's eager root maintenance and say why it, rather than egg's, is the right choice here.

  <details><summary>Answer</summary>

  `merge` swaps so the smaller class is `r1` (`euf_egraph.cpp:536-540`, union by
  `class_size`) and then rewrites `m_root` for every node in it (545–546). Union
  by size means a node is rewritten only when its class at least doubles, so at
  most `log₂ n` times: merging `n = 1000` singletons costs at most
  `1000 × log₂ 1000 ≈ 9,966` root writes **in total**, and every `get_root()`
  is one load with no chain to walk.

  egg does the opposite: `union` writes one pointer (`unionfind.rs:47-50`) and
  `find` walks (`:30-35`). Z3 needs O(1) reads because the SAT core canonicalizes
  constantly during propagation, and — decisively — because `undo_eq`
  (`euf_egraph.cpp:643-644`) restores roots by re-walking one class list. With
  egg's `find_mut` path halving (`unionfind.rs:37-44`), *reading* mutates the
  forest, so undo would need a trail entry per compressed pointer. Backtracking
  chose the data structure.

  </details>

- [ ] You can say precisely what Z3's worklist defers and how that differs from egg's rebuilding, and read the egg-citing comment correctly.

  <details><summary>Answer</summary>

  Z3's `merge` repairs the congruence table **inline**: `remove_parents(r1)`
  (`euf_egraph.cpp:542`) then `reinsert_parents(r1, r2)` (`:551`). What it queues
  is the *consequential merges* — a table collision at `:596-597` pushes onto
  `m_to_merge`, drained by the fixpoint at `:654-677`. egg defers the table
  repair as well, so its hashcons holds non-canonical keys between rebuilds
  (`egraph.rs:63-64`); Z3's table is canonical when `merge` returns, because a
  solver must be able to answer `get_root()` and produce a conflict at any
  instant.

  The comment: `euf_egraph.h:22` says the worklist "is in reality **inherited
  from the legacy SMT solver**" and `:23` that it "is **claimed** to have the same
  effect as delayed congruence table reconstruction from egg." That is a hedged
  note of resemblance, not "Z3 adopted egg's algorithm". egg's paper footnote 5
  makes the mirror-image claim from its side.

  </details>

- [ ] You can explain e-matching and trigger selection, and say which paper to cite for it.

  <details><summary>Answer</summary>

  A quantified axiom has infinitely many instances, so instead of case-splitting
  it, instantiate it only for terms already in the e-graph that match a **trigger**
  — a subterm pattern — **modulo the equalities currently known**, which is what
  makes it *e*-matching rather than plain matching.

  Cite **Bjørner and de Moura, *Efficient E-Matching for SMT Solvers*, CADE 2007**
  (the TACAS paper's reference [4]) and Simplify (reference [8]) for the "well
  known approach". The tool paper says only that Z3 "uses new algorithms that
  identify matches on E-graphs incrementally and efficiently" and never uses the
  word "trigger". The machine is `src/ast/euf/euf_mam.h` — "Matching Abstract
  Machine", authored 2007-02-13, ported into `euf` in 2021.

  Failure modes: too general a trigger floods the solver with instances (partly
  mitigated by the clause GC of the *Deleting clauses* paragraph, which keeps
  only instantiations that produced conflicts); too specific never fires, and the
  solver returns `unknown` on a valid formula — which is not a proof of anything.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the `x/x → 1` encoding over both sorts.

  <details><summary>Answer</summary>

  The shape to check yours against: assert `(and (= x 0) (not (= (div x x) 1)))`
  over `Int`. SMT-LIB makes division **total** — `(div a 0)` is an
  uninterpreted-but-fixed value rather than an error — so the solver can satisfy
  this, and the rewrite `x/x → 1` is unsound at `x = 0` unless your language
  guarantees a non-zero divisor. `Real` behaves the same way for the same reason.
  The theory answering it is linear arithmetic plus EUF for the totalised
  division symbol.

  Record what this tells you about the `div-same` rule the stub suggests
  (`experiments/src/eqsat.rs:87`): it is fine for the trap expression, where the
  only redex is the literal `(/ 2 2)`, and it is not a rule you could ship in a
  real optimiser without a non-zero side condition.

  </details>

## References

**Papers**
- Leonardo de Moura, Nikolaj Bjørner — *Z3: An Efficient SMT Solver*, TACAS 2008,
  LNCS 4963, pp. 337–340. Four pages. The architecture figure of Step 3 is the
  payload; the *Theory Combination* paragraph is Step 4.
- Leonardo de Moura, Nikolaj Bjørner — *Model-based Theory Combination*, SMT 2007
  (the tool paper's [5]) — what Z3 does **instead of** Nelson–Oppen.
- Nikolaj Bjørner, Leonardo de Moura — *Efficient E-Matching for SMT Solvers*,
  CADE 2007, LNCS 4603, pp. 183–198 (the tool paper's [4]) — Step 7's actual
  source.
- David Detlefs, Greg Nelson, James B. Saxe — *Simplify: a theorem prover for
  program checking*, JACM 52(3), 2005 (the tool paper's [8]) — where the term
  "E-graph" and the trigger-based approach come from.
- Bruno Dutertre, Leonardo de Moura — *A Fast Linear-Arithmetic Solver for
  DPLL(T)*, CAV 2006 (the tool paper's [9]) — the Yices algorithm Z3's arithmetic
  solver is based on.
- de Moura, Bjørner — *Relevancy Propagation*, MSR-TR-2007-140 — the don't-care
  algorithm the *Relevancy propagation* paragraph points at.

**Code** — `Z3Prover/z3` at `1d425e5`

| File | Lines | What |
|------|-------|------|
| `src/ast/euf/euf_egraph.h` | 382 | `class egraph` (85), the `to_merge` queue (91–100), scopes (277–278), the `Notes:` block citing egg (16–24) |
| `src/ast/euf/euf_egraph.cpp` | 1120 | `merge` (511), `remove_parents` (563), `reinsert_parents` (585), `undo_eq` (627), `propagate` (654), `push_congruence` (765) |
| `src/ast/euf/euf_enode.h` | 310 | `class enode` (40) — the field-by-field map to the paper's architecture section |
| `src/ast/euf/euf_etable.h` | — | the congruence table: `cg_hash`/`cg_eq` (109, 113), `insert` (166) with its commutativity note |
| `src/ast/euf/euf_justification.h` | 143 | `kind_t` (41–47) — the five reasons two nodes are equal |
| `src/ast/euf/euf_mam.h` | 85 | the Matching Abstract Machine, `class mam` (50) |

**In this topic**
- [reading-egg-popl21.md](reading-egg-popl21.md) — the same data structure with
  the opposite contract; read it first.
- `experiments/src/eqsat.rs:82-98` — the stub's suggested rewrite list, including
  the `div-same` rule Step 8 puts under a solver.
