# Z3 & Cosette: testing every input at once

Everything else in this topic samples the input space; SMT quantifies
over it — "does there EXIST a row where these two plans disagree?"
UNSAT means the rewrite is proven for all databases. Before the code
and the papers, this chapter builds the machinery step by step — SAT,
the CDCL loop, theory solvers, tactics — then applies it
Cosette-style to verify our topic-10 rewrite rules. Read Z3 the way
PLAN.md says to: as a masterclass high-performance search engine over
LOGIC whose architecture rhymes with a query engine.

**Scope, and the split with topic 21.** Z3's internals — the CDCL(T)
loop, Nelson–Oppen theory combination, the e-graph, E-matching — are
topic 21's subject and are walked at length in
[topics/21-formal/reading-z3-tacas08.md](../21-formal/reading-z3-tacas08.md).
This chapter is about *using* a solver as a test oracle: Step 1
compresses the architecture to the parts you must hold to read an
`unsat`, and Steps 2 onward are all encoding, which topic 21 does not
cover. If a claim here about Z3's insides feels thin, that is
deliberate — the deep version is one link away.

Every code anchor is `Z3Prover/z3` at commit **`1d425e5`**, the
revision this repo pins in `resources/codebases.md`, with the line
numbers it occupies there.

## The problem in one sentence

A fuzzer that runs 10 million random rows through two query plans
still checks a measure-zero slice of the input space; encoding both
plans as logic and asking a solver "∃ row where they disagree?"
checks ALL rows at once — and returns either a proof or the exact
counterexample row, usually in milliseconds.

## The concepts, step by step

### Step 1 — SAT, CDCL, SMT, tactics: the four ideas you need to read an answer

> **In:** a formula over booleans and theory atoms like `x < 3`.
> **Out:** `sat` with a model, `unsat`, or `unknown` — and you need
> to know why each is possible.

**SAT** (boolean satisfiability): given a formula over true/false
variables, is there an assignment making it true? A SAT **solver**
is a search engine over the `2^n` assignments. Flip the answer around
and you get verification: to prove a property P holds always, ask
the solver for a case where `NOT P` holds. **UNSAT** ("no satisfying
assignment exists") is then a proof; **SAT** hands you a concrete
counterexample. That inversion — prove by failing to find — is the
whole chapter.

**CDCL** (conflict-driven clause learning) is the algorithm inside:
guess a variable (**decide**), push the consequences (**propagate**),
and on a contradiction (**conflict**) analyze why, record the reason
as a **learned clause** so the dead end is never re-entered, and
**backjump** past the guesses the conflict proved irrelevant.

**SMT** (satisfiability modulo theories) keeps the CDCL engine and
attaches **theory solvers**, each a decision procedure for one domain
(linear arithmetic, bitvectors, arrays, uninterpreted functions,
strings). The SAT core treats `x < 3` as an opaque boolean; when it
proposes `x < 3 ∧ x > 5`, the arithmetic theory vetoes with an
explanation that becomes a learned clause. Theory propagation is
predicate pushdown into specialized engines — same shape as topic 10.

**Tactics** are the fourth idea, and the one that most rewards a
look at the source, because it is where the query-engine analogy
stops being an analogy. Z3 doesn't run one fixed algorithm; it
composes goal transformers into pipelines, chosen by **probes** that
inspect the formula first:

```c
// src/tactic/portfolio/default_tactic.cpp — mk_default_tactic, 36-55 (elided)
    36  tactic * mk_default_tactic(ast_manager & m, params_ref const & p) {
    37      tactic * st = using_params(and_then(mk_simplify_tactic(m, p),
    38                                          cond(mk_and(mk_is_propositional_probe(), mk_not(mk_produce_proofs_probe())),
    39                                               mk_lazy_tactic(m, p, [&](auto& m, auto const& p) { return mk_fd_tactic(m, p); }),
    40                                          cond(mk_is_qfbv_probe(),    ... mk_qfbv_tactic ...
    42                                          cond(mk_is_qflia_probe(),   ... mk_qflia_tactic ...
// ... 41, 43-50: qfaufbv, qfauflia, qflra, qfnra, qfnia, lira, nra, qffp, qffplra ...
    52                                               and_then(mk_preamble_tactic(m), mk_lazy_tactic(m, p, [&](auto& m, auto const& p) { return mk_smt_tactic(m, p);}))))))))))))))),
    53                                 p);
    54      return st;
    55  }
```

Fifty-six lines, and twelve `cond(probe, specialised_tactic)`
branches at `:38-50` before the general fallback at `:52`. Every
branch is "if the formula is in *this* fragment, use the engine built
for it". `mk_is_qflia_probe()` at line 42 detects quantifier-free
linear integer arithmetic — the fragment Step 3's encoding lands in —
and dispatches to a solver that will not pay for anything QF_LIA
doesn't need. `mk_lazy_tactic` means the specialised tactic isn't
even *constructed* unless its probe fires.

That is parse → rewrite (`mk_simplify_tactic`, line 37) →
cost-informed dispatch (the probe cascade) → execute. A planner
dispatching on statistics has the same shape, and probes are its
cardinality estimation.

Why it matters for *this* topic: the twelve branches are the reason
Step 3's encoding is fast. Land in a named fragment and you get a
specialist; land outside one and you get `mk_smt_tactic`, the general
engine, which is where `unknown` answers come from.

For the CDCL(T) loop itself, the e-graph, Nelson–Oppen and
E-matching, go to
[topics/21-formal/reading-z3-tacas08.md](../21-formal/reading-z3-tacas08.md)
— that chapter reads `src/ast/euf/euf_egraph.h` and `euf_mam.h` line
by line. Don't duplicate the work.

### Step 2 — the API surface is three calls

> **In:** a formula you have built.
> **Out:** `l_true` / `l_false` / `l_undef`, and — on `l_true` — a
> model.

You do not need to understand Z3 to use Z3. The entire testing
interface is visible in one header:

```c
// src/solver/solver.h — the assert / check surface, 124 and 177-183 (elided)
   124      void assert_expr(expr* f);
// ... 126: assert_expr_core, the virtual each backend implements ...
// ... 128-130: the vector overload, a loop over the scalar one ...
   177      lbool check_sat(unsigned num_assumptions, expr * const * assumptions);
// ... 179-181: expr_ref_vector / app_ref_vector convenience overloads ...
   183      lbool check_sat() { return check_sat(0, nullptr); }
```

Push formulas with `assert_expr`, ask with `check_sat`. The return
type is `lbool`, a **three**-valued boolean — `l_true` (sat),
`l_false` (unsat), `l_undef` (unknown: resource limit, timeout, or a
fragment Z3 cannot decide). The doc comment at `:172-174` names the
other half of the contract: on unsat with core generation enabled,
"the unsat-core is a subset of these assumptions" — which is how you
find out *which* of your assumptions did the proving.

Do not skip `l_undef`. Line it up against the topic's other oracles:

```
 crash_matrix (this topic):  bug found / not found this seed
                             "not found" ≠ "not present"
 TLP:                        partitions reconcile / don't
 Z3 check_sat:               l_false = PROVEN for all inputs
                             l_true  = counterexample in hand
                             l_undef = you learned nothing

 A harness that treats l_undef as l_false silently converts
 "the solver gave up" into "the rewrite is correct."
```

Why it matters: this is the only failure mode in this chapter that
is *silent*, and it is one `if` away in every solver harness anyone
writes.

### Step 3 — symbolic rows: encoding a query plan as a formula

> **In:** two filter chains you believe are equivalent.
> **Out:** one formula whose unsatisfiability is a proof over every
> row that could ever exist.

To verify a rewrite rule, replace concrete data with one **symbolic
row** — a tuple of solver variables, one per column — and compile
each plan's filter chain into a formula over it. Then ask the Step 1
question:

```
 symbolic row: (a: Int, b: Int, a_null: Bool, b_null: Bool)
 P1 = compile(plan1's filter chain)   — a formula
 P2 = compile(plan2's filter chain)
 ask Z3: ∃ row. P1(row) ≠ P2(row)
   UNSAT → rewrite proven for all rows
   SAT   → the model IS the counterexample row
```

```text
// ILLUSTRATION — the shape of the harness you write in this topic's
// z3 rewrite exercise. Not quoted from Z3; the C++ calls it bottoms
// out in are src/solver/solver.h:124 (assert_expr) and :177 (check_sat).
let a  = Int::fresh("a");  let a_null = Bool::fresh("a_null");
let b  = Int::fresh("b");  let b_null = Bool::fresh("b_null");
let row = Row { a, a_null, b, b_null };

let p1 = compile(plan_before, &row);   // Kleene 3-valued AND/OR/NOT/cmp
let p2 = compile(plan_after, &row);

match solver.check(p1.keeps_row().xor(p2.keeps_row())) {
    Unsat  => Proven,                  // no row distinguishes the plans
    Sat(m) => Counterexample(m),       // the model IS the failing row
    Unknown => Inconclusive,           // NOT Proven — see Step 2
}
```

Now the arithmetic that justifies the whole approach. Compare
exhaustively testing a two-column filter against solving it:

```
 columns: a, b — each a 64-bit nullable integer
 rows to enumerate = (2^64 + 1)^2 ≈ 3.4 × 10^38

 at crash_matrix's measured rate (this topic's notes.md baseline):
   ≈ 200,000 harness runs per second
   3.4 × 10^38 / 2 × 10^5 ≈ 1.7 × 10^33 s ≈ 5 × 10^25 years

 the same question as one QF_LIA query: one check_sat, milliseconds.

 and a fuzzer that samples 10^7 of those rows covers
   10^7 / 3.4 × 10^38 ≈ 3 × 10^-32 of the space — measure zero.
```

That is not a speedup, it is a change of category: the solver never
enumerates rows, it reasons about the *constraint* the rows satisfy.

One subtlety keeps this tractable: filters are row-at-a-time pure
logic, so one symbolic row quantifies over all databases. There is
no "for all rows" quantifier in the formula — the universal
quantification is in the *interpretation* ("a free variable stands
for an arbitrary value"), not in the syntax. That matters because
quantifier-free formulas are Z3's fast path: they land in Step 1's
`mk_is_qflia_probe` branch (`default_tactic.cpp:42`) instead of the
general `mk_smt_tactic` fallback at `:52`, and they avoid E-matching
entirely — which is where topic 21's `euf_mam.h` and the
trigger-selection heuristics come in, and where `l_undef` starts
appearing.

Why it matters: "no quantifiers needed" is the single design
decision that makes a solver a practical test oracle rather than a
research project.

### Step 4 — the NULL trap: encode SQL's three-valued logic honestly

> **In:** a nullable column.
> **Out:** two solver variables, not one — and an operator table
> you have to write out.

SQL predicates evaluate to TRUE, FALSE, or NULL ("unknown"), and
`WHERE` keeps only TRUE — so a two-valued encoding proves rewrites
that are false in real SQL. The honest encoding: each nullable
column becomes a pair `(value, is_null)`, and AND/OR/NOT/comparison
are defined per SQL's Kleene semantics. Write the tables out once;
they are the specification:

```
 NOT:   T→F    F→T    N→N

 AND    T  F  N          OR     T  F  N
   T    T  F  N            T    T  T  T
   F    F  F  F            F    T  F  N
   N    N  F  N            N    T  N  N

 note the two asymmetries that catch people:
   F AND N = F   (falsity is absorbing — you need not know the other side)
   T OR  N = T   (truth is absorbing)
 a two-valued encoder gets both of these wrong in the direction
 of "propagate the unknown", which is a STRICTER filter than SQL's —
 so it proves rewrites SQL does not satisfy.
```

Encoding cost, so you know what you are buying:

```
 n nullable columns
   two-valued encoding:   n solver variables
   honest encoding:       2n variables + 1 is_null term per comparison
   blow-up:               2×  in variables, ~2× in formula size

 for the 2-column filter of Step 3 that is 4 variables instead of 2.
 QF_LIA solves both in milliseconds. There is no reason to cheat.
```

This is the trap AND the point: most real optimizer bugs — TLP's
bread and butter, [reading-pqs-tlp-papers.md](reading-pqs-tlp-papers.md)
— are exactly NULL-semantics violations, and Z3 finds them as SAT
models in milliseconds. Note the pleasing symmetry with TLP: TLP
partitions on `p` / `NOT p` / `p IS NULL` because SQL has three
truth values; the solver encoding needs `(value, is_null)` for the
same reason. Same fact, two techniques.

Why it matters: an encoder that is wrong here does not fail loudly.
It returns `unsat` — a *proof* — of something false.

### Step 5 — Cosette: the full SQL-equivalence prover

> **In:** two arbitrary SQL queries, not two filter chains.
> **Out:** a counterexample, a proof, or neither — from two
> different engines, because no single one can do both.

Cosette answers "are Q1 and Q2 equivalent for ALL databases?" — the
general problem, beyond single-row filters. Start with the fact that
frames it, which the paper states outright: query equivalence for
arbitrary SQL is **undecidable**, "so an automated proof system for
SQL will never be complete." Everything about Cosette's architecture
follows from that.

It compiles SQL to **K-relations**: a relation is a *function from
tuple to multiplicity*, so bag semantics (SQL tables are bags, not
sets) fall out of the algebra rather than being bolted on. Union is
addition of multiplicities, join is multiplication, selection
multiplies by a 0/1 indicator.

Then it splits — and the split is **by outcome, not by difficulty**,
which is the part everyone gets backwards:

```
 constraint solver (Rosette)   can only ever DISPROVE
                               → finds counterexamples
 proof assistant (Coq)         can only ever PROVE
                               → establishes equivalences

 neither can do the other's job. Running both is not a
 "fast path / slow path" — it is two half-oracles.
```

The solver half is bounded, which is the honest reading of this
chapter's title. §3.1: `Tuple := List<Integer>`, `Relation :=
List<Pair<Tuple, Integer>>`; strings are modelled as integers and
floats are unsupported; symbolic relations are **fixed-size** lists
of symbolic values (including symbolic multiplicities), grown by
incremental solving. That is bounded model checking: "testing every
input at once" holds *up to the current bound on relation size*, not
absolutely. Step 3's single-symbolic-row encoding is the degenerate
case where the bound is 1 and therefore exact — because a row filter
cannot see other rows.

The implementation is small enough to be encouraging: about **3k
lines of Rosette and 2k lines of Coq** (Rosette 2.2, Coq 8.5pl1).
And the results are honest about the split: §6 reports that the
solver found counterexamples for every query in the Bugs, Exams and
XData benchmarks it was pointed at, while on the Rules benchmark of
23 known-equivalent rewrite rules Coq **automatically proved 17**
(7 of them via the `CQSolve` tactic) and the remaining 6 needed
human interaction.

```
 Rules benchmark, §6:
   23 rewrite rules known to be equivalent
   17 proved automatically      → 17 / 23 = 73.9%
    6 needed interactive proof  →  6 / 23 = 26.1%

 read that as the price list for the general problem. Step 3's
 filter rules are in the 73.9% — and in fact below it, since a
 quantifier-free single-row encoding needs no proof assistant at all.
```

Our use is the SMT half: filters and projections over symbolic rows,
exactly Steps 3–4, which is all topic 10's rewrite rules need.

Why it matters: knowing that the general problem is undecidable is
what stops you trying to build Cosette. Knowing that *your* fragment
is quantifier-free and single-row is what tells you your version is
a weekend.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| `src/solver/solver.h:58` | 1 | `class solver : public check_sat_result` — the public shape |
| `src/solver/solver.h:124` | 2 | `assert_expr` — push a formula onto the assertion stack |
| `src/solver/solver.h:172-174` | 2 | the unsat-core contract, in the doc comment |
| `src/solver/solver.h:177-183` | 2 | `check_sat` and its overloads; returns three-valued `lbool` |
| `src/smt/smt_context.h:89` | 1 | `class context` — the CDCL(T) core loop (1,980 lines; topic 21 reads it) |
| `src/tactic/tactic.h:34` | 1 | `class tactic` — composable goal transformers |
| `src/tactic/portfolio/default_tactic.cpp:36-55` | 1 | twelve `cond(probe, tactic)` branches, then the `smt` fallback |
| `src/tactic/portfolio/smt_strategic_solver.cpp` | 1 | tactic → solver bridge |
| `src/ast/` | — | hash-consed terms (one node per distinct expr — topic 2's interning) |
| `src/smt/mam.cpp` | — | the matching abstract machine for quantifier triggers — 4,042 lines you do **not** need for Step 3's quantifier-free encoding; topic 21 covers its modern sibling `src/ast/euf/euf_mam.h` |

Reading order for *this* topic: `solver.h` for the three calls in
Step 2, then `default_tactic.cpp` in full — it is 56 lines and it is
the query-planner analogy made literal. Stop there. `smt_context.h`
and the e-graph belong to
[topics/21-formal/reading-z3-tacas08.md](../21-formal/reading-z3-tacas08.md);
reading them twice is not twice as useful.

## Questions for notes.md

1. TACAS '08: what does Z3 do with quantifiers (E-matching +
   triggers via mam.cpp), and why do DB rewrite proofs mostly avoid
   needing them (finite row schemas → quantifier-free)?
2. Hash-consing in src/ast: same trick as our string interning
   (topic 2) and Arrow dictionary encoding — what operation becomes
   O(1) pointer compare?
3. Encode `WHERE NOT (a = b)` vs `WHERE a <> b` over nullable a, b
   in Kleene logic — equivalent or not? (Do it on paper, then check
   what Z3 says in the z3 rewrite exercise.)
4. Why does Cosette need K-relations (bags) rather than sets — which
   standard rewrite is set-valid but bag-INVALID? (DISTINCT
   pushdown...)
5. For M16: our two topic-10 rules to verify — filter reordering
   (commute σ_p σ_q) and filter-past-projection. Write the symbolic
   encoding for each; which needs the (value, is_null) pair and
   which doesn't?

## Done when

Answer each before unfolding it.

- [ ] You can explain CDCL as a search loop that learns, and say what a learned clause is.

  <details><summary>Answer</summary>

  Decide (guess a variable's value) → propagate (push forced
  consequences) → conflict (some clause is now falsified) → analyze
  the conflict to find the subset of decisions responsible → record
  their negation as a **learned clause** → backjump past every
  decision the analysis proved irrelevant, not merely the last one.

  A learned clause is a fact derived from the input formula that was
  not stated in it — logically redundant, operationally decisive,
  because it prunes that entire region of the search space forever.
  It is a materialized negative result, and the reason CDCL beats
  brute force by orders of magnitude on structured formulas.

  The database rhyme: adaptive execution with feedback. The full
  treatment, with Z3's actual data structures, is in
  [topics/21-formal/reading-z3-tacas08.md](../21-formal/reading-z3-tacas08.md).

  </details>

- [ ] You can state the SAT/theory division of labour: SAT proposes, theories veto.

  <details><summary>Answer</summary>

  The CDCL core sees only a **boolean skeleton**: `x < 3` and `x > 5`
  are two opaque propositional variables. It proposes an assignment
  making the skeleton true.

  Each theory solver then checks its own atoms for consistency in its
  domain. Linear arithmetic looks at `x < 3 ∧ x > 5`, declares it
  infeasible, and hands back an **explanation** — a minimal
  inconsistent subset — which becomes a learned clause in the core.
  The core backjumps and proposes differently.

  So the theory never searches and the core never does arithmetic.
  Theory propagation — a theory *deducing* an atom's value rather
  than merely rejecting an assignment — is the same move as predicate
  pushdown in topic 10: send the constraint to the engine that can
  evaluate it cheaply, instead of filtering after the fact.

  </details>

- [ ] You can encode a small query plan as symbolic rows and say what the formula asserts.

  <details><summary>Answer</summary>

  One symbolic row: a fresh solver variable per column (plus an
  `is_null` companion per nullable column, Step 4). Compile each
  plan's filter chain into a boolean formula over those variables —
  `P1(row)`, `P2(row)` — each meaning "this plan keeps this row".
  Assert `P1 XOR P2` and call `check_sat`.

  The formula asserts *there exists a row on which the two plans
  disagree*. `l_false` (unsat) means no such row exists in the entire
  domain of the variables, which is a proof of equivalence for every
  possible database. `l_true` means the model is literally the
  counterexample row — print it and you have a bug report. `l_undef`
  means you learned nothing (Step 2).

  The universal quantification is in the interpretation of a free
  variable, not in the syntax, so the formula stays quantifier-free
  and lands in `default_tactic.cpp:42`'s QF_LIA branch.

  </details>

- [ ] You can encode `WHERE NOT (a = b)` against `WHERE a <> b` over nullable columns and show where they differ.

  <details><summary>Answer</summary>

  They are equivalent — and that is the *interesting* answer, because
  the naive worry is wrong for a reason worth naming.

  Under Kleene semantics with `a` NULL: `a = b` evaluates to NULL,
  `NOT NULL` is NULL, and `WHERE` drops the row. `a <> b` also
  evaluates to NULL, and `WHERE` drops the row. Same on both sides;
  the row is dropped either way. With both non-null the two are
  ordinary boolean negations of each other. So the encoding gives
  `unsat`.

  Where the equivalence *does* break is the moment the predicate
  stops being a top-level `WHERE`: put it inside `NOT EXISTS`, a
  `CHECK` constraint, or a `CASE`, and the difference between "NULL"
  and "not TRUE" becomes observable. Which is the general lesson:
  Kleene expressions are only interchangeable relative to a *context*
  that collapses NULL and FALSE the same way. `WHERE` does. Not
  everything does.

  Run it and check rather than trusting this paragraph — that is what
  the exercise is for.

  </details>

- [ ] You can say why Cosette needs bags (K-relations) rather than sets, and which SQL feature forces it.

  <details><summary>Answer</summary>

  Because SQL tables are bags: `SELECT` without `DISTINCT` preserves
  duplicates, and `UNION ALL` adds multiplicities. A K-relation makes
  that primitive — a relation is a function from tuple to
  multiplicity, so union is addition and join is multiplication.

  The rewrite that forces it: pushing `DISTINCT` (or dropping it) is
  valid under set semantics and invalid under bag semantics, as is
  any rule that changes how many times a row is produced —
  reassociating a join that duplicates rows, or eliminating a
  self-join. A set-semantics prover would happily "prove" those
  correct.

  This is the same gap this topic keeps finding from the other
  direction: SQLancer's TLP comparator checks size then `HashSet`
  equality (`ComparatorHelper.java:91, 108-112`) and turso's checks
  size then two-way containment
  (`generation/property.rs:1138, 1146-1177`) — both blind to
  multiplicity. Cosette is the one tool in this topic that gets bags
  right by construction, and that is precisely because it is doing
  algebra rather than comparing outputs.

  </details>

- [ ] You can say what `l_undef` means and why a solver harness that ignores it is worse than no harness.

  <details><summary>Answer</summary>

  `check_sat` returns `lbool`, three-valued (`src/solver/solver.h:177`):
  `l_true` = sat (counterexample found), `l_false` = unsat (proof),
  `l_undef` = the solver stopped without deciding — a timeout, a
  resource limit, or a fragment outside any decision procedure (which
  in practice means quantifiers, incomplete theory combinations, or
  nonlinear arithmetic).

  A harness that writes `if result != Sat { report_proven() }` turns
  every timeout into a proof. That is worse than no harness, because
  it produces *false confidence* rather than no confidence — and
  unlike a fuzzer that finds nothing, it emits a green check.

  The defence is Step 3's design, not a bigger timeout: keep the
  encoding quantifier-free and in a named fragment so it hits the
  probe cascade at `default_tactic.cpp:38-50` rather than the general
  `mk_smt_tactic` fallback at `:52`. And treat `l_undef` as a test
  failure that must be investigated, exactly like a flaky test.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the two topic-10 rewrite rules you would verify.

  <details><summary>Answer</summary>

  No unfoldable answer — this one is the writing. For question 5, the
  useful observation is that *both* rules are single-row and therefore
  need only Step 3's encoding, but they need different amounts of
  Step 4.

  Commuting `σ_p σ_q` is `(p AND q)` versus `(q AND p)` on one row —
  Kleene AND is commutative, so the honest encoding proves it and the
  two-valued one would too. Filter-past-projection is the one that
  bites: the projection may drop a column the filter reads, or change
  a column's nullability, so the `(value, is_null)` pair is
  load-bearing and the rule has a *side condition* the encoding has
  to state. Encoding a side condition is the skill the exercise is
  actually teaching.

  </details>

## References

**Papers**
- de Moura & Bjørner — "Z3: An Efficient SMT Solver" (TACAS 2008)
  — 4 pages, read whole; then read topic 21's chapter, which walks
  it against the source
- Chu, Wang, Weitz, Cheung, Suciu — "Cosette: An Automated Prover
  for SQL" (CIDR 2017) — §2 for undecidability and the
  prove/disprove split, §3.1 for the bounded symbolic data model
  (`Tuple := List<Integer>`, fixed-size symbolic relations, no
  floats), §6 for the 17-of-23 Rules result; our use is the SMT half

**Cross-references**
- [topics/21-formal/reading-z3-tacas08.md](../21-formal/reading-z3-tacas08.md)
  — CDCL(T), Nelson–Oppen, `src/ast/euf/` and E-matching in depth.
  Everything this chapter compresses into Step 1
- [reading-pqs-tlp-papers.md](reading-pqs-tlp-papers.md) — TLP's
  three-way partition is Step 4's Kleene table, arrived at from the
  testing side
- [reading-sqlancer.md](reading-sqlancer.md) — `ComparatorHelper`'s
  size-plus-set comparison, the bag-semantics gap Cosette closes

**Code** — [z3](https://github.com/Z3Prover/z3) @ `1d425e5`

| File | Lines | What |
|---|---|---|
| `src/solver/solver.h` | 58 | `class solver : public check_sat_result` |
| `src/solver/solver.h` | 124, 177-183 | `assert_expr` and `check_sat` — the entire testing API |
| `src/solver/solver.h` | 172-174 | the unsat-core contract |
| `src/tactic/tactic.h` | 34 | `class tactic` — composable goal transformers |
| `src/tactic/portfolio/default_tactic.cpp` | 36-55 | twelve probe-guarded branches, then `and_then(preamble, smt)` |
| `src/tactic/portfolio/smt_strategic_solver.cpp` | — | tactic → solver bridge |
| `src/smt/smt_context.h` | 89 | `class context` — CDCL(T); topic 21's territory |
| `src/smt/mam.cpp` | — | matching abstract machine; not needed for a quantifier-free encoding |
