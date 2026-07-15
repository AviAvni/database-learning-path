# PQS & TLP: solving the test-oracle problem twice

Random query generation was stuck for decades on one question: you
can generate a million queries, but who knows the right answers?
Manuel Rigger and Zhendong Su answered it twice in one year — PQS by
verifying a single pre-chosen row, TLP by making the DBMS check
itself. Before the papers, this chapter builds the ideas in order —
the oracle problem, why differential testing fails, the rectification
trick, PQS's costs, and the ternary partition that fixes them — then
gives you a reading route through both. Pair with
[reading-sqlancer.md](reading-sqlancer.md) — the code makes the
papers concrete.

## The problem in one sentence

PQS alone found ~100 bugs in SQLite/MySQL/Postgres in ~4 months —
in the three most-tested database engines on earth — because until
2020 nobody had a scalable answer to "what should this random query
return?"

## The concepts, step by step

### Step 1 — the test-oracle problem, and why differential testing fails

An **oracle** is the component of a test that decides whether an
output is wrong; for randomly generated SQL, no such component
existed. Prior art (RAGS, 1998) used **differential testing**: run
the same query on multiple DBMSs and flag disagreements. Two
failures killed it: dialects legitimately diverge (MySQL returns
0/1 booleans, SQLite coerces types by "affinity" — a disagreement is
usually not a bug), and a bug all systems share produces no
disagreement at all. Both PQS and TLP need only ONE system — that's
the breakthrough.

### Step 2 — SQL's third truth value

One database fact both papers pivot on: a SQL predicate (a WHERE
condition) evaluates to TRUE, FALSE, or **NULL** ("unknown" —
`NULL = 5` is neither true nor false), and WHERE keeps only rows
where it is TRUE. Rows evaluating FALSE *or NULL* vanish. Any
two-valued mental model — including the one inside an optimizer
author's head — is wrong in exactly these cases, which is where the
bugs cluster.

### Step 3 — PQS: verify ONE row you chose in advance

Pivoted Query Synthesis inverts the problem. Don't verify the whole
result set of a random query; pick a random existing row (the
**pivot**), then construct a query that provably must return it:

```
 pick pivot row r
 synthesize predicate p with eval(p, r) = TRUE   ← the hard part
 if r ∉ result(SELECT ... WHERE p) → bug
```

Ground truth for one row of one query is cheap to compute — and
because generation costs microseconds, "one row per query" times
millions of queries covers the input space in expectation.

### Step 4 — rectification: make ANY random predicate TRUE on the pivot

The § on *rectified queries* is the algorithmic core. Generate a
random expression tree, evaluate it bottom-up on r's concrete values
under the DBMS's own semantics (dialect-specific NULL rules, casts,
collation — all of it), then **rectify**: TRUE → keep, FALSE → wrap
`NOT`, NULL → wrap `IS NULL` (Step 2's third value gets its own
wrapper):

```rust
// rectify: ANY random predicate becomes TRUE-on-the-pivot
fn rectify(p: Expr, pivot: &Row) -> Expr {
    match eval3(&p, pivot) {      // eval under the DBMS's OWN dialect rules
        True  => p,
        False => not(p),
        Null  => is_null(p),      // SQL's third value gets its own wrapper
    }
}
// then: pivot ∉ result(SELECT * FROM t WHERE rectify(p, pivot)) → BUG
```

Question: why does rectification make EVERY randomly generated
expression usable rather than discarding the ~2/3 that aren't TRUE?

### Step 5 — what PQS costs, and what it cannot see

Two prices. First, that `eval3` is a full expression evaluator *per
dialect* — weeks of work for each DBMS, re-implementing exactly the
quirks (MySQL's 0/1 booleans, SQLite's type affinity) you're testing.
Second, containment-not-equality blindness: PQS asserts the pivot
appears in the result — a bug that returns the pivot row plus
GARBAGE rows passes. Results to internalize anyway: ~100 bugs in
~4 months, most in SQLite — which then fixed its test suite.

### Step 6 — TLP: partition by any predicate, make the DBMS check itself

Ternary Logic Partitioning removes both costs with self-consistency.
Any predicate p splits a query's rows into exactly three disjoint
groups — TRUE, FALSE, NULL (Step 2) — so:

```
 Q ≡ Q' where TRUE
 partition by any predicate p:
   result(Q) = result(Q_p) ⊎ result(Q_NOT_p) ⊎ result(Q_p_IS_NULL)
```

No evaluator: the DBMS runs all four queries itself, and the
optimizer — seeing four different queries — plans each differently.
The ternary part is the SQL-specific insight: two-valued
partitioning (p / NOT p) is WRONG in SQL — NULL rows vanish from
both branches, and real optimizer bugs live exactly in that gap
(NULL-blind predicate pushdown, our tlp.rs stub's injected bug).

### Step 7 — recombination operators: TLP beyond WHERE

The paper generalizes the identity clause by clause: aggregate TLP
(MAX over partitions = MAX of partition MAXes; AVG canNOT be
recombined from partition AVGs — it needs SUM/COUNT recombination),
DISTINCT, GROUP BY. Each needs a *recombination operator* ⊎
appropriate to the clause. Question: why is AVG the canonical
example of a non-decomposable aggregate, and what does that echo
from topic 11's partial aggregation?

### Step 8 — the meta-lesson: completeness traded for portability

A metamorphic oracle trades *completeness* for *portability*: PQS
knows ground truth for one row of one query; TLP knows only that
three queries must reconcile. Both beat differential testing because
they need ONE system — no second implementation to disagree with.
This is the design space our M16 Cypher oracles live in.

## How to read the papers (with the concepts in hand)

Read PQS first; TLP is partly a response to PQS's costs.

1. **PQS (OSDI '20) §1–2** — the test-oracle problem statement (Step
   1) is the keeper; the RAGS comparison tells you why differential
   testing was a dead end.
2. **PQS §on rectified queries** — the algorithmic core (Step 4).
   Work one rectification by hand with a NULL-valued pivot column.
3. **PQS evaluation** — note the bug counts per DBMS and *where*
   they cluster (expression evaluation, exactly what Step 5
   predicts).
4. **TLP (OOPSLA '20) §core identity** — Step 6 in the authors'
   words; check that the three partitions are provably disjoint and
   exhaustive under three-valued logic.
5. **TLP §generalizations** — the recombination table (Step 7); this
   is the part you'll port to Cypher, so read it with M16's
   `count(*)`/`collect` in mind.

## Questions for notes.md

1. PQS §evaluation: why must the pivot evaluator implement the
   DBMS's *dialect* semantics (MySQL 0/1 booleans, SQLite type
   affinity) rather than the SQL standard's?
2. Containment-not-equality: construct a bug PQS provably misses
   and TLP provably catches, and vice versa.
3. TLP with p = `col = col` — why is this predicate USELESS for
   partitioning, and what does that say about predicate generation?
4. Both papers fuzz SCHEMAS and DATA too (random tables, indexes,
   collations). Why do index-present vs index-absent runs of the
   same query make NoREC/TLP sharper?
5. For M16: pick the first three TLP recombinations to implement
   for Cypher (WHERE / count(*) / collect?) and write the ⊎ for
   each.

## References

**Papers**
- Rigger & Su — "Testing Database Engines via Pivoted Query
  Synthesis" (OSDI 2020,
  [arXiv:2001.04174](https://arxiv.org/abs/2001.04174)) — the
  rectified-queries section is the algorithmic core
- Rigger & Su — "Finding Bugs in Database Systems via Query
  Partitioning" (OOPSLA 2020) — Ternary Logic Partitioning; read
  after PQS

**Code**
- [sqlancer](https://github.com/sqlancer/sqlancer) — both papers as
  running code; walked in [reading-sqlancer.md](reading-sqlancer.md)
