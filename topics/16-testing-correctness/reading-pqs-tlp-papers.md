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

Every number below is quoted from the papers themselves, with the
section, table or figure it came from. Two of the three are open
access: PQS is [arXiv:2001.04174](https://arxiv.org/abs/2001.04174),
and preprints of TLP and NoREC are at `manuelrigger.at/preprints/`.
Download them before reading further — this chapter is a route
through the papers, not a substitute for them.

## The problem in one sentence

PQS alone found 123 bugs in SQLite/MySQL/Postgres in about three
months — in the three most-tested database engines on earth —
because until 2020 nobody had a scalable answer to "what should this
random query return?"

## The concepts, step by step

### Step 1 — the test-oracle problem, and why differential testing fails

> **In:** a generator that emits syntactically valid random SQL at
> microsecond cost.
> **Out:** no verdict — the missing piece is not inputs, it is
> ground truth.

An **oracle** is the component of a test that decides whether an
output is wrong; for randomly generated SQL, no such component
existed. Prior art (RAGS, Slutz 1998) used **differential testing**:
run the same query on multiple DBMSs and flag disagreements. Two
failures killed it: dialects legitimately diverge (MySQL returns
0/1 booleans, SQLite coerces types by "affinity" — a disagreement is
usually not a bug), and a bug all systems share produces no
disagreement at all.

The scale of the second problem is worth stating precisely. PQS §4.7
Table 4 reports the SQLancer implementation cost per DBMS: SQLite
6,501 LOC against SQLite's 49,703 LOC (13.1%), MySQL 3,995 against
707,803 (0.6%), PostgreSQL 4,981 against 329,999 (1.5%) — and only
918 LOC shared between them. A "reference implementation" of SQL is
not a thing you can cheaply have; a *relationship* is.

Both PQS and TLP need only ONE system — that's the breakthrough.

Why it matters: this is the constraint that shapes every oracle in
topic 16, including the crash-recovery oracle `crash_matrix`
measures. You never get a second correct system for free.

### Step 2 — SQL's third truth value

> **In:** a **predicate** — a boolean expression in a `WHERE`
> clause — and a row.
> **Out:** TRUE, FALSE, or NULL — three outcomes, of which only one
> keeps the row.

One database fact both papers pivot on: a SQL predicate evaluates to
TRUE, FALSE, or **NULL** ("unknown" — `NULL = 5` is neither true nor
false), and `WHERE` keeps only rows where it is TRUE. Rows
evaluating FALSE *or NULL* vanish. Any two-valued mental model —
including the one inside an optimizer author's head — is wrong in
exactly these cases, which is where the bugs cluster.

Count the arms of the case analysis a correct optimizer rewrite has
to survive:

```
 two-valued reasoning:  p ∈ {T, F}                        2 cases
 SQL reasoning:         p ∈ {T, F, N}                     3 cases
 a rewrite over p AND q: 2×2 = 4 cases assumed, 3×3 = 9 real
                         → 5 of 9 cases (56%) involve a NULL
                           and are the ones nobody wrote a test for
```

Why it matters: Step 4's rectification and Step 6's partition are
both, structurally, "handle the third case" — and PQS's evaluator
and TLP's third query exist for no other reason.

### Step 3 — PQS: verify ONE row you chose in advance

> **In:** a database where every table holds at least one row.
> **Out:** a **pivot row** — one row from *each* table — and a
> query that provably must return it.

Pivoted Query Synthesis inverts the problem. Don't verify the whole
result set of a random query; pick a pivot, then construct a query
that provably must return it:

```
 pick pivot row r
 synthesize predicate p with eval(p, r) = TRUE   ← the hard part
 if r ∉ result(SELECT ... WHERE p) → bug
```

"Pivot row" is more specific than "a random row", and the difference
matters as soon as there is a `JOIN`. §3.1: "We ensure that each
table holds at least one row. We then select a random row from each
of the tables (see step 2), to which we refer as the pivot row." The
pivot is a row of the cross product — one component per table in the
`FROM` clause — which is what makes `t0.c1` and `t1.c0` both
substitutable in step 3.

Ground truth for one row of one query is cheap to compute — and
because generation costs microseconds, "one row per query" times
millions of queries covers the input space in expectation. The
evidence that this is enough is §4.3: the reduced bug-triggering
test cases averaged **3.71 lines of code**, 13 of them needed a
single line, and the largest was 8 statements (with one 27-statement
outlier). Bugs did not need big inputs; they needed the right one.

Why it matters: PQS is the only oracle in this topic that knows a
*fact* about the answer rather than a relationship — and Step 5 is
the bill for that.

### Step 4 — rectification: make ANY random predicate TRUE on the pivot

> **In:** a randomly generated expression tree and the pivot row's
> concrete values.
> **Out:** a predicate guaranteed to evaluate TRUE on the pivot —
> whatever the original expression did.

The section on *rectified queries* (§3.2) is the algorithmic core.
Generate a random expression tree, evaluate it bottom-up on r's
concrete values under the DBMS's own semantics (dialect-specific
NULL rules, casts, collation — all of it), then **rectify**: TRUE →
keep, FALSE → wrap `NOT`, NULL → wrap `IS NULL` (Step 2's third
value gets its own wrapper). That is the paper's Algorithm 3,
`rectifyCondition`, driven by Algorithm 1's `generateExpression` and
Algorithm 2's per-node `execute`:

```text
// ILLUSTRATION — pseudocode for PQS §3.2 Algorithm 3, rectifyCondition.
// Not quoted from SQLancer; the running equivalent is
// src/sqlancer/common/oracle/PivotedQuerySynthesisBase.java:125
// (abstract getRectifiedQuery, "steps 2-5 of the PQS paper").
fn rectify(p: Expr, pivot: &Row) -> Expr {
    match eval3(&p, pivot) {      // eval under the DBMS's OWN dialect rules
        True  => p,
        False => not(p),
        Null  => is_null(p),      // SQL's third value gets its own wrapper
    }
}
// then: pivot ∉ result(SELECT * FROM t WHERE rectify(p, pivot)) → BUG
```

Do the arithmetic on what rectification buys. Suppose a generated
predicate is TRUE on the pivot a third of the time:

```
 without rectification: keep only TRUE-valued predicates
   usable fraction              ≈ 1/3
   generated per usable query   ≈ 3
   and the discarded 2/3 are exactly the FALSE and NULL cases —
   the ones Step 2 says the bugs live in

 with rectification: every predicate becomes usable
   usable fraction              = 1
   generated per usable query   = 1
   speedup                      ≈ 3×, and the NULL arm is now
                                  over-represented rather than absent
```

The `IS NULL` arm is the one that matters: without it a NULL-valued
predicate is unusable, and a NULL-valued predicate is the shape of
test that finds NULL-blind optimizer rewrites.

Question: why does rectification make EVERY randomly generated
expression usable rather than discarding the ~2/3 that aren't TRUE?

Why it matters: this is the trick, and it is three lines. Everything
expensive about PQS is in `eval3`, not here.

### Step 5 — what PQS costs, and what it cannot see

> **In:** a working PQS implementation for one DBMS.
> **Out:** an expression evaluator you now own, and one bug class
> the oracle structurally cannot see.

Two prices. First, that `eval3` is a full expression evaluator *per
dialect* — re-implementing exactly the quirks (MySQL's 0/1 booleans,
SQLite's type affinity) you're testing. §3.2 measures one operator:
"the implementation of the LIKE regular expression operator has over
50 LOC in SQLancer." Multiply by an operator table.

Second, containment-not-equality blindness. PQS asserts the pivot
*appears* in the result; a bug that returns the pivot row plus
garbage rows passes. The paper says so itself: "we cannot detect
logic bugs where a DBMS erroneously fetches duplicate rows."

Now correct the folklore about the results, because the real numbers
are more interesting than "~100 bugs". From the abstract and §4.2
Table 2:

```
 reports opened                        123
 true bugs (fixed or verified)          99   = 77 code fixes + 8 doc fixes + 14 confirmed
   of which SQLite       65 fixed
             MySQL       15 fixed + 10 verified
             PostgreSQL   5 fixed +  4 verified
 not bugs                               24   = 12 "intended behaviour" + 12 duplicates
 testing period                    ~3 months (§4.1)
```

And §4.2 Table 3 splits the 99 by *which* oracle caught them:

```
 Contains (the pivot-row oracle)   61      SQLite 46, MySQL 14, PostgreSQL 1
 Error    (unexpected error)       34
 SEGFAULT (crash)                   4
                                   ──
                                   99

 the pivot oracle's share = 61 / 99 = 61.6%
```

So the headline technique found under two thirds of the bugs the
harness found; a third came free from "the DBMS raised an error it
shouldn't have", which needs no oracle at all. Any harness you build
should log unexpected errors even before it has an oracle — that arm
is a third of the yield for a day of work.

Why it matters: both prices are *design* costs, not bug counts, and
they are what TLP was written to remove.

### Step 6 — TLP: partition by any predicate, make the DBMS check itself

> **In:** one query with its `WHERE` clause cleared, and any
> randomly generated predicate `p`.
> **Out:** four result sets that must reconcile — with no evaluator
> anywhere.

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
(NULL-blind predicate pushdown, our `tlp.rs` stub's injected bug).

Two corrections to carry into the paper. First, the paper's title is
"Finding Bugs in Database Systems via **Query Partitioning**": the
general framework is query partitioning, and TLP is the instance
where the partitioning is done by three-valued logic. Second, the
results, from the abstract and §4 Table 3:

```
 reports opened                       181
 true bugs                            175    MySQL, TiDB, SQLite, CockroachDB, DuckDB
   fixed                              125
   of which logic bugs                 77    (Table 4)
```

Why it matters: TLP needs no per-dialect evaluator, which is why it
is the technique the SQLancer README calls "among the most widely
adopted testing techniques" (`README.md:82`) while PQS is
"currently unmaintained" (`:80`).

### Step 7 — recombination operators: TLP beyond WHERE

> **In:** a clause other than `WHERE` — `GROUP BY`, `DISTINCT`,
> `HAVING`, an aggregate.
> **Out:** a different composition operator `⋄` per clause, and one
> aggregate that needs a rewrite instead.

The paper generalizes the identity clause by clause. **Table 1**
lists nine oracles with the operator each uses to put the partitions
back together (`⊎` is `UNION ALL`, `∪` is `UNION`):

| oracle | partitions on | ⋄ | note |
|---|---|---|---|
| WHERE | the `WHERE` predicate | `⊎` | duplicates must survive |
| WHERE Extended | predicate + `ORDER BY`/`LIMIT` interplay | `⊎` | |
| GROUP BY | the grouping | `∪` | duplicate groups collapse anyway |
| HAVING | the `HAVING` predicate | `⊎` | |
| DISTINCT | the predicate, under `DISTINCT` | `∪` | |
| MIN / MAX | the predicate | `MIN` / `MAX` of the parts | self-decomposable |
| SUM | the predicate | `SUM` of the parts | self-decomposable |
| COUNT | the predicate | **`SUM`** of the parts | note the operator changes |
| AVG | the predicate | `SUM(s)/SUM(c)` | needs a rewrite — see below |

`COUNT` is the small surprise: you recombine counts with `SUM`, not
with `COUNT`. `AVG` is the interesting one, and the paper's
vocabulary for it is precise (§2.1, quoting Jesus et al. 2015):

> "An aggregate function `f` is **self-decomposable** when a merge
> operator `⊕` exists so that, given two non-empty multi-sets X and
> Y, the following holds: `f(X ⊎ Y) = f(X) ⊕ f(Y)`. … An aggregate
> function `f` is **composable** if for some function `g` and a
> self-decomposable aggregate function `h`, it can be expressed as
> `f = g ∘ h`."

So AVG is not "non-decomposable" — it is **composable but not
self-decomposable**, with `h({x}) = (x, 1)` and `g((s,c)) = s/c`:

```
 partition sizes:  |A| = 3, sum 30      AVG(A) = 10
                   |B| = 1, sum 100     AVG(B) = 100

 wrong (recombine the AVGs):  (10 + 100) / 2      = 55
 right (recombine (sum,count)): (30 + 100) / (3 + 1) = 32.5
 true AVG of A ⊎ B:            130 / 4             = 32.5
```

That is exactly topic 11's partial aggregation: you must ship the
partial state `(sum, count)` between workers, not the finished
average. Same algebra, different reason for caring.

Now the payoff table. §4 Table 4 splits TLP's 77 logic bugs by
oracle:

```
 WHERE       60
 Aggregate   10
 HAVING       3
 GROUP BY     2
 DISTINCT     2
             ──
             77       (plus, separately, 62 error bugs and 25 crashes)

 the WHERE oracle's share = 60 / 77 = 77.9%
```

The paper's own summary: "The WHERE oracle detected 60 bugs …
the most effective one"; "The other oracles detected 17 bugs in
total". If you are porting TLP to a new query language, port the
`WHERE` oracle and stop until it stops finding things.

Why it matters: the recombination operator is the *only* part of TLP
that has to be redesigned per clause — and per query language, which
is precisely M16's problem.

### Step 8 — the meta-lesson: completeness traded for portability

> **In:** two oracles that both work against a single system.
> **Out:** a design axis — how much you know about the answer versus
> how much it costs to know it.

A metamorphic oracle trades *completeness* for *portability*: PQS
knows ground truth for one row of one query; TLP knows only that
three queries must reconcile. Both beat differential testing because
they need ONE system — no second implementation to disagree with.

The third point on the axis is NoREC (ESEC/FSE 2020), which knows
even less — one integer — and is worth reading between the two. Its
transformation (§3.1) is smaller than folklore suggests: `SELECT *
FROM t0 WHERE φ` becomes `SELECT (φ IS TRUE) FROM t0`, and the
comparison is the *cardinality* of the first against the count of
TRUEs in the second. Content comparison is §3.3, an extension. Its
results: 159 true bugs of 168 reported, 141 fixed, of which **51**
were logic bugs (§4.3 Table 3: SQLite 39, CockroachDB 7, MariaDB 5,
PostgreSQL 0).

Line the three up:

```
 oracle   knows                              per-dialect evaluator   logic bugs found
 PQS      one row must be present             YES                     61  (of 99, §4.2 T3)
 NoREC    two counts must be equal            no                      51  (of 159, §4.3 T3)
 TLP      three partitions must reconstruct   no                      77  (of 175, §4 T4)
                                                                     ───
                                                                     189 logic bugs
 total true bugs across the three papers: 123 + 159 + 175 = 457
```

And one more measurement, because it disciplines the whole
enterprise. TLP §5.3 ran each configuration against DuckDB for 10
hours and measured line coverage:

```
 database generation alone, no oracle at all       48.3%
 any single TLP oracle                             55.3% – 55.9%   (spread 0.6%)
 all oracles together                              56.1%
 PQS, for comparison (PQS §4.7)                    23.7% (PostgreSQL) – 43.0% (SQLite)

 marginal coverage from adding every oracle beyond the first:
   56.1% − 55.9% = 0.2 percentage points
 marginal coverage from having any oracle at all:
   55.3% − 48.3% = 7.0 percentage points
```

Most of the coverage comes from *generating databases and queries*,
not from the oracle. The oracle's job is not to reach new code — it
is to notice when the code it already reached is wrong. Jung et al.
found DBMS cores exceed 95% coverage after tens of queries; SQLite
has 100% MC/DC coverage and TLP still found bugs in it. Coverage is
a bad proxy for oracle quality, and this is the measurement that
says so.

This is the design space our M16 Cypher oracles live in.

Why it matters: when you pick an oracle for M16 you are picking a
point on this axis, and the axis is "what do I know" versus "what
does knowing cost", not "which found more bugs".

## How to read the papers (with the concepts in hand)

Read PQS first; TLP is partly a response to PQS's costs.

1. **PQS (OSDI '20) §1–2** — the test-oracle problem statement (Step
   1) is the keeper; the RAGS comparison tells you why differential
   testing was a dead end.
2. **PQS §3.2, Algorithms 1–3** — the algorithmic core (Step 4).
   `generateExpression`, `NotNode::execute`, `rectifyCondition`.
   Work one rectification by hand with a NULL-valued pivot column.
3. **PQS §4.2, Tables 2 and 3** — the bug counts, and the split
   showing only 61 of 99 came from the containment oracle (Step 5).
   §4.7 Table 4 for the implementation cost per DBMS.
4. **TLP (OOPSLA '20) §2.1 and Table 1** — Step 6 and Step 7 in the
   authors' words; check that the three partitions are provably
   disjoint and exhaustive under three-valued logic, and read the
   self-decomposable / composable definitions carefully.
5. **TLP §4 Table 4 and §5.2–5.3** — where the bugs actually came
   from (60 of 77 from `WHERE`), the five cases where NoREC's
   record-count comparison was insufficient, and the coverage
   measurement that says coverage is the wrong metric.
6. **NoREC (ESEC/FSE '20) §3.1** — optional but short: the third
   point on Step 8's axis, and the one whose transformation you can
   hold in your head.

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

## Done when

Answer each before unfolding it.

- [ ] You can state the test-oracle problem and explain why differential testing against another DBMS is not a solution.

  <details><summary>Answer</summary>

  The oracle problem: generating a valid random query is cheap;
  deciding whether its result is *correct* requires knowing the
  answer, and nothing knows the answer.

  Differential testing fails twice. **False positives**: dialects
  legitimately differ (MySQL's 0/1 booleans, SQLite's type
  affinity), so most disagreements are not bugs and triaging them
  costs more than the bugs are worth. **False negatives**: any bug
  the two implementations share — and shared misreadings of the
  standard are common — produces no disagreement.

  There is a third, quieter reason: you would have to *have* a second
  implementation. PQS §4.7 Table 4 shows SQLancer needed 6,501 LOC
  for SQLite alone, against SQLite's 49,703 — and that is just an
  expression evaluator, not an engine.

  </details>

- [ ] You can explain rectification: how any random predicate is made TRUE on the pivot row, and why three-valued logic makes that delicate.

  <details><summary>Answer</summary>

  Evaluate the generated expression on the pivot's concrete values
  using the DBMS's own semantics, then wrap by the result: TRUE →
  leave it, FALSE → wrap in `NOT`, NULL → wrap in `IS NULL` (PQS
  §3.2, Algorithm 3).

  Three-valued logic makes it delicate because the NULL arm cannot
  use `NOT`: `NOT NULL` is `NULL`, not TRUE, so a two-valued
  rectifier would silently produce a predicate that drops the pivot
  and report a bug on every NULL. It needs a *different operator*,
  `IS NULL`, which is the only thing in SQL that converts unknown to
  known.

  The payoff: instead of discarding roughly two thirds of generated
  predicates, all of them become usable — and the NULL-valued third,
  which is where the bugs are, becomes over-represented rather than
  absent.

  </details>

- [ ] You can construct a bug PQS provably misses, using containment rather than equality.

  <details><summary>Answer</summary>

  Any bug that returns *extra* rows. PQS checks that the pivot row is
  contained in the result set; a `JOIN` that emits every matching row
  twice still contains the pivot, so the containment query returns
  non-empty and the oracle is satisfied.

  The paper concedes exactly this: "we cannot detect logic bugs where
  a DBMS erroneously fetches duplicate rows."

  The symmetric point is worth making: TLP catches that one (the
  partitions won't reconstruct) but misses bugs that are *symmetric*
  across all three partitions — if a scan drops the same row from the
  whole and from the `p` partition, both sides shrink together and
  the identity still holds. Neither oracle is complete; they have
  different holes.

  </details>

- [ ] You can write the TLP identity and say why `col = col` is a useless partitioning predicate.

  <details><summary>Answer</summary>

  `result(Q) = result(Q WHERE p) ⊎ result(Q WHERE NOT p) ⊎ result(Q
  WHERE p IS NULL)`, where `⊎` is multiset addition — `UNION ALL`,
  per TLP Table 1's `WHERE` row.

  `col = col` is degenerate because it is TRUE for every non-NULL row
  and NULL for every NULL row: the `NOT p` partition is always empty,
  and the partition boundary falls exactly on "is this column NULL",
  which the engine already special-cases. The identity still holds,
  so no bug is found — it burns a run.

  The general lesson: the value of a partitioning predicate is
  proportional to how *unevenly and unpredictably* it cuts the rows,
  which is why the generator wants deep, mixed-type expressions
  rather than simple comparisons. It is also why TLP's coverage
  barely moves when you add oracles (§5.3: 55.3% → 56.1%) but
  collapses without a generator (48.3% from generation alone).

  </details>

- [ ] You can state the trade the two papers make — completeness for portability — and which one you would reach for first.

  <details><summary>Answer</summary>

  PQS buys *completeness for one row* by paying for a per-dialect
  expression evaluator (§3.2: the `LIKE` operator alone is over 50
  LOC in SQLancer). TLP buys *portability* by knowing nothing about
  the answer beyond an identity three queries must satisfy.

  Reach for TLP first, and the evidence is not opinion: TLP's `WHERE`
  oracle alone found 60 of its 77 logic bugs (§4 Table 4), it needs
  no evaluator, and the SQLancer README calls it "among the most
  widely adopted testing techniques" (`README.md:82`) while PQS is
  "currently unmaintained" (`:80`).

  Reach for PQS when you have a bug class TLP is structurally blind
  to — most usefully, when you suspect the *expression evaluator*
  rather than the optimizer, since TLP runs the same expression on
  all four sides and a wrong-but-consistent evaluator satisfies it.

  </details>

- [ ] You can say what fraction of each paper's bugs came from its headline oracle, and what the rest came from.

  <details><summary>Answer</summary>

  **PQS** (§4.2 Table 3): 61 of 99 true bugs (61.6%) from the
  containment oracle; 34 from unexpected errors, 4 from segfaults.

  **TLP** (§4 Table 4): of 175 true bugs, 77 were logic bugs, 62 were
  error bugs and 25 were crashes — and within the 77, the `WHERE`
  oracle found 60 (77.9%).

  **NoREC** (§4.3 Table 3): of 159, only 51 were logic bugs; 58 were
  error bugs and 50 were crashes (23 release, 27 debug).

  The pattern: in all three papers roughly half the yield is "the
  generator made the engine crash or raise an error it shouldn't
  have" — which requires no oracle at all. Build the generator and
  the error-log arm first; the oracle is the second half of the
  value, not the first.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including your first three TLP recombinations for M16.

  <details><summary>Answer</summary>

  No unfoldable answer — this one is the writing. For question 5, TLP
  Table 1 is the template: pick the clause, then the `⋄` that
  reconstructs it. `WHERE` → `⊎` (`UNION ALL` of three `MATCH`es);
  `count(*)` → `SUM` of the three counts, not `COUNT`; `collect` →
  list concatenation, which is `⊎` again but forces you to decide
  whether order is part of the contract.

  Whichever three you pick, check them against
  [reading-sqlancer.md](reading-sqlancer.md)'s Step 4: SQLancer's own
  comparator checks size then `HashSet` equality
  (`ComparatorHelper.java:91, 108-112`), which is weaker than `⊎`.
  Write yours to compare multiplicities and you will already have a
  sharper oracle than the reference implementation.

  </details>

## References

**Papers**
- Rigger & Su — "Testing Database Engines via Pivoted Query
  Synthesis" (OSDI 2020,
  [arXiv:2001.04174](https://arxiv.org/abs/2001.04174)) — §3.1 pivot
  selection, §3.2 Algorithms 1–3 (the rectified-queries core), §4.2
  Tables 2–3 (123 reports / 99 true / 61 from containment), §4.3
  (3.71 LOC average reduced test), §4.7 Table 4 (LOC and coverage
  per DBMS)
- Rigger & Su — "Detecting Optimization Bugs in Database Engines via
  Non-Optimizing Reference Engine Construction" (ESEC/FSE 2020) —
  §3.1 the `(φ IS TRUE)` transformation, §3.3 the content-comparison
  extension, §4.3 Tables 2–3 (159 true / 141 fixed / 51 logic)
- Rigger & Su — "Finding Bugs in Database Systems via Query
  Partitioning" (OOPSLA 2020) — §2.1 self-decomposable vs
  composable, Table 1 the nine oracles and their `⋄`, §4 Tables 3–4
  (175 true / 125 fixed / 77 logic, 60 of them from `WHERE`), §5.2
  the five NoREC-insufficient cases, §5.3 the DuckDB coverage
  measurement

**Code**
- [sqlancer](https://github.com/sqlancer/sqlancer) @ `af6ae85` —
  both papers as running code; walked in
  [reading-sqlancer.md](reading-sqlancer.md)
- turso's independent TLP implementation —
  `simulator/generation/property.rs:1073-1177`, walked in
  [reading-turso-simulator.md](reading-turso-simulator.md) Step 5

| Paper section | What to take from it |
|---|---|
| PQS §3.1 | pivot = one row from *each* table, not one row total |
| PQS §3.2 | Algorithms 1–3; `LIKE` alone is >50 LOC of evaluator |
| PQS §4.2 T2–T3 | 123 reported, 99 true, 61 from the containment oracle |
| PQS §4.7 T4 | 6,501 LOC for SQLite; 43.0% / 24.4% / 23.7% line coverage |
| NoREC §3.1 | `SELECT * FROM t WHERE φ` → `SELECT (φ IS TRUE) FROM t` |
| NoREC §4.3 T3 | 159 true, 51 logic — the rest errors and crashes |
| TLP §2.1 | self-decomposable vs composable; AVG needs `(sum, count)` |
| TLP Table 1 | nine oracles, `⊎` vs `∪`, COUNT recombines with SUM |
| TLP §4 T4 | 77 logic bugs; `WHERE` found 60 of them |
| TLP §5.3 | 48.3% coverage from generation alone, 56.1% with every oracle |
