# SQLancer: 450+ bugs from three tiny oracles

SQLancer turned the PQS/TLP papers into running code and found 450+
bugs in SQLite/MySQL/Postgres/DuckDB/CockroachDB — and each oracle's
core check is a handful of lines. This chapter walks the oracle base
classes (`src/sqlancer/common/oracle/`), not the per-DBMS adapters;
the comparative table at the end is what you carry into M16's Cypher
oracles.

## Anchor map

| anchor | what it is |
|---|---|
| PivotedQuerySynthesisBase.java:14 | the PQS skeleton |
| PivotedQuerySynthesisBase.java:30 | `pivotRow` — the chosen row |
| PivotedQuerySynthesisBase.java:37-51 | `check()`: rectified query → containment query → "pivot missing" = bug |
| TLPWhereOracle.java:76-92 | `check()`: original result vs 3-way partition union |
| NoRECOracle.java (reproducer) | `optimizedQuery != unoptimizedQuery` → bug |
| TernaryLogicPartitioningOracleBase.java | generates p / NOT p / p IS NULL |

## 1. PQS — `PivotedQuerySynthesisBase.check()` (:37)

```
 1. pick pivotRow from an existing table (random row)
 2. getRectifiedQuery(): synthesize WHERE that is TRUE on pivotRow
    — generate a random expression, EVALUATE it yourself on the
    pivot; if it's FALSE wrap NOT, if NULL wrap IS NULL (rectify)
 3. getContainmentCheckQuery(): wrap the DB's own result to ask
    "is pivotRow in there?"
 4. containsRows == false → reportMissingPivotRow → BUG
```

The price: step 2 requires SQLancer to implement its OWN expression
evaluator per DBMS dialect (constant folding over one concrete
row). That's why PQS finds *evaluation* bugs — it re-implements
evaluation, and disagreement is a bug in one of the two. Question:
whose bug? How does SQLancer triage false positives where its OWN
evaluator is wrong?

## 2. TLP — `TLPWhereOracle.check()` (:76)

```
 Q:        SELECT * FROM t [JOIN ...]
 Q_p:      ... WHERE p
 Q_notp:   ... WHERE NOT p
 Q_null:   ... WHERE p IS NULL
 assert multiset(Q) == Q_p ⊎ Q_notp ⊎ Q_null
```

No evaluator needed — the DB is checked against ITSELF. The 3-way
split exists because SQL is three-valued: WHERE keeps only TRUE
rows, so FALSE and NULL rows must land in the other partitions.

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

## 3. NoREC — the reproducer lambda

```
 optimized:    SELECT COUNT(*) FROM t WHERE p        (planner ON)
 unoptimized:  SELECT SUM(CASE WHEN p THEN 1 ELSE 0) (scan + eval)
```

Forcing the predicate into the SELECT list defeats index use and
pushdown — same semantics, no optimizer. A count mismatch means the
optimizer changed RESULTS, not just speed. Question: which of our
topic 10 rewrite rules would NoREC exercise, and which are invisible
to it (ordering? LIMIT?)?

## 4. The comparative table

| oracle | needs own evaluator | finds | blind to |
|---|---|---|---|
| PQS | YES (per dialect) | expression eval bugs | bugs off the pivot row |
| TLP | no | optimizer logic bugs | bugs symmetric across partitions |
| NoREC | no | pushdown/index bugs | anything both paths share |

They compose: run all three on the same generated schema/data
(`CompositeTestOracle.java`).

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
