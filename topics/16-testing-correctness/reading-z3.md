# Z3 & Cosette: testing every input at once

Everything else in this topic samples the input space; SMT quantifies
over it — "does there EXIST a row where these two plans disagree?"
UNSAT means the rewrite is proven for all databases. Before the code
and the papers, this chapter builds the machinery step by step — SAT,
the CDCL loop, theory solvers, tactics — then applies it
Cosette-style to verify our topic-10 rewrite rules. Read Z3 the way
PLAN.md says to: as a masterclass high-performance search engine over
LOGIC whose architecture rhymes with a query engine.

## The problem in one sentence

A fuzzer that runs 10 million random rows through two query plans
still checks a measure-zero slice of the input space; encoding both
plans as logic and asking a solver "∃ row where they disagree?"
checks ALL rows at once — and returns either a proof or the exact
counterexample row, usually in milliseconds.

## The concepts, step by step

### Step 1 — SAT: search over boolean assignments

SAT (boolean satisfiability) is the question "given a formula over
true/false variables, is there an assignment making it true?" A SAT
**solver** is a search engine over the 2^n assignments — and modern
ones routinely handle formulas with millions of variables because
the search is ruthlessly pruned. Flip the answer around and you get
verification: to prove a property P holds always, ask the solver for
a case where `NOT P` holds. **UNSAT** ("no satisfying assignment
exists") is then a proof; **SAT** hands you a concrete
counterexample. That inversion — prove by failing to find — is the
whole chapter.

### Step 2 — CDCL: the search loop that learns from every dead end

CDCL (conflict-driven clause learning) is the algorithm inside every
modern SAT solver: guess a variable (**decide**), push the logical
consequences (**propagate**), and when a contradiction appears
(**conflict**), *analyze why*, record the reason as a new learned
clause so the same dead end is never entered again, and jump back
(**backjump**) past the guesses the conflict proved irrelevant. The
learned clauses are why CDCL beats brute force by orders of
magnitude: every failure permanently shrinks the remaining search
space. The DB analogy runs deep — CDCL is adaptive execution with
feedback, and learned clauses are materialized negative results.

### Step 3 — SMT: SAT proposes, theories veto

Real verification needs more than booleans — integers, arrays, bit
patterns. SMT (satisfiability modulo theories) keeps the CDCL engine
and attaches **theory solvers**, each a decision procedure for one
domain (linear arithmetic, bitvectors, arrays, uninterpreted
functions, strings):

```
 SAT solver:  boolean skeleton (CDCL: decide → propagate →
              conflict → learn clause → backjump)
      +
 theory solvers: linear arithmetic, bitvectors, arrays,
              uninterpreted functions, strings...
      =
 SMT: SAT proposes boolean assignments; theories veto with
      conflict explanations ("x<3 ∧ x>5 is impossible") that
      become learned clauses
```

The SAT core treats `x < 3` as an opaque boolean; when it proposes
`x < 3 ∧ x > 5`, the arithmetic theory vetoes with an explanation
that becomes a learned clause. Theory propagation is predicate
pushdown into specialized engines — same shape as topic 10.

### Step 4 — tactics: query plans for proofs

Z3 doesn't run one fixed algorithm; it composes **tactics** —
transformers that rewrite a goal (simplify, eliminate equalities,
blast bitvectors to SAT) — into pipelines, chosen by **probes** that
inspect the formula first. `(then simplify solve-eqs bit-blast sat)`
is a pipeline of rewrites ending in an executor, and
`default_tactic.cpp` dispatches on the detected logic the way a
planner dispatches on statistics — probes are cardinality
estimation for proofs. This is why PLAN.md calls Z3 a query engine
for logic: the architecture is parse → rewrite → cost-informed
dispatch → execute.

### Step 5 — symbolic rows: encoding a query plan as a formula

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

```rust
// verify a rewrite for ALL rows by asking for ONE disagreeing row
let a  = Int::fresh("a");  let a_null = Bool::fresh("a_null");
let b  = Int::fresh("b");  let b_null = Bool::fresh("b_null");
let row = Row { a, a_null, b, b_null };

let p1 = compile(plan_before, &row);   // Kleene 3-valued AND/OR/NOT/cmp
let p2 = compile(plan_after, &row);

match solver.check(p1.keeps_row().xor(p2.keeps_row())) {
    Unsat  => Proven,                  // no row distinguishes the plans
    Sat(m) => Counterexample(m),       // the model IS the failing row
}
```

One subtlety keeps this tractable: filters are row-at-a-time pure
logic, so one symbolic row quantifies over all databases —
no quantifiers needed, and quantifier-free formulas are Z3's fast
path.

### Step 6 — the NULL trap: encode SQL's three-valued logic honestly

SQL predicates evaluate to TRUE, FALSE, or NULL ("unknown"), and
WHERE keeps only TRUE — so a two-valued encoding proves rewrites
that are false in real SQL. The honest encoding: each nullable
column becomes a pair (value, is_null), and AND/OR/NOT/comparison
are defined per SQL's Kleene semantics (NULL AND FALSE = FALSE, NULL
AND TRUE = NULL, …). This is the trap AND the point: most real
optimizer bugs (TLP's bread and butter — reading-pqs-tlp-papers.md)
are exactly NULL-semantics violations, and Z3 finds them as SAT
models in milliseconds.

### Step 7 — Cosette: the full SQL-equivalence prover

Cosette answers "are Q1 and Q2 equivalent for ALL databases?" — the
general problem, beyond single-row filters. It compiles SQL to
**K-relations** (relations where each row carries a multiplicity, so
bag/duplicate semantics work — SQL tables are bags, not sets), then
splits: easy fragments → SMT for counterexamples, hard equivalences
→ Coq proof search over HoTT encodings. Our use is the SMT half:
filters and projections over symbolic rows, exactly Steps 5–6, which
is all topic 10's rewrite rules need.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| src/solver/solver.h:58 | 1 | `class solver` — check_sat over assertions |
| src/smt/smt_context.h:89 | 2–3 | `smt::context` — the CDCL(T) core loop |
| src/tactic/tactic.h:34 | 4 | `class tactic` — composable transformers |
| src/tactic/portfolio/default_tactic.cpp | 4 | the default strategy: probe → dispatch by logic |
| src/tactic/portfolio/smt_strategic_solver.cpp | 4 | tactic → solver bridge |
| src/ast/ | — | hash-consed terms (one node per distinct expr — topic 2's interning) |
| src/smt/mam.cpp | — | matching abstract machine for quantifier triggers — a compiled pattern matcher (topic 19 vibes) |

Reading order: `solver.h` for the public shape, `smt_context.h` for
the CDCL(T) loop (don't read it all — find decide/propagate/
conflict), then the tactic machinery. The `src/ast/` hash-consing
and `mam.cpp` are optional side quests that rhyme with topics 2
and 19.

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

## References

**Papers**
- de Moura & Bjørner — "Z3: An Efficient SMT Solver" (TACAS 2008)
  — 4 pages, read whole
- Chu, Wang, Weitz, Cheung, Suciu — "Cosette: An Automated Prover
  for SQL" (CIDR 2017) — read for the K-relations encoding and the
  SMT/Coq split; our use is the SMT half

**Code**
- [z3](https://github.com/Z3Prover/z3) — `src/` — start from
  `src/solver/solver.h` and `src/smt/smt_context.h`, then the
  tactic machinery in `src/tactic/` (tactics ARE query plans for
  proofs)
