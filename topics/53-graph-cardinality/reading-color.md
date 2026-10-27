# Color: summarise the graph into a few kinds of vertex, then count on the summary

Chapters 1–3 each pick a statistic — an average degree, a degree
sequence, a small-pattern count — and live with what it cannot see. A fourth
route summarises the *graph itself* so that the thing the independence
assumption gets wrong, the difference between hubs and the rest, is encoded
in which summary vertex a vertex belongs to. If every vertex in a group has
nearly the same neighbours-by-group counts, an average over the group is
nearly exact, and a multi-hop estimate becomes a weighted path count over a
graph of a few dozen vertices.

**"Color: A Framework for Applying Graph Coloring to Subgraph Cardinality
Estimation"** — Kyle Deeds, Diandre Sabale, Moe Kayali, Dan Suciu. PVLDB
18(2), 2024, pp. 130–143 (arXiv:2405.06767; this guide is written against
arXiv v2, 29 April 2025). Code: <https://github.com/uwdb/color>. The
evaluation is on eight benchmark datasets from G-CARE and from a
subgraph-matching survey; nothing here was re-measured on those. What
*was* measured here is the effect Color exploits — hubs shared across edge
types — which lane 1 builds in on purpose, and an arithmetic example from
the paper that this guide works end to end.

## The problem in one sentence

Averages over *all* vertices are wrong because vertices differ; grouping
vertices so that those in a group look alike — in how many neighbours they
have in each other group — makes the group average nearly exact, so how
few groups suffice, and how do you count patterns on the groups?

## The concepts, step by step

### Step 1 — the traditional estimator is an Erdős–Rényi graph in disguise

> **In:** a data graph `G = (V, E)` and a query graph `Q`. **Out:** the
> baseline estimate `Π_{v∈V_Q} |V| · Π_{e∈E_Q} |E|/|V|²` of Def. 4, and its
> value on a path.

Def. 4 (§2) writes the classical estimator for graphs. The number of ways
to place the query's vertices on data vertices is `|V|` per query vertex;
multiply by the probability that a random pair of vertices is joined by an
edge, `|E|/|V|²`, once per query edge:

```
trad(Q) = |V|^{|V_Q|} · ( |E| / |V|² )^{|E_Q|}
```

The paper's reading: "this estimation procedure assumes that the data graph
is distributed like an Erdős–Rényi random graph with |E| edges and |V|
vertices" — each potential edge present independently with the same
probability. For a path of `k` edges, `|V_Q| = k + 1`, so
`trad = |V|^{k+1} · |E|^k / |V|^{2k} = |V| · (|E|/|V|)^k`: the number of
vertices times the *average degree* to the `k`-th power. That is lane 1's
independence estimator with one label (Example 1 in the paper shows it
equals the relational `|R||S| / max(|R.y|, |S.y|)` when both relations are
the edge relation).

### Step 2 — colorings and the lifted graph

> **In:** the data graph. **Out:** a **coloring**, the **lifted graph** that
> summarises it, and the three edge statistics `τ_min`, `τ_avg`, `τ_max`.

A **coloring** `σ : V → C` assigns each vertex one of a small set of
**colors** (`|C| ≪ |V|`). The paper wants one where vertices of a color are
alike in the following sense (Def. 5), called **quasi-stable**: for any two
colors `c_0` and `c`, any two vertices of color `c_0` have *approximately*
the same number of neighbours colored `c`. With "approximately" replaced by
"exactly" the coloring is **stable**, the notion from graph-isomorphism
algorithms; it is useless here because "stable colorings of real-world
graphs require a very large number of colors" and "in a random graph, every
vertex has a distinct color with high probability" (§3). The relaxation is
the design: pay a small approximation error for a small number of colors.

The **lifted graph** `𝒢 = (F, ψ, τ)` (Def. 6) is the summary:

- `F`: one vertex per color, an edge `(c_1, c_2)` when any data edge goes
  between those colors;
- `ψ(c) = |V_c|`: how many data vertices have color `c`;
- `τ(c_1, c_2)`: a statistic of the edges `E_{c1c2}` between the two colors,
  one of

```
τ_min(c1,c2) = min over v1 ∈ V_c1 of  |{ (v1,v2) ∈ E : v2 ∈ V_c2 }|     fewest edges into c2
τ_avg(c1,c2) = |E_{c1c2}| / |V_c1|                                       average
τ_max(c1,c2) = max over v1 ∈ V_c1 of  |{ (v1,v2) ∈ E : v2 ∈ V_c2 }|     most edges into c2
```

"Unless otherwise noted" the paper uses `τ_avg`. With predicates
(`hasLabel:X`, ranges) `ψ` and `τ` take the predicate as an argument, and
may be exact (a small set of label combinations) or approximate
(histograms, n-grams) (§3). The paper reports that 32 colors typically
suffice (§1) and that the average range of degrees between color pairs
drops by two orders of magnitude (§3; the introduction says "1–2 orders" —
the two statements differ and the figure would settle it).

### Step 3 — the estimator on the lifted graph

> **In:** Step 2's `ψ` and `τ_avg`, a query `Q` that is acyclic. **Out:**
> Def. 8's `W(π)`, the sum Φ over colorings `π`, and Example 4 worked in
> full.

A **homomorphism** `π : Q → F` assigns each query vertex a color — a
*coloring of the query*. For each `π` the **acyclic estimator** (Def. 8,
Eq. 4) processes the query's vertices in a topological order
`x_1, …, x_n` (every `x_j` adjacent to an earlier `x_i`):

```
W(π) = ψ(π(x_1)) · Π_{(x_i, x_j) ∈ E_Q} τ( π(x_i), π(x_j) )
```

— the number of vertices of the first color, times, for each query edge,
the average number of edges from a vertex of its (earlier) endpoint's color
to the color of the later endpoint. The total estimate is the sum over all
colorings (Def. 7, Eq. 3):

```
Φ(Q, 𝒢) = Σ_{π ∈ hom(Q, F)} W(π)
```

**Example 4** (§4.1), worked. The data graph is the disjoint union of `G_1`,
a 2-regular graph on 10,000 vertices (each vertex has out-degree 2), and
`G_2`, a clique of 100 vertices (out-degree 99). The query is a path of `k`
edges.

- *Traditional estimate.* `|V| = 10,100` and `|E| = 10,000·2 + 100·99 =
  29,900`, so the average degree is `29,900/10,100 = 2.960` and
  `trad = 10,100 · 2.960^k`. (The paper's text says "since the average
  degree in G is ≈ 4, a traditional estimate … is 10100·4^k"; the average
  out-degree computed from the stated graphs is 2.96, not 4, so this guide
  uses its own arithmetic. The conclusion is the same.)
- *Lifted graph with two colors.* Green = `G_1`'s vertices, red = `G_2`'s;
  edges (green, green) with `τ = 2` and (red, red) with `τ = 99`. Only two
  colorings `π` exist (all-green, all-red):
  `W(green) = 10,000 · 2^k`, `W(red) = 100 · 99^k`, and
  `Φ = 100·99^k + 10,000·2^k` — "for our simple data graph, an exact count".

Evaluate for `k = 2, 3, 4`:

| k | exact `100·99^k + 10000·2^k` | traditional `10100·2.960^k` | q-error (under) |
|---|---|---|---|
| 2 | 1,020,100 | 88,516 | 11.5 |
| 3 | 97,109,900 | 262,042 | 370.6 |
| 4 | 9,606,120,100 | 775,748 | 12,383.0 |

Two colors turn an estimate wrong by 12,383× at four edges into an exact
one. The mechanism is the same as lane 1's: the clique is a set of vertices
whose in- and out-degrees are both high, and the average degree smears them
into 10,000 vertices of degree 2; the color separates them. The error grows
by ×32 to ×33 per edge — `99/2.96 = 33.4` for the clique's term — which is
lane 1's geometric growth in the number of hops.

When is the lifted estimator exact? **Theorem 1** (§4.2): if the coloring
is stable (`τ_min = τ_avg = τ_max`), `Φ` equals `|hom(Q, G)|` for every
acyclic `Q`. **Corollary 1** (Eq. 6–7): for a quasi-stable coloring with
`ε = max_{c1,c2} τ_max(c1,c2) / τ_min(c1,c2)`, the q-error of the estimate
is at most `ε^{|V_Q| − 1}` — the error compounds with the query's size,
and the way to shrink it is a coloring that shrinks `ε`.

### Step 4 — which coloring, and how to choose

> **In:** Step 2's idea that a color is "vertices that look alike".
> **Out:** the colorings the paper compares and its verdict.

Any function `V → C` is a coloring; the paper uses **divisive** ones that
start with every vertex in one color and repeatedly split the worst color
in two (§6). The splits it studies:

- **Quasi-stable coloring** (Kayali & Suciu, ref. [19]): split the color
  with the widest spread of neighbour counts toward another color.
- **Degree coloring**: split the color with the widest degree range into the
  vertices above and below its average degree. Lane 1's hub effect is a
  degree effect, so this is the minimal coloring for it — and the starting
  point of exercise 4 in the [README](README.md).
- **Vertex-label coloring** and **neighbor-label coloring**: split on
  `hasLabel` predicates or on the labels of neighbours.
- **Mixture coloring**: layer several of the above; "the most accurate
  coloring across a range of workloads" (§6; Figure 9).
- **Hash coloring**: random assignment. It "offers limited improvement"
  because it ignores topology and attributes; it is the partitioning
  BoundSketch uses ([reading-gcare.md](reading-gcare.md) Step 5) and "performs
  the worst across all benchmarks" (§8.1).

### Step 5 — cycles, and what is conservative about it

> **In:** Step 3's acyclic estimator. **Out:** the path-closure
> probability γ and how Eq. 8 uses it, without the sampling details.

A cycle is a tree plus *non-tree edges*, and the estimator of Step 3 has
nothing to say about whether a non-tree edge `(x, y)` is present given that
`x` and `y` are already connected by a path. The paper defines the **path
closure probability** (Def. 9) for a direction pattern `D ∈ {←,→}^k` and
two colors:

```
γ(c1, c2, D) = |closed paths| / |paths|        over paths of direction pattern D from color c1 to color c2
```

where a path is **closed** if there is an edge from its start to its end.
For a non-tree edge `e_i`, take every simple path in the query already
placed from `e_i`'s source to its destination; treat the closure of each as
independent and combine (§5.2, "a conservative assumption")

```
P(E_i | E_1, …, E_{i-1}) = 1 − Π_{(c1,c2,D)} ( 1 − γ(c1,c2,D) )
```

and multiply the lifted estimate by that factor instead of `τ` (Eq. 8–9).
The paper estimates γ by sampling 100,000 paths from the lifted graph
rather than computing it. Compare with lane 1's triangle row: the
independence estimator's closing probability `|E_{r4}| / (|L_2| · |L_0|)`
is the *unconditional* edge density, 229× too small on the shared-
popularity graph; γ conditions it on the path's end colors, which is what
the hubs make informative.

### Step 6 — what it measured

> **In:** Steps 1–5. **Out:** the claims in §8 that this guide can cite, and
> the caveats.

§8 lists five claims for Color over nine competing methods on eight
datasets. The ones with numbers:

- "Has a median error < 10 across all workloads and is up to 10³× more
  accurate than competing methods." (§8, claim 2)
- "Requires up to 80–800× less space than competing methods and is up to
  10–100× faster to construct." (§8, claim 4)
- "Handles updates gracefully with only 3× worse median error when half the
  graph is updated." (§8, claim 5)
- "Never experiences estimation failure on any query across all workloads."
  (§8, claim 1) — contrast sampling methods' 60–99% failure rates on the
  hardest workloads (§1).

Its introduction places the older approaches' failures on this scale:
pattern-count methods ("combin[ing] them at runtime … by assuming
independence between their counts") reach "median underestimates of up to
10¹⁰×" on larger-query workloads and summarisation methods with "predefined
rules or hashing" "median error of up to 10¹²×" (§1, pointing to §8) —
numbers this guide quotes as the authors' and did not reproduce.

The honest caveats. The 32-color default and the median-error claim are on
workloads of 3–12-edge queries over graphs of up to millions of edges, and
Color's strengths are accuracy on *large* queries; for the one- and
two-edge patterns of lane 3's catalog, exact storage already wins. The
estimator needs the lifted graph in memory and a sampled γ for cycles; its
inference cost is controlled by the paper's optimisations — tree
decomposition with partial aggregation ("reduce the inference latency by
over 100×") and importance sampling (a "6x lower error than a naive sampling
approach") (§1, §7).

## How to read the paper (with the concepts in hand)

1. **§1 and Figure 1** — the lifted-counting picture and the claim list.
2. **§2** — Defs. 1–4; skim for the notation table.
3. **§3–§4** — Steps 2–3; do Example 4 yourself before reading its answer.
4. **§5** — Step 5. Read Def. 9 beside lane 1's triangle row.
5. **§6** — the colorings; Figure 9 is the accuracy comparison across them.
6. **§7 and §8** — optimisations and evaluation. Read Figure 3 (accuracy by
   method and workload) after you know what each method assumes.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | `est.rs` `indep_path` | the average-degree product, with labels |
| 3 | Example 4 above; exercise 4 in the [README](README.md) | a degree-bucket lifted graph on lane 1 |
| 5 | lane 1 triangle table | independence vs AGM vs ℓ2, 229× / 131× / 13× |

## Questions (answer in notes.md)

1. Redo Example 4 with a clique of 50 vertices instead of 100. What are the
   exact count, the traditional estimate and the q-error at `k = 3`?
2. A lifted graph has colors red (500 vertices) and blue (9,500). Each red
   vertex has 40 edges into blue and each blue vertex 2 edges into red.
   Compute `W` for the query `x → y → z` with `π(x) = red, π(y) = blue,
   π(z) = red` using `τ_avg`.
3. Corollary 1 says the q-error is at most `ε^{|V_Q|−1}`. If `ε = 1.5`,
   how large can the q-error be for a five-edge path? Compare with lane 1's
   four-edge q-error of 324 for plain independence.
4. Why is a coloring by degree a poor summary for lane 1's *independent
   popularity* graph, and a good one for the shared-popularity graph?

## Done when

Answer each before unfolding it.

- [ ] You can write the traditional estimator for a graph and say what random graph it assumes.

  <details><summary>Answer</summary>

  `|V|^{|V_Q|} · (|E|/|V|²)^{|E_Q|}`; for a `k`-edge path `|V| · (|E|/|V|)^k`.
  It is exact for an Erdős–Rényi graph with the same `|V|` and `|E|`, where
  every potential edge is present independently with the same probability;
  real graphs with hubs differ from that by orders of magnitude (Example 4).

  </details>

- [ ] You can define a quasi-stable coloring and the lifted graph built from it.

  <details><summary>Answer</summary>

  A coloring in which any two vertices of one color have approximately the
  same number of neighbours of each other color (Def. 5). The lifted graph
  has a vertex per color, `ψ(c) = |V_c|`, and for each connected color pair
  a statistic `τ` (min, average or max of the per-vertex edge count),
  `τ_avg = |E_c1c2| / |V_c1|` by default (Def. 6).

  </details>

- [ ] You can compute the lifted estimate for a path and reproduce Example 4's numbers.

  <details><summary>Answer</summary>

  `W(π) = ψ(π(x_1)) · Π τ` along the query's edges, summed over colorings.
  Two colors give `100·99^k + 10,000·2^k`: 1,020,100 at `k = 2`, 97,109,900
  at 3, 9,606,120,100 at 4, exact, against `10,100 · 2.960^k` = 88,516,
  262,042 and 775,748 — q-errors 11.5, 370.6 and 12,383 (the paper writes
  `10100·4^k`; the stated graphs have average out-degree 2.96).

  </details>

- [ ] You can state when the lifted estimator is exact and how its error is bounded otherwise.

  <details><summary>Answer</summary>

  Exact for every acyclic query when the coloring is stable, so
  `τ_min = τ_avg = τ_max` (Theorem 1). For a quasi-stable coloring the
  q-error is at most `ε^{|V_Q|−1}` with `ε = max τ_max/τ_min` over color
  pairs (Corollary 1), so a better coloring means a smaller `ε`, and the
  bound grows with the query's size.

  </details>

- [ ] You can explain what γ is and why it is stated as conservative.

  <details><summary>Answer</summary>

  The probability that a path of a given direction pattern between two
  colors is closed by an edge (Def. 9). A non-tree edge's presence is
  estimated as `1 − Π(1 − γ)` over the already-placed paths between its
  endpoints, treating those paths' closures as independent events — which
  the paper calls a conservative assumption (§5.2) — instead of the
  unconditional edge density.

  </details>

## References

- Kyle Deeds, Diandre Sabale, Moe Kayali, Dan Suciu, **"Color: A Framework
  for Applying Graph Coloring to Subgraph Cardinality Estimation"**, PVLDB
  18(2) 2024 / arXiv:2405.06767v2 — §1 (claims, Figures 1–2), §2 (Defs.
  1–4), §3 (Defs. 5–6, τ), §4 (Defs. 7–8, Examples 3–4, Theorem 1,
  Corollary 1), §5 (Def. 9, Eq. 8–9), §6 (colorings), §7 (optimisations),
  §8 (evaluation, Figure 3, Figure 9).
- Moe Kayali and Dan Suciu, **"Quasi-stable coloring for graph
  compression"**, PVLDB 2022 — the coloring algorithm Color builds on (ref.
  [19]); not read.
- Previous: [reading-glogue-gopt.md](reading-glogue-gopt.md). Next:
  [reading-engine-estimators.md](reading-engine-estimators.md) — what the
  engines actually do.
