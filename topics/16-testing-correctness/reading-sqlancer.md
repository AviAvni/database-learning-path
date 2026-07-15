# SQLancer: 450+ bugs from three tiny oracles

SQLancer turned the PQS/TLP papers into running code and found 450+
bugs in SQLite/MySQL/Postgres/DuckDB/CockroachDB — and each oracle's
core check is a handful of lines. This chapter builds the three
oracles from first principles — the test-oracle problem, SQL's third
truth value, then PQS, TLP, and NoREC one at a time — before walking
the oracle base classes (`src/sqlancer/common/oracle/`), not the
per-DBMS adapters. The comparative table at the end is what you
carry into M16's Cypher oracles.

## The problem in one sentence

Generating a million random SQL queries is trivial; knowing the
correct answer to even ONE of them requires a second correct
database — SQLancer's three oracles each manufacture ground truth
from nothing, and found 450+ real bugs doing it.

## The concepts, step by step

### Step 1 — the test-oracle problem

An **oracle** is whatever tells a test harness that a result is
wrong. For random inputs, the oracle is the hard part: a generator
can emit `SELECT * FROM t0 JOIN t1 ON ... WHERE (c3 << 2) IS NOT
FALSE` in microseconds, but nothing knows what that should return.
Comparing two DBMSs against each other (differential testing) fails
for SQL — dialects legitimately diverge, and a bug both systems
share is invisible. The escape is **metamorphic testing**: instead
of knowing Q's answer, know a *relationship* between Q and a derived
query Q' that must hold if the engine is correct. All three SQLancer
oracles are one choice of relationship each.

### Step 2 — SQL is three-valued: TRUE, FALSE, and NULL

Every SQLancer oracle leans on one fact a smart programmer from
outside databases won't expect: a SQL predicate (a boolean
expression in a WHERE clause) evaluates to one of THREE values —
TRUE, FALSE, or NULL ("unknown": `NULL = 5` is neither true nor
false). WHERE keeps only rows where the predicate is TRUE; rows
where it is FALSE *or NULL* are dropped. Most real optimizer bugs
live exactly in the NULL cases, because programmers — including the
ones writing optimizers — reason two-valued by default.

### Step 3 — PQS: pick one row, force the query to contain it

Pivoted Query Synthesis manufactures ground truth for exactly one
row. Pick a random existing row (the **pivot**), then *synthesize* a
WHERE clause guaranteed TRUE on it — and if the pivot doesn't come
back, the engine is wrong. The skeleton
(`PivotedQuerySynthesisBase.check()`, :37):

```
 1. pick pivotRow from an existing table (random row)
 2. getRectifiedQuery(): synthesize WHERE that is TRUE on pivotRow
    — generate a random expression, EVALUATE it yourself on the
    pivot; if it's FALSE wrap NOT, if NULL wrap IS NULL (rectify)
 3. getContainmentCheckQuery(): wrap the DB's own result to ask
    "is pivotRow in there?"
 4. containsRows == false → reportMissingPivotRow → BUG
```

Step 2's rectification wrappers (`NOT` for FALSE, `IS NULL` for
NULL) mean every randomly generated expression is usable, not just
the ~1/3 that happen to be TRUE. The price: step 2 requires
SQLancer to implement its OWN expression evaluator per DBMS dialect
(constant folding over one concrete row). That's why PQS finds
*evaluation* bugs — it re-implements evaluation, and disagreement is
a bug in one of the two. Question: whose bug? How does SQLancer
triage false positives where its OWN evaluator is wrong?

### Step 4 — TLP: partition by a predicate, demand the pieces sum

Ternary Logic Partitioning needs no evaluator at all. Any predicate
p splits a query's rows into exactly three disjoint groups — the
rows where p is TRUE, FALSE, and NULL (Step 2) — so the whole must
equal the union of the parts (`TLPWhereOracle.check()`, :76):

```
 Q:        SELECT * FROM t [JOIN ...]
 Q_p:      ... WHERE p
 Q_notp:   ... WHERE NOT p
 Q_null:   ... WHERE p IS NULL
 assert multiset(Q) == Q_p ⊎ Q_notp ⊎ Q_null
```

The DB is checked against ITSELF — the optimizer sees three
different queries and may plan each differently (push p into an
index, rewrite NOT p, …); any semantic slip breaks the identity:

```rust
// TLP: no ground truth needed — the DB is its own oracle
fn tlp_check(db: &Db, q: &Query, p: &Pred) -> Result<(), Bug> {
    let whole = db.run(q);                          // SELECT * FROM t …
    let mut parts = db.run(&q.filter(p));           // WHERE p
    parts.extend(db.run(&q.filter(&not(p))));       // WHERE NOT p
    parts.extend(db.run(&q.filter(&is_null(p))));   // WHERE p IS NULL ← 3-valued!
    if multiset(&whole) != multiset(&parts) {
        return Err(Bug::PartitionMismatch);         // optimizer changed RESULTS
    }
    Ok(())
}
```

Extensions in the codebase: TLP for aggregates (SUM over partitions
must sum), DISTINCT, GROUP BY. Question: why does TLP need the
partitioning predicate p to be deterministic and side-effect free —
what breaks with `random() > 0.5`?

### Step 5 — NoREC: run the same predicate with the optimizer off

NoREC ("non-optimizing reference engine construction") targets the
optimizer specifically, by making the engine compute the same
predicate two ways — once where the planner can optimize, once where
it can't:

```
 optimized:    SELECT COUNT(*) FROM t WHERE p        (planner ON)
 unoptimized:  SELECT SUM(CASE WHEN p THEN 1 ELSE 0) (scan + eval)
```

Forcing the predicate into the SELECT list defeats index use and
predicate pushdown (moving a filter earlier in the plan) — same
semantics, no optimizer. A count mismatch means the optimizer
changed RESULTS, not just speed. Question: which of our topic 10
rewrite rules would NoREC exercise, and which are invisible to it
(ordering? LIMIT?)?

### Step 6 — composition: three lenses, one schema

Each oracle has a blind spot, and they don't overlap:

| oracle | needs own evaluator | finds | blind to |
|---|---|---|---|
| PQS | YES (per dialect) | expression eval bugs | bugs off the pivot row |
| TLP | no | optimizer logic bugs | bugs symmetric across partitions |
| NoREC | no | pushdown/index bugs | anything both paths share |

They compose: run all three on the same generated schema/data
(`CompositeTestOracle.java`). That composition — cheap oracles with
disjoint blind spots over one generator — is the design M16's Cypher
oracles copy.

## Where each step lives in the code

Read the base classes, not the per-DBMS adapters:

| anchor | step | what it is |
|---|---|---|
| PivotedQuerySynthesisBase.java:14 | 3 | the PQS skeleton |
| PivotedQuerySynthesisBase.java:30 | 3 | `pivotRow` — the chosen row |
| PivotedQuerySynthesisBase.java:37-51 | 3 | `check()`: rectified query → containment query → "pivot missing" = bug |
| TernaryLogicPartitioningOracleBase.java | 4 | generates p / NOT p / p IS NULL |
| TLPWhereOracle.java:76-92 | 4 | `check()`: original result vs 3-way partition union |
| NoRECOracle.java (reproducer) | 5 | `optimizedQuery != unoptimizedQuery` → bug |
| CompositeTestOracle.java | 6 | run all oracles over one schema/data |

Reading order: PQS base class first (it's the most mechanical), then
the TLP pair, then NoREC — each `check()` is under 20 lines once you
skip the adapter plumbing.

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

## References

**Papers**
- Rigger & Su — the PQS (OSDI 2020) and TLP (OOPSLA 2020) papers
  behind these classes — see
  [reading-pqs-tlp-papers.md](reading-pqs-tlp-papers.md)

**Code**
- [sqlancer](https://github.com/sqlancer/sqlancer) —
  `src/sqlancer/common/oracle/` — read the base classes
  (`PivotedQuerySynthesisBase`, `TLPWhereOracle`,
  `TernaryLogicPartitioningOracleBase`, `NoRECOracle`), not the
  per-DBMS adapters
