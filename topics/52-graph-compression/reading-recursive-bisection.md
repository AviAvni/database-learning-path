# Recursive graph bisection: choosing vertex ids to minimise the gaps

WebGraph ([reading-webgraph-framework.md](reading-webgraph-framework.md)) takes
the vertex numbering as given and compresses what it finds. This paper asks
the question the other way round: if you may *choose* the numbering, which one
minimises the size of the gap-coded lists, and how do you find it on a graph
with a billion vertices? Its answer — split the vertices in two halves so that
neighbour lists concentrate in one half, then recurse — is the algorithm lane 2
asks you to write.

**"Compressing Graphs and Indexes with Recursive Graph Bisection"** — Laxman
Dhulipala, Igor Kabiljo, Brian Karrer, Giuseppe Ottaviano, Sergey Pupyrev,
Alon Shalita. KDD 2016. The copy read is arXiv:1602.08820v1
([pdf](https://arxiv.org/pdf/1602.08820)); section, table and figure numbers
below are that version's, and the KDD proceedings text may differ. "BP" is the
paper's name for the algorithm (bisection partitioning); this guide uses it.

## The problem in one sentence

Given a graph, find a permutation of vertex ids that minimises the total number
of bits needed to write every adjacency list as gaps — a problem that is
NP-hard, so what is a cheap local-search procedure whose objective is the
compressed size itself?

## The concepts, step by step

### Step 1 — the objective: how many bits does a list cost?

> **In:** sorted adjacency lists (as in the WebGraph guide's Step 2).
> **Out:** a function of the numbering `π` that approximates the compressed
> size — **LogGap** — and the paper's reason to trust it.

Let `π(v)` be the new id of vertex `v`. A list's neighbours, sorted by `π`,
are `u1 < u2 < … < uk` (in new ids). The gaps between consecutive ones cost
about `log(gap)` bits each, where the paper writes `log(x) = 1 + ⌊log₂ x⌋`:
the number of bits in the binary representation of `x` (§3). The cost of one
list is `Σ log(π(u_{i+1}) − π(u_i))`.

Worked: a list whose new ids are `3, 10, 12` has gaps `7` and `2`;
`log(7) = 1 + 2 = 3`, `log(2) = 1 + 1 = 2`; the list costs 5 bits and
**LogGap**, the cost averaged over the gaps, is `5 / 2 = 2.5`. The same
neighbours numbered `3, 4, 5` have gaps `1, 1` and cost `1 + 1 = 2`, LogGap 1.

The paper calls the problem of minimising the sum **MLogGapA** (minimum
logarithmic gap arrangement), proves it NP-hard (Theorem 1, by reduction from
MLogA, which splits every edge by a degree-2 vertex), and generalises it to
**BiMLogA**: a bipartite graph of *query* vertices (the lists) and *data*
vertices (the ids being numbered). For a graph, every vertex is both: a query
(its out-list) and a datum (a possible neighbour). For an inverted index the
queries are terms and the data are documents. Only data are renumbered.

Why trust LogGap? §5.4 measures bits per edge under WebGraph's BV coding
against the LogGap of the order for the same graphs: the Pearson correlation
is **0.9853** (0.8487 for Partitioned Elias–Fano, 0.8436 for Binary
Interpolative Coding, the two index codecs they also use). LogGap is
therefore a stand-in you can optimise without running an encoder. This topic's
`gapcode::log_gap` is exactly this quantity, over the gaps `t[i] − t[i−1]`.

### Step 2 — why bisection

> **In:** Step 1's objective. **Out:** the divide-and-conquer shape of the
> algorithm (§3.2, Algorithm 1).

Most approximation algorithms for linear-arrangement problems share one
shape: split the vertices into two equal halves, order each half recursively,
concatenate. The first half gets ids `[1, n/2]`, the second `[n/2 + 1, n]`.
Hansen proved this gives an `O(α log n)` approximation for the classic MLA
problem, where `α` measures how good the bisection is (§3.2). For the new
problem the paper only notes that *every* order is within `O(log n)` of
optimal, since each gap is between 1 and `log n` bits, and then builds a
practical algorithm on the same recursion rather than a provable one — it is
"inspired by" the theory, not an approximation algorithm for BiMLogA.

The recursion also explains why ids end up with a useful *hierarchy*: after
`d` levels there are `2^d` id ranges, each a community of data vertices, and
the first split is the coarsest. A list that sits entirely inside one range at
depth `d` has all its gaps below `n / 2^d`.

### Step 3 — the bisection step: a cost per query, a gain per vertex

> **In:** a set of data vertices `D` to be split into halves `V1`, `V2` (sizes
> `n1`, `n2`) and the queries that touch them. **Out:** a swap rule that
> improves a split, as in Kernighan–Lin (§4, Algorithm 2).

Split `D` into `V1` and `V2` (the first split is random in the paper's default
for graphs). For a query `q`, let `deg1(q)` be its number of neighbours in
`V1` and `deg2(q)` its number in `V2`. If the neighbours in `V1` were spread
evenly over `V1`'s `n1` ids, the average gap would be `n1 / (deg1 + 1)`,
costing `log(n1 / (deg1 + 1))` bits each, and likewise for `V2`. The cost of
the split is the estimate (§4):

cost = Σ_q [ deg1(q) · log(n1 / (deg1(q) + 1)) + deg2(q) · log(n2 / (deg2(q) + 1)) ]

The paper drops the one gap between the last neighbour in `V1` and the first
in `V2` as a constant (assuming `n1 = n2`). The cost rewards concentration.
Worked with `n1 = n2 = 8` and a query with four neighbours (using real
`log₂`, as an implementation would):

| split (deg1, deg2) | cost |
|---|---|
| (4, 0) | 4·log₂(8/5) = 2.712 |
| (2, 2) | 2·log₂(8/3) + 2·log₂(8/3) = 5.660 |
| (3, 1) | 3·log₂(8/4) + 1·log₂(8/2) = 5.000 |

The **move gain** of a data vertex `v` is the decrease of the total cost if
`v` hops to the other half, summed over the queries that contain `v`. From
(2, 2), moving one neighbour to the other side gives (1, 3) or (3, 1), cost
5.000: a gain of 0.660. From (4, 0) a move gives (3, 1): a gain of −2.288 —
the vertex wants to stay with its list-mates.

Algorithm 2: compute every vertex's gain; sort `V1` and `V2` by gain
descending into `S1` and `S2`; walk both lists, swapping the pair at each
position while `gain(v) + gain(u) > 0`. Repeat until no pair swaps or an
iteration limit. The one departure from classic Kernighan–Lin is
"we do not update move gains after every swap": all gains are computed once
per iteration, which is what makes the step parallel (a swap can make a
neighbour's gain stale, and the algorithm accepts that).

### Step 4 — recursion, depth, iterations, and what they cost

> **In:** Step 3's split. **Out:** the full algorithm, its parameters, and its
> complexity (§4, Theorem 2, §5.3).

Recurse on `V1` and `V2` (each as the data set; every query that touches them
stays, restricted to the part it touches). Parameters the paper settles
experimentally (§5.3):

* **Depth.** Stop at `log₂ n − 5` — leaves of about 32 vertices. Figure 4
  shows the last few splits "make insignificant contributions".
* **Iterations.** 20 per bisection; Figure 5 (LiveJournal) shows the share of
  vertices that move falls below 1% by iteration 20 even at the deepest
  levels.
* **Initialisation.** Random split for graphs, minhash for indexes (Figure 3:
  no consistent winner; BFS is best on three graphs but not on indexes, where
  high-degree queries "make the BFS order essentially random").

Complexity (Theorem 2): `O(m log n + n log² n)` — `⌈log n⌉` levels, each doing
`O(m)` gain computation and an `O(n log n)` sort. Two sibling recursive
calls are independent, and one bisection is a data-parallel gain computation
plus a sort, which is how the paper's C++ implementation uses 32 cores
(§4.1). The distributed Giraph version replaces the sort with an approximate
histogram of gains.

### Step 5 — results, and the one place BP loses

> **In:** Steps 1–4. **Out:** what the paper measured, with the figures that
> matter for this topic.

Table 2 (bits per edge under WebGraph BV, `Natural` = the order the data came
in, URL order for the web graphs):

| graph | Natural | BFS | Minhash | LLP | BP |
|---|---|---|---|---|---|
| web-Google (356,648 vertices, 5.19 M edges) | 20.08 | 7.69 | 6.87 | 5.13 | **4.68** |
| LiveJournal (4.85 M, 85.7 M) | 14.61 | 14.69 | 15.07 | 11.12 | **10.73** |
| Twitter (41.7 M, 2.4 G) | 21.56 | 17.99 | 14.76 | — | **11.62** |

The paper's summary: "BP yields the best compression on all but one instance,
providing an 5–20% improvement over the best alternative"; the exception is
web-Google, where Multiscale reaches 4.10 against BP's 4.68. Run times (§5.5):
LiveJournal 3 minutes and Twitter 149 minutes for BP (32 cores), against 23
minutes for LLP on LiveJournal, which "is not able to process Twitter within a
reasonable time".

Read the table for what this topic needs. On LiveJournal the *natural* order
(14.61) is better than BFS and minhash — an order is not an improvement just
because it is computed. And web-Google's natural URL order at 20.08 versus
4.68 with BP: the same graph, a 4.3× size difference, from the numbering
alone, with identical codec. For indexes (Table 3), BP gives 22% and 15%
over alternatives and is 4.34 against 4.99 bits per edge for ClueWeb09 under
PEF.

Section 6's last paragraph is the sentence that ties into cache behaviour:
"various graph traversal algorithms can be accelerated if the in-memory graph
layout takes advantage of the cache architecture … We are currently working on
exploring this area". The paper does **not** measure traversal speed; the
[Gorder replication](reading-gorder-replication.md) and lane 1's scan columns
do.

### Step 6 — what a re-implementation must decide

> **In:** Algorithm 2 as printed. **Out:** the choices the paper leaves open,
> so lane 2's contract is a spec and not a guess.

* **Gains need the queries of a data vertex.** In a graph with out-lists as
  queries, a vertex's queries are its *in-neighbours*; you need the
  transpose. (`Csr::transpose` is provided.)
* **Queries restricted to the subproblem.** At depth `d` a query only counts
  its neighbours inside the current range; a neighbour outside is a constant.
* **Ties.** Sorting by gain needs a deterministic tiebreak (vertex id) or two
  runs differ.
* **The logarithm.** The paper's `log(x) = 1 + ⌊log₂ x⌋` is the *bit length*;
  the cost estimate divides by `deg + 1` and so needs real values. Use `log₂`
  as real numbers for the estimate and bit-length for reporting LogGap.
* **Memory.** The single-machine implementation "utilizes less than twice the
  space required to store the graph edges" (§5.5).

## How to read the paper (with the concepts in hand)

Read §3.1 for the model and skip the NP-hardness proof on a first pass; read
§4 with Step 3's table beside it, and go to Figures 3–5 and Table 2. The
paper's source was not released ("due to corporate restrictions", §5.1). Where
a *released* implementation can be read — the reordering step of WebGraph's
LLP in Rust — see [reading-webgraph-rs.md](reading-webgraph-rs.md).

## Where each step lives in this topic

| step | here |
|---|---|
| LogGap | `gapcode::log_gap` (lane 1 column `LogGap`) |
| bisection, gains, recursion | `bisect.rs` (lane 2 stub); reference numbers in [notes.md](notes.md) |
| in-neighbour lists | `Csr::transpose` |
| the baseline orders | `order.rs`: random, in-degree, BFS |

## Questions (answer in notes.md)

1. For `n1 = n2 = 8`, compute the move gain of a vertex that is in two lists:
   one currently at (2, 2) and one at (4, 0). Does it move?
2. On lane 1, planted order's LogGap is far below random's. Predict where
   BP's LogGap lands between them before you run lane 2.
3. Table 2 has BFS *worse* than Natural on LiveJournal and better on web-Google.
   What property of the starting order explains the sign?
4. Why does the algorithm not update gains after each swap, and what does it
   risk?

## Done when

Answer each before unfolding it.

- [ ] You can state the objective BP minimises and compute it for one list.

  <details><summary>Answer</summary>

  LogGap: for a list with new ids u1 < … < uk, the sum of
  `1 + ⌊log₂(u_{i+1} − u_i)⌋` over consecutive gaps, averaged over gaps. New
  ids 3, 10, 12: gaps 7 and 2, bits 3 + 2 = 5, LogGap 2.5. It tracks WebGraph's
  bits per edge with Pearson correlation 0.9853 (§5.4).

  </details>

- [ ] You can compute a vertex's move gain from the paper's cost function.

  <details><summary>Answer</summary>

  cost(q) = deg1·log(n1/(deg1+1)) + deg2·log(n2/(deg2+1)); gain is the cost
  before minus the cost after the vertex moves, summed over its queries. With
  n1 = n2 = 8 and a query at (2, 2) the cost is 5.660; after one neighbour
  moves it is 5.000, a gain of 0.660. From (4, 0) the cost rises from 2.712 to
  5.000, so the gain is −2.288 and the vertex stays.

  </details>

- [ ] You can give the algorithm's shape, parameters and complexity.

  <details><summary>Answer</summary>

  Random initial split into halves; up to 20 iterations of: compute every
  vertex's gain, sort each half by gain, swap pairs while the gains sum to
  more than zero; then recurse on each half down to leaves of about 32
  (depth log n − 5). O(m log n + n log² n) (Theorem 2). Gains are not updated
  within an iteration, which allows data-parallel computation (§4–§4.1).

  </details>

- [ ] You can say where BP lost in the paper's own table and what the table
  says about natural orders.

  <details><summary>Answer</summary>

  On web-Google Multiscale reached 4.10 bits per edge against BP's 4.68
  (Table 2). On LiveJournal the natural order (14.61) beat BFS (14.69) and
  minhash (15.07): computing an order is no guarantee of improving on the order
  the data arrived in. On web-Google the natural order was 20.08 against 4.68
  for BP, the same codec and graph, 4.3× from the numbering alone.

  </details>

## References

- Dhulipala, Kabiljo, Karrer, Ottaviano, Pupyrev & Shalita, *Compressing Graphs
  and Indexes with Recursive Graph Bisection*, KDD 2016, arXiv:1602.08820v1 —
  §3.1 model, §4 Algorithms 1–2 and Theorem 2, §5.3–§5.5, Tables 1–3,
  Figures 3–5, §6.
- Chierichetti et al., *On compressing social networks*, KDD 2009 — the
  MLogA/shingle-ordering model this paper extends; not read.
- Boldi, Rosa, Santini & Vigna, *Layered Label Propagation* (WWW 2011) — "LLP",
  the main competitor; not read.
- [reading-gorder-replication.md](reading-gorder-replication.md) — reordering
  measured on traversal speed rather than on bits.
