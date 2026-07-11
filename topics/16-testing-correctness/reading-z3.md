# Reading guide — Z3 (TACAS '08 + the codebase) and Cosette (CIDR '17)

Clone: `~/repos/z3` (`src/`). Paper: "Z3: An Efficient SMT Solver"
(de Moura & Bjørner, TACAS '08 — 4 pages, read whole). Then
"Cosette: An Automated Prover for SQL" (CIDR '17) for the
DB application. Treat Z3 the way PLAN.md says: a masterclass
high-performance search engine over LOGIC — the architecture rhymes
with a query engine more than you'd expect.

## SMT in one box

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

The DB analogy: CDCL = adaptive execution with feedback; learned
clauses = materialized negative results; theory propagation =
predicate pushdown into specialized engines.

## Codebase anchors

| anchor | what it is |
|---|---|
| src/solver/solver.h:58 | `class solver` — check_sat over assertions |
| src/smt/smt_context.h:89 | `smt::context` — the CDCL(T) core loop |
| src/tactic/tactic.h:34 | `class tactic` — composable transformers |
| src/tactic/portfolio/default_tactic.cpp | the default strategy: probe → dispatch by logic |
| src/tactic/portfolio/smt_strategic_solver.cpp | tactic → solver bridge |
| src/ast/ | hash-consed terms (one node per distinct expr — topic 2's interning) |
| src/smt/mam.cpp | matching abstract machine for quantifier triggers — a compiled pattern matcher (topic 19 vibes) |

Tactics ARE query plans for proofs: `(then simplify solve-eqs
bit-blast sat)` is a pipeline of rewrites ending in an executor,
chosen by a probe (cardinality estimation!). `default_tactic.cpp`
dispatches on the detected logic the way a planner dispatches on
statistics.

## Cosette: proving SQL rewrites

Cosette answers "are Q1 and Q2 equivalent for ALL databases?" — it
compiles SQL to K-relations (rows with multiplicities, so bag
semantics work), then splits: easy fragments → SMT for
counterexamples, hard equivalences → Coq proof search over HoTT
encodings. Our use is the SMT half:

```
 symbolic row: (a: Int, b: Int, a_null: Bool, b_null: Bool)
 P1 = compile(plan1's filter chain)   — a formula
 P2 = compile(plan2's filter chain)
 ask Z3: ∃ row. P1(row) ≠ P2(row)
   UNSAT → rewrite proven for all rows
   SAT   → the model IS the counterexample row
```

Three-valued logic is the trap AND the point: encode each nullable
column as (value, is_null) and define AND/OR/NOT/comparison per SQL
Kleene semantics — most real optimizer bugs (TLP's bread and
butter) are exactly NULL-semantics violations, and Z3 finds them as
SAT models in milliseconds.

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
