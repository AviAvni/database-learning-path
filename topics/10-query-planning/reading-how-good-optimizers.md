# Cardinality is the whole ballgame: the JOB audit

The humbling paper. Leis, Gubichev, Mirchev, Boncz, Kemper and Neumann
(VLDB '15) built the Join Order Benchmark (JOB) — 113 queries over IMDB, real
correlated data instead of TPC-H's synthetic uniformity — and audited every
layer of the classical optimizer stack. The verdict reorders this whole topic:
cardinality error dwarfs cost-model error dwarfs search-space limits. Before
the paper, this chapter builds the layers being audited and the estimator being
indicted, step by step — then hands you the reading route.

**Every number below is cited to the section, figure or table of the paper it
came from**, and every number *not* in the paper is arithmetic done here on
assumptions stated here. This guide is written against the VLDB proceedings
version (PVLDB vol. 9 no. 3, pp. 204–215); if you have the extended VLDB
Journal 2018 version, section numbers shift. **Topic 10 has no measured lane** —
its benchmark harness measures only your own code — so nothing here is a
measurement taken on this machine, and none of these figures appear in
`FINDINGS.md`.

## The problem in one sentence

Every cost-based optimizer ranks plans using guessed row counts; on real
correlated data those guesses go wrong in a way that gets *systematically
worse the more joins you add* — the paper measures PostgreSQL estimates wrong
by 10× or more for 16% of one-join subplans, 32% at two joins and 52% at three
(§3.2) — so the question the paper asks is: of the optimizer's three parts
(estimates, cost model, search), which one is actually responsible for slow
queries?

## The concepts, step by step

### Step 1 — the three claims an optimizer makes

> **In:** a SQL query and a set of catalog statistics.
> **Out:** a decomposition of "the optimizer" into three separately testable
> components, which is the paper's whole experimental design.

Two terms first, because everything below is stated in them.

- A **logical plan** is an expression in **relational algebra** — the small
  algebra of operators over sets/bags of tuples: select (σ, a filter), project
  (π, choose columns), join (⋈), aggregate, union. It says *what* result you
  want. `σ_year>2000(title) ⋈ movie_info` is a logical plan.
- A **physical plan** picks an *algorithm* for each logical operator: this join
  is a hash join, that scan is an index scan, this aggregate sorts first. Two
  physical plans with wildly different runtimes can compute the identical
  logical result. The optimizer's job is choosing among them.

A classical cost-based optimizer is three separable components, each making its
own claim:

1. **cardinality estimation** — **cardinality** is simply the number of rows a
   (sub)expression produces. Estimation is a guess at that number for each
   subplan, before running it: "this filter keeps ~500 of 6M rows".
2. a **cost model** — a formula turning estimated cardinalities into a single
   comparable number, intended to be monotone in runtime: "a hash join
   producing 500 rows from these inputs costs X". It is a *proxy*: you cannot
   run every candidate plan to time it, so you score them.
3. **plan search** — an algorithm (dynamic programming, greedy, genetic) that
   walks the space of join orders and tree shapes, scores each candidate with
   the cost model, and keeps the cheapest.

```
   estimates ──► cost model ──► search ──► chosen plan
   (guessed        (formula        (which of the
    row counts)     over guesses)   candidate plans wins)
```

Each layer consumes the previous one's output, so a failure anywhere poisons
everything downstream — and until this paper, nobody had *measured* which layer
fails in practice on real data. That is the entire contribution. Everything
after Step 4 is machinery for answering it.

Why it matters: this decomposition is the reason the paper can assign blame at
all. If you only measure end-to-end runtime, a slow query tells you nothing
about which of the three to go fix.

### Step 2 — how every system estimates joins: three assumptions

> **In:** per-column catalog statistics — row counts, distinct-value counts,
> and **histograms** (a table of value ranges with the row count falling in
> each, so that a range predicate can be answered by summing buckets).
> **Out:** a single estimated cardinality per subplan, and three named
> assumptions to hold responsible when it is wrong.

**Selectivity** is the fraction of its input rows a predicate keeps: a
selectivity of 0.01 on a 1M-row table means an estimated 10,000 rows out. The
estimator that all five audited systems run (PostgreSQL and three unnamed
commercial engines, "DBMS A/B/C", plus HyPer) rests on the same three
assumptions, which the paper names in §2.3:

- **Uniformity** — every value of a column is equally frequent (once you are
  inside a histogram bucket), so an equality predicate keeps 1/NDV of the rows,
  where NDV is the number of distinct values in the column.
- **Independence** — predicates do not correlate, so a conjunction's
  selectivity is the *product* of the individual selectivities.
- **Principle of inclusion** (the paper's phrase; you will also see
  "containment") — in a join between key sets, every value of the smaller
  domain appears in the larger one, so nothing is lost to non-matching keys.

Those three assumptions collapse into one formula, which the paper prints in
§2.3 for PostgreSQL's equi-join:

```
   |T1 ⋈_{x=y} T2|  =  |T1| · |T2| / max( dom(x), dom(y) )

   |T|       cardinality of T (its row count)
   x, y      the join columns
   dom(x)    the number of distinct values of x  (postgres's n_distinct)
```

An implementation is a handful of lines:

```rust
// ILLUSTRATION — not quoted from any repo in this course. This is the §2.3
// formula above, transcribed. The real implementations are postgres
// src/backend/utils/adt/selfuncs.c (eqjoinsel) and duckdb
// src/optimizer/join_order/cardinality_estimator.cpp:908, which divides a
// product of base cardinalities by a product of per-equivalence-class domains.
  1  fn estimate_join_card(tables: &[Table], preds: &[EquiPred]) -> f64 {
  2      let mut card: f64 = tables.iter().map(|t| t.rows as f64).product();
  3      for p in preds {
  4          card /= p.ndv_left.max(p.ndv_right) as f64;  // uniformity: 1/NDV
  5      }   // each predicate applied INDEPENDENTLY — on correlated data the
  6      card // true overlap is larger, so factors compound toward zero
  7  }
```

Cheap to compute (a few scalars per column), and exactly right on uniform,
independent data. Real data is neither. The paper measures how wrong the
*base-table* half of this is before any join is involved, over 629 base-table
selections (§3.1, Table 1), reporting q-error (defined in Step 3) quantiles:

```
   Table 1 (§3.1) — q-error of base-table selection estimates
                median    90th     95th      max
   PostgreSQL     1.00    2.08     6.10       207
   DBMS A         1.01    1.33     1.98      43.4
   DBMS B         1.00    6.03     30.2   104,000
   DBMS C         1.06    1,677    5,367    20,471
   HyPer          1.02    4.47     8.00     2,084
```

Read that table twice. The *median* is essentially perfect everywhere — half of
all base-table estimates are dead on. It is the tail that kills you, and the
tail is already four to five orders of magnitude wide before a single join has
happened.

Why it matters: every later error in this paper is this error, multiplied.

### Step 3 — q-error, and why errors compound with join count

> **In:** an estimated cardinality and the true cardinality for the same
> subplan.
> **Out:** one scale-free error number per subplan, and the argument for why
> that number's *distribution* widens exponentially in the number of joins.

The standard metric is **q-error**. The paper defines it in §3.1 as "the factor
by which an estimate differs from the true cardinality. For example, if the true
cardinality of an expression is 100, the estimates of 10 or 1000 both have a
q-error of 10." Formally:

```
   q-error(est, true) = max( est/true , true/est )      (always ≥ 1)
```

It is deliberately symmetric and multiplicative: being 10× low and 10× high are
both "q-error 10", and — unlike absolute or relative error — it does not care
whether the true value is 100 or 100 million. That is what makes it summable
across a whole workload.

**Work it on a real pair.** The paper's footnote 6 (§3.2) reports that for one
JOB two-join query with a **true cardinality of 2,600**, PostgreSQL produced
estimates of **3, 9, 128 or 310** — *for the identical query*, varying only the
textual order of relations in `FROM` and predicates in `WHERE`:

```
   true = 2600
   est = 3     q-error = max(3/2600, 2600/3)     = 2600/3   =  866.67
   est = 9     q-error = max(9/2600, 2600/9)     = 2600/9   =  288.89
   est = 128   q-error = max(128/2600, 2600/128) = 2600/128 =   20.31
   est = 310   q-error = max(310/2600, 2600/310) = 2600/310 =    8.39
```

Every one of these is an *underestimate*, and the best of them is still 8× low.
Note also what the spread means: two syntactically identical queries get
estimates 100× apart, so the estimator is not even a function of the query's
semantics.

**Now the compounding, worked on three concrete predicates.** Take a `title`
table and three filters. The row count and the three selectivities below are my
assumptions, chosen to have IMDB's *shape*; they are not measured figures from
the paper.

```
   |title| = 2,500,000 rows      (assumption)

   p1: production_year BETWEEN 2000 AND 2005     sel(p1) = 0.20
   p2: country = 'FR'                            sel(p2) = 0.05
   p3: genre   = 'Drama'                         sel(p3) = 0.25

   INDEPENDENCE (what the estimator computes):
     sel(p1 ∧ p2 ∧ p3) = 0.20 × 0.05 × 0.25 = 0.0025
     estimate           = 2,500,000 × 0.0025 = 6,250 rows

   CORRELATED (what the data actually is), written as conditionals:
     P(p2)            = 0.05     French titles
     P(p3 | p2)       = 0.50     French cinema skews to drama (0.25 overall)
     P(p1 | p2 ∧ p3)  = 0.30     these cluster in the 2000s (0.20 overall)
     true sel         = 0.05 × 0.50 × 0.30 = 0.0075
     truth            = 2,500,000 × 0.0075 = 18,750 rows

   q-error = 18,750 / 6,250 = 3.0
```

Two mild correlations — one factor of 2, one of 1.5 — produced a 3× error on a
*single* table. Now push it through joins. Cardinalities multiply, so their
errors multiply too, and if each of six joins carries an independent 3× error
in the same direction:

```
   3^6 = 729×   composed error after six joins
   2^6 =  64×   the same arithmetic with a mild 2× error per join
```

That is arithmetic on my assumption of a constant per-join factor, not a
measurement. **The paper's actual measurement** is Figure 3 (§3.2), which
boxplots over 100,000 estimates from all five systems, grouped by the number of
joins in the subplan (0 through 6). Read three things off it:

1. The vertical axis spans **underestimation by 10⁸ and overestimation by 10⁴**
   — the error is wildly asymmetric, and it is asymmetric toward *under*.
2. The boxes get taller monotonically with join count. In the paper's words,
   the errors "grow exponentially… as the number of joins increases", and "for
   all systems we routinely observe misestimates by a factor of 1000 or more".
3. The quantified progression for PostgreSQL: **16%** of one-join estimates are
   wrong by ≥10×, **32%** at two joins, **52%** at three. DBMS A is better and
   fails the same way: 15%, 25%, 36%.

Be precise about what grows. It is **not** the median — the medians stay near 1
and drift downward. It is the *width of the distribution*, and it widens
asymmetrically downward. A guide that says "median q-error reaches 10²–10⁴ at
six joins" is misreading Figure 3.

Why underestimation is the dangerous direction: an optimizer told "only 3 rows
come out of this" cheerfully picks a nested-loop join, which then runs orders
of magnitude more iterations than promised. Overestimation makes you buy a hash
table you did not need; underestimation makes you buy a quadratic algorithm.

Why it matters: q-error is the unit the rest of the paper is denominated in,
and its asymmetry is the reason Step 7's mitigation works.

### Step 4 — the benchmark: real data is part of the method

> **In:** the observation that nobody had caught this before.
> **Out:** a dataset and a query set on which the failure is *detectable* —
> the paper's first contribution, prior to any measurement.

TPC-H, the standard benchmark of the era, is *generated* data: uniform value
distributions, independent columns. On it, Step 2's three assumptions are true
by construction, and estimates look fine. The paper confirms this directly in
§3.3 — TPC-H estimates are far better behaved than JOB's. **The standard
benchmark was structurally incapable of detecting the standard failure.**

So the authors built JOB on the real IMDB dataset (§2.1): a May 2013 snapshot,
**21 tables**, 3.6 GB as CSV, with `cast_info` at 36M rows and `movie_info` at
15M rows. The correlations are the point — actors correlate with genres
correlate with production years correlate with countries.

The queries (§2.2) are **33 query structures × 2–6 variants = 113 queries**,
with **3 to 16 joins each and 8 joins on average**. Variants of one structure
share a join graph and differ only in their selection predicates, which
isolates estimation quality from plan-shape effects.

Why it matters: this is the transferable lesson even if you never touch a
relational optimizer. **The data distribution is part of the benchmark.** A
generator that produces independent uniform columns cannot falsify an
independence assumption, no matter how many queries you run through it.

### Step 5 — the method: extract ground truth, then inject it

> **In:** the 113 queries and the five systems.
> **Out:** *two* datasets that feed different later steps — (a) each system's
> estimated cardinality for every subplan, compared against the true
> cardinality, which is all of §3; and (b) a modified PostgreSQL that can be
> *fed* any cardinalities you like, which is the instrument for §4, §5 and §6.

This is the fork in the paper, and it is the piece worth stealing.

First, extraction (§2.4). For every one of the 113 queries they enumerate its
subplans and run a `COUNT(*)` query per subplan to obtain the **true**
cardinality of every intermediate result. Separately they read each system's
*estimate* for the same subplans out of its `EXPLAIN` output. Dataset (a) is
the pairing of those two, and §3 is simply its q-error distribution.

Second, injection. They patched PostgreSQL so that its cardinality estimates
can be overridden from outside — you hand it a table of cardinalities and it
optimizes as if those were its own estimates. Now every later question becomes
a controlled experiment:

1. **Is the estimator to blame?** Run each query twice, once with PostgreSQL's
   own estimates and once with true cardinalities injected, and compare
   *runtimes* of the resulting plans (§4).
2. **Is the cost model to blame?** Hold cardinalities at truth and vary only
   the cost function (§5). Any difference that remains is the cost model's.
3. **Is the search to blame?** Hold cardinalities at truth and vary only the
   enumeration algorithm and the admissible tree shapes (§6).

The experimental setup is worth noting so you can judge what transfers (§2.5):
two Intel Xeon X5570 @ 2.9 GHz (8 cores), 64 GB RAM, PostgreSQL 9.4, one core
per query, `work_mem` 2 GB, `shared_buffers` 4 GB, `effective_cache_size`
32 GB — and `geqo_threshold` raised to 18 so that PostgreSQL runs its dynamic
programming rather than its genetic optimizer on the big queries. This is a
*main-memory* setting, which matters a lot for §5.

Why it matters: injecting ground truth one layer at a time is how you assign
blame in any layered system. If plans become good the moment true cardinalities
arrive, the estimator was the problem and no amount of cost-model tuning would
have helped. (This is topic 0's fair-benchmarking discipline applied to a
brain rather than a loop.)

### Step 6 — the verdict: cardinality ≫ cost model ≫ search

> **In:** the injection instrument from Step 5.
> **Out:** a ranking of the three components by measured impact on runtime,
> with a number attached to each.

**Cardinality is the whole ballgame (§4).** With PostgreSQL's own estimates
replaced by true cardinalities, the distribution of runtime change across the
113 queries is (§4.1):

```
   slowdown of a system's estimates vs. true cardinalities, share of queries
                 <0.9  [0.9,1.1)  [1.1,2)  [2,10)  [10,100)  >100
   PostgreSQL    1.8%     38%       25%     25%     5.3%     5.3%
   DBMS A        2.7%     54%       21%     14%     0.9%     7.1%
   DBMS B        0.9%     35%       18%     15%     7.1%      25%
   DBMS C        1.8%     38%       35%     13%     7.1%     5.3%
   HyPer         2.7%     37%       27%     19%     8.0%     6.2%
```

Roughly a tenth of queries are 10× or worse off because of estimation error
alone, and for DBMS B a quarter of the workload is over 100× off.

**The cost model matters, but an order of magnitude less (§5).** PostgreSQL's
cost model is "over 4000 lines of C" (§5.1), and with true cardinalities
injected its median runtime-prediction error is **38%** (§5.2). Tuning its CPU
cost parameters up by 50× for a main-memory machine takes that to **30%**
(§5.3) — the defaults imply that processing a tuple is 400× cheaper than
reading it from a page, which was true of 1990s disks and is not true here.

Then §5.4 replaces the whole thing with a deliberately trivial model, `Cmm`,
which counts only tuples flowing through operators:

```
   Cmm(R)          = τ·|R|                              base scan or selection
   Cmm(T1 ⋈HJ T2)  = |T|  + Cmm(T1) + Cmm(T2)           hash join
   Cmm(T1 ⋈INL R)  = Cmm(T1) + λ·|T1|·max(|T1⋈R|/|T1|, 1)   index-nested-loop

   τ = 0.2   scans are cheaper per tuple than joins
   λ = 2     an index lookup costs ~2× a hash-table lookup
```

Note this is *not* pure Cout (the sum of intermediate cardinalities and nothing
else); it discounts scans by τ and prices index lookups at λ. On true
cardinalities, geometric mean over all queries: the **tuned** PostgreSQL model
is **41% faster** than the standard one, and the trivial `Cmm` is **34%
faster** (§5.4). The paper's own conclusion from those two numbers: the
improvement "is not insignificant, but… it is dwarfed by improvement in query
runtime observed when we replace estimated cardinalities with the real ones".

**Search matters least, and mostly by not being catastrophic (§6).** Two
results. First, join order is not free: over 10,000 random plans per query
(§6.1), the average ratio between the worst and best plan is **101×** with no
indexes, **115×** with primary-key indexes and **48,120×** with PK+FK indexes —
so a *randomly* chosen order is a disaster. But an optimizer only has to find a
good one, not the best one, and the share of random plans within 1.5× of
optimal is **44% / 39% / 4%** for those three index configurations.

Second, tree shape. A **left-deep** plan is one where every join's right input
is a base table, so the tree is a single spine — this is System R's restriction
and it makes the DP tractable. A **bushy** plan allows a join whose *both*
inputs are themselves joins. Table 2 (§6.2), all with true cardinalities and
normalized against the optimal bushy plan:

```
   Table 2 (§6.2) — slowdown of restricted tree shapes vs. optimal bushy
                    PK indexes                PK + FK indexes
                median   95%    max       median    95%       max
   zig-zag        1.00   1.06   1.33        1.00    1.60      2.54
   left-deep      1.00   1.14   1.63        1.06    2.49      4.50
   right-deep     1.87   4.97   6.80       47.2    30,931   738,349
```

Left-deep costs you essentially nothing at the median and 2.5× at the 95th
percentile with FK indexes. Right-deep-only is a catastrophe. So "bushy trees
beat left-deep by 10–40%" is not what this table says: bushy's advantage is in
the tail, not the median.

Third, enumeration algorithm. Table 3 (§6.3) compares exhaustive dynamic
programming against Quickpick-1000 (1000 random plans, keep the best) and GOO
(greedy operator ordering), each normalized by that configuration's optimal
plan:

```
   Table 3 (§6.3) — plan quality by search algorithm
                        PK indexes                    PK + FK indexes
                 PG estimates      true card.    PG estimates       true card.
                med   95%   max   med  95%  max   med   95%    max   med  95%  max
   Dyn. Prog.   1.03  1.85  4.79  1.00 1.00 1.00  1.66  169  186,367 1.00 1.00 1.00
   QuickPick-1000 1.05 2.19 7.29  1.00 1.07 1.14  2.52  365  186,367 1.02 4.72 32.3
   Greedy (GOO) 1.19  2.29  2.36  1.19 1.64 1.97  2.35  169  186,367 1.20 5.77 21.0
```

This one table is the paper's whole thesis in numbers. Compare *along a row*:
exhaustive DP goes from 1.66 median / 186,367 max with PostgreSQL's estimates
to a perfect 1.00 / 1.00 with true cardinalities. Now compare *down a column*:
with true cardinalities, swapping the world's best search for a greedy
heuristic costs you 1.20 at the median. The search algorithm is worth ~20%; the
estimates are worth five orders of magnitude.

```
   what the paper actually measured, ranked
   cardinality estimates   10× or worse for ~11% of queries (§4.1);
                           DP max 186,367 → 1.00 on truth (Table 3)
   cost model              34–41% geometric-mean runtime (§5.4)
   search algorithm        1.00 → 1.20 median, DP → greedy on truth (Table 3)
   tree shape              1.00 → 1.06 median, bushy → left-deep (Table 2)
```

Why it matters: it tells you where to spend engineering effort, and it explains
why DuckDB ships a cost model that fits on one screen
(`src/optimizer/join_order/cost_model.cpp:40-48`) while spending real
complexity on its cardinality estimator.

### Step 7 — living with wrong estimates: robust plans

> **In:** the finding that estimates cannot be fixed cheaply.
> **Out:** two concrete mitigations the paper measures, both of which trade a
> little best case for a lot of worst case.

Since Step 6 says the estimates are the problem and Step 3 says they are
structurally hard, the pragmatic move is to prefer plans that are **robust to
misestimation** — plans whose cost degrades gracefully when the estimate is
wrong, rather than plans that are optimal if the estimate is right.

The argument is asymptotic, and the paper makes it in §4.1: a hash join is O(n)
in its input size, while a non-index nested-loop join is O(n²). Under a 100×
underestimate the hash join is ~100× slower than predicted; the nested-loop
join is ~10,000× slower. Same wrong estimate, quadratically different
consequence. The measurements:

- Disabling non-index nested-loop joins removed **all** timeouts from the
  workload (§4.1, Figure 6b) — the entire catastrophic tail was one operator
  choice.
- Adding rehashing to the hash table, so an undersized hash table grows instead
  of degrading, brought it to **less than 4%** of queries off by more than 2×
  (§4.1, Figure 6c).
- And the price of this insurance is small: fully cached, a hash join's
  advantage over an index-nested-loop join is at most **5× in PostgreSQL and 2×
  in HyPer** (§4.2). You are giving up at most 5× best case to remove an
  unbounded worst case.

One caution before you conclude "so just collect better statistics": §3.4 shows
the authors injecting *true* distinct-value counts into PostgreSQL, which made
underestimation **worse**, not better. The uniformity error and the
independence error had been partially cancelling — two wrongs making a right.
Fixing one input of a wrong model does not give you a right model.

Why it matters: this is the shape of every mitigation in a system with
irreducible uncertainty — you do not chase the best expected case, you bound
the worst one.

## How to read the paper (with the concepts in hand)

~1.5 h. The methodology (§2) is worth as much as the findings. Section numbers
below are the PVLDB version's.

- **§2 — Background and Methodology** (Steps 4 and 5). §2.1 the IMDB data,
  §2.2 the queries, §2.3 PostgreSQL's estimator and its three assumptions,
  §2.4 the extraction/injection instrument, §2.5 the hardware. Note *why* each
  query family exists: the correlations are chosen deliberately.
- **§3 — Cardinality Estimation. Read carefully; this is the core.** Table 1
  for base tables (§3.1), **Figure 3 for joins (§3.2)** — spend real time on
  Figure 3, it is the paper's central result. §3.3's TPC-H comparison is the
  control. §3.4 is the "two wrongs make a right" trap from Step 7.
- **§4 — When Do Bad Cardinality Estimates Lead to Slow Queries?** Step 6's
  first result and Step 7's mitigations. Figure 6's three panels are the
  argument: (a) estimates vs. truth, (b) nested loops disabled, (c) rehashing.
- **§5 — Cost Models** (Step 6's second result). §5.1 the 4000 lines, §5.2 the
  38% prediction error, §5.3 the tuning, §5.4 the trivial `Cmm` — note it is
  *not* pure Cout.
- **§6 — Plan Space** (Step 6's third result). §6.1 how much join order
  matters, §6.2 Table 2 on bushy vs left-deep vs right-deep, §6.3 Table 3 on
  DP vs Quickpick vs greedy. Table 3 is the single best summary of the paper.
- **§7–8 — Related work and conclusions.** §4.4's "join-crossing correlations"
  is the open problem the authors flag. The learned-cardinality papers in the
  topic README (Kipf '19, Neo, Bao) are its direct descendants — read them
  after, not instead.

## Questions for notes.md

1. Why does independence UNDERestimate on correlated data rather than
   overestimate? Construct a two-predicate example where sel(a)×sel(b) is 100×
   low, and state the conditional probability that makes it so.
2. Table 3 shows DP with PostgreSQL's estimates at a median of 1.66 and greedy
   with true cardinalities at 1.20. Write the one-sentence engineering
   conclusion, then say which of your engines' behaviour it explains.
3. §3.4: injecting *true* distinct counts made estimates worse. What does that
   tell you about the practice of "improving statistics" in isolation, and what
   is the analogous trap in a system you have tuned?
4. "Robust plans": hash join degrades linearly with a bad estimate,
   nested-loop quadratically, and the price is ≤5× (§4.2). Frame it as a
   minimax decision — what is the regret matrix?
5. Design JOB-for-graphs: what is the correlated-data equivalent for Cypher
   patterns (degree skew × label correlation × triangle density)? Sketch three
   queries where independence-based nnz estimation (matrix-product size) blows
   up the same way. This is the M10/M22 benchmark seed — write it down
   properly.

## Takeaway

The optimizer's three layers fail by wildly different amounts, and the paper
measured the gap: with true cardinalities injected, exhaustive DP finds the
optimal plan every time (Table 3, 1.00/1.00/1.00), while with real estimates
its worst case is 186,367× optimal. Swapping the search algorithm for a greedy
heuristic costs 20% at the median; swapping the 4000-line cost model for a
three-line one costs 34–41% in geometric mean. Cardinality estimation is not
*a* problem in query optimization — on real data it is *the* problem, and the
right response is not better guesses but plans whose cost does not explode when
the guess is wrong.

## Done when

Answer each before unfolding it.

- [ ] State the definition of q-error, and compute it for an estimate of 310
      against a true cardinality of 2,600.
  <details><summary>Answer</summary>

  q-error(est, true) = max(est/true, true/est), so it is always ≥ 1 and treats
  a 10× under- and a 10× overestimate identically. Here 310 < 2600, so the
  max is 2600/310 = **8.39**. This is the paper's own footnote-6 example
  (§3.2): the same two-join query, true cardinality 2,600, got estimates of 3,
  9, 128 or 310 depending only on the textual order of relations in `FROM` —
  q-errors of 866.67, 288.89, 20.31 and 8.39. Even the *best* of the four is
  8× low.

  </details>

- [ ] Figure 3 shows the error growing with join count. Say precisely what
      grows — and what does *not*.
  <details><summary>Answer</summary>

  The **distribution widens**; the median does not move much. The medians in
  Table 1 (base tables, §3.1) are 1.00–1.06 across all five systems, and in
  Figure 3 they stay near 1 and drift *downward* with join count. What grows
  is the spread, and it grows asymmetrically: the axis spans underestimation
  by 10⁸ against overestimation by only 10⁴. The paper's quantified version
  (§3.2) is the fraction of estimates wrong by ≥10×: PostgreSQL 16% at one
  join, 32% at two, 52% at three; DBMS A 15%, 25%, 36%. Saying "median q-error
  reaches 10²–10⁴ at six joins" misreads the figure.

  </details>

- [ ] Three predicates with selectivities 0.20, 0.05 and 0.25 on a 2,500,000-row
      table. Give the independence estimate, then the true count if
      P(p3 | p2) = 0.50 and P(p1 | p2 ∧ p3) = 0.30, and the resulting q-error.
  <details><summary>Answer</summary>

  Independence multiplies: 0.20 × 0.05 × 0.25 = 0.0025, so the estimate is
  2,500,000 × 0.0025 = **6,250 rows**. The correlated truth chains conditionals
  instead: 0.05 × 0.50 × 0.30 = 0.0075, so 2,500,000 × 0.0075 = **18,750
  rows**. q-error = 18,750/6,250 = **3.0**. Two mild correlations (a 2× and a
  1.5×) produced a 3× error on a single table with no joins at all — and
  because cardinalities multiply along a join tree, a constant 3× per join
  compounds to 3⁶ = 729× after six. (The selectivities here are stated
  assumptions for the arithmetic, not measurements from the paper; the paper's
  measured version is Figure 3.)

  </details>

- [ ] Rank cardinality estimation, cost model and search by measured impact,
      with one number each.
  <details><summary>Answer</summary>

  **Cardinality** first, by orders of magnitude: ~11% of queries are 10× or
  worse purely from estimation error (§4.1), and in Table 3 exhaustive DP goes
  from a max of 186,367× optimal on PostgreSQL's estimates to exactly 1.00 on
  true cardinalities. **Cost model** second: with truth injected, a tuned model
  is 41% faster and the trivial three-line `Cmm` is 34% faster than
  PostgreSQL's 4000-line one (§5.4) — real, but the paper itself calls it
  "dwarfed". **Search** last: with truth injected, replacing DP with greedy GOO
  costs 1.20 at the median (Table 3), and restricting bushy to left-deep costs
  1.00–1.06 at the median (Table 2). Note that all three of these are measured
  *with the layers below held at truth* — that is what makes them comparable.

  </details>

- [ ] The paper's cost-model replacement is often called "Cout". Why is that
      wrong, and what is it?
  <details><summary>Answer</summary>

  Cout is the sum of all intermediate result cardinalities and nothing else.
  The paper's model (§5.4) is `Cmm`, which is Cout *plus two constants*: scans
  and selections are charged τ·|R| with **τ = 0.2**, discounting a scan
  relative to a join, and an index-nested-loop join is charged
  λ·|T1|·max(|T1⋈R|/|T1|, 1) with **λ = 2**, pricing an index lookup at twice a
  hash-table lookup. Only the hash join term, |T| + Cmm(T1) + Cmm(T2), is pure
  Cout. It is also explicitly a *main-memory* model — it does not model I/O at
  all, which is why §2.5's fully-cached setup matters when you decide whether
  the result transfers to your system.

  </details>

- [ ] Why could TPC-H never have found this result?
  <details><summary>Answer</summary>

  TPC-H's data is generated with uniform value distributions and independent
  columns, so Step 2's uniformity and independence assumptions are true *by
  construction* and the estimator is right for the right reasons. §3.3 shows
  exactly this — TPC-H estimates are far better behaved than JOB's. The
  benchmark could not falsify the hypothesis it was implicitly testing. That is
  why the paper's first contribution is a *dataset* (real IMDB: 21 tables,
  36M-row `cast_info`) and a *query set* (33 structures × 2–6 variants = 113
  queries, 3–16 joins, 8 on average) rather than a measurement. The data
  distribution is part of the benchmark.

  </details>

## References

**Papers**
- Leis, Gubichev, Mirchev, Boncz, Kemper, Neumann — "How Good Are Query
  Optimizers, Really?" PVLDB 9(3):204–215, 2015. ~1.5 h. The methodology (§2)
  is worth as much as the findings — extracting true cardinalities and
  injecting them one layer at a time is what makes the blame assignable.
  <http://www.vldb.org/pvldb/vol9/p204-leis.pdf>
- Leis et al. — "Query optimization through the looking glass, and what we
  found running the Join Order Benchmark", VLDB Journal 27(5), 2018. The
  extended version; same results, more configurations, different section
  numbers.
- Moerkotte, Neumann, Steidl — "Preventing Bad Plans by Bounding the Impact of
  Cardinality Estimation Errors", PVLDB 2009. Where the q-error metric and its
  optimality properties come from.

**Code**
- `duckdb/duckdb@6c0c1a68` — `src/optimizer/join_order/cost_model.cpp:40-48`,
  a cost model small enough to read in one sitting, which is this paper's §5
  conclusion shipped as a product.
- `postgres/postgres@701f021` — `src/backend/utils/adt/selfuncs.c`, the
  estimator being audited; `src/backend/optimizer/` for the search it feeds.

**In this topic**
- `reading-postgres-optimizer.md` — the estimator and search this paper
  measures, read as code.
- `reading-duckdb-optimizer.md` — a modern optimizer built with this paper's
  conclusion already assumed.
- `reading-selinger-cascades.md` — the 1979 design whose assumptions this paper
  finally tested at scale.
