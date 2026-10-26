# G-CARE: how wrong are graph cardinality estimators, and why

Topic 10's JOB audit showed that on real, correlated relational data the
optimizer's row-count guesses are the weakest layer of the stack. Graph
databases inherit the same estimators — a Cypher `MATCH` is a join — and
until 2020 nobody had put the graph-specific ones side by side. This paper
did: it re-implements seven estimators on one framework, scores them with
one metric over one set of queries, and reports which of them survive
multi-hop patterns. Its headline is uncomfortable: the one that wins was not
designed for graphs.

**"G-CARE: A Framework for Performance Benchmarking of Cardinality
Estimation Techniques for Subgraph Matching"** — Yeonsu Park, Seongyun Ko,
Sourav S. Bhowmick, Kyoungmin Kim, Kijae Hong, Wook-Shin Han. SIGMOD 2020
([pdf](https://personal.ntu.edu.sg/assourav/papers/SIGMOD-20-GCARE.pdf),
DOI 10.1145/3318464.3389702). This guide is written against that 16-page
SIGMOD version; every number is cited to its section. The paper's
experiments are on RDF and small-label graphs (LUBM, YAGO, DBpedia, AIDS,
Human) on a 1.5 TB Xeon machine; nothing here was re-measured on those
datasets. What *was* measured on this repository's own generator is in
[README.md](README.md) and [notes.md](notes.md), and the guide says where it
agrees.

Lane 2 of `card_bench` is one of the paper's estimators —
characteristic sets — built on this repository's graph. The original
characteristic-sets paper (Neumann & Moerkotte, ICDE 2011) could not be
obtained as a PDF; everything this guide says about it is what G-CARE §3.2
says, which is the code the experiments ran.

## The problem in one sentence

A subgraph-matching query's plan quality depends on estimates of how many
embeddings each sub-pattern has, and every estimator makes an assumption
about how the pattern's pieces are correlated — so which assumption fails
first, on which query shape, and does the failure change the plan?

## The concepts, step by step

### Step 1 — the pattern, the count, and q-error

> **In:** a data graph and a query graph, nothing else. **Out:** the three
> objects every later step uses: an embedding, the cardinality of a
> query, and the q-error that scores an estimate of it.

A **data graph** `G = (V, E, L)` is a set of vertices and directed edges
with a label function `L` (§2). A **query graph** `Q` has the same shape. A
**graph homomorphism** is a mapping `M` of Q's vertices to G's vertices that
preserves labels and maps every query edge to a data edge with the same
label; each such mapping is an **embedding**, and the **cardinality** of Q
in G is the number of embeddings (§2). "Homomorphism" means two query
vertices may map to the same data vertex — no distinctness is demanded.
Lane 1's oracle counts exactly this, so its numbers are comparable.

The paper scores an estimate `ĉ` against the true cardinality `c` with the
**q-error**, the factor by which the estimate is off in whichever direction
it errs (§5.1):

```
q-error = max( max(1,c) / max(1,ĉ) ,  max(1,ĉ) / max(1,c) )
```

Both counts are floored at 1, so an estimate of 0 for a non-empty answer
is scored as an estimate of 1 rather than as infinitely wrong, and a q-error
of 1 is a perfect estimate. A q-error alone does not say which way the
estimate erred, so the paper's figures draw under- and over-estimation on
opposite sides of the axis; lane 1's tables do the same with an `under`
column.

Worked on three numbers from lane 1 (seed 1, shared popularity): the
pattern `r0>r1` has true count 3,009,074 and independence estimates
305,602.0. Then `c / ĉ = 3,009,074 / 305,602.0 = 9.85` and `ĉ / c = 0.10`,
so the q-error is 9.85 and the estimate is an underestimate. For the
four-edge `r0>r1>r2>r3` the true count is 6,150,191,151 against an estimate
of 17,659,856.7: q-error 348.3.

### Step 2 — one framework for sampling and summaries

> **In:** Step 1's definitions. **Out:** the five-step skeleton (Algorithm
> 1) into which every estimator in the paper is poured, and the two families
> it distinguishes.

Seven estimators look unrelated; G-CARE's contribution is to show they
share a skeleton (§3.1, Algorithm 1): (1) **prepare** a summary of `G`
offline, if the technique uses one; (2) **decompose** `Q` into sub-queries
`q_1 … q_m`; (3) for each sub-query obtain **target substructures** — a
random walk for a sampling technique, a matched entry of a summary for a
summary technique — and compute an estimate `EstCard` from each; (4)
**aggregate** a sub-query's estimates (`SUM` or `AVG`); (5) multiply the
sub-queries' cardinalities by the **selectivity** of the joins between them.

The two families the rest of the paper compares:

- **Summary-based** techniques precompute a compact description of `G` and
  never look at `G` again: characteristic sets, SumRDF and BoundSketch
  (§3.2, §3.3, §4.4).
- **Sampling-based** techniques look at `G` at estimation time and average
  over random samples: Impr, CorrelatedSampling, WanderJoin and JSUB (§3.4,
  §4.1–4.3).

The line to hold on to is step (5): *multiplying* sub-query cardinalities by
a join selectivity is an independence assumption between the sub-queries.
Whatever a technique does inside a sub-query, it pays that assumption at the
seams.

### Step 3 — characteristic sets, worked on the paper's own example

> **In:** Step 2's skeleton; the paper's Figure 2 (two characteristic sets
> of the example graph). **Out:** the star estimate `Σ count · Π
> freq/count`, computed on Figure 2's numbers, and the reason it fails on
> paths.

A **characteristic set** is the set of edge labels a vertex has: in G-CARE's
adaptation it is `(VL, EL)`, a set of vertex labels and a set of outgoing
(or incoming) edge labels (§3.2). The summary stores, for each distinct
characteristic set `cs`, `cs.count` — how many vertices have exactly that
set — and `cs.freq(el)` — how many edges labelled `el` leave those vertices
in total. Figure 2's first two rows, for the example graph of Figure 1:

```
 cs                       count   freq(a)   freq(b)   freq(c)   freq(d)
 VL {A}, EL {a, c}          1        2         —         1         —
 VL {A}, EL {a, b, d}       1        1         1         —         1
```

**DecomposeQuery** splits Q into star-shaped sub-queries (a centre vertex and
its incident edges) plus single edges between unlabelled vertices. A star
with vertex labels `VL_j` and edge labels `EL_j` is answered by every
characteristic set that contains all of them (`VL_j ⊆ VL`, `EL_j ⊆ EL`),
and each contributes (§3.2, `EstCard`)

```
cardVec[i] = cs.count · Π_{el ∈ EL_j} ( cs.freq(el) / cs.count )
```

then `AggCard` sums. Read the formula: `freq(el) / count` is the *average
number of el-edges per vertex of that set*, so the product is "the number
of vertices, times the average of each edge, multiplied". That is exact for
a star only if, inside a set, the degrees of different edge labels are
unrelated — an independence assumption *within* a set, but a much weaker one
than a graph-wide average because the set conditions on which labels exist.

Worked on Figure 2 for the star `VL = {A}, EL = {a}` (Figure 3's left
sub-query): both rows contain A and `a`. Row 1 contributes
`1 · (2/1) = 2`; row 2 contributes `1 · (1/1) = 1`; `SUM` gives **3**. For a
star with `EL = {a, c}` only row 1 matches: `1 · (2/1) · (1/1) = 2`.

What the formula cannot do is look *across* a vertex's neighbours. A path
`a→b→c` is two stars sharing b, and the paper combines sub-queries by the
product of pairwise edge selectivities (§3.2): independence again. The
paper's summary of the consequence (§6.6): "CharacteristicSets … suffers
from significant underestimation due to the independence assumption", while
its original paper "demonstrated [high accuracy] using only star-shaped
queries".

### Step 4 — WanderJoin: a sampler that stays unbiased

> **In:** Step 2's skeleton (a sampling instance) and the idea of a random
> walk. **Out:** WanderJoin's estimate `1/P(s)` and the number that decides
> whether it works — the fraction of walks that complete.

**WanderJoin** (§4.2) was built for online aggregation in relational
engines; G-CARE adapts it by adding a sampling ratio and a `COUNT`. It
treats each relation as a vertex and each join condition as an edge, picks
a walk order over a spanning tree, and samples one tuple per relation in
that order: the first uniformly from `R_1`, each later one uniformly from
the tuples that join with the one chosen before it. If the walk dead-ends
(no joining tuple) or fails a non-tree join condition, the sample is
**invalid** and contributes 0. A valid sample contributes `1 / P(s)` where
(§4.2, `EstCard`)

```
P(s) = 1/|R_1| · Π_{i=2..n} 1 / |t_p(i) ⋈ R_i|
```

`|t_p(i) ⋈ R_i|` is the number of tuples joining with the previously
chosen one, so `1 / P(s)` is the **Horvitz–Thompson** estimate (inverse
probability weighting) and its average over samples is *unbiased*: the
expected value of the estimate is the true count, with no independence or
uniformity assumption about the data at all.

Worked on a path in this repository's graph: take `r0>r1`. A sample
chooses an r0-edge `(a,b)` uniformly — `P = 1/|E_0|` — then an r1-edge from
`b` uniformly among `b`'s `d` out-edges. The sample's estimate is
`|E_0| · d`. If `b` is a hub with `d = 2,000`, the estimate is
`34,374 · 2,000 = 68.7 M` and it is rare; if `b` has `d = 1`, the estimate
is 34,374 and it is common. The average over many walks converges to
3,009,074, but the variance is the hubs' — which is why the paper reports
"successful sampling rates" and why unbiasedness does not mean small error
on a few thousand walks.

The paper's own diagnosis of when this fails is in its numbers: for
queries of more than ten edges the fraction of walks that complete is 27.7%
for star and 16.8% for cycle queries but **0.16% for chains, 8.6% for trees
and 0.04% for petals** (§6.1.3), and WanderJoin's errors are largest on
exactly the chain, tree, petal and flower shapes "due to the sampling
failure". A walk that starts on a random edge and must hit a specific label
at the far end is rare when labels are many — **sampling failure** — and an
estimate built from zero valid samples is 0, an underestimate by the whole
answer.

### Step 5 — what the benchmark found

> **In:** Steps 1–4 and §5's setup (Table 1: five datasets, eight query
> topologies, query sizes 3, 6, 9, 12 edges, six result-size buckets).
> **Out:** the paper's five findings, each tied to the step that explains it.

1. **WanderJoin wins** on accuracy across datasets, topologies, result
   sizes and query sizes (§6.1.1–6.1.4, §6.2, abstract). The sampler that
   does not summarise beats the summaries.
2. **Independence-based summaries underestimate**, and the underestimate
   grows with the result size and the query size. Of characteristic sets:
   "tends to underestimate as the result size increases due to the
   independence assumption" (§6.2.1); "underestimate for larger queries"
   (§6.2.3). In §6.1.2 "the estimated cardinality values are close to 0,
   which leads to larger q-error values for queries with larger result
   sizes."
3. **Sampling failure underestimates in a different way**: Impr "finds no
   single random walk satisfying the join conditions" for LUBM Q7, Q8, Q12
   (§6.1.1); Impr cannot process queries above five edges on AIDS (§6.2.3).
4. **Upper bounds overestimate and grow with size**: BoundSketch "consistently
   overestimates" (§6.1.1) and "shows clear trends of increasing errors for
   larger queries" because "the upper bound computation involves the product
   of counts or degrees" (§6.1.4). Step 3 of
   [reading-pessimistic-bounds.md](reading-pessimistic-bounds.md) works
   that product.
5. **Estimates can fail to change the plan.** §6.5 plugs each estimator into
   RDF-3X and runs LUBM and DBpedia queries. For the star-shaped LUBM Q4
   "the optimizer in RDF-3X generates a robust query plan" because "the
   validity ranges for the optimal plan … are wide"; P1 and P2 on DBpedia are
   stars too and "RDF-3X generates a robust query plan regardless of
   cardinalities". The **validity range** (the paper takes the concept from Markl et
   al.'s progressive optimization, SIGMOD 2004) of a join child is the range of
   row counts over which the chosen plan stays optimal for the cost model:
   a wide range tolerates any error inside it. The same section reports
   the opposite case — characteristic sets with *good* q-error but "very
   narrow" validity ranges on DBpedia P6, giving "a significantly worse
   plan".

Finding 5 is the bridge to this topic's measurement: lane 1's plan-choice
table has estimates wrong by 348× that nevertheless pick a plan within 0.5%
of the best, for a reason the validity-range idea names — the plan's
alternatives were never close.

## How to read the paper (with the concepts in hand)

1. **§1 and §2** — ten minutes. The introduction's four open questions are
   the paper's table of contents.
2. **§3.1 Algorithm 1, then §3.2 and §4.2** — Steps 2–4 of this guide.
   Skim §3.3 (SumRDF) and §3.4 (Impr) for the shape of the other two.
3. **§5 and Table 1** — the parameter grid; note that query *size* tops out
   at 12 edges and the RDF datasets have many edge labels, which is the
   opposite of the few-label graphs of lane 1.
4. **Figures 6–9** — read the y-axis convention first (under on one side,
   over on the other), then find characteristic sets and WanderJoin on each.
5. **§6.5 and Figure 11** — the plan-quality experiment; read the Q4 and Q8
   paragraphs for the cases where bad estimates do not hurt.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | `experiments/src/est.rs`, `q_error` | the same formula, floors included |
| 3 | lane 2, `experiments/src/cset.rs` (stub) | the star formula on this crate's graph; reference rows in [notes.md](notes.md) |
| 4 | exercise 5 in the [README](README.md) | a WanderJoin lane on the same graph |
| 5 | lane 1 plan-choice table | where a 348× error costs nothing |

## Questions (answer in notes.md)

1. Compute the characteristic-set estimate for the star `VL = {A}, EL =
   {a, b}` from Figure 2's two rows, by hand.
2. The paper reports the successful-sample rate for chains as 0.16% and for
   stars as 27.7% (queries above ten edges). Using Step 4's walk, say why a chain is harder to sample
   than a star on a graph with many labels.
3. Lane 2's characteristic-set estimate on this repository's graph
   improves 9.6× to 8.7× at two hops. G-CARE says characteristic sets
   underestimate "due to the independence assumption". Which independence
   assumption is left after conditioning on the set?
4. A planner chooses between two orders whose true costs are 100 and 120
   rows. What q-error can the cost estimate of the cheaper one tolerate
   before the choice flips, assuming the other is estimated exactly?

## Done when

Answer each before unfolding it.

- [ ] You can compute a q-error, including the floor, and say what it does not tell you.

  <details><summary>Answer</summary>

  `max(max(1,c)/max(1,ĉ), max(1,ĉ)/max(1,c))`: with `c = 3,009,074` and
  `ĉ = 305,602.0` it is 9.85, and the floor at 1 means an estimate of 0 for
  a non-empty result is scored as 1, so q-error is finite. It does not say
  the direction of the error — the paper plots under- and over-estimation
  on opposite axis sides for that reason (§5.1) — and it says nothing about
  whether the error changes the chosen plan (§6.5).

  </details>

- [ ] You can state what Algorithm 1 factors out of the seven estimators and where independence enters.

  <details><summary>Answer</summary>

  Prepare a summary, decompose Q, get target substructures per sub-query,
  aggregate each sub-query's estimates, then multiply sub-query
  cardinalities by the selectivity between them (§3.1). Independence enters
  at the last step for every technique, and again inside a technique where
  it assumes edges at one vertex are unrelated (characteristic sets within a
  set) or that two sub-patterns occur independently.

  </details>

- [ ] You can compute a characteristic-set star estimate and explain why it fails on paths.

  <details><summary>Answer</summary>

  Sum over every set containing the star's labels of `count · Π freq/count`.
  Figure 2's two sets give `1·(2/1) + 1·(1/1) = 3` for the star `{A},{a}`.
  A path is two stars that share a vertex and the paper multiplies them by
  pairwise edge selectivities (§3.2) — an independence assumption between
  the stars, which is false whenever a vertex's in-degree and out-degree are
  correlated, as lane 1's shared-popularity graph makes them.

  </details>

- [ ] You can explain why WanderJoin is unbiased and why that still leaves a failure mode.

  <details><summary>Answer</summary>

  Each valid walk contributes `1/P(s)`, the inverse of the probability of
  drawing it (Horvitz–Thompson), so the expected estimate is the true count
  with no assumption about the data. The failure mode is walks that do not
  complete: with many labels a random walk rarely reaches a specific
  far-end label (0.16% of chain-query samples were valid for queries above ten edges,
  §6.1.3), the estimate from zero valid samples is 0, and the few samples
  that do complete are dominated by hubs, so the variance is large even
  though the mean is right.

  </details>

- [ ] You can name the §6.5 result that complicates "lower q-error means better plans".

  <details><summary>Answer</summary>

  For star-shaped queries (LUBM Q4, DBpedia P1 and P2) RDF-3X's plan has
  wide validity ranges and is the same whatever the estimate, while on P6
  characteristic sets with good q-error produce a very different, much worse
  plan because the optimal plan's validity range is narrow. Whether an error
  matters depends on how close the competing plans are, not only on how big
  the error is.

  </details>

## References

- Yeonsu Park, Seongyun Ko, Sourav S. Bhowmick, Kyoungmin Kim, Kijae Hong,
  Wook-Shin Han, **"G-CARE: A Framework for Performance Benchmarking of
  Cardinality Estimation Techniques for Subgraph Matching"**, SIGMOD 2020 —
  §2 (definitions), §3.1 (Algorithm 1), §3.2 (characteristic sets), §4.2
  (WanderJoin), §4.4 (BoundSketch), §5.1 (q-error), §6.1–6.4 (accuracy),
  §6.5 (plan quality), §6.6 (summary). Code: <https://github.com/yspark-dblab/gcare>.
- Thomas Neumann and Guido Moerkotte, **"Characteristic Sets: Accurate
  Cardinality Estimation for RDF Queries with Multiple Joins"**, ICDE 2011,
  pp. 984–994 — not obtained as a PDF; cited only through G-CARE §3.2.
- Volker Markl, Vijayshankar Raman, David Simmen et al., **"Robust query
  processing through progressive optimization"**, SIGMOD 2004 — the source
  of the validity-range concept (cited as [27] in G-CARE §6.5); not read.
- Viktor Leis et al., **"How Good Are Query Optimizers, Really?"**, VLDB
  2015 — the relational audit this paper continues; see
  [topic 10's guide](../10-query-planning/reading-how-good-optimizers.md).
- Next: [reading-pessimistic-bounds.md](reading-pessimistic-bounds.md) — what
  to do when you would rather be too high than too low.
