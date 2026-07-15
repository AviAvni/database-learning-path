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

## The problem in one sentence

"Engine A runs TPC-H 3× faster than engine B" is meaningless until
you know *which of the 22 queries* won and *which of 28 distinct
engine capabilities* each one actually exercises — Q1 and Q6 can both
be "fast" while the optimizer that Q9 needs is broken.

## The concepts, step by step

### Step 1 — what TPC-H is: one schema, one generator, 22 queries

TPC-H is the industry-standard analytical benchmark (a fixed,
published test everyone runs so numbers are comparable): a fixed
8-table schema modeling orders and suppliers, a data generator
(**dbgen**) that produces deterministic data at a chosen **scale
factor** (SF — SF1 ≈ 1 GB, SF100 ≈ 100 GB; the biggest table,
`lineitem`, has SF × 6M rows), and 22 read-only SELECT queries plus
two refresh (insert/delete) streams. Because the generator is seeded
and spec-exact, an SF1 run in 2013 and an SF1 run today scan
byte-identical data — that determinism is the whole value.

Cost of that fixedness: everyone optimizes *for these 22 queries*,
which is exactly why a decoder ring for what each one stresses
matters.

### Step 2 — the choke point: naming what a query actually stresses

A **choke point** is a named engine capability that dominates a
query's runtime — the thing the query is *really* measuring, beneath
the SQL. The paper's contribution is a 28-entry catalog mapping every
TPC-H query to its choke points, in six families:

```
  CP1 aggregation      dominated by GROUP BY machinery
      CP1.1 ordered agg / CP1.2 small group-by keys (Q1!) /
      CP1.4 dependent group-by (Q18)
  CP2 joins            order (Q5,Q7-Q9), semijoin (Q4,Q21,Q22),
                       large vs selective probes
  CP3 locality         materialized views would help (Q14/Q15),
                       physical column order
  CP4 expressions      arithmetic-heavy (Q1 again), string match
                       LIKE '%green%' (Q9), date logic everywhere
  CP5 correlated subq  Q2, Q11, Q17, Q20-Q22
  CP6 parallelism      all of them, but skew hits Q9/Q18 hardest
```

Why the framing matters: a benchmark result becomes a *diagnosis*.
"Slow on Q4/Q21/Q22" doesn't mean "slow engine" — it means "no
semijoin rewrite". The choke-point method was so useful it was
reused to design LDBC SNB (topic 13) from scratch.

### Step 3 — aggregation, and why Q1's hash table is free

**Aggregation** (GROUP BY) means partitioning rows by a key and
computing sums/counts per partition — normally via a **hash table**
(a structure mapping each distinct key to its running totals). The
cost of aggregation is usually that hash table: hashing, probing,
resizing, cache misses on millions of distinct groups.

Q1 is the deliberate degenerate case: it groups 6M × SF rows by
`(returnflag, linestatus)` — which has only **~4–6 distinct values
total**. The hash table degenerates into a flat 6-slot array that
lives in registers, so what's left to measure is pure **expression
evaluation** (the per-row arithmetic) and fused accumulation. That
is exactly what our `q1_flat` stub implements:

```rust
// Q1: "GROUP BY returnflag, linestatus" has ~6 groups TOTAL, so the
// hash table degenerates into a flat array — all that's left to
// measure is expression evaluation and fused accumulation.
fn q1_flat(c: &LineItemColumns) -> [Agg; 6] {
    let mut g = [Agg::default(); 6];
    for i in 0..c.len {
        if c.shipdate[i] > CUTOFF { continue; }
        let k = group_code(c.returnflag[i], c.linestatus[i]);  // 0..5
        let disc_price = c.extendedprice[i] * (1.0 - c.discount[i]);
        g[k].sum_qty        += c.quantity[i];
        g[k].sum_disc_price += disc_price;
        g[k].sum_charge     += disc_price * (1.0 + c.tax[i]);
        g[k].count          += 1;
    }
    g
}
```

Cost of not knowing this: a benchmark win on Q1 (CP1.2 + CP4) says
*nothing* about high-cardinality GROUP BY — that's why ClickBench
and TPC-DS exist. Our measured baseline: the row-at-a-time HashMap
oracle does SF 0.25 in 10.2 ms; `q1_flat` shows how much of that was
the map.

### Step 4 — selectivity, and why Q6 is the "GB/s" headline query

**Selectivity** is the fraction of rows a filter keeps. Q6 is a
single-table scan with three range predicates that keep **~2%** of
`lineitem` — no join, no meaningful aggregation, just "how fast can
you evaluate predicates over columns". That makes it:

- the SIMD/vectorization showcase (topic 17's filter shapes), and
- the source of every "our engine scans N GB/s" headline number.

At 2% selectivity a *branchy* scalar loop is competitive — the branch
predictor guesses "skip" and is right 98% of the time. Branchless
mask-multiply evaluation (`q6_branchless`) wins near 50% selectivity,
where branches mispredict constantly (topic 17's crater). Our branchy
oracle already hits 15.7 GB/s at SF 0.25 — half of memory bandwidth —
so predict what branchless adds *at this selectivity* before
implementing (maybe nothing!).

### Step 5 — join order, and why Q9 punishes optimizers

A **join** matches rows across tables; with N tables there are
exponentially many orders to do it in, and the optimizer picks one
using **cardinality estimates** (predicted result sizes). A bad
order can materialize billions of intermediate rows where a good one
touches thousands — orders of magnitude, not percent.

Q9 is the punisher: a **6-way join**, plus `LIKE '%green%'` (a
substring match whose selectivity is nearly impossible to estimate),
plus per-nation **skew** (some groups far bigger than others, which
also breaks parallel load balance — CP6). Get any of the three wrong
and Q9's runtime explodes. The three queries everyone profiles,
side by side:

| query | choke points | what it really measures |
|---|---|---|
| Q1 | CP1.2 + CP4 | expression evaluation + tiny-domain aggregation: ~4 groups, so the hash table is FREE and fused arithmetic dominates — our `q1_flat` stub makes this explicit |
| Q6 | CP4 + scan | pure selection: ~2% selectivity, SIMD-able predicates — DBMS "GB/s scanned" headline numbers are usually Q6 |
| Q9 | CP2 + CP4 (LIKE) + CP6 skew | 6-way join order + `%green%` string matching + per-nation skew — the query that punishes optimizers |

A fourth family hides in CP5: **correlated subqueries** (a subquery
that re-runs per outer row unless the optimizer decorrelates it into
a join) — Q2, Q11, Q17, Q20–Q22. An engine without decorrelation
runs Q17 thousands of times slower. Different capability than
join order, same "optimizer or bust" flavor.

### Step 6 — dbgen's dirty secret: uniform, independent data

dbgen generates values **uniformly** (every value equally likely)
and **independently** (no correlation between columns — shipdate
doesn't predict discount). Real data is neither. Two consequences:

- **Cardinality estimation is EASY on TPC-H** — multiply independent
  selectivities and you're right. The JOB benchmark (topic 10) was
  built on real IMDB data precisely because TPC-H lets naive
  estimators look good.
- **Uniformity is a lie you can exploit**: an engine tuned on TPC-H
  may have never seen skewed group sizes or correlated filters.

Our dbgen-lite is uniform and independent like the real thing —
question 2 asks which correlations would break `q1_flat`.

### Step 7 — reading published numbers: refresh streams and scale factors

Two more hidden messages that change how you read any "TPC-H" claim:

- **Refresh functions (RF1/RF2) are always skipped** in informal
  runs — published numbers are usually just the power test's 22
  SELECTs, i.e. read-only. Official audited results require the
  refresh streams; say "TPC-H-derived" for anything else (the spec
  police are real, and so is the Fair Benchmarking paper — topic 0
  guide).
- **Scale factor changes the winner**: SF1 (~1 GB) fits in cache,
  SF100 doesn't — engine rankings flip between them, exactly topic
  0's memory ladder. A comparison at one SF is a data point, not a
  ranking.

## How to read the paper (with the concepts in hand)

TPCTC 2013, ~20 pages, one evening — but only with the queries open
(`extension/tpch/dbgen/queries/q01.sql…q22.sql` in DuckDB):

- **§1–2** History and benchmark-design philosophy — skim; the "a
  benchmark shapes a decade of engine development" argument is the
  keeper.
- **§3–4 — read carefully.** The 28 choke points (Step 2's taxonomy
  expanded), each with the queries that hit it and what an engine
  must implement to pass. For each CP, ask: does FalkorDB have this
  capability? That turns the catalog into an audit list.
- For every choke point, open the actual query text and find the
  clause that triggers it — Q1's tiny GROUP BY domain (Step 3),
  Q6's three range predicates (Step 4), Q9's join graph and LIKE
  (Step 5) are the three to do first.
- **§5 (lessons/hidden messages)** — this is where Step 6 and
  Step 7 live: uniformity, refresh-stream skipping, scale-factor
  sensitivity.

## Questions (answer in notes.md)

1. Map Q1/Q6/Q9 onto FalkorDB-relevant analogues: which Cypher query
   shapes hit the same choke points (small-domain agg, scan+filter,
   join-order + skew)?
2. Our dbgen-lite is uniform AND independent like the real dbgen.
   Which columns would need correlation to break `q1_flat`'s
   perfect-group-code trick?
3. Why does Q6's ~2% selectivity favor branchy evaluation while 50%
   would favor branchless (topic 17's crater)? Predict the measured
   crossover for `q6_branchless`.
4. Choke point CP3 (materialization): which of the 22 queries would
   an incremental-view engine (topic 27 preview) answer in O(1)?
5. TPC-H says nothing about updates. What does TPC-C's NewOrder mix
   test that no TPC-H query can (see reading-oltpbench-tpcc.md)?

## References

**Papers**
- Boncz, Neumann, Erling — "TPC-H Analyzed: Hidden Messages and
  Lessons Learned from an Influential Benchmark" (TPCTC 2013) — the
  choke-point catalog; read with the queries open

**Code**
- [duckdb](https://github.com/duckdb/duckdb)
  `extension/tpch/dbgen/queries/q01.sql … q22.sql` — the 22
  queries; reference answers in `dbgen/answers/`
