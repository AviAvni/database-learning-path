# GLogue in GOpt: store the counts of small patterns, chain them with Markov

The estimators of the first chapter assume that each edge of a pattern is
independent of the others. The pessimistic bounds of the second assume the
worst. A third answer is to stop assuming for the part you can afford to
count: **precompute the exact number of occurrences of every small pattern**
— every single edge, every two-edge path, every triangle — and use those
counts to estimate the big ones, assuming independence only *between* the
small pieces. Graph optimizers call that catalog a **GLogue**. This chapter
reads the way a recent production-oriented system uses it, and builds the
arithmetic on this repository's graph.

**"GOpt: A Graph-Native Query Optimization Framework"** — Bingqing Lyu,
Xiaoli Zhou, Longbin Lai, Yufan Yang, Yunkai Lou, Wenyuan Yu, Ying Zhang,
Jingren Zhou (arXiv:2401.17786). §6.3.1 is the cardinality section; §8.2 has
its ablation. GOpt (an Alibaba GraphScope component, run on GraphScope and
on Neo4j in the paper) inherits GLogue from the earlier **GLogS** paper,
which this guide did not read; GOpt's §6.3.1 states what it takes from it
("graph sparsification, pattern encoding, and efficient maintenance and
lookup in GLogue") without explaining it, and this guide teaches only what
GOpt's text and this crate's lane 3 show. Numbers from the paper are
cited to its sections; the lane's numbers are this repository's.

## The problem in one sentence

How many of a pattern's correlations can you capture by *storing* the
counts of its sub-patterns, how does the estimate combine them, and what
does it cost to store?

## The concepts, step by step

### Step 1 — what a GLogue stores

> **In:** a data graph. **Out:** the catalog, `F(P)` for every pattern `P`
> with at most `k` vertices, and the two special cases this chapter uses.

The **frequency** `F(P)` of a pattern `P` is its number of matches in the
data graph — the cardinality of Chapter 1's Step 1 with homomorphism
semantics (GOpt §6.3.2 adopts "homomorphism-based matching semantics").
GOpt's GLogue "precomputed the frequencies of patterns as higher-order
statistics" for "small graph patterns (a.k.a., motifs) with up to k vertices
(k ≥ 3) in the data graph, going beyond the low-order statistics which only
count the vertices and edges" (§4, System Overview). The **order** of a statistic is the
size of the pattern it counts: *low-order* = single vertices and edges (what
every engine in [reading-engine-estimators.md](reading-engine-estimators.md)
keeps), *high-order* = anything bigger.

In this crate's schema (five edge types over four node labels) the catalog
for `k = 3` vertices holds:

- `F1[t]` — one entry per edge type: the number of `t` edges. 5 entries.
- `F2[a][b]` — one entry per pair of *label-consistent* types: the exact
  number of `a`-then-`b` two-hop paths. Types connect when the first's
  destination label equals the second's source label; in this schema that
  is 6 ordered pairs (`r0r1, r1r2, r1r4, r2r3, r3r0, r4r0`).
- (triangles `r0r1r4` and its rotations would be a third kind of 3-vertex
  entry; lane 3 does not store them, so a closing edge is an exercise.)

Lane 3's reference build stores **11 patterns** (5 edges + 6 paths). The
count grows like the number of connected label-typed patterns: with `T`
types it is `O(T)` edges, up to `O(T²)` paths and up to `O(T³)` three-edge
patterns, which is why `k` is small; GOpt reuses GLogS's "graph
sparsification" to keep building the larger entries affordable (a technique
this guide did not read).

### Step 2 — Equation 1: glue two sub-patterns by their overlap

> **In:** Step 1's `F` values for two sub-patterns whose union is the
> target. **Out:** Eq. 1, worked on a three-edge path, and the assumption it
> hides.

To estimate a pattern `P_t` that is not in the catalog, take two
sub-patterns `P_s1`, `P_s2` that *are* (or can be estimated) with
`E(P_t) = E(P_s1) ∪ E(P_s2)`. "By assuming the independent presence of
patterns in the data graph" GOpt computes (§6.3.1, Eq. 1)

```
F(P_t) = F(P_s1) · F(P_s2) / F(P_s1 ∩ P_s2)
```

where `P_s1 ∩ P_s2` is the part they share. Read it as a conditional
probability: the matches of `P_s1`, times the number of ways to extend a
match of the shared part into `P_s2` *on average*, `F(P_s2) / F(shared)`.
The assumption it hides is exactly that the extension does not depend on
anything in `P_s1` outside the shared part.

Take a three-edge path `a –t0→ b –t1→ c –t2→ d`. Let
`P_s1 = (a –t0→ b –t1→ c)` and `P_s2 = (b –t1→ c –t2→ d)`; they share the
middle edge `b –t1→ c`. Then

```
F(t0 t1 t2) = F2[t0][t1] · F2[t1][t2] / F1[t1]
```

Worked on the hub graph of the pessimistic chapter, extended by an `r2`
edge out of each `r1` destination: `r0`: `0→4, 1→4, 2→4, 3→5`; `r1`:
`4→8, 4→9, 4→10, 5→11`; `r2`: `8→12, 9→12, 10→13, 11→13`. The catalog holds
`F2[0][1] = 10` (the pessimistic chapter's two-hop count),
`F2[1][2] = 4` (each of the four `r1` edges continues along exactly one `r2`
edge) and `F1[1] = 4`. Then `F(r0 r1 r2) = 10 · 4 / 4 = 10`. The true
three-hop count is 10 — paths `(0|1|2) → 4 → (8|9|10) → …` each continue
once and `(3, 5, 11, 13)` once — so the estimate is exact, as the assumption
(each `r1` edge has the same number of `r2` continuations whatever came
before it) holds in this graph by construction. Lane 1's independence
estimate for the same path is `4 · (4/4) · (4/4) = 4`: 2.5× low.

The pattern generalises to a path of `k` edges: apply Eq. 1 repeatedly
until only catalog entries are left,

```
F(t0 … t_{k-1}) = Π_{i=0}^{k-2} F2[t_i][t_{i+1}]  /  Π_{i=1}^{k-2} F1[t_i]
```

This is the **order-2 Markov** estimate: each edge is allowed to depend on
the previous edge (through the stored two-edge count) and on nothing
earlier. For `k = 1` and `k = 2` it is the catalog entry itself, so those
patterns are **exact by construction**; the first estimate is at `k = 3`.

### Step 3 — Equation 2: the expand ratio, and why it contains lane 1

> **In:** Eq. 1's simple case. **Out:** the expand ratio σ, worked on GOpt's
> own Example 6.2, and its equality with the independence estimator.

When the new pattern differs from a known one by a single new vertex `v`
with edges `e_1 … e_n` appended one at a time, GOpt defines an **expand
ratio** `σ_{e_i}` — the factor by which the pattern's frequency changes
when edge `e_i = (v_i, v)` is added (§6.3.1, Eq. 2) — and
`F(P_t) = F(P_s) · Π σ_{e_i}`. Two cases:

```
σ_e = F(e) / F(v_i)                    if v is a new vertex        (expand)
σ_e = F(e) / ( F(v_i) · F(v) )         if v is already in the pattern (close)
```

with `F` the sum of frequencies over the vertex or edge *types* the
constraint allows. The first is "the average number of `e`-edges leaving a
`v_i`": the **average degree**. The second is "the probability that the
edge exists between a random `v_i` and a random `v`": the **closing
probability**. These are precisely lane 1's estimator for a path
(`|E_t| / |L_src|` per hop) and for the closing edge of a triangle
(`|E_{t2}| / (|L_c| · |L_a|)`), and Neo4j's and Kùzu's, as the next chapter
shows. GOpt's low-order mode is the independence estimator; its high-order
mode seeds the expansion from a *stored* pattern instead of a single edge.

Example 6.2 (Figure 6), worked. The catalog has `Person: 10`, `Product: 20`,
`Place: 5` (vertex frequencies), and the source pattern `P_s` has `F = 70`.
Expanding `e_2`, an edge of union type `LocatedIn | ProducedIn` into a new
vertex, gives `σ_{e_2} = (F_LocatedIn + F_ProducedIn) / (F_Person +
F_Product) = 1.0`; expanding `e_3`, a `LocatedIn` edge that closes onto an
existing vertex, gives `σ_{e_3} = F_LocatedIn / (F_Person · F_Place) = 0.2`.
So `F(P_t) = 70 · 1.0 · 0.2 = 14` (the paper's value). Check the second
ratio by hand: with `F_Person = 10` and `F_Place = 5` the divisor is 50, so
`0.2 · 50 = 10` — that is the figure's implied `F_LocatedIn`, which the
excerpt does not print.

### Step 4 — the measured gain on this graph

> **In:** Steps 1–2 and lane 3's reference build on lane 1's graphs. **Out:**
> the by-hop q-errors of the catalog beside lane 1's, and what remains.

Lane 3 builds the 11-entry catalog on each of five seeds of the
shared-popularity graph and estimates every path of two, three and four
edges (30, 35 and 45 patterns). The reference run gives, next to lane 1's
independence estimator on the same graphs:

| hops | independence, median q (under) | two-edge catalog, median q (under) |
|---|---|---|
| 2 | 9.6 (100%) | **1.0** (0%) |
| 3 | 57.5 (100%) | **2.0** (0%) |
| 4 | 324.0 (100%) | **3.2** (0%) |

Two readings. First, the catalog turns the geometric growth of lane 1
(roughly ×6 per hop) into a slow one (×1.6 per hop) by removing one
correlation per hop — the one between consecutive edges — and what it
leaves is the correlation between an edge and the one *two* steps back.
Second, the catalog's error flips sign: it never underestimates on this
graph (0% under), the opposite of every independence-based estimate in the
topic. The sign has an exact cause: the truth is
`Σ_{(b,c) ∈ E_t1} in(b) · out(c)` over the middle edges and the order-2
estimate is `(Σ in(b)) · (Σ out(c)) / |E_t1|` with the same sums, so the
estimate is at least the truth exactly when `in(b)` and `out(c)` are
negatively or un-correlated across the middle edges (Chebyshev's sum
inequality). The reference run
measured that covariance on seed 1: it is negative for all seven
three-edge patterns, and the sign of (estimate − truth) matches
([notes.md](notes.md)). *Why* it is negative — de-duplication collapsing
repeated hub–hub edges is the suspect — was not isolated; exercise 6 in the
[README](README.md) is the measurement.

### Step 5 — what GOpt does with it, and what it measured

> **In:** Steps 1–3 as one component of an optimizer. **Out:** the paper's
> own evidence that high-order statistics change plans, and its
> limitations.

GOpt feeds `GLogueQuery.getFreq(P)` into a cost-based pattern optimizer
(§6.3.3) that searches over hybrid plans combining binary joins and
worst-case-optimal expand-intersect, with backend-registered cost models
(§6.3.2). Its cardinality component adds two things to the plain catalog:
**type inference** (§6.2) to turn a pattern with unconstrained or union
types into basic types GLogue can answer, and the alternating application
of Eqs. 1 and 2 when the pattern is not in the catalog ("we alternately and
iteratively apply Eq. 1 and Eq. 2 … until the subpattern either exists in
GLogue … or simplifies to a single vertex or edge").

What it measured (§8.2, "Cost-based Optimization"): on four graph patterns
— a triangle, a square, a 5-path and a 7-vertex, 8-edge pattern — each with
a basic-type and a union-type variant (eight queries), plans built with
the high-order statistics beat plans built with low-order statistics only on
**3 of the 8 queries, by an average of 13.1× across those three; on the
complex pattern QC4a, 31.9×**. On the other five queries the plans did not
differ enough to count. Two limits the paper states: filters on a pattern
use predefined selectivities ("e.g., 0.1", §7, following GLogS) rather than
histograms — "future work" — and the catalog is only as accurate as its
independence assumption between the glued pieces. Read the 3-of-8 as the
honest middle: high-order statistics did not matter often, and mattered a
lot when they did, which is the same shape as the plan-choice rows of this
topic's lane 1.

## How to read the paper (with the concepts in hand)

1. **§1–§4** — what GOpt is; §4 (System Overview) introduces GLogue in one paragraph.
2. **§6.3.1** — the whole of this guide's Steps 2–3; Figure 6 is Example 6.2.
3. **§6.3.3** — the cost-based search that consumes the estimates.
4. **§8.2, Figure 8(d)** — the high-order vs low-order ablation.
5. Skip §5–§6.2 on first reading (query parsing, type inference), returning
   when a union type in Step 3's formula is unclear.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1–2 | lane 3, `experiments/src/catalog.rs` (stub) | `build` and `estimate_path`; tests include the hub graph above |
| 3 | `est.rs` `indep_path`, `indep_triangle` | σ_expand and σ_close for lane 1 |
| 4 | reference rows in [notes.md](notes.md) | the by-hop table |

## Questions (answer in notes.md)

1. Derive the four-edge case of the Markov formula by applying Eq. 1 twice,
   naming the shared part each time.
2. On the hub graph, change `r2` to `8→12, 8→13, 9→12, 10→12` (node 11 now
   has no `r2` edge). What are the true count and the catalog estimate of
   `r0 r1 r2` now? Which sign is the error, and which dependence caused it?
3. Lane 3 stores the exact two-hop counts but only the independence
   assumption between them. What would a *three-edge* catalog cost on this
   schema (count the label-consistent walks of three edges), and what would
   it fix at four hops?
4. GOpt's Eq. 2 divides by `F(v_i) · F(v)` for a closing edge. What does
   lane 1's triangle row (independence 229× low on the shared graph) say
   about when that ratio is wrong, and what catalog entry would repair it?

## Done when

Answer each before unfolding it.

- [ ] You can state Eq. 1, its assumption, and apply it to a three-edge path.

  <details><summary>Answer</summary>

  `F(P_t) = F(P_s1) · F(P_s2) / F(P_s1 ∩ P_s2)` for sub-patterns whose
  union is `P_t`, assuming the two occur independently given the shared
  part. For the path `t0 t1 t2` with `P_s1 = t0 t1`, `P_s2 = t1 t2`, shared
  edge `t1`: `F2[t0][t1] · F2[t1][t2] / F1[t1]`. On the hub graph
  `10 · 4 / 4 = 10`, equal to the true count.

  </details>

- [ ] You can write the expand and close ratios and say how they relate to the independence estimator.

  <details><summary>Answer</summary>

  Expand to a new vertex: `F(e) / F(v_i)`, the average out-degree; close onto
  an existing vertex: `F(e) / (F(v_i) · F(v))`, the closing probability
  (Eq. 2). A path built from expand ratios is `|E_t0| · Π |E_ti| / |L_src|`
  and a triangle closes with `|E_t2| / (|L_c| · |L_a|)` — lane 1's
  independence estimator exactly. High-order statistics replace the starting
  pattern, not the ratio.

  </details>

- [ ] You can say why catalog patterns of one and two edges are estimated exactly and where the first error appears.

  <details><summary>Answer</summary>

  They are the entries: `estimate_path` returns `F1` or `F2` unchanged for
  one and two edges. The first estimated pattern has three edges, where the
  rule assumes the third edge's extension does not depend on the first edge
  given the second. Lane 3's median q-error is 1.0 at two hops and 2.0 and
  3.2 at three and four, against independence's 9.6, 57.5 and 324.0.

  </details>

- [ ] You can report what GOpt's ablation showed, with its denominator.

  <details><summary>Answer</summary>

  On eight cost-based-optimization queries (§8.2), plans using high-order
  statistics were better on 3, by an average of 13.1× across those three,
  and 31.9× for the complex pattern QC4a. On the other five the plan was not
  materially better. High-order statistics changed the plan rarely and
  mattered when they did.

  </details>

## References

- Bingqing Lyu, Xiaoli Zhou, Longbin Lai, Yufan Yang, Yunkai Lou, Wenyuan
  Yu, Ying Zhang, Jingren Zhou, **"GOpt: A Graph-Native Query Optimization
  Framework"**, arXiv:2401.17786 — §4 (GLogue as the statistics provider),
  §6.3.1 (Eq. 1, Eq. 2, Example 6.2, Figure 6), §6.3.3 (search), §8.2 and
  Figure 8(d) (ablation).
- Longbin Lai et al., **"GLogS: Interactive Graph Pattern Matching Query At
  Large Scale"**, USENIX ATC 2023, pp. 53–69 — where GLogue comes from
  (GOpt's ref. [40]); not read.
- Previous: [reading-pessimistic-bounds.md](reading-pessimistic-bounds.md).
  Next: [reading-color.md](reading-color.md) — summarising the *graph*
  instead of the patterns.
