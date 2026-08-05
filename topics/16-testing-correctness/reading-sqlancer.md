# SQLancer: 450+ bugs from three tiny oracles

SQLancer turned the PQS/TLP papers into running code and found 450+
bugs in SQLite/MySQL/Postgres/DuckDB/CockroachDB — and each oracle's
core check is a handful of lines. This chapter builds the three
oracles from first principles — the test-oracle problem, SQL's third
truth value, then PQS, TLP, and NoREC one at a time — before walking
the oracle base classes (`src/sqlancer/common/oracle/`), not the
per-DBMS adapters. The comparative table at the end is what you
carry into M16's Cypher oracles.

Every anchor below is SQLancer at commit **`af6ae85`**, the revision
this repo pins (`resources/codebases.md`), quoted with the line
numbers the code occupies at that commit. Where the code and the
papers disagree — and they do, twice — this chapter shows both.

Start with the headline, because it is the first thing to make
honest. The repo's own README claims only that "SQLancer has found
hundreds of bugs" (`README.md:6`); there is no 450 anywhere in the
tree. The number is a sum of the three founding papers' own
evaluations:

```
 PQS   (OSDI '20, §4.2 Table 2)      123 true bugs   SQLite, MySQL, PostgreSQL
 NoREC (ESEC/FSE '20, §4.3 Table 2)  159 true bugs   SQLite, MariaDB, PostgreSQL, CockroachDB
 TLP   (OOPSLA '20, §4 Table 3)      175 true bugs   SQLite, MySQL, CockroachDB, TiDB, DuckDB
                                     ───
                                     457 true bugs across three papers
```

457, from three oracles, in five years of engine-decades. The DBMS
list in the title is right as a union but incomplete: MariaDB and
TiDB belong on it too.

## The problem in one sentence

Generating a million random SQL queries is trivial; knowing the
correct answer to even ONE of them requires a second correct
database — SQLancer's three oracles each manufacture ground truth
from nothing, and found 450+ real bugs doing it.

## The concepts, step by step

### Step 1 — the test-oracle problem

> **In:** a generator that can emit valid random SQL at
> microsecond cost.
> **Out:** nothing usable — until something can decide whether a
> result is wrong.

An **oracle** is whatever tells a test harness that a result is
wrong. For random inputs, the oracle is the hard part: a generator
can emit `SELECT * FROM t0 JOIN t1 ON ... WHERE (c3 << 2) IS NOT
FALSE` in microseconds, but nothing knows what that should return.

**Differential testing** — comparing two DBMSs against each other —
fails for SQL, for two independent reasons: dialects legitimately
diverge (a disagreement is usually not a bug), and a bug both
systems share is invisible. The escape is **metamorphic testing**:
instead of knowing Q's answer, know a *relationship* between Q and a
derived query Q' that must hold if the engine is correct. All three
SQLancer oracles are one choice of relationship each.

Why it matters: every design decision downstream — including which
oracle is cheap enough to keep maintaining — follows from the fact
that ground truth is the scarce resource, not test inputs.

### Step 2 — SQL is three-valued: TRUE, FALSE, and NULL

> **In:** a **predicate** — a boolean expression in a `WHERE`
> clause.
> **Out:** one of *three* values, not two.

Every SQLancer oracle leans on one fact a smart programmer from
outside databases won't expect: a SQL predicate evaluates to TRUE,
FALSE, or NULL ("unknown": `NULL = 5` is neither true nor false).
`WHERE` keeps only rows where the predicate is TRUE; rows where it
is FALSE *or NULL* are dropped.

Most real optimizer bugs live exactly in the NULL cases, because
programmers — including the ones writing optimizers — reason
two-valued by default. Concretely: `p OR NOT p` is a tautology in
two-valued logic and is **not** one in SQL, because it evaluates to
NULL whenever `p` does.

Why it matters: Steps 3, 4 and 5 each need a different answer to
"what do I do with the third value", and each answer is a different
line of code.

### Step 3 — PQS: pick one row per table, force the query to contain it

> **In:** a populated database, at least one row per table.
> **Out:** a query guaranteed to return a chosen row — and a
> containment check that fails if it doesn't.

**Pivoted Query Synthesis** manufactures ground truth for exactly
one row. The paper's step 2 is more specific than "pick a row": "We
then select a random row from **each** of the tables (see step 2),
to which we refer as the pivot row" (PQS §3.1). With three tables in
the `FROM` clause the pivot is a row of the cross product, one
component per table — which is what makes `t0.c0, t1.c0, t2.c1` a
legal thing to compare against.

The skeleton lives in one abstract class:

```java
// src/sqlancer/common/oracle/PivotedQuerySynthesisBase.java — check(), 36-53
    36      @Override
    37      public final void check() throws Exception {
    38          rectifiedPredicates.clear();
    39          Query<C> pivotRowQuery = getRectifiedQuery();
// ... 40-42: optional logging of the rectified query ...
    43          Query<C> isContainedQuery = getContainmentCheckQuery(pivotRowQuery);
// ... 44-47: logging ...
    48          // combines step 6 and 7 described in the PQS paper
    49          boolean pivotRowIsContained = containsRows(isContainedQuery);
    50          if (!pivotRowIsContained) {
    51              reportMissingPivotRow(pivotRowQuery);
    52          }
    53      }
```

Four lines of logic. `getRectifiedQuery` (declared abstract at
`:125`, documented as "steps 2-5 of the PQS paper") synthesizes a
`WHERE` clause the pivot must satisfy; `getContainmentCheckQuery`
(`:114`) wraps it into a query that returns *at least one row* iff
the pivot is present; `containsRows` (`:66-73`) reduces the whole
oracle to "did anything come back". The failure report at `:75-99`
dumps not just the pivot but every rectified predicate with the
value PQS expected it to take — which is the difference between a
bug report and a bug report someone can act on.

Rectification (Step 4 of
[reading-pqs-tlp-papers.md](reading-pqs-tlp-papers.md)) is what
makes every randomly generated expression usable rather than the
third that happen to be TRUE. The price is the class comment at
`:19-22`: `rectifiedPredicates` holds "the predicates used in WHERE
and JOIN clauses, which yield TRUE for the pivot row" — SQLancer has
to *know* they yield TRUE, which means implementing its own
expression evaluator per dialect.

That price is why PQS is where it is today. The repo README is
explicit:

> "PQS effectively detects bugs, but requires more implementation
> effort than other testing approaches that follow a metamorphic
> testing or differential testing methodology. Thus, it is currently
> unmaintained." — `README.md:80`

**Unmaintained, not removed.** At `af6ae85` the base class is live
and eight DBMS still have PQS tests (`test/sqlancer/dbms/Test*PQS.java`
for Databend, Doris, Materialize, MySQL, OceanBase, Postgres,
SQLite, YSQL) against fifteen for TLP. Reading it is still the
fastest way to understand what the other two oracles bought by
giving up ground truth.

Why it matters: this is the only oracle in the tree that knows a
*fact* about the answer. Everything else knows only a relationship.

### Step 4 — TLP: partition by a predicate, demand the pieces sum

> **In:** one original query with **no** `WHERE` clause, and one
> randomly generated predicate `p`.
> **Out:** four result sets — the original, and three partitions —
> that must reconcile.

**Ternary Logic Partitioning** needs no evaluator at all. Any
predicate `p` splits a query's rows into exactly three disjoint
groups — the rows where `p` is TRUE, FALSE, and NULL (Step 2) — so
the whole must equal the union of the parts:

```
 Q:        SELECT <cols> FROM t [JOIN ...]      -- WHERE explicitly cleared
 Q_p:      ... WHERE p
 Q_notp:   ... WHERE NOT p
 Q_null:   ... WHERE p IS NULL
 paper's assertion (OOPSLA '20 Table 1, WHERE row):
           RS(Q) = RS(Q_p) ⊎ RS(Q_notp) ⊎ RS(Q_null)     -- ⊎ = multiset addition
```

The generic implementation is 44 lines:

```java
// src/sqlancer/common/oracle/TLPWhereOracle.java — check(), 75-118 (elided)
    75      @Override
    76      public void check() throws SQLException {
// ... 77-87: pick non-empty tables, generate the select, joins, from-list ...
    88          select.setWhereClause(null);
    89
    90          String originalQueryString = select.asString();
// ... 91-93: run it, keep firstResultSet ...
    95          boolean orderBy = Randomly.getBooleanWithSmallProbability();
// ... 96-98: if orderBy, attach ORDER BY clauses ...
   100          TestOracleUtils.PredicateVariants<E, C> predicates = TestOracleUtils.initializeTernaryPredicateVariants(gen,
   101                  gen.generateBooleanExpression());
   102          select.setWhereClause(predicates.predicate);
   103          String firstQueryString = select.asString();
   104          select.setWhereClause(predicates.negatedPredicate);
   105          String secondQueryString = select.asString();
   106          select.setWhereClause(predicates.isNullPredicate);
   107          String thirdQueryString = select.asString();
   108
   109          List<String> combinedString = new ArrayList<>();
   110          List<String> secondResultSet = ComparatorHelper.getCombinedResultSet(firstQueryString, secondQueryString,
   111                  thirdQueryString, combinedString, !orderBy, state, errors);
   112
   113          ComparatorHelper.assumeResultSetsAreEqual(firstResultSet, secondResultSet, originalQueryString, combinedString,
   114                  state);
```

Line 88 is the one people miss: the "original" query is built by
*clearing* the `WHERE` clause, so the original and the three
partitions differ by exactly the predicate under test. The three
variants come from one generator call (`:100-101`) into
`TernaryLogicPartitioningOracleBase`'s trio — `predicate`,
`negatedPredicate`, `isNullPredicate`
(`TernaryLogicPartitioningOracleBase.java:19-21`, built at `:34-51`
via `gen.negatePredicate` and `gen.isNull`).

The `!orderBy` at line 111 selects the recombination strategy:
`getCombinedResultSet` (`ComparatorHelper.java:144-163`) builds one
`firstQuery UNION ALL secondQuery UNION ALL thirdQuery` string when
`asUnion` is true (`:148-152`), and otherwise runs the three
separately and concatenates client-side (`:153-161`). An `ORDER BY`
inside a `UNION ALL` arm is not portable, so the presence of an
`ORDER BY` forces the client-side path.

Now read the assertion, because it is weaker than the `⊎` above:

```java
// src/sqlancer/ComparatorHelper.java — assumeResultSetsAreEqual, 89-112 (elided)
    89      public static void assumeResultSetsAreEqual(List<String> resultSet, List<String> secondResultSet,
    90              String originalQueryString, List<String> combinedString, SQLGlobalState<?, ?> state) {
    91          if (resultSet.size() != secondResultSet.size()) {
// ... 92-105: format and throw "The size of the result sets mismatch (%d and %d)!" ...
   106          }
   107
   108          Set<String> firstHashSet = new HashSet<>(resultSet);
   109          Set<String> secondHashSet = new HashSet<>(secondResultSet);
   110
   111          boolean validateResultSizeOnly = state.getOptions().validateResultSizeOnly();
   112          if (!validateResultSizeOnly && !firstHashSet.equals(secondHashSet)) {
```

Equal size, then equal *set*. Work the gap:

```
 whole  = {a, a, b}      size 3, HashSet {a, b}
 parts  = {a, b, b}      size 3, HashSet {a, b}

 line 91   3 == 3                   → no throw
 line 112  {a,b}.equals({a,b})      → no throw
 verdict:  PASS

 multiset addition would require count(a) = 2 on both sides.
 The code never counts.
```

A duplicate-multiplicity bug passes. turso's independent
implementation of the same paper has the identical gap — size at
`generation/property.rs:1138`, containment both ways at `:1146-1177`
(see [reading-turso-simulator.md](reading-turso-simulator.md) Step
5). Two implementations, one paper, the same corner cut.

One more narrowing worth knowing: the "result set" being compared is
one column wide. `getResultSetFirstColumnAsString`
(`ComparatorHelper.java:39-87`) reads `result.getString(1)` — column
1 only, line 61 — and strips trailing zeros from decimals at line
63, with the comment "as many DBMS treat it as non-bugs". That is
why `TLPWhereOracle` calls `gen.generateFetchColumns(true)` with
`shouldCreateDummy = true` (`:84-85`): the query is shaped to
produce one comparable column.

Why it matters: "we implemented TLP" is a claim about which queries
you send. Whether you implemented the *oracle* is a claim about how
you compare — and the comparison is where two independent
implementations both stopped short.

### Step 5 — NoREC: run the same predicate where the optimizer can't help

> **In:** one randomly generated predicate `φ` and a table.
> **Out:** two integers that must be equal.

NoREC ("non-optimizing reference engine construction") targets the
optimizer specifically, by making the engine compute the same
predicate two ways — once where the planner can optimize, once where
it can't. The paper's transformation (§3.1) is `SELECT * FROM t0
WHERE φ` → `SELECT (φ IS TRUE) FROM t0`; SQLancer's SQLite
implementation wraps that in a `SUM`:

```java
// src/sqlancer/sqlite3/gen/SQLite3ExpressionGenerator.java — 783-792
   783      @Override
   784      public String generateUnoptimizedQueryString(SQLite3Select select, SQLite3Expression whereCondition) {
   785          SQLite3PostfixUnaryOperation isTrue = new SQLite3PostfixUnaryOperation(PostfixUnaryOperator.IS_TRUE,
   786                  whereCondition);
   787          SQLite3PostfixText asText = new SQLite3PostfixText(isTrue, " as count", null);
   788          select.setFetchColumns(Arrays.asList(asText));
   789          select.setWhereClause(null);
   790
   791          return "SELECT SUM(count) FROM (" + select.asString() + ")";
   792      }
```

So the pair actually sent is:

```
 optimized:    SELECT COUNT(*) FROM t WHERE φ            (planner ON)
               -- or SELECT * FROM t WHERE φ, counted client-side
 unoptimized:  SELECT SUM(count) FROM (
                 SELECT (φ) IS TRUE as count FROM t )    (full scan + per-row eval)
```

`IS TRUE` (line 785) is the load-bearing operator and the reason
this is a *SQL* technique rather than a generic one: it collapses
Step 2's three values to two, mapping both FALSE and NULL to 0, so
the sum is exactly the count of rows `WHERE φ` would have kept. Drop
`IS TRUE` and NULL rows would poison the sum.

The comparison is cardinality only:

```java
// src/sqlancer/common/oracle/NoRECOracle.java — check(), 72-93 (elided)
    72          boolean shouldUseAggregate = Randomly.getBoolean();
    73          String optimizedQueryString = gen.generateOptimizedQueryString(select, randomWhereCondition,
    74                  shouldUseAggregate);
// ... 75-79: logging ...
    80          String unoptimizedQueryString = gen.generateUnoptimizedQueryString(select, randomWhereCondition);
// ... 81-83: logging ...
    85          int optimizedCount = shouldUseAggregate ? extractCounts(optimizedQueryString, errors, state)
    86                  : countRows(optimizedQueryString, errors, state);
    87          int unoptimizedCount = extractCounts(unoptimizedQueryString, errors, state);
// ... 89-91: a -1 from either side means "ignore this run" ...
    93          if (unoptimizedCount != optimizedCount) {
```

`countRows` (`:123-146`) counts result rows; `extractCounts`
(`:148-170`) sums `rs.getInt(1)` across rows (line 157). The
`shouldUseAggregate` coin at line 72 decides whether the optimized
side is `COUNT(*)` (summed) or `SELECT *` (row-counted) — two
different plan shapes for the same predicate, for free.

Forcing the predicate into the SELECT list defeats index use and
**predicate pushdown** (moving a filter earlier in the plan) — same
semantics, no optimizer. A count mismatch means the optimizer
changed RESULTS, not just speed.

The cardinality-only comparison is a real, measured limitation, and
the TLP paper measured it: re-deriving NoREC test cases from the 60
bugs TLP's `WHERE` oracle found, "in 5 of these cases, comparing the
record count was insufficient to detect the bug; also the contents
had to be compared, contrary to prior suggestions" (TLP §5.2). Five
of forty-eight.

Why it matters: NoREC is the cheapest of the three to implement and
the second most productive (51 logic bugs, NoREC §4.3 Table 3) — and
its blind spot is a single design choice you can see in one line.

### Step 6 — composition: three lenses, one schema, round-robin

> **In:** a list of `TestOracle`s and one generated
> schema-plus-data.
> **Out:** each generated database state exercised by every oracle
> in turn.

Each oracle has a blind spot, and they don't overlap:

| oracle | needs own evaluator | compares | finds | blind to |
|---|---|---|---|---|
| PQS | YES (per dialect) | containment of one pivot row | expression-evaluation bugs | anything about rows *other* than the pivot |
| TLP | no | size + set of the first column | optimizer logic bugs, aggregates, DISTINCT, GROUP BY | duplicate multiplicity; bugs symmetric across all three partitions |
| NoREC | no | one integer | pushdown / index / filter bugs | content bugs that preserve cardinality; anything both paths share |

They compose — and the composition is deterministic, not random:

```java
// src/sqlancer/common/oracle/CompositeTestOracle.java — check(), 19-31
    19      @Override
    20      public void check() throws Exception {
    21          try {
    22              oracles.get(i).check();
    23              iLast = i;
    24              boolean lastOracleIndex = i == oracles.size() - 1;
    25              if (!lastOracleIndex) {
    26                  globalState.getManager().incrementSelectQueryCount();
    27              }
    28          } finally {
    29              i = (i + 1) % oracles.size();
    30          }
    31      }
```

Line 29 is a **round-robin** in a `finally` block: the index advances
even when the oracle throws, so a crashing oracle can't monopolise
the rotation. With `k` oracles registered, each generated database
state gets `1/k` of the checks — which is the argument for keeping
`k` small and each oracle cheap.

That composition — cheap oracles with disjoint blind spots over one
generator — is the design M16's Cypher oracles copy. Note also what
`af6ae85` has grown beyond the three: the README table
(`README.md:78-87`) lists eight techniques — PQS, NoREC, TLP, DQE
(ICSE '23), QPG (ICSE '23), CERT (ICSE '24), DQP (SIGMOD '24), and
CODDTest (SIGMOD '25) — and `src/sqlancer/common/oracle/` carries
base classes for `CERTOracle`, `CODDTestBase` and `DQEBase`
alongside the three this chapter reads.

Why it matters: the blind-spot table, not the bug count, is what
tells you which oracle to write next.

## Where each step lives in the code

Read the base classes, not the per-DBMS adapters:

| anchor | step | what it is |
|---|---|---|
| `README.md:6` | — | "SQLancer has found hundreds of bugs" — the repo's own claim |
| `README.md:78-87` | 6 | the eight-technique table (PQS…CODDTest) with venues |
| `README.md:80` | 3 | PQS "is currently unmaintained" — the reason, in the authors' words |
| `common/oracle/PivotedQuerySynthesisBase.java:14` | 3 | the PQS skeleton class declaration |
| `common/oracle/PivotedQuerySynthesisBase.java:19-22` | 3 | `rectifiedPredicates` — "yield TRUE for the pivot row" |
| `common/oracle/PivotedQuerySynthesisBase.java:30` | 3 | `pivotRow` — the chosen row |
| `common/oracle/PivotedQuerySynthesisBase.java:36-53` | 3 | `check()`: rectified query → containment query → "pivot missing" = bug |
| `common/oracle/PivotedQuerySynthesisBase.java:66-73` | 3 | `containsRows` — the whole oracle is "did anything come back" |
| `common/oracle/PivotedQuerySynthesisBase.java:75-99` | 3 | the failure report: pivot + every predicate's expected value |
| `common/oracle/TernaryLogicPartitioningOracleBase.java:19-21` | 4 | `predicate` / `negatedPredicate` / `isNullPredicate` |
| `common/oracle/TernaryLogicPartitioningOracleBase.java:34-51` | 4 | the trio built via `negatePredicate` and `isNull` |
| `common/oracle/TLPWhereOracle.java:75-118` | 4 | `check()`: clear WHERE (`:88`), three variants, compare |
| `ComparatorHelper.java:39-87` | 4 | `getResultSetFirstColumnAsString` — **column 1 only** (`:61`) |
| `ComparatorHelper.java:89-130` | 4 | `assumeResultSetsAreEqual` — size (`:91`) then `HashSet` (`:108-112`) |
| `ComparatorHelper.java:144-163` | 4 | `getCombinedResultSet` — one `UNION ALL` or three client-side runs |
| `common/oracle/NoRECOracle.java:59-111` | 5 | `check()`: two counts, `!=` is the bug (`:93`) |
| `common/oracle/NoRECOracle.java:123-170` | 5 | `countRows` vs `extractCounts` (`SUM` of column 1 at `:157`) |
| `sqlite3/gen/SQLite3ExpressionGenerator.java:765-792` | 5 | the actual SQL: `COUNT(*)` vs `SUM(count)` over `(φ) IS TRUE` |
| `common/oracle/CompositeTestOracle.java:19-31` | 6 | round-robin over oracles, advancing in `finally` |

Reading order: PQS base class first (it's the most mechanical), then
the TLP pair plus `ComparatorHelper` — the comparator is where the
oracle actually is — then NoREC with one concrete generator beside
it, because the base class alone never shows you the SQL.

## Questions for notes.md

1. PQS checks ONE row per query. Why is that enough in expectation
   (think: bugs are input-conditioned, generation is cheap)?
2. Rectification: predicate evaluates NULL on the pivot. Show why
   `WHERE p` loses the row but `WHERE p IS NULL` keeps it.
3. Write the TLP identity for `COUNT(*)` and for `MAX(c)` — which
   aggregate makes the partition check subtle, and why?
4. turso's `SelectSelectOptimizer` / `WhereTrueFalseNull` properties
   (reading-turso-simulator.md) — map each to PQS/TLP/NoREC.
5. Cypher TLP for M16: partition `MATCH (a)-[e]->(b) WHERE p` — what
   plays the role of NULL in a graph pattern (missing property!),
   and what's the union assertion?

## Done when

Answer each before unfolding it.

- [ ] You can describe all three oracles — PQS, TLP, NoREC — in one sentence each, and say what each one compares.

  <details><summary>Answer</summary>

  **PQS**: pick one row per table, synthesize a `WHERE` clause you
  have proved TRUE on that pivot, and check the pivot comes back —
  comparing *containment of one row*
  (`PivotedQuerySynthesisBase.java:49-52`).

  **TLP**: run a query with no `WHERE`, then the same query filtered
  by `p`, `NOT p`, and `p IS NULL`, and check the three partitions
  reconstruct the whole — comparing *size plus set of the first
  column* (`ComparatorHelper.java:91, 108-112`).

  **NoREC**: run `... WHERE φ` against `SELECT SUM(count) FROM
  (SELECT (φ) IS TRUE as count FROM t)` and check the two agree —
  comparing *one integer* (`NoRECOracle.java:93`).

  </details>

- [ ] You can state PQS's current status in SQLancer and why, without using the word "removed".

  <details><summary>Answer</summary>

  It is **unmaintained but present**. `README.md:80`: "PQS effectively
  detects bugs, but requires more implementation effort than other
  testing approaches that follow a metamorphic testing or differential
  testing methodology. Thus, it is currently unmaintained."

  At `af6ae85`, `PivotedQuerySynthesisBase.java` is a live 138-line
  class and eight DBMS still carry `Test*PQS.java` — against fifteen
  `Test*TLP.java`. The cause is Step 3's price: PQS is the only oracle
  that needs a per-dialect expression evaluator, and there are
  nineteen supported DBMS (`README.md:72`).

  </details>

- [ ] You can explain why NoREC's `IS TRUE` wrapper is not decoration.

  <details><summary>Answer</summary>

  `SQLite3ExpressionGenerator.java:785` wraps the predicate in
  `IS_TRUE` before summing it. That collapses SQL's three values
  (Step 2) to two: TRUE → 1, and **both** FALSE and NULL → 0. The sum
  is then exactly the number of rows `WHERE φ` would have kept, which
  is the quantity the optimized side produces.

  Without it, a NULL-valued predicate would contribute NULL to the
  sum and — depending on the engine — either poison the total or be
  silently skipped. Either way the two sides would no longer be
  comparing the same thing, and every NULL-bearing row would generate
  a false alarm.

  </details>

- [ ] You can say why TLP's implemented check is weaker than the paper's identity, and construct an input that slips through.

  <details><summary>Answer</summary>

  The paper's composition operator for the `WHERE` oracle is `⊎`,
  multiset addition (OOPSLA '20 Table 1). `assumeResultSetsAreEqual`
  checks `resultSet.size() != secondResultSet.size()`
  (`ComparatorHelper.java:91`) and then `HashSet` equality
  (`:108-112`) — size plus set, which is strictly weaker.

  `{a, a, b}` vs `{a, b, b}`: same size (3), same set (`{a, b}`).
  Passes. Any bug that changes *how many copies* of a row come back —
  a join emitting a duplicate, a `UNION ALL` arm dropping one copy —
  is invisible.

  It is narrower still than that: only column 1 is compared
  (`ComparatorHelper.java:61`), and trailing decimal zeros are
  stripped (`:63`). And `--validate-result-size-only` (`:111`)
  degrades TLP to a NoREC-style cardinality check on purpose.

  </details>

- [ ] You can explain how the oracles are scheduled when several are enabled, and why that argues for keeping each one cheap.

  <details><summary>Answer</summary>

  `CompositeTestOracle.check()` (`:19-31`) is a **round-robin**:
  `i = (i + 1) % oracles.size()` at line 29, inside a `finally`, so
  the index advances even when an oracle throws. It is not a random
  choice, and one oracle cannot starve the others.

  Consequence: with `k` oracles, each generated database state is
  examined by each oracle once every `k` checks — so the marginal
  oracle costs `1/k` of every other oracle's throughput. That is the
  argument for the TLP paper's own finding (§5.3) that the `WHERE`
  oracle alone found 60 of 77 logic bugs while all five oracles
  together raised DuckDB line coverage only from 55.3% to 56.1%.

  </details>

- [ ] You can sketch a Cypher TLP partition for `MATCH (a)-[e]->(b) WHERE p` and name what makes graph patterns harder than SQL rows here.

  <details><summary>Answer</summary>

  The partition itself transfers directly: run the pattern with no
  predicate, then with `p`, `NOT p`, and `p IS NULL`, and require the
  three to reconstruct the whole under multiset addition — being
  careful to *count* multiplicities rather than repeating
  `ComparatorHelper`'s size-plus-set shortcut.

  What is harder: in SQL, NULL arises from a value; in a property
  graph, a property can be **absent from the node entirely**, and
  `a.age > 30` on a node with no `age` is a third state that also has
  to land in the `IS NULL` partition. And the "row" being compared is
  a whole path binding `(a, e, b)`, so equality is structural — which
  makes the multiplicity question sharper, not softer, than in SQL.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  No unfoldable answer — this one is the writing. Question 4's mapping
  is the one to get right, and
  [reading-turso-simulator.md](reading-turso-simulator.md) Step 4
  gives it away: `SelectSelectOptimizer` is NoREC (its doc comment at
  `model/property.rs:142-148` cites the NoREC paper by name), and
  `WhereTrueFalseNull` is TLP (`:153-160` cites the TLP paper). turso
  implements no PQS-shaped property at all — consistent with
  `README.md:80`'s reason.

  </details>

## References

**Papers**
- Rigger & Su — "Testing Database Engines via Pivoted Query
  Synthesis" (OSDI 2020) — §3.1 for pivot selection, §4.2 Table 2
  for the 123 bugs
- Rigger & Su — "Detecting Optimization Bugs in Database Engines via
  Non-Optimizing Reference Engine Construction" (ESEC/FSE 2020) —
  §3.1 for the `(φ IS TRUE)` transformation, §4.3 Table 3 for the 51
  logic bugs
- Rigger & Su — "Finding Bugs in Database Systems via Query
  Partitioning" (OOPSLA 2020) — Table 1 for the composition
  operators, §5.2 for the count-vs-content measurement
- All three walked in
  [reading-pqs-tlp-papers.md](reading-pqs-tlp-papers.md)

**Code**
- [sqlancer](https://github.com/sqlancer/sqlancer) @ `af6ae85` —
  `src/sqlancer/common/oracle/` — read the base classes, not the
  per-DBMS adapters

| File | Lines | What |
|---|---|---|
| `README.md` | 6, 72, 78-87 | bug claim, supported DBMS, the eight-technique table |
| `common/oracle/PivotedQuerySynthesisBase.java` | 14-53, 66-99 | PQS: rectify → containment → report |
| `common/oracle/TernaryLogicPartitioningOracleBase.java` | 19-21, 34-51 | the three predicate variants |
| `common/oracle/TLPWhereOracle.java` | 75-118 | the TLP `WHERE` oracle end to end |
| `ComparatorHelper.java` | 39-87, 89-130, 144-163 | first-column extraction, the actual comparison, recombination |
| `common/oracle/NoRECOracle.java` | 59-111, 123-170 | NoREC: two counts, `!=` is the bug |
| `sqlite3/gen/SQLite3ExpressionGenerator.java` | 765-792 | the concrete optimized / unoptimized SQL pair |
| `common/oracle/CompositeTestOracle.java` | 19-31 | round-robin scheduling of oracles |
