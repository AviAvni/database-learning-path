# Pessimistic cardinality estimation: bounds from degree sequences

Every estimator in the previous chapter tries to be right on average and has
no way to say when it is not. A different school gives up on "right" and
asks for a number that is *guaranteed* to be at least the true answer, as
small as the available statistics can make it. This chapter builds that
bound from the ground up — degree sequences, the AGM bound, the chain bound,
the degree-sequence bound, ℓp-norms — and reads two papers that make it
practical.

**"Pessimistic Cardinality Estimation"** — Mahmoud Abo Khamis, Kyle Deeds,
Dan Olteanu, Dan Suciu (arXiv:2412.00642, December 2024), a six-page
overview, and **"LpBound: Pessimistic Cardinality Estimation using
ℓp-Norms of Degree Sequences"** — Haozhe Zhang, Christoph Mayer, Mahmoud Abo
Khamis, Dan Olteanu, Dan Suciu (arXiv:2502.05912, February 2025), the
system. Section and equation numbers below are those of the two arXiv
versions (v1 of each). The overview paper has no experiments of its own;
LpBound's are on relational benchmarks (JOB, STATS) and a cyclic
subgraph-matching workload over DBLP, not on a graph database, and nothing
here was re-measured there. What was measured on this repository's generator
is the bound's *looseness*, in lane 1 of [README.md](README.md).

## The problem in one sentence

An optimizer that wants a safe plan (or a memory limit, or a server count)
needs an estimate that can never be below the truth; how large must that
estimate be, given only per-relation statistics, and is it small enough to
be useful?

## The concepts, step by step

### Step 1 — the degree sequence and its norms

> **In:** a relation, which for a graph is the edge list of one edge type.
> **Out:** the degree sequence of a column and its ℓp-norms — the only
> statistics the rest of the chapter uses.

Take a relation `R` and two sets of its columns `U` and `V`. The **degree
sequence** `deg_R(V|U) = (d_1, d_2, …, d_n)` is built by listing the
distinct values `u_1 … u_n` of column(s) `U`, letting `d_i` be the number of
distinct `V`-parts that go with `u_i`, and sorting so `d_1 ≥ d_2 ≥ …`
(Abo Khamis et al. §5). The **rank** of a value is its position in that
sorted list. For a graph's edge type `r`, `deg_r(*|dst)` is the in-degree of
every node that has any, largest first, and `deg_r(*|src)` the out-degrees.

The paper's own Figure 1 relation `R(X, Y, Z)` has eight tuples:
`(1,a,·) (1,b,·) (1,b,·) (2,a,·) (2,b,·) (3,b,·) (3,c,·) (4,d,·)`. Grouping by
`X`: value 1 has 3 tuples, 2 has 2, 3 has 2, 4 has 1, so
`deg_R(YZ|X) = (3, 2, 2, 1)`; counting only distinct `Y` per `X` gives
`deg_R(Y|X) = (2, 2, 2, 1)` (value 1 has Y ∈ {a, b}).

The **ℓp-norm** of a sequence is `‖d‖_p = (Σ d_i^p)^(1/p)` for `p > 0`, and
`‖d‖_∞ = max d_i`. Three of them are old friends (§5): `‖d‖_1` is the total,
i.e. the relation's cardinality `|R|`; `‖d‖_∞` is the **maximum degree**; and
the sequence's length `n` is the number of distinct values. On
`(3, 2, 2, 1)`: `ℓ1 = 8 = |R|`, `ℓ2 = √(9+4+4+1) = √18 = 4.243`,
`ℓ∞ = 3`. As `p` grows the norm slides from the cardinality down to the
maximum degree — LpBound's Figure 2 shows this on DBLP's author–publication
relation, whose cardinality is 24·10⁶ and whose maximum degrees are 2,951
and 450 for the two columns.

A **functional dependency** `U → V` makes every degree 1: a key column's
sequence is `(1, 1, …, 1)`. That is why a primary-key join contributes no
blow-up to any bound below.

### Step 2 — what a classical estimator uses, and the first upper bounds

> **In:** Step 1's statistics. **Out:** the density estimator as a function
> of two of them, and the three trivial upper bounds for one join, on the
> worked two-hop of a hub graph.

The **density-based estimator** every system descends from (System R) keeps
only `|R|` and the number of distinct values `|Dom(R.X)|` of a column. For
`R(X,Y) ⋈ S(Y,Z)` it returns (Abo Khamis et al. Eq. 3)

```
Est(J) = |R| · |S| / max( |Dom(R.Y)|, |Dom(S.Y)| )
```

i.e. each tuple of `R` is assumed to find `|S| / |Dom(S.Y)|` partners — the
*average* degree. Its assumptions are **uniformity** (every value of `Y` has
the same degree), **independence** (predicates and columns are unrelated),
**containment of values** (the smaller domain lies inside the larger) and
**preservation of values**; the paper says it "underestimate[s]
significantly due to [its] strong reliance on assumptions like data
uniformity and independence".

An **upper bound** replaces the average degree with a worst case. With
`a = ‖deg_R(X|Y)‖_∞` and `b = ‖deg_S(Z|Y)‖_∞` the largest degrees on the
join column, `|J| ≤ min(|R|·b, a·|S|)` (Eq. 4), and trivially `|J| ≤ |R|·|S|`.
Worked on a graph small enough to hold in your head — the one the crate's
tests use (`est.rs`, `the_worked_example_in_the_pessimistic_guide`): four
`r0` edges `0→4, 1→4, 2→4, 3→5` into the middle nodes 4 and 5, and four `r1`
edges out of them `4→8, 4→9, 4→10, 5→11`.

```
 middle node          4    5        in-degree via r0:  a = (3, 1)
 out-degree via r1    3    1        out-degree via r1: b = (3, 1)

 true two-hop count          = 3·3 + 1·1 = 10
 density estimate (Eq. 3)    = 4 · 4 / max(2, 2)        = 8   (2 distinct middle nodes on each side)
 label-count estimate        = 4 · 4 / 4 middle nodes   = 4   (what lane 1's estimator does)
 |R|·|S|                     = 16
 |R| · max b  = 4·3 = 12      max a · |S| = 3·4 = 12      (Eq. 4)
```

Every upper bound is ≥ 10 and the two estimates are below it — and the two
estimates already differ from each other by a factor of 2 depending on
whether the divisor is the number of *distinct* join values or the size of
the node *label*. The real engines of
[reading-engine-estimators.md](reading-engine-estimators.md) divide by the
label's size.

### Step 3 — AGM and the chain bound; why products explode

> **In:** Step 2's cardinality and maximum-degree bounds. **Out:** the AGM
> bound for the triangle, the chain bound for a path, and the product that
> makes both loose on long patterns.

The **AGM bound** (Atserias, Grohe, Marx) is the first bound that uses only
the relation sizes. A **fractional edge cover** of a query is a weight
`w_i ≥ 0` for every relation such that every variable is covered with total
weight at least 1; then `|Q| ≤ Π |R_i|^{w_i}` (Eq. 7), minimised over covers.
For a **triangle** `R(X,Y) ∧ S(Y,Z) ∧ T(Z,X)` the covers `(1,1,0)`,
`(1,0,1)`, `(0,1,1)` and `(½,½,½)` give `|R||S|`, `|R||T|`, `|S||T|` and
`(|R||S||T|)^½` (Eq. 8 and the text after it), and the bound is their
minimum. It is **tight**: for any sizes some database reaches it. For an
acyclic query the best cover is integral, so the bound is a product of
sizes — for a path `J2 = R ∧ S` it is `|R|·|S|`, *worse* than the density
estimator (§6).

The **chain bound** adds maximum degrees (§7). To bound a path
`R(X,Y) ∧ S(Y,Z) ∧ T(Z,U)` fix an order in which variables are "covered":
`R` covers `X, Y` with its cardinality `|R|`, then `S` covers `Z` with its
maximum degree from `Y`, then `T` covers `U` with its maximum degree from `Z`
(Eq. 12):

```
|J3| ≤ |R| · ‖deg_S(Z|Y)‖_∞ · ‖deg_T(U|Z)‖_∞
```

Compare this with the density estimator on the same query,
`|R| · |S|/|S.Y| · |T|/|T.Z|`: it is the *same expression with maximum
degrees where the average degrees were* (§7). That one change turns an
estimate into a bound and costs a factor per hop: each hop multiplies by
(max degree / average degree), the skew.

Lane 1 measures exactly that. On the shared-popularity graph the max-degree
bound's median q-error is **4.4 at two hops, 40.8 at three, 294.4 at four**;
on the independent-popularity graph, where degrees are as skewed but no node
is a hub twice, it is **40.8, 1,741.3 and 83,367.0**. A bound is always an
overestimate, so its q-error is the factor by which it is too high — here
growing roughly with `skew^hops`.

The triangle shows the AGM version. For `r0>r1>r4` (seed 1) the sizes are
34,374, 53,343 and 28,132, and `(34,374 · 53,343 · 28,132)^½ = 7,182,143`.
The true triangle count is 54,738 on the shared-popularity graph and 234 on
the independent one: the AGM bound is **131× too high** in the first case and
**30,678× too high** in the second.

### Step 4 — the degree-sequence bound: use the whole sequence

> **In:** Step 1's sorted sequences and Step 2's worked example. **Out:**
> Eq. 22, the degree-sequence bound (DSB) for one join, shown to be exact when
> ranks align and loose when they do not.

The maximum degree is one number from a sequence; the **degree-sequence
bound** (§9) uses all of it. For `R(X,Y) ∧ S(Y,Z)` let
`deg_R(*|Y) = (a_1 ≥ a_2 ≥ …)` and `deg_S(*|Y) = (b_1 ≥ b_2 ≥ …)`. If every
value of `Y` had the same rank in both, the join would have exactly
`Σ a_i b_i` tuples; in general (Eq. 22)

```
|J2| ≤ Σ_i a_i · b_i
```

*Why*: pairing the largest with the largest is the way to make the sum of
products as large as possible (the rearrangement inequality), so whichever
way the ranks really line up, the sum cannot exceed this. It improves on
Eq. 4 because `Σ a_i b_i ≤ a_1 Σ b_i = a_1 |S|`.

On Step 2's hub graph `a = (3, 1)` and `b = (3, 1)`, so
`DSB = 3·3 + 1·1 = 10` — exactly the true count, because the middle node
that is the in-hub is also the out-hub. Flip it: let `r1` be
`4→8, 5→9, 5→10, 5→11`, so node 4 has out-degree 1 and node 5 has 3. Then
`b` is still `(3, 1)` sorted, `DSB` is still 10, but the true count is
`3·1 + 1·3 = 6`. The bound cannot know which way the ranks line up, so it
assumes the worst.

That is the whole story of lane 1's degree-sequence row, and it is the
mirror image of the independence estimator's:

| 2-hop q-error, median over 30 patterns | shared popularity | independent popularity |
|---|---|---|
| independence (average degree) | 9.6, under | 1.1 |
| degree-sequence bound | **1.0**, over | 9.7, over |
| ℓ2-norm bound | 1.3, over | 12.4, over |

Where hubs coincide the DSB is tight and the independence estimate is
ten times too low; where they do not, independence is nearly right and the
bound is ten times too high. A bound is tight on exactly the correlation an
estimate cannot see.

DSB has limits (§9): it is for **Berge-acyclic** queries (hypergraphs with
no Berge cycle — in particular no two relations share two variables; a path
or a tree qualifies),
and it is an algorithm, not an inequality. Its statistics are the full
degree sequences, as large as the data, so the paper compresses them
(§10): run-length encoding; a lossy replacement `a'` with `a ≤ a'`
element-wise, which stays an upper bound because DSB is monotone in its
inputs but inflates `‖a'‖_1`; and finally bounding the *cumulative*
distribution `A_i = Σ_{j≤i} a_j` rather than the sequence — the paper's
example is `(4, 2, 2, 1, 1, 1)` against `(4, 3.5, 3.5, 0, 0, 0)`, which
exceeds the original at rank 2 yet keeps the total at 11.

### Step 5 — ℓp-norms and the polymatroid bound

> **In:** Steps 1–4. **Out:** the ℓ2 bound for one join and for the
> triangle, worked; and what the linear program in the title does.

Storing four numbers per column instead of the whole sequence is the next
idea. By the Cauchy–Schwarz inequality `Σ a_i b_i ≤ ‖a‖_2 · ‖b‖_2`, so for
`J2` (Eq. 16)

```
|J2| ≤ ‖deg_R(X|Y)‖_2 · ‖deg_S(Z|Y)‖_2
```

On the hub graph `‖(3,1)‖_2 = √10`, so the bound is `√10 · √10 = 10` (tight
again). Lane 1's ℓ2 row above is 1.3× over on the shared graph and 12.4×
over on the independent one — never better than DSB, which it bounds from
above, but a constant number of statistics per column.

The **polymatroid bound** (§8) takes *any* set of norms `‖deg_R(V|U)‖_p` and
returns the best bound they jointly prove, which can be better than any one
inequality. It is the optimum of a **linear program**: one variable `h(U)`
for every subset `U` of the query's variables (read: the entropy of those
variables in the uniform distribution over the answer), maximise
`h(all variables)` — that is `log |Q|` — subject to (i) one constraint per
statistic, `(1/p)·h(U) + h(V|U) ≤ log ‖deg_R(V|U)‖_p` (Eq. 21), and (ii) the
Shannon inequalities (`h(∅) = 0`, monotonicity, submodularity) — the facts
that make `h` behave like an entropy. The paper derives the triangle bounds
this way: Eq. 18 is AGM; Eq. 19 is

```
|C3| ≤ ( ‖deg_R(Y|X)‖₂² · ‖deg_S(Z|Y)‖₂² · ‖deg_T(X|Z)‖₂² )^(1/3)
```

which lane 1 evaluates for `r0>r1>r4`. On the shared-popularity graph (seed
1) it is **712,488 against a truth of 54,738 — 13× over, against AGM's
131×**; on the independent graph it is 3,019× over against AGM's 30,678×.
More statistics never make a bound worse (§8), and the paper reports
accuracy improving "up to ℓ30 for JOB queries … although with diminishing
returns" (§10). The cost is the LP: `2^n` variables for `n` query variables
(§8), with a polynomial special case for statistics on single columns
(§10) — which graph patterns, whose edge relations have two columns each,
mostly are.

### Step 6 — LpBound: making the LP practical, and what it measured

> **In:** Step 5's LP. **Out:** LpBound's design choices and the numbers it
> reports, each cited to a section or table.

LpBound (Zhang et al.) takes a multi-join query with selections, group-by
and **cyclic** shapes, and solves Step 5's LP with HiGHS 1.7.2 (§6.1). Its
two practical devices:

- **Statistics.** It stores a few ℓp-norms of each column's degree sequence
  (their Figure 2 plots `p ∈ {1, …, 10, ∞}`) instead of the sequence, and — because "the
  degrees typically decrease exponentially and sequence prefixes for i > 4
  have norms close to those for the entire degree sequence" — norms of the
  first four prefixes of a large sequence as well as of the whole
  (footnote 5). Selections are handled by storing the norms *conditioned*
  on the most common values of the predicate column (up to 5,000 MCVs on
  JOB, §6.4) and on histogram buckets.
- **Query-structure optimisations** (their §5) that shrink the LP — "two
  optimizations that exploit the structure of the query" (abstract) —
  because the exponential LP is otherwise the bottleneck.

What it reports (§6): errors orders of magnitude smaller than
Postgres, DuckDB and a commercial system "DbX" on JOB and STATS; on the cyclic
subgraph-matching queries (11–28 edge relations over DBLP) Postgres and DbX
return an estimate of 1 for every query "an underestimation by 7–8 orders of
magnitude", and DuckDB's estimate "increases exponentially in the number of
edge relations, eventually leading to an overestimation by over 12 orders of
magnitude" (§6.2). Estimator speed is comparable to the others and the
statistics are 1.6× smaller than SafeBound's, 1.2–93× smaller than the
ML-based estimators' (§6.4). The headline for plans (abstract; Figures 10–11):
"When injected the estimates of LpBound, Postgres derives query plans at
least as good as those derived using the true cardinalities."

Read that last sentence with the previous chapter's result in mind: a plan
is chosen by *comparing* estimates, and a bound that is consistently high
by a similar factor on every sub-plan ranks plans correctly. LpBound's own
estimation-error figure is a ratio (estimate ÷ truth), not a q-error, so a
direct comparison to lane 1's tables is by shape only.

## How to read the papers (with the concepts in hand)

1. **Abo Khamis et al. §1–§4** — fifteen minutes; §3's six-property wish
   list (accuracy, locality, composition, combination, incremental updates,
   guarantees) is the vocabulary for comparing every estimator in this
   topic.
2. **§5–§9** — Steps 1–5 above, in the paper's order. Verify Eq. 16 on your
   own relation; the Appendix has the proofs of Eqs. 17–20.
3. **§10** — the pragmatic section: statistics selection, conditional
   statistics, `∧` becoming `min` and `∨` becoming `+`, lossy compression.
4. **LpBound §3** — the q-inequalities (an explanation of the bound as a
   simple inequality, so a surprising bound can be debugged) and, in §3.4,
   the LP.
5. **LpBound §6, Figures 5–8** — read Figure 7 (cyclic subgraph matching)
   beside lane 1's triangle row.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | `experiments/src/est.rs`, `degree_sequence` | sorted degrees of nodes that have any edge |
| 2 | `est.rs` `indep_path` vs the paper's Eq. 3 | label size vs distinct values as the divisor |
| 3 | `maxdeg_bound`, `agm_triangle` | the max-degree and AGM rows of lane 1 |
| 4 | `dsb_two_hop`; the 2-hop table above | 1.0 vs 9.7 |
| 5 | `l2_two_hop`, `l2_triangle` | ℓ2 rows |

## Questions (answer in notes.md)

1. Prove `Σ a_i b_i ≤ a_1 · Σ b_i` and say when it is an equality.
2. Compute the DSB for `a = (4, 2, 2, 1, 1, 1)` and `b = (3, 3, 1)`, and the
   maximum-degree bound. Which is smaller?
3. Lane 1: why is the max-degree bound's two-hop median q-error *lower* on
   the shared-popularity graph (4.4) than on the independent one (40.8),
   when the degree distributions are identical?
4. Which of the paper's six wish-list properties does a degree-sequence
   bound satisfy, and which does a pessimistic estimator lose by being
   one-sided?
5. A Cypher planner picks the cheaper of two expansion orders using
   upper bounds. Under what condition does the bound's looseness change the
   choice?

## Done when

Answer each before unfolding it.

- [ ] You can build a degree sequence and its ℓ1, ℓ2 and ℓ∞ norms by hand.

  <details><summary>Answer</summary>

  List each distinct value of the join column with the number of tuples it
  has, sort descending, and take `(Σ d^p)^(1/p)`. For Figure 1's
  `deg_R(YZ|X) = (3, 2, 2, 1)`: `ℓ1 = 8 = |R|`, `ℓ2 = √18 = 4.243`,
  `ℓ∞ = 3`. A key column gives `(1, …, 1)`.

  </details>

- [ ] You can show that a bound is an overestimate by exactly the factor the data's skew compounds per hop.

  <details><summary>Answer</summary>

  The chain bound `|R| · max deg_S · max deg_T` equals the density
  estimator with maximum degrees replacing average degrees (§7), so each hop
  contributes `max degree / average degree`. Lane 1's max-degree bound on the
  independent-popularity graph has median q-error 40.8, 1,741.3, 83,367.0 at
  two, three and four hops: roughly ×43 and ×48 per extra hop.

  </details>

- [ ] You can state Eq. 22, why it is an upper bound, and when it is exact.

  <details><summary>Answer</summary>

  `|J2| ≤ Σ a_i b_i` over the two descending degree sequences of the join
  column. Pairing largest with largest maximises the sum of products
  (rearrangement), so any real alignment of ranks gives at most this. It is
  exact when every join value has the same rank on both sides — on the hub
  graph `a = b = (3, 1)` and both sides give 10 — and loose when the ranks
  oppose (true 6 against the same bound of 10).

  </details>

- [ ] You can explain why the independence estimate and the degree-sequence bound are wrong in opposite situations.

  <details><summary>Answer</summary>

  Independence multiplies average in- and out-degrees, correct only when a
  node's in-degree is unrelated to its out-degree; with shared popularity the
  true count is the sum of products of correlated degrees and the estimate
  is 9.6× low (median, two hops). The bound assumes the worst alignment of
  ranks, correct when degrees are perfectly correlated; with independent
  popularity the true alignment is random, the truth is far below the
  bound and the bound is 9.7× high.

  </details>

- [ ] You can write the ℓ2 triangle bound and say what it buys over AGM.

  <details><summary>Answer</summary>

  `(‖deg_R(Y|X)‖₂² · ‖deg_S(Z|Y)‖₂² · ‖deg_T(X|Z)‖₂²)^(1/3)` (Eq. 19), from
  the polymatroid LP with the ℓ2 constraints. It uses the out-degree sums of
  squares rather than only the edge counts, so it sees skew: on lane 1's
  shared-popularity triangle it is 13× over the truth against AGM's 131×
  (712,488 and 7,182,143 against 54,738).

  </details>

## References

- Mahmoud Abo Khamis, Kyle Deeds, Dan Olteanu, Dan Suciu, **"Pessimistic
  Cardinality Estimation"**, arXiv:2412.00642 (2024) — §2 (density-based
  CE), §3 (wish list), §4 (Eq. 4), §5 (degree sequences, Figure 1), §6 (AGM,
  Eq. 7–8), §7 (chain bound, Eq. 12), §8 (polymatroid bound, Eq. 16–21), §9
  (DSB, Eq. 22), §10 (practical considerations).
- Haozhe Zhang, Christoph Mayer, Mahmoud Abo Khamis, Dan Olteanu, Dan Suciu,
  **"LpBound: Pessimistic Cardinality Estimation using ℓp-Norms of Degree
  Sequences"**, arXiv:2502.05912 (2025) — abstract, §1 (Figures 1–2), §6.1
  (benchmarks, Table 1), §6.2 (errors, Figures 5–8), §6.4 (space).
- Albert Atserias, Martin Grohe, Dániel Marx, **"Size bounds and query plans
  for relational joins"**, FOCS 2008 — the AGM bound; cited through the
  overview paper, not read.
- Previous: [reading-gcare.md](reading-gcare.md). Next:
  [reading-glogue-gopt.md](reading-glogue-gopt.md) — storing counts of
  small patterns instead of degrees.
