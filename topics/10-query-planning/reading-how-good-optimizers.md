# Cardinality is the whole ballgame: the JOB audit

The humbling paper. Leis et al. (VLDB '15) built the Join Order Benchmark
(JOB) — 113 queries over IMDB, REAL correlated data instead of TPC-H's
synthetic uniformity — and audited every layer of the classical optimizer
stack. The verdict reorders this whole topic: cardinality error dwarfs
cost-model error dwarfs search-space limits. Before the paper, this
chapter builds the layers being audited and the estimator being indicted,
step by step — then hands you the reading route.

## The problem in one sentence

Every cost-based optimizer ranks plans using guessed row counts, and on
real correlated data those guesses are off by 10⁴–10⁶ after a handful of
joins — so the question the paper asks is: of the optimizer's three parts
(estimates, cost model, search), which one is actually responsible for
bad plans?

## The concepts, step by step

### Step 1 — the three claims an optimizer makes

A classical cost-based optimizer is three separable components, each
making its own claim:

1. **cardinality estimation** — a guess at how many rows each subplan
   produces ("this filter keeps ~500 of 6M rows");
2. a **cost model** — a formula turning cardinalities into a comparable
   cost number ("a join producing 500 rows from these inputs costs X");
3. **plan search** — an algorithm (DP, greedy, genetic) that explores
   the space of join orders and tree shapes and keeps the cheapest.

```
   estimates ──► cost model ──► search ──► chosen plan
   (guessed        (formula        (which of 10^18
    row counts)     over guesses)   candidates wins)
```

Each layer consumes the previous one's output, so a failure anywhere
poisons everything downstream — and until this paper, nobody had measured
*which* layer fails in practice. That is the entire contribution.

### Step 2 — how every system estimates joins: three assumptions

The estimator all audited systems run (postgres and three commercial
engines) rests on the same three assumptions. **Uniformity**: every value
of a column is equally frequent, so an equality predicate keeps 1/NDV of
the rows (NDV = number of distinct values in the column).
**Independence**: predicates don't correlate, so combined selectivity
(the fraction of rows a predicate keeps) is the product of individual
selectivities. **Containment**: in a join, every value of the smaller
side's key set appears in the larger side's. In code:

```rust
// the estimator every audited system runs, and why it under-shoots
fn estimate_join_card(tables: &[Table], preds: &[EquiPred]) -> f64 {
    let mut card: f64 = tables.iter().map(|t| t.rows as f64).product();
    for p in preds {
        card /= p.ndv_left.max(p.ndv_right) as f64;  // uniformity: 1/NDV
    }   // each predicate applied INDEPENDENTLY — on correlated data the
    card // true overlap is larger, so factors compound toward zero
}
```

Cheap to compute (a few stats per column), and exactly right on uniform,
independent data. Real data is neither.

### Step 3 — errors are multiplicative, so they grow exponentially with joins

The standard error metric is **q-error**: max(estimate/truth,
truth/estimate) — a q-error of 100 means off by 100× in *some* direction.
Because each join's estimate is built by multiplying the previous
estimate by another guessed factor, per-predicate errors *compound*: a
2× error per predicate is 2⁶ = 64× after six joins if you're lucky, far
worse when correlations align. The paper measures exactly this: median
q-error degrades **exponentially with join count, reaching 10²–10⁴ at 6
joins** across all tested systems — and **underestimation dominates**,
because independence multiplies selectivities toward zero while
correlated predicates actually overlap (city = 'Paris' AND country =
'France' is not sel × sel; it's ~sel).

Why underestimation is the dangerous direction: an optimizer told "only
3 rows will come out of this" happily picks a nested-loop join — which
then runs 10⁴× more iterations than promised.

### Step 4 — the benchmark: real data is part of the method

TPC-H, the standard benchmark, is *generated* data: uniform value
distributions, independent columns. On it, Step 2's assumptions are
true by construction and estimates look fine — the standard benchmark
was structurally incapable of detecting the standard failure. So the
authors built **JOB**: 113 queries (3–16 joins each) over the real IMDB
dataset, where actors correlate with genres correlate with production
years. **Benchmark data distribution is part of the benchmark** — the
lesson to carry to every benchmark you ever build.

### Step 5 — the method: inject ground truth, isolate the blame

The experimental design is worth copying forever. Factor the optimizer
into its three claims (Step 1) and test each *in isolation* by feeding
the layers below it perfect inputs:

1. cardinality estimates — compare against TRUE cardinalities (computed
   offline) for every subplan;
2. cost model — feed it TRUE cardinalities, see if better cost = faster;
3. plan space — with perfect estimates, how much do bushy trees /
   exhaustive search matter?

Injecting ground truth at each layer isolates the blame: if plans become
good the moment true cardinalities are injected, the estimator was the
problem, not the cost model or the search. (This is the fair-benchmarking
discipline of topic 0, applied to a brain.)

### Step 6 — the verdict: cardinality ≫ cost model ≫ search space

- **Cardinality is the whole ballgame** — Step 3's 10²–10⁴ q-errors
  translate directly into catastrophic plan choices.
- **The cost model barely matters**: with true cardinalities, even a
  trivial cost model (they use Cout — the sum of intermediate result
  cardinalities, nothing else) picks plans within ~2× of optimal.
  Cost-model tuning is polishing the wrong layer. (DuckDB's
  `cost_model.cpp:40` being literally Cout is this finding, shipped.)
- **Plan space matters at the margins**: exhaustive beats
  greedy/quickpick meaningfully; bushy trees beat left-deep-only by
  ~10–40% on some queries. But all of it is noise next to cardinality
  error.

```
 error source        typical impact on runtime
 cardinality (6-way) 10×–1000× (catastrophic plans)
 cost model          ~2×
 search space        ~1.1×–1.4×
```

### Step 7 — living with wrong estimates: robust plans

Since estimates can't be fixed cheaply, the paper's pragmatic mitigation
is to prefer plans **robust to misestimation**: a hash join costs roughly
linearly in input size, so a 10⁴× underestimate makes it 10⁴× slower —
bad; a nested-loop join costs the *product* of its inputs, so the same
underestimate is quadratically catastrophic. When unsure, take the
algorithm whose worst case degrades gracefully. Postgres's notorious
nested-loop disasters are exactly underestimates of 10⁴ feeding "it's
only 3 rows" decisions.

## How to read the paper (with the concepts in hand)

~1.5 h. The methodology (§2–3) is worth as much as the findings —
injecting ground truth per layer isolates the blame.

- **§2 (the benchmark)** — Step 4. Note *why* each JOB query family
  exists: correlations are chosen deliberately, not sampled at random.
- **§3 (cardinality estimation) — read carefully.** Steps 2–3 measured:
  the q-error-vs-join-count figures are the paper's core result. Check
  that underestimation dominates and that all systems (including the
  commercial ones) degrade the same way.
- **§4 (cost model)** — Step 6's second bullet: watch Cout with true
  cardinalities land within ~2× of the full-blown model.
- **§5 (plan space)** — Step 6's third bullet: bushy vs left-deep,
  exhaustive vs heuristic, all quantified with truth injected.
- **§6 (discussion)** — Step 7's robustness argument, plus the pointers
  to sampling-based estimation. The learned-cardinality papers in the
  topic README (Kipf '19, Neo, Bao) are this section's direct
  descendants — read them after, not instead.

## Questions for notes.md

1. Why does independence UNDERestimate join sizes on correlated data?
   Construct a 2-table example where sel(a)×sel(b) is 100× low.
2. Cout (sum of intermediate sizes) as the whole cost model: which of
   your engines' knobs does that validate (DuckDB cost_model.cpp:40 is
   literally this)?
3. "Robust plans": hash join degrades linearly with a bad estimate,
   nested-loop quadratically. Frame it as a minimax decision — what's
   the regret matrix?
4. Design JOB-for-graphs: what's the correlated-data equivalent for
   Cypher patterns (degree skew × label correlation × triangle
   density)? Sketch 3 queries where independence-based nnz estimation
   (matrix-product size) blows up the same way. This is the M10/M22
   benchmark seed — write it down properly.

## Done when

You can rank cardinality/cost/search by measured impact, explain WHY
independence fails low, and have the graph-JOB sketch in notes.md.

## References

**Papers**
- Leis, Gubichev, Mirchev, Boncz, Kemper, Neumann — "How Good Are Query
  Optimizers, Really?" (VLDB 2015) — ~1.5 h; the methodology (§2–3) is
  worth as much as the findings — injecting ground truth per layer
  isolates the blame
