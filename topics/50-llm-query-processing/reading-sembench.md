# SemBench: the cost of a semantic query lives in joins, LIMIT and prompts

SemBench (Lao, Zimmerer, Ovcharenko, Cong, Russo, Vitagliano, Cochez, Özcan,
Gupta, Hottelier, Jagadish, Kissel, Schelter, Kipf & Trummer; PVLDB 19(8),
2026; arXiv 2511.01716) is the TPC-H moment for semantic operators: 55
queries over five scenarios, run on three academic engines (LOTUS,
Palimpzest, ThalamusDB) and one commercial one (BigQuery's AI functions),
all forced onto the same model. Its value for this topic is that it measures
where the money goes when the systems of [reading-lotus-semantic-operators.md](reading-lotus-semantic-operators.md)
and [reading-abacus.md](reading-abacus.md) meet real queries. The answer is
not where an optimizer paper would look first. The largest factors — more
than 100× between systems on the same query — come from whether `LIMIT`
stops the scan, from joins that are quadratic in calls and in memory, and
from prompt templates. This chapter builds the benchmark's vocabulary, then
reads the per-scenario and aggregate results with those three effects in
view, and records one place where the text and its table disagree.

## The problem in one sentence

When every row of a semantic operator is an LLM call, what actually
separates a cheap engine from an expensive one on the same query — the
optimizer, the operator implementations, or the plumbing around them?

## The concepts, step by step

### Step 1 — a semantic query processing engine, and the benchmark's shape

> **In:** semantic operators as LOTUS defines them (filter, join, map, rank,
> classify).
> **Out:** the systems under test, the five scenarios and their sizes — the
> workload every later step's numbers come from.

A **semantic query processing engine** (SQPE) "extend[s] relational algebra
by semantic operators that perform tasks described in natural language" over
tables whose cells may hold text, images or audio (§1). SemBench writes them
in BigQuery's syntax: `WHERE AI.IF(pic, 'the picture shows a red car')` is a
semantic filter (Example 1.1). **Modalities** are the data types an operator
reads: text, image, audio.

Table 2 (counts of semantic operators per scenario, and table sizes):

| scenario | queries | filter | join | map | rank | classify | text rows | image rows | audio rows |
|---|---|---|---|---|---|---|---|---|---|
| Movie | 10 | 4 | 3 | – | 2 | 1 | 1,375,738 | – | – |
| Wildlife | 10 | 17 | – | – | – | – | – | 8,718 | 650 |
| E-Commerce | 14 | 12 | 9 | 3 | 1 | 2 | 44,446 | 44,446 | – |
| MMQA | 11 | 5 | 3 | 4 | – | – | 5,000 | 1,000 | – |
| Cars | 10 | 12 | – | – | – | 1 | 157,376 | 30,131 | 1,387 |
| **Total** | **55** | 50 | 15 | 7 | 3 | 4 | 1,582,560 | 84,295 | 2,037 |

Experiments run at a **base scale** (Table 3) — the number of rows in each
scenario's main table — far below the full sizes: Movie 2,000, Wildlife
200, E-Commerce 500, Cars 19,672, MMQA 200.

Why it matters: the benchmark's "small" scales are small by database
standards and still expensive — the scalability runs cost **$9,935.80** of
LLM calls and took **about 18 days** (§5.1).

### Step 2 — three metrics on one scale

> **In:** the queries of Step 1 and their ground truth (manual labels from
> the Kaggle datasets the scenarios combine, §1).
> **Out:** quality, cost and latency per query, normalized so they can be
> averaged — the columns of Tables 4 and 5.

Quality depends on the query's kind (§3.3, §5.1): **F1** for queries that
return rows; **relative error** |estimate − truth| / truth for aggregates,
mapped into [0, 1] by 1 / (1 + relative error); **Spearman's rank
correlation** for rankings (how well two orderings agree, from −1 to 1); and
the **Adjusted Rand Index** (ARI) for clustering-like joins (agreement of two
groupings, corrected for chance). Higher is better for all four. Cost is the
dollars paid for LLM calls; latency is wall time.

Worked: an aggregate query whose true answer is 40 and an engine's 50 has
relative error 10 / 40 = 0.25 and quality 1 / 1.25 = **0.80**; an answer of
80 (error 1.0) scores **0.50**; an exact answer scores 1.0.

Why it matters: the transform is gentle — doubling the answer still scores
0.5 — so aggregate queries sit higher in averages than their errors suggest.
Compare systems per query type, as Table 5 does, not on one grand mean.

### Step 3 — the controls: one model, parallelism 20, five runs

> **In:** the systems of Step 1.
> **Out:** the experimental controls that make cost differences attributable
> to the engines rather than the models.

Every system uses **gemini-2.5-flash** with reasoning disabled and
temperature 0 where the system allows it; "the choice of the model has the
biggest influence on system performance" (§5.1). LLM invocation
**parallelism** — requests in flight at once — is set to 20 (BigQuery does
not expose it). Each experiment runs five times. Versions: LOTUS 1.1.3,
Palimpzest 0.8.2 **with the Abacus optimizer turned off** (its MaxQuality
default operators instead), ThalamusDB 0.1.15 (§5.2).

Why it matters: two consequences for reading the results. First, Palimpzest
here is not Abacus — none of SemBench's numbers measure Abacus's search.
Second, parallelism 20 is the constant this topic's simulation borrows
(`src/model.rs`), so its "seconds" columns assume the same concurrency.

### Step 4 — LIMIT: the largest factor is whether the scan stops

> **In:** the Movie scenario's results (Table 4a) and the controls of Step 3.
> **Out:** the >100× cost gap and its cause — a physical-plan decision every
> relational engine already makes.

"For Queries Q1, Q5, and Q6, processing costs vary by more than a factor of
100× across different systems. All three queries use a LIMIT clause, but the
systems handle it differently. In LOTUS, the LIMIT clause is applied only as
a post-processing step after semantic operators are evaluated on all data"
(§5.3). Q2 also has a LIMIT but first restricts to one movie's reviews with an
ordinary equality predicate, so evaluating everything is cheap there. The
scalability run shows the same bug from the other side: LOTUS times out at
scale 16K on Q6, a join with a LIMIT, because it "does not terminate early
after collecting the number of rows required" (§5.4).

**LIMIT pushdown** — stopping operator evaluation once k rows are produced —
turns a scan of N rows into one of about k / s rows, where s is the
predicate's selectivity. Worked on this topic's lane 1e (simulated): LIMIT 10
over a predicate the oracle passes 12.35% of the time, 10,000 rows. Evaluate
everything: 10,000 calls. Stream in waves of 20 and stop: 10 / 0.1235 ≈ 81,
rounded up to whole waves = **120** calls, an **83×** saving — the same order
as SemBench's gap, and it is arithmetic, not optimization.

Why it matters: the biggest win in the benchmark is a rule from the 1970s.
A semantic engine that runs its operators as bulk DataFrame transformations
(LOTUS's design) loses the pipelining that makes LIMIT free in a
Volcano-style executor (topic 11).

### Step 5 — joins: quadratic calls, threshold variance, and memory

> **In:** the MMQA and E-Commerce scenarios and the join row of Table 5.
> **Out:** what a semantic join costs at small scale, the quality each
> cost-cutting strategy gives up, and why LOTUS's join cost varies run to run.

A nested-loop semantic join makes one call per pair. MMQA's Q7 at base scale
200 "required 40,000 LLM calls for join predicate evaluation" — 200 × 200 —
and took Palimpzest over 35 minutes and LOTUS nearly 40; its cost "was more
than 1,000 times that of a filtering query" (§5.3). At scale 400 the same
query is 160,000 calls (400 × 400), about $50 on LOTUS and $60 on Palimpzest;
an E-Commerce text-image join at scale 4,000 completed only on BigQuery, at
about $140 (§5.4).

Table 5's join row (base scale, queries all systems support):

| system | strategy (§5.5) | quality | latency | cost |
|---|---|---|---|---|
| Palimpzest | nested loop | 0.703 | 454.3 s | $3.29 |
| LOTUS | embedding pre-filter, LLM in the band | 0.614 | 541.1 s | $2.74 |
| ThalamusDB | batches of rows from both sides per prompt | 0.487 | 387.3 s | $0.14 |
| BigQuery | not described | 0.601 | 69.4 s | $1.02 |

Derived: LOTUS's pre-filter saves 1 − 2.74 / 3.29 = **17%** of the cost for
0.703 − 0.614 = 0.089 quality; ThalamusDB's batching saves **96%** for 0.216.
Neither is free, and the cheap one is cheap by changing the *prompt*, not the
plan.

LOTUS's join cost "demonstrates the highest variance across all systems" in
E-Commerce, because it "analyzes a data sample to determine thresholds …
that sample, and therefore the thresholds, differ" between runs (§5.3) — the
same variance Abacus reports for its EmbeddingJoin
([reading-abacus.md](reading-abacus.md), Step 6). Memory is the other bill:
E-Commerce Q8 "requires about 250 GB, 150 GB, and 100 GB of memory for
ThalamusDB, LOTUS, and Palimpzest" because image pairs are materialized and
not released (§5.4).

Why it matters: the join is where an optimizer can win orders of magnitude
(lane 3 of this topic: 60,000 pairs to 3,000 calls in the reference), and
where the current systems mostly buy savings with quality. A cost-based
optimizer needs the variance of an operator's cost, not just its mean.

### Step 6 — prompts, early termination and failures: the plumbing

> **In:** the Wildlife and Cars results, and Table 5's filter row.
> **Out:** the remaining differences between engines, none of which is plan
> choice.

Filters differ mainly by **prompt template** (§5.3, §5.5): Palimpzest's
templates are long, with few-shot examples; ThalamusDB's are concise. Table
5's filter row: LOTUS quality 0.896 at $0.34, Palimpzest 0.777 at $0.40,
ThalamusDB 0.698 at **$0.19**, BigQuery 0.574 at $0.36. ThalamusDB also
**terminates early** when further rows "cannot change the final result" —
for "cities with images or recordings of elephants" it stops evaluating a
city's rows after the first hit (§5.4). BigQuery's cost grows linearly with
scale while its time does not, because it "adjust[s] resources for each query
based on input size" (§5.4).

Failures are a cost too: "LLM invocation failures are common due to network
issues, unstable APIs, or rate limits", and LOTUS on Cars at scale 157k "may
stall for over 48 hours" (§5.4). The lessons list (§5.5) is five items:
exploit LIMIT, decide during execution whether more rows are needed, size
parallelism to the input, retry with exponential backoff, limit memory.
Operator **fusion** (one LLM call for several operators) and **caching**
across overlapping queries are named as missing in every system (§6).

Why it matters: the paper's own conclusion is that "the implementation of
semantic operators matters" (§6) more than anything the optimizers of the
previous two chapters search over. Caching is lane 1d of this topic: ten
agents' overlapping probes cost 26,150 oracle calls without a shared cache and
11,615 with one (simulated).

### Step 7 — reading the tables against the text

> **In:** Tables 2–5 and the prose of §5.
> **Out:** one inconsistency to avoid repeating, and how to cite the
> aggregate numbers.

§5.3 describes the Movie scenario as running at "a small scale of 500", but
Table 3 gives Movie's base scale as **2,000** (500 is E-Commerce's). The
per-query numbers in Table 4a are at whatever scale was actually used; the
text and the table cannot both be right, and this reader cannot tell which
from the paper.

Table 5's "Avg (All Operators)" row averages operator groups with very
different query counts; §5.5's "BigQuery achieves the best latency, with an
average latency of 48.1s" quotes the "Avg (Filter + Join)" row, not the
all-operator row (31.7 s). Cite the row you mean.

Why it matters: a benchmark is quoted for years. Quote it by table, row and
scale.

## How to read the paper with these concepts in hand

| section | what to take from it | step |
|---|---|---|
| §1, Example 1.1 | SQPE, AI.IF, why LLM cost dominates | 1 |
| §3, §4, Tables 1–2 | operators, scenarios, sizes | 1 |
| §5.1, Table 3 | model, parallelism 20, five runs, base scales, $9,935.80 | 1, 3 |
| §5.2 | versions; Palimpzest with Abacus off | 3 |
| §5.3, Table 4 | LIMIT >100×; Q7's 40,000 calls; LOTUS join variance; Movie "scale 500" | 4, 5, 7 |
| §5.4, Figure 1 | scaling; 160,000 calls; $140; 250/150/100 GB; 48-hour stalls | 5, 6 |
| §5.5, Table 5 | per-operator aggregates; lessons | 5, 6, 7 |
| §6 | implementation matters; fusion and caching | 6 |

## Questions to answer

1. LIMIT pushdown needs the engine to pipeline. Which LOTUS design choice
   (DataFrame-at-a-time operators) prevents it, and what would a fix cost?
2. ThalamusDB's join is 23× cheaper than Palimpzest's at 0.216 lower quality.
   Express the trade as dollars per quality point and compare with LOTUS's.
3. Why does LOTUS's join cost vary between runs while its filter cost does
   not? Which line of its source sets the sample?
4. A cost-based optimizer sees mean cost. What extra statistic would have
   made Abacus avoid MMQA's 0.81× min-cost plan, and does SemBench report it?
5. Which of §5.5's five lessons would a relational engine get for free?

## Done when

Answer each before unfolding it.

- [ ] You can state the benchmark's size and controls, and say what "Palimpzest" means in its tables.

  <details><summary>Answer</summary>

  55 queries in 5 scenarios (Table 2), run at base scales of 200–19,672 rows
  (Table 3) on LOTUS 1.1.3, Palimpzest 0.8.2, ThalamusDB 0.1.15 and BigQuery,
  all with gemini-2.5-flash, parallelism 20, five runs (§5.1–5.2). Palimpzest
  ran with the Abacus optimizer off, using its default MaxQuality operators —
  so SemBench measures Palimpzest's operators, not Abacus.

  </details>

- [ ] You can explain the >100× cost gap on Movie Q1, Q5 and Q6 and reproduce its order of magnitude from lane 1e.

  <details><summary>Answer</summary>

  All three have LIMIT; LOTUS applies LIMIT after evaluating the semantic
  operator on every row, while engines that stop early pay for about k / s
  rows (§5.3). Lane 1e: 10,000 rows, selectivity 0.1235, LIMIT 10 —
  evaluate-all 10,000 calls, stop-early 120 (81 rounded up to waves of 20),
  83×.

  </details>

- [ ] You can give Table 5's join row and compute what each cost-cutting strategy saved and lost against the nested loop.

  <details><summary>Answer</summary>

  Palimpzest (nested loop) 0.703 / $3.29; LOTUS (embedding pre-filter) 0.614 /
  $2.74 — 17% cheaper, −0.089 quality; ThalamusDB (batched prompts) 0.487 /
  $0.14 — 96% cheaper, −0.216 quality; BigQuery 0.601 / $1.02. MMQA Q7 at
  scale 200 needed 40,000 = 200² calls, 160,000 at 400.

  </details>

- [ ] You can name the text/table inconsistency and the averaging trap in Table 5.

  <details><summary>Answer</summary>

  §5.3 says Movie ran at "a small scale of 500"; Table 3 says Movie's base
  scale is 2,000 (500 is E-Commerce). And §5.5's "average latency of 48.1s"
  for BigQuery is the Filter + Join average; the all-operator average is
  31.7 s.

  </details>

## References

**Paper** — Jiale Lao, Andreas Zimmerer, Olga Ovcharenko, Tianji Cong,
Matthew Russo, Gerardo Vitagliano, Michael Cochez, Fatma Özcan, Gautam
Gupta, Thibaud Hottelier, H. V. Jagadish, Kris Kissel, Sebastian Schelter,
Andreas Kipf, Immanuel Trummer. "SemBench: A Benchmark for Semantic Query
Processing Engines." PVLDB 19(8): 1754–1767, 2026.
doi:10.14778/3811243.3811249; [arXiv:2511.01716](https://arxiv.org/abs/2511.01716).
Artifact: [sembench.org](https://sembench.org).

| where | what this chapter takes from it |
|---|---|
| §1 | SQPE definition, Example 1.1 |
| §5.1, Tables 2–3 | sizes, base scales, gemini-2.5-flash, parallelism 20, $9,935.80, 18 days |
| §5.2 | versions; Abacus off |
| §5.3, Table 4 | >100× on LIMIT queries; 40,000 calls; >1,000× a filter; LOTUS variance; "scale of 500" |
| §5.4 | 160,000 calls, ~$50 / ~$60, ~$140; 250 / 150 / 100 GB; 48 h |
| §5.5, Table 5 | per-operator quality / latency / cost; the five lessons |
| §6 | implementation matters; fusion and caching missing everywhere |

**This topic** — [README.md](README.md) Step 5 (lanes 1d, 1e and 3),
[reading-lotus-source.md](reading-lotus-source.md) Step 3 (where LOTUS's join
sample comes from).
