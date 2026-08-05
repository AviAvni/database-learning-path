# TPC-H decoded: 22 queries, 28 choke points

TPC-H's 22 queries are not arbitrary — each stresses a named set of
engine capabilities ("choke points"), and Boncz, Neumann, and
Erling's TPCTC 2013 paper catalogs all 28 of them. Internalize the
map and a benchmark number stops being a score and becomes a
diagnosis. Before you open the paper, this chapter builds the ideas
it assumes step by step — what TPC-H actually is, what a choke point
is, and why three specific queries dominate every engine's marketing
slide — then hands you a reading plan. Read the paper WITH the
queries open (DuckDB vendors them — see References and
[reading-duckdb-tpch.md](reading-duckdb-tpch.md)).

Every clause number below is from **TPC-H Standard Specification
revision 3.0.1**; every choke-point number is from the TPCTC 2013
paper's Table 1; every SQL line number belongs to
**duckdb/duckdb@6c0c1a68**, whose `extension/tpch/dbgen/queries/`
holds the 22 queries with the spec's validation parameters already
substituted. Rust line numbers are this topic's `experiments/`.
Where the paper, the spec and the shipped data disagree — and they
do, twice — the disagreement is the lesson, so all three are quoted.

## The problem in one sentence

"Engine A runs TPC-H 3× faster than engine B" is meaningless until
you know *which of the 22 queries* won and *which of 28 distinct
engine capabilities* each one actually exercises — Q1 and Q6 can both
be "fast" while the optimizer that Q9 needs is broken.

## The concepts, step by step

### Step 1 — what TPC-H is: one schema, one generator, 22 queries, 2 refresh functions

> **In:** nothing but the name.
> **Out:** the four moving parts (schema, generator, query set, refresh
> functions), the exact SF-1 row counts every later step computes with, and
> the one sentence about "SF1 = 1 GB" that you must never repeat carelessly.

TPC-H is the industry-standard analytical benchmark — a fixed,
published test everyone runs so numbers are comparable. It has four
parts:

1. A fixed **8-table schema** modelling parts, suppliers, customers
   and orders (Clause 1.2, and the diagram in Clause 1.4).
2. **dbgen**, a data generator producing deterministic data at a
   chosen **scale factor** (SF).
3. **22 read-only SELECT queries** (Clause 2.4), each with
   *substitution parameters* — the constants in the WHERE clauses are
   drawn from spec-defined ranges, so no engine can precompute a
   literal answer.
4. **Two refresh functions**, RF1 (insert new orders and their
   lineitems) and RF2 (delete the same), which Step 7 shows are half
   the reason published numbers are hard to compare.

The scale factor is not a free dial. Clause 4.1.3.1 lists the only
permitted values — 1, 10, 30, 100, 300, 1000, 3000, 10000, 30000,
100000 — and says "SF = 1; approximately 1GB as per Clause 4.2.5",
where GB is 2^30 bytes. SF 3 is not a legal TPC-H scale factor.

Clause 4.2.5.1's Table 3 gives the SF-1 cardinalities. These are the
numbers every derivation in this guide uses:

```
  table      rows @ SF1     scales with SF?
  SUPPLIER       10,000     yes
  PART          200,000     yes
  PARTSUPP      800,000     yes
  CUSTOMER      150,000     yes
  ORDERS      1,500,000     yes
  LINEITEM    6,001,215     yes, but not exactly (see below)
  NATION             25     NO — fixed
  REGION              5     NO — fixed
  total       8,661,245
                              — Clause 4.2.5.1, Table 3
```

Two things in that table are load-bearing.

**LINEITEM is not SF × 6,000,000.** Table 3's footnote 3 says the
cardinality "is not a strict multiple of SF since the number of
lineitems in an order is chosen at random with an average of four".
Check it: 6,001,215 lineitems ÷ 1,500,000 orders = **4.0008 lineitems
per order**, and Clause 4.2.3 sets the per-order count to a random
value in [1..7], whose mean is 4. Clause 4.2.5.2's Table 4 spells out
what that does at scale — SF 10 is **59,986,052** rows, not
60,012,150. If your loader asserts `rows == sf * 6001215`, it will
fail at SF 10.

**"SF1 ≈ 1 GB" is about generated data volume, not your database.**
Table 3's footnote 2: "Typical lengths and sizes given here are
examples, not requirements, of what could result from an
implementation (sizes do not include storage/access overheads)."
The 641 MB it lists for LINEITEM is one illustrative row layout, not
a promise. A column store with dictionary-encoded flags and
delta-encoded dates will hold SF1 in a fraction of it; a row store
with per-row headers and indexes will hold it in several times it.
"DuckDB stores SF1 in X MB" is a fact about DuckDB, never a fact
about TPC-H.

Why the fixedness is worth the cost: the generator is seeded and
spec-exact, so an SF1 run in 2013 and an SF1 run today scan the same
data — that determinism is the whole value. The price is that
everyone optimizes *for these 22 queries*, which is exactly why a
decoder ring for what each one stresses matters.

### Step 2 — the choke point: 28 of them, six groups, three layers

> **In:** Step 1's four parts.
> **Out:** the paper's actual Table 1 — the six group names, the numbering
> scheme, and the QOPT/QEXE/STORAGE tag on each entry — plus an arithmetic
> check that you have the whole catalog and not a paraphrase of it.

A **choke point** is a named engine capability that dominates a
query's runtime — the thing the query is *really* measuring, beneath
the SQL. The paper's abstract states the catalog's shape exactly:

> "We identify **28** different such choke points, grouped into **six**
> categories: Aggregation Performance, Join Performance, Data Access
> Locality, Expression Calculation, Correlated Subqueries and Parallel
> Execution."

Table 1 lists all 28 with a three-way tag saying *which layer of the
system* must implement the capability — **QOPT** (query optimizer),
**QEXE** (execution engine), **STORAGE** (physical layout):

```
  CP1 Aggregation Performance
    CP1.1 QEXE    Ordered Aggregation
    CP1.2 QOPT    Interesting Orders
    CP1.3 QOPT    Small Group-by Keys (array lookup)          ← Q1
    CP1.4 QEXE    Dependent Group-By Keys (removal of)        ← Q10
  CP2 Join Performance
    CP2.1 QEXE    Large Joins (out-of-core)
    CP2.2 QEXE    Sparse Foreign Key Joins (bloom filters)
    CP2.3 QOPT    Rich Join Order Optimization                ← Q9
    CP2.4 QOPT    Late Projection (column stores)
  CP3 Data Access Locality
    CP3.1 STORAGE Columnar Locality
    CP3.2 STORAGE Physical Locality by Key (clustered index, partitioning)
    CP3.3 QOPT    Detecting Correlation (ZoneMap, MinMax, multi-attr histograms)
  CP4 Expression Calculation
    CP4.1  Raw Expression Arithmetic
      CP4.1a QEXE Arithmetic Operation Performance             ← Q1
      CP4.1b QEXE Overflow Handling
      CP4.1c QEXE Compressed Execution
      CP4.1d QEXE Interpreter Overhead (vectorization, JIT)    ← Q1
    CP4.2  Complex Boolean Expressions in Joins and Selections
      CP4.2a QOPT Common Subexpression Elimination
      CP4.2b QOPT Join-Dependent Expression Filter Pushdown
      CP4.2c QOPT Large IN Clauses (invisible join)
      CP4.2d QEXE Evaluation Order in Conjunctions/Disjunctions
    CP4.3  String Matching Performance
      CP4.3a QOPT Rewrite LIKE(X%) into a Range Query          ← Q9 can't
      CP4.3b QEXE Raw String Matching Performance (SSE4.2)
      CP4.3c QEXE Regular Expression Compilation (JIT/FSA)
  CP5 Correlated Subqueries
    CP5.1 QOPT    Flattening Subqueries (into join plans)
    CP5.2 QOPT    Moving Predicates into a Subquery
    CP5.3 QEXE    Overlap between Outer- and Subquery
  CP6 Parallelism and Concurrency
    CP6.1 QOPT    Query Plan Parallelization
    CP6.2 QEXE    Workload Management
    CP6.3 QEXE    Result Re-use
                              — TPCTC 2013, Table 1, verbatim names
```

Count them and you have a cheap check that you copied the catalog
and not a summary of it:

```
  by group:  CP1 4 + CP2 4 + CP3 3 + CP4 (4 + 4 + 3) + CP5 3 + CP6 3 = 28 ✓
  by layer:  QOPT 12 + QEXE 14 + STORAGE 2                            = 28 ✓
```

Two things fall out of that arithmetic that a prose summary hides.
Almost **40% of the catalog (11 of 28) is CP4, expression
calculation** — a benchmark reputed to be about joins spends most of
its named capabilities on arithmetic, booleans and strings. And
**only two entries are STORAGE**: everything else is code you write,
not a layout you choose.

Why the framing matters: a benchmark result becomes a *diagnosis*.
"Slow on Q4/Q21/Q22" doesn't mean "slow engine" — it means "no
semijoin rewrite" (CP2.2) or "no subquery flattening" (CP5.1). The
choke-point method was so useful it was reused to design LDBC SNB
(topic 13) from scratch.

### Step 3 — CP1.3: why Q1's hash table is free, and what "four groups" costs to state

> **In:** Step 2's catalog, and Step 1's 6,001,215 SF-1 lineitem rows.
> **Out:** the derivation of Q1's group count and its selectivity from the
> spec's population rules, checked twice against DuckDB's shipped SF-1
> answer file — and the discovery that the spec's own stated intent for
> Q1's selectivity is not what the spec's own rules produce.

**Aggregation** (GROUP BY) means partitioning rows by a key and
computing sums and counts per partition — normally via a **hash
table** (a structure mapping each distinct key to its running
totals). The cost of aggregation is usually that hash table:
hashing, probing, resizing, cache misses across millions of distinct
groups.

CP1.3 is the deliberate degenerate case, and the paper states it in
one sentence:

> "CP1.3: Small Group-By Keys. **Q1 computes eight aggregates: a
> count, four sums and three averages.** Group-by keys are
> `l_returnflag`, `l_linestatus`, with **just four occurring value
> combinations.** … if all group-by expressions can be represented as
> integers in a small range, one can use an array to keep the
> aggregate totals by position, rather than keeping them in a
> hash-table."

That is CP**1.3**, not CP1.2 — CP1.2 is *Interesting Orders*, about
reusing sort orders a clustered index already provides. And the
paper's headline query for CP1.4 (Dependent Group-By Keys) is
**Q10**, not Q18: "Q10 has a group-by on `c_custkey` and the columns
`c_comment, c_address, n_name, c_phone, c_acctbal, c_name`", which
`c_custkey` functionally determines.

Here is the query itself, with the eight aggregates and the two group
keys where the paper says they are:

```sql
-- duckdb extension/tpch/dbgen/queries/q01.sql, all 21 lines
     1  SELECT
     2      l_returnflag,
     3      l_linestatus,
     4      sum(l_quantity) AS sum_qty,
     5      sum(l_extendedprice) AS sum_base_price,
     6      sum(l_extendedprice * (1 - l_discount)) AS sum_disc_price,
     7      sum(l_extendedprice * (1 - l_discount) * (1 + l_tax)) AS sum_charge,
     8      avg(l_quantity) AS avg_qty,
     9      avg(l_extendedprice) AS avg_price,
    10      avg(l_discount) AS avg_disc,
    11      count(*) AS count_order
    12  FROM
    13      lineitem
    14  WHERE
    15      l_shipdate <= CAST('1998-09-02' AS date)
    16  GROUP BY
    17      l_returnflag,
    18      l_linestatus
    19  ORDER BY
    20      l_returnflag,
    21      l_linestatus;
```

Lines 4-11 are the eight aggregates (four sums, three averages, one
count). Line 15's constant is not arbitrary: Clause 2.4.1.3 defines
Q1's only substitution parameter as `DELTA`, "randomly selected
within [60. 120]", subtracted from 1998-12-01; DuckDB ships the
validation value DELTA = 90, and 1998-12-01 − 90 days = 1998-09-02.

**Why exactly four groups.** `l_returnflag` takes values R, A or N
and `l_linestatus` takes O or F, so the Cartesian product is six —
but Clause 4.2.3's population rules make two of them impossible:

```
  L_LINESTATUS = "O" if L_SHIPDATE > CURRENTDATE else "F"
  L_RETURNFLAG = "R" or "A" if L_RECEIPTDATE <= CURRENTDATE else "N"
  L_RECEIPTDATE = L_SHIPDATE + random[1..30]          (so receipt > ship)
  CURRENTDATE = 1995-06-17                            (Clause 4.2.2.12)

  receiptdate <= CURRENTDATE  ⇒  shipdate < CURRENTDATE  ⇒  linestatus = F
  ⇒ R and A can only ever pair with F; (R,O) and (A,O) cannot exist.
  surviving combinations: (A,F) (R,F) (N,F) (N,O) = 4  ✓
```

That is a **functional dependency between the two group keys** — CP1.4's
subject — hiding inside CP1.3's four groups. And it is checkable
without running anything, because DuckDB ships the answer:

```
  duckdb extension/tpch/dbgen/answers/sf1/q01.csv — 4 data rows
    A|F| … |1478493
    N|F| … |  38854
    N|O| … |2920374
    R|F| … |1478870
```

**Q1's selectivity, derived and then checked.** Sum that last column
— the `count_order` aggregate — and divide by Step 1's lineitem
cardinality:

```
  rows Q1 aggregates = 1,478,493 + 38,854 + 2,920,374 + 1,478,870
                     = 5,916,591
  SF-1 LINEITEM      = 6,001,215                    (Clause 4.2.5.1)
  measured selectivity = 5,916,591 / 6,001,215 = 0.98590 = 98.59%
```

Now derive the same figure from the population rules alone, with no
data. Clause 4.2.3 sets `L_SHIPDATE = O_ORDERDATE + random[1..121]`
and Clause 4.2.2.12 makes `O_ORDERDATE` uniform over
[STARTDATE, ENDDATE − 151 days] = [1992-01-01, 1998-08-02], which is
2406 days. Let `d` be the number of days from a row's orderdate back
from 1998-08-02 (uniform on 0..2405) and `k` its shipdate offset
(uniform on 1..121). The cutoff 1998-09-02 is 31 days after
1998-08-02, so a row is *excluded* exactly when `k > d + 31`:

```
  d = 0   → k ∈ [32..121] → 90 of 121 offsets excluded
  d = 1   → k ∈ [33..121] → 89
  …
  d = 89  → k ∈ [121..121] →  1
  d ≥ 90  → none
  excluded pairs = 90 + 89 + … + 1 = 90·91/2 = 4,095
  total pairs    = 2406 · 121              = 291,126
  P(excluded) = 4,095 / 291,126 = 0.014066
  P(scanned)  = 1 − 0.014066   = 0.98593 = 98.593%
```

Derived 98.593%, measured 98.590% — 0.003 percentage points apart on
a finite generated instance. The paper rounds this to "the large
amount of tuples to go through in Q1, **which selects 99% of
LINEITEM**" (§2.4).

**And now the disagreement.** Clause 2.4.1.3's Comment says: "The
intent is to choose DELTA so that **between 95% and 97%** of the rows
in the table are scanned." Run the same derivation across the whole
legal DELTA range and the intent is never met:

```
  DELTA=60  (cutoff 1998-10-02, 61 days out): 60·61/2 = 1,830 excluded → 99.37% scanned
  DELTA=90  (cutoff 1998-09-02, 31 days out): 90·91/2 = 4,095 excluded → 98.59% scanned
  DELTA=120 (cutoff 1998-08-03,  1 day  out): 120·121/2 = 7,260 excluded → 97.51% scanned
```

The spec's population rules and the spec's stated intent for the same
query do not agree, and the shipped SF-1 answer file sides with the
rules. This is the single most useful habit this topic can give you:
when a spec, a paper and a data file all describe the same number, do
the arithmetic and find out which two are wrong.

Our own generator does *not* reproduce the functional dependency —
it draws the two flags independently, so it has six live groups where
real TPC-H has four:

```rust
// experiments/src/lineitem.rs — the two Q1 group keys, 44-45
    44          t.returnflag.push(*[b'A', b'N', b'R'].get(rng.gen_range(0..3)).unwrap());
    45          t.linestatus.push(if rng.gen_bool(0.5) { b'O' } else { b'F' });
```

```rust
// experiments/src/tpch.rs — q1_oracle's hash-per-row, 25-39 (body elided at 31-35)
    25  pub fn q1_oracle(t: &LineItem) -> HashMap<Q1Key, Q1Agg> {
    26      let mut groups: HashMap<Q1Key, Q1Agg> = HashMap::new();
    27      for i in 0..t.len() {
    28          if t.shipdate[i] <= 2450 {
    29              let g = groups.entry((t.returnflag[i], t.linestatus[i])).or_default();
    30              let disc_price = t.extendedprice[i] * (1.0 - t.discount[i]);
    // ... 31-35: four accumulations and a count ...
    36          }
    37      }
    38      groups
    39  }
```

Line 29 is the whole point of CP1.3: a hash of a two-byte key,
computed 6 million times, to reach one of **six** slots. Replacing it
with `g[rf_idx * 2 + ls_idx]` is the array-lookup optimization the
paper describes, and it is what the `q1_flat` stub asks you to build.
Line 28's cutoff of 2450 out of the generator's 0..=2526 shipdate
range gives our Q1 a selectivity of 2451/2527 = **97.0%**, close
enough to real Q1's 98.59% that the comparison is fair.

Cost of not knowing this: a benchmark win on Q1 (CP1.3 + CP4.1) says
*nothing* about high-cardinality GROUP BY — that's why ClickBench and
TPC-DS exist. Our measured baseline (notes.md, M3 Pro, 2026-07-10):
the row-at-a-time HashMap oracle does SF 0.25 in 10.2 ms; `q1_flat`
shows how much of that was the map.

### Step 4 — CP4.1: selectivity, and why Q6 is the "GB/s" headline query

> **In:** Step 1's cardinalities and Step 3's habit of deriving a selectivity
> from the population rules.
> **Out:** Q6's ~1.9% selectivity, derived from three spec clauses and
> cross-checked against DuckDB's shipped answer to within 0.14% — plus why
> that number, not the SQL, decides whether branchy or branchless wins.

**Selectivity** is the fraction of rows a filter keeps. Q6 is a
single-table scan with three range predicates, no join and no
GROUP BY — just "how fast can you evaluate predicates over columns":

```sql
-- duckdb extension/tpch/dbgen/queries/q06.sql, all 10 lines
     1  SELECT
     2      sum(l_extendedprice * l_discount) AS revenue
     3  FROM
     4      lineitem
     5  WHERE
     6      l_shipdate >= CAST('1994-01-01' AS date)
     7      AND l_shipdate < CAST('1995-01-01' AS date)
     8      AND l_discount BETWEEN 0.05
     9      AND 0.07
    10      AND l_quantity < 24;
```

Clause 2.4.6.3 defines the three substitution parameters — DATE is
1 January of a year in [1993..1997], DISCOUNT is in [0.02..0.09],
QUANTITY is 24 or 25 — and Clause 2.4.6.4 gives the validation values
DuckDB shipped: 1994-01-01, 0.06, 24. The `BETWEEN` window on lines
8-9 is DISCOUNT ± 0.01.

**Deriving 1.9%.** Three independent predicates, three spec clauses:

```
  shipdate: O_ORDERDATE uniform over 2406 days, + random[1..121]
            ⇒ the interior of the shipdate range is flat at 121/291,126
              = 1/2406 per day, and 1994 lies wholly inside it
            P(one calendar year) = 365 / 2406          = 0.151704

  discount: L_DISCOUNT is "random value [0.00 .. 0.10]" in steps of
            0.01 ⇒ 11 distinct values; the window {0.05,0.06,0.07} is 3
            P(discount in window) = 3 / 11             = 0.272727

  quantity: L_QUANTITY is "random value [1..50]" ⇒ 50 values;
            "< 24" keeps 1..23
            P(quantity < 24) = 23 / 50                 = 0.460000

  independent, so multiply:
    0.151704 × 0.272727 × 0.460000 = 0.019032 = 1.90%
    rows kept at SF1 = 6,001,215 × 0.019032 = 114,215
```

**Cross-checking it against the shipped answer.** DuckDB's
`answers/sf1/q06.csv` holds one number, `revenue = 123141078.2283`.
Clause 4.2.3 says `L_EXTENDEDPRICE = L_QUANTITY * P_RETAILPRICE` and
`P_RETAILPRICE = (90000 + ((P_PARTKEY/10) modulo 20001) + 100 *
(P_PARTKEY modulo 1000))/100`, whose mean over the 200,000 SF-1 parts
is (90000 + 10000 + 49950)/100 = 1499.50. So:

```
  E[quantity | quantity < 24]  = mean(1..23)          = 12
  E[extendedprice | that]      = 12 × 1499.50         = 17,994
  E[discount | 0.05..0.07]     = 0.06
  E[revenue per qualifying row] = 17,994 × 0.06       = 1,079.64
  implied row count = 123,141,078.2283 / 1,079.64     = 114,058
  derived above                                       = 114,215
  agreement: 0.14%
```

Two routes to the same number, neither of which required running a
database. That makes Q6:

- the SIMD/vectorization showcase (topic 17's filter shapes), and
- the source of every "our engine scans N GB/s" headline number.

At ~2% selectivity a *branchy* scalar loop is competitive — the
branch predictor guesses "skip" and is right 98% of the time.
Branchless mask-multiply evaluation (`q6_branchless`) wins near 50%
selectivity, where branches mispredict constantly (topic 17's
crater). Our branchy oracle:

```rust
// experiments/src/tpch.rs — q6_oracle, the branchy scalar scan, 43-56
    43  pub fn q6_oracle(t: &LineItem) -> f64 {
    44      let mut rev = 0.0;
    45      for i in 0..t.len() {
    46          if t.shipdate[i] >= 730
    47              && t.shipdate[i] < 1095
    48              && t.discount[i] >= 0.05
    49              && t.discount[i] <= 0.07
    50              && t.quantity[i] < 24.0
    51          {
    52              rev += t.extendedprice[i] * t.discount[i];
    53          }
    54      }
    55      rev
    56  }
```

Lines 46-50 are the same three predicates with the same constants;
only the date encoding differs (days since 1992-01-01). Our
generator's shipdate is uniform over 0..=2526 rather than the real
convolution, so our Q6 selectivity is
(365/2527) × (3/11) × (23/50) = **1.81%** against real TPC-H's 1.90% —
close enough that the branch-prediction story transfers, and
different enough that our absolute row counts are ours alone.

The topic's headline (FINDINGS.md row 22) is **Q1 at 5.2–5.7 GB/s and
Q6 at 9.0–14.4 GB/s effective**; notes.md's baseline table records the
SF-0.25 end of those ranges at 5.6 and 15.7 GB/s on an M3 Pro. Q6's
branchy oracle is already a large fraction of memory bandwidth *at
this selectivity* — so predict what branchless adds before
implementing it (topic 17's answer: at 2%, possibly nothing).

### Step 5 — CP2.3, CP4.3a and CP6.1: why Q9 punishes optimizers

> **In:** Steps 3 and 4's single-table queries.
> **Out:** the three separate choke points Q9 stacks, each anchored to the
> line of `q09.sql` that triggers it, and the group-cardinality contrast with
> Q1 that explains why Q9's aggregation is a different problem.

A **join** matches rows across tables; with N tables there are
exponentially many orders to do it in, and the optimizer picks one
using **cardinality estimates** (predicted result sizes). The paper's
CP2.3 says why that matters here: "TPC-H has queries which join up to
**eight** tables … the execution times of different join orders differ
by orders of magnitude."

Q9 is the punisher, and the FROM clause settles the arity:

```sql
-- duckdb extension/tpch/dbgen/queries/q09.sql, the join and its predicates, 10-27
    10      FROM
    11          part,
    12          supplier,
    13          lineitem,
    14          partsupp,
    15          orders,
    16          nation
    17      WHERE
    18          s_suppkey = l_suppkey
    19          AND ps_suppkey = l_suppkey
    20          AND ps_partkey = l_partkey
    21          AND p_partkey = l_partkey
    22          AND o_orderkey = l_orderkey
    23          AND s_nationkey = n_nationkey
    24          AND p_name LIKE '%green%') AS profit
    25  GROUP BY
    26      nation,
    27      o_year
```

Six tables (11-16), six equi-join predicates (18-23) — a **6-way
join**, which is where the "6-way" in every summary of Q9 comes from.
Three choke points stack on it:

- **CP2.3 (QOPT), join order.** Six tables joined through LINEITEM,
  the 6-million-row table; an order that materializes
  `part × partsupp` before filtering is orders of magnitude worse
  than one that starts from the `%green%` selection on PART.
- **CP4.3a (QOPT), string matching.** The paper lists Q9 among
  "Q2,9,13,14,16,20 contain expensive LIKE predicates", and says the
  optimizable special case is *prefix* search, `LIKE 'xxx%'`, which
  "occurs in Q14,16,20" and can be prefiltered by a range comparison.
  Line 24's `'%green%'` is not a prefix, so that rewrite is
  unavailable — the engine must actually scan strings (CP4.3b) and
  the optimizer must estimate the selectivity of a substring match,
  which it cannot do well.
- **CP6.1 (QOPT), parallelization.** Line 26-27's GROUP BY is over
  `nation` × `o_year`. NATION is fixed at 25 rows (Step 1) and
  O_ORDERDATE spans 1992-01-01 to 1998-08-02, so o_year takes 7
  values: **at most 25 × 7 = 175 groups**. Compare Step 3's four.
  175 groups is small enough that the array trick still applies and
  large enough that per-nation size differences make partitioned
  parallel aggregation imbalanced.

The three queries everyone profiles, side by side:

| query | choke points | what it really measures |
|---|---|---|
| Q1 | CP1.3 (small group-by keys) + CP4.1a/d (arithmetic, interpreter overhead) | expression evaluation over 98.6% of LINEITEM into **4** groups — the hash table is free, so fused arithmetic dominates; our `q1_flat` stub makes this explicit |
| Q6 | CP4.1a + CP4.2d (evaluation order in conjunctions) | pure selection at **1.9%** — SIMD-able predicates; "GB/s scanned" headlines are usually Q6 |
| Q9 | CP2.3 (join order) + CP4.3a/b (`%green%`, no prefix rewrite) + CP6.1 (175 groups, skewed) | 6-way join order + substring matching + parallel load balance — the query that punishes optimizers |

A fourth family hides in CP5: **correlated subqueries** (a subquery
whose result depends on the current outer row, so it re-runs per row
unless the optimizer *decorrelates* it into a join). CP5.1
("Flattening Subqueries") is the capability; CP5.3 names
"Q2,11,15,17 and Q20" as the queries where outer and subquery overlap
so much that the shared work should be computed once. An engine
without decorrelation runs Q17 thousands of times slower. Different
capability than join order, same "optimizer or bust" flavour.

### Step 6 — dbgen's uniformity, and the correlation the paper insists IS there

> **In:** Steps 3-5's derivations, every one of which multiplied independent
> probabilities.
> **Out:** the precise statement of dbgen's independence — including the
> place where the paper says the opposite, which is the part most summaries
> get wrong.

dbgen draws most column values **uniformly** (every value equally
likely) from spec-fixed ranges: `L_QUANTITY` random [1..50],
`L_DISCOUNT` random [0.00..0.10], `L_TAX` random [0.00..0.08],
`O_ORDERDATE` uniform across 2406 days (Clause 4.2.3). Steps 3 and 4
exploited exactly that: every derivation there was a product of
independent probabilities, and each landed within a fraction of a
percent of the shipped answer. That *is* the demonstration —
**cardinality estimation is easy on TPC-H**, and the JOB benchmark
(topic 10) was built on real IMDB data precisely because TPC-H lets
naive estimators look good.

But "dbgen has no correlation between columns" is too strong, and the
paper says so directly. CP3.3, *Detecting Correlation*, is an entire
choke point about the correlations dbgen **does** create:

> "in case of LINEITEM the question then is which of the three date
> columns to use as key … in fact it should not matter which column is
> used, as **range-propagation between correlated attributes of the
> same table is relatively easy** … even if the LINEITEM is clustered
> on `l_receiptdate`, this will still find tight tuple position ranges
> for predicates on `l_shipdate` (and vice versa)."

The mechanism is in Step 3's population rules: all three LINEITEM
dates are derived from the same `O_ORDERDATE` by small random offsets
(`L_SHIPDATE = O_ORDERDATE + [1..121]`, `L_COMMITDATE = O_ORDERDATE +
[30..90]`, `L_RECEIPTDATE = L_SHIPDATE + [1..30]`), so they are
tightly correlated with each other and with tuple position. That is
why zone maps and MinMax indexes work so well on TPC-H — a choke
point in their own right (CP3.3, QOPT). Step 3 also showed
`l_returnflag` and `l_linestatus` are *functionally* dependent through
those same dates.

The accurate statement is therefore narrower and more useful:

> dbgen's *value distributions* are uniform and its *unrelated*
> columns are independent — but its date columns are strongly
> correlated with each other, and its two Q1 group keys are
> functionally dependent. Uniformity is what flatters cardinality
> estimators; the date correlation is what flatters zone maps.

Our dbgen-lite is uniform *and* fully independent, including the
dates and flags — a stronger simplification than the real generator's
(`lineitem.rs:39-47` draws every column from its own `rng.gen_range`).
Question 2 asks which correlations you would have to add back to
break `q1_flat`'s perfect-group-code trick.

### Step 7 — reading a published number: the two refresh functions and the metric

> **In:** everything above, plus a vendor's press release.
> **Out:** the exact definition of Power@Size from Clause 5.4.1.1, and the
> arithmetic showing what dropping RF1/RF2 does to it — the difference
> between "TPC-H" and "TPC-H-derived".

Two hidden messages change how you read any "TPC-H" claim.

**The refresh functions are part of the metric, not an extra.**
Clause 5.3.3.2 defines the power test as RF1, then the 22 queries,
then RF2 — and Clause 5.3.3.3 requires all 24 intervals to be timed.
Clause 5.4.1.1 then defines:

```
                                       3600 × SF
  TPC-H Power@Size = ───────────────────────────────────────────
                     geomean of the 24 timing intervals (22 + RF1 + RF2)

  TPC-H Throughput@Size = (S × 22 × 3600) / Ts × SF        (Clause 5.4.2)
  QphH@Size             = sqrt(Power@Size × Throughput@Size)
```

A geometric mean over 24 terms is not a geometric mean over 22. Drop
the two refresh intervals and every remaining term's weight rises
from 1/24 to 1/22 — and the two you dropped were the write-heavy
ones, which on a column store are usually the slowest. The published
number goes up, and it is no longer Power@Size. Informal runs skip
RF1/RF2 almost universally; say "TPC-H-derived" for anything that
isn't audited (topic 0's Fair Benchmarking guide is the methodology
companion here).

**Scale factor changes the winner.** SF1 (~1 GB of generated data)
fits in a modern server's cache hierarchy; SF100 does not. Engine
rankings flip between them, exactly topic 0's memory ladder. A
comparison at one SF is a data point, not a ranking — and per Step 1,
"SF1 ≈ 1 GB" describes generated volume, not what any engine stores.

## How to read the paper (with the concepts in hand)

TPCTC 2013, 16 pages, one evening — but only with the queries open
(`extension/tpch/dbgen/queries/q01.sql…q22.sql` in DuckDB). The paper
has **three sections**, not five, and almost all of it is §2:

- **§1 Introduction** — skim. The keeper is the argument that a
  benchmark shapes a decade of engine development, and the framing of
  TPC-H as a design document rather than a scoreboard.
- **§2 TPC-H Choke Point Analysis** — the whole paper. Table 1 first
  (Step 2's catalog, one page), then the six subsections in order:
  §2.1 Aggregation, §2.2 Join, §2.3 Data Access Locality,
  §2.4 Expression Calculation, §2.5 Correlated Subqueries,
  §2.6 Parallelism and Concurrency. §2.4 is the longest — 11 of the 28
  choke points live there.
- **§3 Conclusion** — short.

Read it in this order:

1. Table 1, and check the two counts from Step 2 (28 by group, 28 by
   layer). If your copy doesn't add up, you're reading a summary.
2. §2.1 CP1.3 with `q01.sql` and `answers/sf1/q01.csv` open — Step 3's
   four groups and 98.59% are both visible in those two files.
3. §2.4 CP4.1a-d with `q06.sql` open. Note footnote 5 ("Some notes on
   Q1"): Q1 has more computation per tuple than Q6, parallelizes
   trivially, and is the only query where cross-system
   back-of-the-envelope compute estimates are meaningful.
4. §2.2 CP2.3 and §2.4 CP4.3a with `q09.sql` open — Step 5's three
   stacked choke points.
5. §2.3 CP3.3 last, because it is the one that contradicts the folk
   version of dbgen (Step 6).

For each CP, ask: does FalkorDB have this capability? That turns the
catalog into an audit list.

## Questions (answer in notes.md)

1. Map Q1/Q6/Q9 onto FalkorDB-relevant analogues: which Cypher query
   shapes hit the same choke points (CP1.3 small-domain aggregation,
   CP4.1 scan+filter arithmetic, CP2.3+CP6.1 join-order and skew)?
2. Our dbgen-lite draws `returnflag` and `linestatus` independently
   (`lineitem.rs:44-45`), so it has six live groups where real TPC-H
   has four. Which *other* correlations from Step 6 would you have to
   add back to break `q1_flat`'s perfect-group-code trick — and which
   ones would leave it untouched?
3. Step 4 derived Q6 at 1.90% for real dbgen and 1.81% for ours.
   Rework the derivation for a predicate that keeps 50% of rows, and
   predict the branchy/branchless crossover for `q6_branchless`
   against topic 17's measured sweep.
4. Choke point CP3.1/CP3.2 (data access locality): which of the 22
   queries would an incremental-view engine (topic 27 preview) answer
   in O(1), and which of the 28 choke points does that make irrelevant?
5. TPC-H says nothing about updates beyond RF1/RF2. What does TPC-C's
   NewOrder mix test that no TPC-H query can (see
   [reading-oltpbench-tpcc.md](reading-oltpbench-tpcc.md))?

## Done when

Answer each before unfolding it.

- [ ] You can define a choke point, state how many there are, and name the six groups and the three implementation layers Table 1 tags them with.

  <details><summary>Answer</summary>

  A choke point is a named engine capability that dominates a query's
  runtime — what the query is really measuring beneath the SQL. There are
  **28**, in six groups: Aggregation Performance, Join Performance, Data
  Access Locality, Expression Calculation, Correlated Subqueries, and
  Parallelism and Concurrency (the abstract names the sixth "Parallel
  Execution"; Table 1 calls it "Parallelism and Concurrency").

  Table 1 tags each entry **QOPT**, **QEXE** or **STORAGE**. The counts
  check out two ways: 4 + 4 + 3 + (4+4+3) + 3 + 3 = 28 by group, and
  12 QOPT + 14 QEXE + 2 STORAGE = 28 by layer. Two consequences worth
  carrying: CP4 (expression calculation) is 11 of the 28, and only two
  entries are about physical layout — the rest is code.

  </details>

- [ ] You can name Q1's choke point by its correct number, say how many groups it has and why, and derive its selectivity without running anything.

  <details><summary>Answer</summary>

  **CP1.3, Small Group-By Keys** (QOPT) — not CP1.2, which is *Interesting
  Orders*. The paper: "Q1 computes eight aggregates: a count, four sums and
  three averages. Group-by keys are `l_returnflag`, `l_linestatus`, with just
  four occurring value combinations."

  Four, not six, because Clause 4.2.3 makes the keys functionally dependent:
  `L_RECEIPTDATE = L_SHIPDATE + [1..30]`, so `receiptdate <= CURRENTDATE`
  implies `shipdate < CURRENTDATE` implies `linestatus = 'F'`. R and A
  therefore only ever pair with F, killing (R,O) and (A,O). Survivors:
  (A,F), (R,F), (N,F), (N,O).

  Selectivity from the rules alone: orderdate is uniform over 2406 days
  ending 1998-08-02, shipdate adds [1..121], and DELTA=90 puts the cutoff 31
  days past the last orderdate. A row is excluded when `k > d + 31`, giving
  90·91/2 = 4,095 excluded pairs out of 2406 × 121 = 291,126, so 98.593% is
  scanned. DuckDB's `answers/sf1/q01.csv` sums to 5,916,591 of 6,001,215 rows
  = 98.590%. The paper rounds to "99%". Clause 2.4.1.3's Comment claims the
  intent is 95–97%, which no legal DELTA in [60..120] achieves — the range is
  97.51% to 99.37%.

  </details>

- [ ] You can derive Q6's selectivity from three spec clauses and check it against a file DuckDB ships.

  <details><summary>Answer</summary>

  Three independent predicates. Shipdate: the interior of the shipdate
  distribution is flat at 1/2406 per day (orderdate uniform over 2406 days
  convolved with a uniform [1..121] offset), and 1994 lies inside it, so
  365/2406 = 0.151704. Discount: `L_DISCOUNT` is random [0.00..0.10] in 0.01
  steps = 11 values, and `BETWEEN 0.05 AND 0.07` keeps 3, so 3/11 = 0.272727.
  Quantity: random [1..50], `< 24` keeps 23, so 0.46. Product = 0.019032, and
  6,001,215 × 0.019032 = **114,215 rows, 1.90%**.

  The check: `answers/sf1/q06.csv` says revenue = 123,141,078.2283. Since
  `L_EXTENDEDPRICE = L_QUANTITY × P_RETAILPRICE` and P_RETAILPRICE averages
  1499.50 over the SF-1 parts, a qualifying row contributes on average
  12 × 1499.50 × 0.06 = 1,079.64, implying 114,058 rows. The two routes agree
  to 0.14%.

  Ours differs slightly: our shipdate is uniform over 0..=2526 rather than a
  convolution, giving (365/2527) × (3/11) × 0.46 = 1.81%.

  </details>

- [ ] You can name the three choke points Q9 stacks and point at the line of `q09.sql` that triggers each.

  <details><summary>Answer</summary>

  **CP2.3, Rich Join Order Optimization** (QOPT) — `q09.sql:11-16` lists six
  tables and `:18-23` six equi-join predicates, all routed through the
  6-million-row LINEITEM. The paper notes TPC-H joins "up to eight tables" and
  that join orders "differ by orders of magnitude".

  **CP4.3a/b, string matching** — `q09.sql:24`, `p_name LIKE '%green%'`. The
  paper's optimizable case is *prefix* search (`LIKE 'xxx%'`, in Q14/16/20),
  rewritable to a range comparison. `%green%` is not a prefix, so the engine
  must scan strings (CP4.3b) and the optimizer must estimate a substring
  match's selectivity, which it cannot do well.

  **CP6.1, Query Plan Parallelization** — `q09.sql:25-27` groups by nation ×
  o_year. NATION is fixed at 25 rows and orderdates span 1992–1998, so at most
  175 groups; unequal per-nation sizes make partitioned parallel aggregation
  imbalanced.

  </details>

- [ ] You can state what dbgen's uniformity does and does not imply — including the correlation the paper devotes a choke point to.

  <details><summary>Answer</summary>

  Uniform value distributions plus independence between *unrelated* columns
  make cardinality estimation easy: multiply selectivities and you are right,
  which Steps 3 and 4 demonstrated to within 0.14% twice. That is why JOB
  (topic 10) exists on real IMDB data.

  But "no correlation between columns" is wrong. CP3.3, *Detecting
  Correlation* (QOPT), is entirely about the correlation dbgen does create:
  all three LINEITEM dates derive from the same `O_ORDERDATE` by small random
  offsets, so they are correlated with each other and with tuple position.
  The paper: "it should not matter which column is used, as range-propagation
  between correlated attributes of the same table is relatively easy … even if
  the LINEITEM is clustered on `l_receiptdate`, this will still find tight
  tuple position ranges for predicates on `l_shipdate`." That correlation is
  why zone maps and MinMax indexes look so good on TPC-H. And the two Q1 group
  keys are functionally dependent through those same dates.

  Our dbgen-lite is *more* independent than the real thing — it draws dates
  and flags from separate generators (`lineitem.rs:39-47`).

  </details>

- [ ] You can read a published TPC-H number and say exactly what skipping the refresh functions does to it.

  <details><summary>Answer</summary>

  Clause 5.3.3.2 defines the power test as RF1 → the 22 queries → RF2, and
  Clause 5.3.3.3 times all 24 intervals. Clause 5.4.1.1 sets
  `Power@Size = 3600 × SF / geomean(those 24 intervals)`; Clause 5.4.2 defines
  Throughput@Size, and QphH is their geometric mean.

  Skipping RF1/RF2 turns a 24-term geometric mean into a 22-term one, raising
  every surviving term's weight from 1/24 to 1/22 — and the two dropped terms
  are the write-heavy ones, typically the slowest on a column store. The
  reported figure rises and is no longer Power@Size. Call it "TPC-H-derived".

  Scale factor matters as much: SF1's ~1 GB of generated data fits in cache
  where SF100 does not, and rankings flip. And "SF1 ≈ 1 GB" is generated
  volume — Table 3's footnote 2 says its byte sizes are "examples, not
  requirements", so any on-disk figure is a fact about one engine.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the Cypher analogues of Q1/Q6/Q9.

  <details><summary>Answer</summary>

  Self-check — the answers belong in `notes.md`, not here. The one worth
  arguing about is question 1: Q1's analogue is an aggregation over a
  small label/type domain (`MATCH (n:Person) RETURN n.country, count(*)`),
  Q6's is a scan with independent property range filters and no traversal,
  and Q9's is a multi-hop pattern where the traversal order is the optimizer's
  choice and the per-node degree distribution is skewed — which in a graph is
  worse than TPC-H's, because real degree distributions are power-law where
  NATION is uniform at 25.

  </details>

## References

**Papers**
- Boncz, Neumann, Erling — "TPC-H Analyzed: Hidden Messages and
  Lessons Learned from an Influential Benchmark", TPCTC 2013,
  LNCS 8391, pp. 61-76 (16 pages). Structure: §1 Introduction,
  §2 TPC-H Choke Point Analysis (§2.1–§2.6), §3 Conclusion. Table 1
  is the 28-entry catalog; there is no §4 or §5.
  [PDF](https://www.cwi.nl/~boncz/snb-challenge/chokepoints-tpctc.pdf)

**Specification**
- TPC BenchmarkTM H Standard Specification, revision 3.0.1
  ([tpc.org](https://www.tpc.org/tpch/)). Clauses used above:

| Clause | What |
|---|---|
| 2.4.1.1–2.4.1.4 | Q1's text, `DELTA ∈ [60..120]`, the 95–97% intent Comment, and the validation value 90 |
| 2.4.6.2–2.4.6.4 | Q6's text, substitution parameter ranges, and validation values 1994-01-01 / 0.06 / 24 |
| 4.1.3.1 | the ten legal scale factors; "SF = 1; approximately 1GB" |
| 4.2.2.12 | STARTDATE 1992-01-01, CURRENTDATE 1995-06-17, ENDDATE 1998-12-31 |
| 4.2.3 | population rules: quantity [1..50], discount [0.00..0.10], the three date offsets, RETURNFLAG/LINESTATUS derivation, `P_RETAILPRICE`, `L_EXTENDEDPRICE` |
| 4.2.5.1 Table 3 | SF-1 cardinalities; footnote 2 (sizes are examples, not requirements) |
| 4.2.5.2 Table 4 | LINEITEM cardinality per SF; footnote 3 (not a strict multiple of SF) |
| 5.3.3.2–5.3.3.3 | the power test is RF1 → 22 queries → RF2, all 24 intervals timed |
| 5.4.1.1 / 5.4.2 | Power@Size, Throughput@Size, QphH definitions |

**Code**

| File | Lines | What |
|---|---|---|
| duckdb `extension/tpch/dbgen/queries/q01.sql` | 1-21 | Q1: eight aggregates (4-11), two group keys (17-18), DELTA=90 cutoff (15) |
| duckdb `extension/tpch/dbgen/queries/q06.sql` | 1-10 | Q6: three range predicates with the validation parameters |
| duckdb `extension/tpch/dbgen/queries/q09.sql` | 10-27 | Q9: six tables (11-16), six join predicates (18-23), `%green%` (24), 175-group aggregation (25-27) |
| duckdb `extension/tpch/dbgen/answers/sf1/q01.csv` | 1-5 | four result rows; `count_order` sums to 5,916,591 |
| duckdb `extension/tpch/dbgen/answers/sf1/q06.csv` | 2 | `revenue = 123141078.2283` — Step 4's cross-check |
| `experiments/src/lineitem.rs` | 39-47 | dbgen-lite: every column from its own `gen_range`, no correlation at all |
| `experiments/src/tpch.rs` | 25-39 | `q1_oracle` — the per-row hash into six slots that CP1.3 says to delete |
| `experiments/src/tpch.rs` | 43-56 | `q6_oracle` — the branchy scan at 1.81% selectivity |

Pinned revisions: duckdb/duckdb@6c0c1a68 (regenerate the pin table
with `python3 tools/pin-table.py`).

**Cross-topic**
- topic 0 `reading-fair-benchmarking.md` — the methodology companion
  to Step 7's "TPC-H-derived" distinction.
- topic 10 — JOB, built because Step 6's uniformity flatters
  cardinality estimators.
- topic 13 `reading-ldbc-snb.md` — the choke-point method reused to
  design a benchmark from scratch.
- topic 17 — the branchy/branchless filter crater that Step 4's 1.9%
  selectivity sits on the safe side of.
- topic 34 — coordinated omission, the OLTP-side measurement error
  this guide's read-only queries never encounter.
