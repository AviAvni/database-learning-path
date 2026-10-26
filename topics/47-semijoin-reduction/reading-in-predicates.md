# IN predicates: when a semi-join is not a semi-join

Everything in this topic so far rests on one move: replace "does this row
have a partner over there?" by a semi-join, and the semi-join by a filter.
SQL's `IN` looks like the most direct way to ask that question, and most
engines plan `x IN (SELECT …)` as a semi-join. This paper shows that the
equivalence holds only while no NULLs are involved, that the general case is
provably hard, and that most systems either get it wrong or go quadratic.
It is the guard rail for every filter-pushdown idea in this topic: a filter
derived from a build side is only valid where a semi-join is.

**"On the Vexing Difficulty of Evaluating IN Predicates"** — Altan Birler,
Thomas Neumann (TUM). CIDR 2026
([pdf](https://www.cidrdb.org/cidr2026/papers/p3-birler.pdf)). Topic 11's
README has the one-paragraph version; this chapter goes through the
algorithm.

## The problem in one sentence

`(3, 4) IN ((1, 2), (3, NULL))` is not FALSE but NULL, so an `IN` evaluator
must find *partial* matches as well as exact ones — and with several nullable
columns nobody knows how to do that in less than quadratic time.

## The concepts, step by step

### Step 1 — three-valued logic

> **In:** nothing. **Out:** the truth tables every later step uses.

SQL comparisons have three outcomes, TRUE, FALSE and **NULL** ("unknown").
`=` returns NULL if either side is NULL. AND and OR return the determined
answer when the non-NULL operands decide it — FALSE AND NULL is FALSE, TRUE
OR NULL is TRUE — and NULL otherwise (§3.1, Figure 2). A WHERE clause keeps a
row only if its predicate is TRUE, so NULL and FALSE look the same there; they
differ under NOT, because NOT NULL is NULL while NOT FALSE is TRUE. The paper
also uses **IS** for `IS NOT DISTINCT FROM`, which treats NULL as an ordinary
value and only returns TRUE or FALSE.

### Step 2 — IN, and the third answer

> **In:** Step 1. **Out:** the definition of IN as a disjunction of
> conjunctions, worked on the paper's opening example.

For a row x = (x₁, …, x_d) and a relation R, `x IN R` is the OR over rows r
of R of the AND over columns j of (x_j = r_j) (§3.2). Work it on §1's
example, x = (3, 4), R = {(1, 2), (3, NULL)}:

```
r = (1, 2):     (3 = 1) AND (4 = 2)       = FALSE AND FALSE = FALSE
r = (3, NULL):  (3 = 3) AND (4 = NULL)    = TRUE  AND NULL  = NULL
IN              FALSE OR NULL             = NULL
```

The rule that falls out (§3.2): TRUE if there is an exact match; else NULL if
some row matches on every column where neither side is NULL — a **partial
match**; else FALSE. Finding TRUE is a hash lookup. Telling NULL from FALSE is
not, because a partial match is not found by hashing the full key. That is
why `NOT IN` is the hard one: `x NOT IN R` is TRUE only when `x IN R` is
FALSE, so the engine must *prove* no partial match exists.

### Step 3 — EXISTS, mark joins, and when a semi-join is allowed

> **In:** Step 2's three-valued IN. **Out:** the operator that represents IN
> after decorrelation, and the exact condition under which it becomes a
> semi-join or anti-join.

`EXISTS (SELECT … WHERE (r₁…r_d) = (x₁…x_d))` is the same disjunction with
every comparison wrapped in COALESCE(…, FALSE): two-valued, so "if a
database optimizer is able to recognize such a query, it can be executed
efficiently, usually by using a semi join operator" (§3.2).

To plan IN, the paper uses the **mark join** (§4): for each row of R it
computes the disjunction of predicate p over all rows of S and attaches the
result — TRUE, FALSE or NULL — as a new column m. A selection on m turns it
into a semi-join, and a selection on NOT m into an anti-join — but the
anti-join form needs COALESCE(p, TRUE): a row whose marker is NULL must be
*kept out* of NOT IN's result, which a plain anti-join (which keeps every row
without a TRUE match) would let in. Only when p cannot be NULL — no nullable
key columns — do these collapse to the ordinary semi-join and anti-join that
the rest of this topic filters with.

The engines in this topic encode that boundary in code. DuckDB refuses to
generate join filters for MARK and ANTI joins (`join_filter_pushdown_optimizer.cpp:110-126`
at `6c0c1a68`); DataFusion refuses dynamic filters when NULLs compare equal
(`exec.rs:869-871` at `1e77af8`), because "a probe-side NULL key evaluates
such predicates to NULL and would be pruned, even though it can match a
build-side NULL". See [reading-duckdb-join-filters.md](reading-duckdb-join-filters.md)
Step 2 and [reading-datafusion-dynamic-filters.md](reading-datafusion-dynamic-filters.md)
Step 2.

### Step 4 — the hardness: orthogonal vectors

> **In:** Step 2's partial matches. **Out:** why no general linear-time
> algorithm is expected, worked on Figure 3's three vectors.

The **orthogonal vectors** problem: given n binary vectors of length d, is
there a pair whose dot product is 0? It is conjectured to need n^(2−o(1))
time, a consequence of the **strong exponential time hypothesis** (SETH:
k-SAT cannot be solved in 2^(n(1−ε)) time, §5). The reduction (Figure 3)
builds S from R by mapping each vector y to (NULLIF(1 − y_i, 1))_i: a 1
becomes 0 and a 0 becomes NULL. Then x = s_y is FALSE exactly when some
position has y_i = 1 and x_i = 1 — when x and y are *not* orthogonal — and
otherwise TRUE or NULL. So `x NOT IN S` is TRUE exactly when x has no
orthogonal partner, and counting such x answers the problem.

Worked on Figure 3's R = {(0,1,0), (1,1,0), (1,1,1)}: S = {(NULL,0,NULL),
(0,0,NULL), (0,0,0)}. For x = (0,1,0), every s has 0 in the middle position
and x has 1 there, so every comparison has a FALSE conjunct: IN is FALSE,
NOT IN is TRUE. The same holds for the other two x (all three vectors have a
1 in the middle). Count = 3, not less than 3: no orthogonal pair — correct,
since every pair shares the middle bit. An engine evaluating that NOT IN in
o(n²) would refute the conjecture.

### Step 5 — the right mark join: linear for one nullable column

> **In:** the predicate split into nullable keys x (compared with =),
> non-nullable keys y (compared with IS) and a residual. **Out:** the
> paper's probe algorithm, and why it is constant time per row in the common
> case.

Both algorithms build two hash tables on the smaller side (§6.1, Figure 4):
**htFull** on the whole key, for exact matches, and **htProj** on the
non-nullable keys only, each entry holding two lists — build rows with a
NULL in the nullable keys (`null`) and without (`nonNull`). The **right mark
join** pipelines the other side and, per row r (§6.2, Figure 5): probe htFull;
if no TRUE, scan htProj[notNullable(r)].null for a row making p NULL; if r
itself has a NULL key, also scan htProj[…].nonNull.

With one nullable column and no residual, each scan stops at its first
element: the `null` list holds rows whose single nullable value is NULL, and
any one of them makes p NULL for a non-NULL r. So the work per probe row is
constant and the operator is linear (§6.2). With two or more nullable
columns and no non-nullable key to partition by, htProj collapses to one
entry holding the whole build side, and each probe can scan all of it —
quadratic again (§6.4). Bellamkonda et al.'s alternative is O(2^d · n) for d
nullable attributes, which only helps while d is tiny.

### Step 6 — the left mark join, and choosing between them

> **In:** Step 5. **Out:** the variant that materialises the *produced* side,
> and the measured boundary between the two.

When the side whose rows carry the marker (the **produced** side) is the
smaller one, materialise it instead: the **left mark join** (§6.3, Figure 6)
builds on the produced side, streams the other side to set markers, stores
at most one NULL-bearing streamed row per non-nullable key in
`htProj.rightTuple`, and finishes with a pass over the build that resolves
remaining FALSE markers against that stored row. Memory stays linear in the
smaller side.

Measured in Umbra (§7, Figure 10), single nullable attribute, all cores: with
10⁶ rows on both sides the right mark join takes **9 ms** and the left
**15 ms**, "around 5ns (7ns) per tuple" — 9 ms / 2×10⁶ = 4.5 ns and
15 ms / 2×10⁶ = 7.5 ns. The fitted boundary has slope ≈ **1.3**: the right
variant stays preferable until the hidden side is about 30% larger than the
produced side.

### Step 7 — the survey

> **In:** the semantics. **Out:** which systems get it right, and how fast.

§1 tested a common `NOT IN`/`IN` over nullable attributes:

- **Wrong results:** ClickHouse 25.8, Hyper 9.1.0, DuckDB 1.3.0, Snowflake
  9.21.0, Redshift 1.0.118447, Firebolt Core 4.23.5.
- **No multi-column IN, otherwise correct:** SQL Server 2022, BigQuery
  2025-07-22.
- **Correct but quadratic even in simple cases:** Materialize v0.151.0,
  Databricks 2025.16, CedarDB v2025-07-23, SQLite 3.46.1, DB2 11.1,
  MariaDB 11.4, Oracle 23c, PostgreSQL 17, TimescaleDB 2.14, YugabyteDB 2.18.

In §7's two-attribute test (a nullable, b not; one `(NULL, 1)` row added),
only Umbra ran the *correlated* form in linear time, and only PostgreSQL,
DBMSY and Umbra the decorrelated form; with `IN` instead of `NOT IN` all
systems were linear except SQLite and MariaDB on the correlated form.

## How to read the paper (with the concepts in hand)

1. **§1** — the two examples and the survey (Step 7).
2. **§3** — Figure 2's tables, then the IN/SOME/EXISTS definitions.
3. **§4** — mark join; the two semi/anti-join equivalences are the
   load-bearing lines for this topic.
4. **§5** — the reduction; redo Step 4's three vectors.
5. **§6.1–6.2, Figures 4–5** — read the pseudocode with Step 5's argument.
6. **§6.3–6.4, §7** — left mark join and the measured boundary.

## Where each step lives in this topic

| step | where | what to look at |
|---|---|---|
| 3 | `duckdb` `src/optimizer/join_filter_pushdown_optimizer.cpp:110-126` | MARK/ANTI excluded from join filters |
| 3 | `datafusion` `datafusion/physical-plan/src/joins/hash_join/exec.rs:859-886` | no dynamic filter when NULLs compare equal |
| 3 | `duckdb` `src/planner/filter/table_filter_bloom_function.cpp:151-183` | how a Bloom probe treats a NULL key |
| — | lanes 1–3 | all keys non-NULL by construction: this chapter is why that matters |

## Questions (answer in notes.md)

1. Cypher's `x IN list` follows the same three-valued rule when the list
   holds `null`. Which FalkorDB operator would need a mark-join-like
   evaluation, and what does a filter pushed from it look like?
2. Why is `IN` (not `NOT IN`) easy inside a WHERE clause even with NULLs?
   (Hint: what does WHERE do with NULL?)
3. Lane 3's bitmaps drop a probe row whose key is not in the build set. If a
   key could be NULL and the join were `NOT IN`, what would the bitmap have
   to do with NULL-keyed rows?
4. Is DuckDB 1.3.0's wrong answer on the survey still present at the pinned
   commit? Design a query that would tell.

## Done when

Answer each before unfolding it.

- [ ] You can evaluate `(3, 4) IN ((1, 2), (3, NULL))` step by step.

  <details><summary>Answer</summary>

  IN is an OR over rows of an AND over columns. Row (1,2): FALSE AND FALSE =
  FALSE. Row (3,NULL): (3 = 3) AND (4 = NULL) = TRUE AND NULL = NULL. Overall
  FALSE OR NULL = NULL (§1). It is not TRUE (no exact match) and not FALSE
  (there is a partial match on the non-NULL column).

  </details>

- [ ] You can state when a mark join collapses to an ordinary semi-join or anti-join.

  <details><summary>Answer</summary>

  σ_m(mark join) is a semi-join on p; σ_¬m is an anti-join on
  COALESCE(p, TRUE), i.e. a row with a NULL marker must be excluded (§4).
  Only when p cannot evaluate to NULL — no nullable key columns — are these
  the plain semi-join and anti-join that hash-based filtering implements.
  That is why DuckDB generates no join filters for MARK and ANTI joins and
  DataFusion none for null-equal joins.

  </details>

- [ ] You can explain the orthogonal-vectors reduction on Figure 3's input.

  <details><summary>Answer</summary>

  S maps each vector y to NULLIF(1 − y_i, 1): 1 → 0, 0 → NULL. Then x = s_y
  is FALSE iff some i has y_i = 1 and x_i = 1 (not orthogonal), else TRUE or
  NULL. So x NOT IN S is TRUE iff x has no orthogonal partner; counting those
  and comparing with n answers orthogonal vectors (§5). On R = {(0,1,0),
  (1,1,0), (1,1,1)} every vector has the middle bit set, all three NOT IN are
  TRUE, count 3 = n, no orthogonal pair.

  </details>

- [ ] You can say why the right mark join is linear with one nullable column and quadratic with two.

  <details><summary>Answer</summary>

  With one nullable column, htProj's `null` list holds rows whose only
  nullable value is NULL — any one makes p NULL — and a NULL-keyed probe row
  is made NULL by any `nonNull` entry, so each scan stops at its first element:
  constant per probe (§6.2). With two nullable columns and no non-nullable
  key, all build rows share one htProj entry, and a probe may have to scan
  the entire build to rule out a partial match: quadratic (§6.4).

  </details>

- [ ] You can name which surveyed systems returned wrong results and which were correct but quadratic.

  <details><summary>Answer</summary>

  Wrong: ClickHouse 25.8, Hyper 9.1.0, DuckDB 1.3.0, Snowflake 9.21.0,
  Redshift 1.0.118447, Firebolt Core 4.23.5. Correct but quadratic in simple
  cases: Materialize, Databricks, CedarDB, SQLite 3.46.1, DB2 11.1, MariaDB
  11.4, Oracle 23c, PostgreSQL 17, TimescaleDB 2.14, YugabyteDB 2.18. SQL
  Server 2022 and BigQuery lack multi-column IN but are otherwise correct
  (§1).

  </details>

- [ ] You can give the measured cost of the two mark-join variants.

  <details><summary>Answer</summary>

  Umbra, one nullable attribute, 10⁶ rows on each side: right mark join 9 ms,
  left 15 ms — 4.5 and 7.5 ns per input tuple (§7, Figure 10). The break-even
  line has slope ≈ 1.3: prefer the right variant until the hidden side is
  ~30% larger than the produced side.

  </details>

## References

- Altan Birler, Thomas Neumann, **"On the Vexing Difficulty of Evaluating IN
  Predicates"**, CIDR 2026 — §1 (examples, survey), §3 (semantics), §4 (mark
  join), §5 (OV reduction), §6 (right/left mark join), §7 (Figures 7–10).
- Thomas Neumann, Viktor Leis, Alfons Kemper, **"The Complete Story of Joins
  (in HyPer)"**, BTW 2017 — the mark join.
- Topic 11's [README](../11-execution-models/README.md), "Newer work".
- Previous: [reading-sqlserver-bitmaps.md](reading-sqlserver-bitmaps.md).
  Next: [reading-duckdb-join-filters.md](reading-duckdb-join-filters.md).
