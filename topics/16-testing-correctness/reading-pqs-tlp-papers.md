# Reading guide — the PQS & TLP papers

Two papers, one author (Manuel Rigger, with Zhendong Su):

- "Testing Database Engines via Pivoted Query Synthesis" (OSDI '20)
- "Finding Bugs in Database Systems via Query Partitioning" —
  Ternary Logic Partitioning (OOPSLA '20)

Read PQS first; TLP is partly a response to PQS's costs. Pair with
reading-sqlancer.md — the code makes the papers concrete.

## PQS (OSDI '20)

The problem statement is the keeper: random query generation was
stuck on the **test-oracle problem** — you can generate a million
queries, but who knows the right answers? Prior art (RAGS)
compared multiple DBMSs against each other — but dialects diverge,
and shared bugs hide.

PQS's move: don't verify the whole result set. Verify ONE row you
chose in advance:

```
 pick pivot row r
 synthesize predicate p with eval(p, r) = TRUE   ← the hard part
 if r ∉ result(SELECT ... WHERE p) → bug
```

§ on *rectified queries* is the algorithmic core: generate a random
expression tree, evaluate it bottom-up on r's concrete values under
the DBMS's semantics (dialect-specific NULL rules, casts, collation
— all of it), then rectify: TRUE → keep, FALSE → wrap `NOT`, NULL →
wrap `IS NULL`. Question: why does rectification make EVERY randomly
generated expression usable rather than discarding the ~2/3 that
aren't TRUE?

Results to internalize: ~100 bugs across SQLite/MySQL/Postgres in
~4 months, most in SQLite — which then fixed its test suite. Note
what PQS canNOT see: a bug that returns the pivot row plus GARBAGE
rows passes (containment, not equality).

## TLP (OOPSLA '20)

PQS's costs: an evaluator per dialect (weeks of work each) and
single-row blindness. TLP removes both with self-consistency:

```
 Q ≡ Q' where TRUE
 partition by any predicate p:
   result(Q) = result(Q_p) ⊎ result(Q_NOT_p) ⊎ result(Q_p_IS_NULL)
```

The ternary part is the SQL-specific insight: two-valued
partitioning (p / NOT p) is WRONG in SQL — NULL rows vanish from
both branches, and real optimizer bugs live exactly in that gap
(NULL-blind predicate pushdown, our tlp.rs stub's injected bug).

The paper generalizes beyond WHERE: aggregate TLP (MAX over
partitions = MAX of partition MAXes; AVG needs SUM/COUNT
recombination), DISTINCT, GROUP BY. Each needs a *recombination
operator* ⊎ appropriate to the clause. Question: why is AVG the
canonical example of a non-decomposable aggregate, and what does
that echo from topic 11's partial aggregation?

## The meta-lesson (both papers)

A metamorphic oracle trades *completeness* for *portability*: PQS
knows ground truth for one row of one query; TLP knows only that
three queries must reconcile. Both beat differential testing
because they need ONE system — no second implementation to
disagree with. This is the design space our M16 Cypher oracles
live in.

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
