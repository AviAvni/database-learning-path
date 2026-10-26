# k²-trees: compress the adjacency matrix itself, and get both directions from one structure

Everything so far compresses *lists*: each vertex's neighbours as gaps, copies
and intervals. A GraphBLAS engine thinks in *matrices*, and a matrix invites a
different idea: a sparse adjacency matrix is mostly empty, and the empty parts
are large and rectangular, so describe the matrix as a quadtree and prune every
quadrant that contains no edge. Brisaboa, Ladra and Navarro's k²-tree does
exactly that, and its distinguishing property for an engine like FalkorDB —
which stores both a relation matrix and its transpose — is that **one structure
answers forward and reverse neighbour queries in equal time**.

**"k²-trees for Compact Web Graph Representation"** — Nieves R. Brisaboa, Susana
Ladra, Gonzalo Navarro. SPIRE 2009, LNCS 5721
([pdf](https://users.dcc.uchile.cl/~gnavarro/ps/spire09.1.pdf)). Section, table
and figure numbers are that paper's. The 2014 journal extension (Information
Systems, "Compact Representation of Web Graphs with Extended Functionality")
reports 1–3 bits per link in its abstract; it was not read, and no number from
it is used here.

## The problem in one sentence

How can a sparse 0/1 matrix be stored so that large empty areas cost almost
nothing, a row or a column can be listed in time proportional to its output,
and a single cell can be tested without listing anything?

## The concepts, step by step

### Step 1 — the tree: split into k² quadrants, keep only the non-empty

> **In:** an `n × n` adjacency matrix `a_ij` (1 if vertex `i` links to `j`).
> **Out:** a tree of bits whose nodes are submatrices (§2).

Choose `k` (2 is typical). Split the matrix into `k × k` equal submatrices —
`k²` children. Each child gets one bit: **1** if the submatrix contains at least
one 1, **0** if it is all zeros. A 0 child is not split further: the whole
empty quadrant is one bit. A 1 child is split again, until the submatrices are
single cells, whose bits are the matrix's values. The height is
`h = ⌈log_k n⌉`; if `n` is not a power of `k` the matrix is conceptually padded
with zeros to `k^h`. Children are ordered row by row, left to right.

Worked on a 4 × 4 matrix with `k = 2` (this guide's own example, not the
paper's Figure 1):

```
        cols 0 1 2 3
row 0:       1 1 0 0
row 1:       0 0 0 0
row 2:       0 0 1 0
row 3:       0 0 1 1
```

Level 1 has four quadrants: top-left `[[1,1],[0,0]]` contains a 1, top-right
(rows 0–1, cols 2–3) is empty, bottom-left (rows 2–3, cols 0–1) is empty,
bottom-right `[[1,0],[1,1]]` contains 1s: bits `1 0 0 1`. The two non-empty
quadrants are split into cells: top-left's cells `1 1 0 0`, bottom-right's
`1 0 1 1`. Five links, `4 + 8 = 12` bits, against 16 for the plain matrix.

### Step 2 — the layout: two bit arrays and rank

> **In:** Step 1's tree. **Out:** the paper's `T` and `L` arrays and the rule
> that finds a node's children with no pointers (§3.1–3.2, Lemma 1).

Write the tree level by level (a **levelwise** traversal) into two bit arrays:
**`T`** holds every level except the last, **`L`** the last level (the matrix
cells). For the example: `T = 1001` (level 1) and `L = 1100 1011` (level 2,
top-left's four cells then bottom-right's four, in the order of the 1s in `T`).
There are no pointers: the children of the node at position `x` of `T` (a 1) are
at

position `rank(T, x) · k² + i`, for `i = 0 … k² − 1`,

where **`rank(T, x)`** is the number of 1s in `T[0..x]` (inclusive), and `T:L`
means `L` is addressed as a continuation of `T`. Lemma 1's proof is one
sentence: every 1 before `x`, plus the root, has already contributed `k²`
children to the stream. Check on the example: the second 1 of `T` is at
position 3; `rank(T, 3) = 2`; its children start at `2 · 4 = 8`, which is
position `8 − |T| = 4` of `L` — the block `1 0 1 1`, the bottom-right quadrant's
cells. ✓.

`rank` is the only auxiliary structure: a counter table over `T` that answers in
constant time at 5% extra space (slow) or 37.5% (fast) (§3.1; Gonzalez et al.
2005). No rank is needed over `L`, which is why it is a separate array.

### Step 3 — forward and reverse neighbours, symmetric by construction

> **In:** Step 2's layout. **Out:** the `Direct` and `Reverse` traversals
> (§3.3, Figure 2), worked on the example.

To list the links *from* page `p`, find the 1s in row `p`: walk down from the
root and at each node enter only the `k` children that overlap row `p` (one row
of the `k × k` grid of children). Reverse neighbours (who links *to* `q`) are
the same walk over a column — "searching for direct or for reverse neighbors in
the k²-tree is completely symmetric. The only difference is the formula to
compute the children".

```text
Direct(n, p, q, x)                     // from the paper's Figure 2
  if x >= |T|:                         // leaf level
      if L[x - |T|] = 1: output q
  else if x = -1 or T[x] = 1:          // x = -1 is the root
      y = rank(T, x) * k^2 + k * floor(p / (n/k))
      for j = 0 .. k-1:
          Direct(n/k, p mod (n/k), q + (n/k) * j, y + j)
```

(`q` accumulates the column offset of the current submatrix.) Worked for row
`p = 3`, `n = 4`, `k = 2`:

* Root, `x = −1`: `y = 0 + 2·⌊3/2⌋ = 2`; visit children `2` and `3`.
* Child 2: `T[2] = 0`: an empty quadrant, stop. The whole bottom-left block
  (rows 2–3, cols 0–1) costs one bit and one test.
* Child 3: `T[3] = 1`; now `n = 2`, `p = 3 mod 2 = 1`, column offset 2;
  `y = rank(T,3)·4 + 2·⌊1/1⌋ = 8 + 2 = 10`: visit positions 10 and 11, i.e. `L[6]`
  and `L[7]`, both 1: output columns `2` and `3`.

Row 3 = `{2, 3}`, as in the matrix.

Cost. The worst case is `O(n)` per query (a row of 1s next to a row of 0s forces
the walk to the leaves and returns nothing), and for `m` uniformly placed 1s the
paper bounds the expected work by `O(√m)`, "not the ideal `O(m/n)` … but much
better than `O(n)` or `O(m)`" (§3.3). On clustered matrices it is better
because a visited cell near row `p` predicts useful cells in row `p`.

### Step 4 — space: what a tree costs, and why `k = 2` wins on space

> **In:** Steps 1–2. **Out:** the paper's worst-case bound and a worked number.

In the worst case every 1 creates one distinct internal node per level, each
costing `k²` bits: `k² · m · (log_{k²}(n²/m) + O(1))` bits (§3.2, "Analysis").
A smaller `k` is better in the worst case; for `k = 2` it is
`2m·log₂(n²/m) + O(m)`, which "is asymptotically twice the information-theoretic
minimum necessary to represent all the matrices of `n × n` with `m` 1s". Worked
for `n = 10⁶`, `m = 8·10⁶`: `log₂(n²/m) = log₂(1.25·10⁵) = 16.93`, so the bound is
about `2 · 16.93 = 33.9` bits per link — worse than the 32-bit CSR *if the graph
had no structure*. The point of the experiments is that real graphs are far
from the worst case.

The paper's other design choice: large `k` makes a shorter tree and faster
queries but wastes bits in `L` (a `k²`-cell leaf block holding one 1 spends
`k² − 1` zeros). The **hybrid** uses `k = 4` for the top levels and `k = 2`
near the leaves (§4).

### Step 5 — what it measured

> **In:** Steps 1–4. **Out:** the paper's numbers (§5, Table 3, Figure 3),
> labelled with what was compared.

Machine: a 2 GHz Xeon, 16 GB; times are "average cpu user time per neighbor
retrieved" over all pages in random order. Graphs are WebGraph-project crawls,
vertices in URL order (Table 3a): CNR 0.325 M pages / 3.2 M links, EU 0.862 M /
19.2 M, Indochina 7.4 M / 194 M, UK 18.5 M / 298 M links (1,208 MB as plain
4-byte lists).

Table 3(b), the EU graph, bits per edge (`bpe`) and µs per neighbour retrieved:

| variant | space (bpe) | direct (µs/e) | reverse (µs/e) |
|---|---|---|---|
| 2 × 2 | 5.21 | 2.56 | 2.47 |
| 3 × 3 | 6.02 | 1.78 | 1.71 |
| 4 × 4 | 7.22 | 1.47 | 1.42 |
| hybrid, k = 4 for 7 levels, then 2 | 5.24 | 2.10 | 1.96 |
| hybrid, k = 4 for 9 levels, then 2 | 5.73 | 1.79 | 1.67 |

Reading it: the hybrid at 7 levels is 18% faster than `2 × 2` (2.10 vs 2.56) for
0.6% more space (5.24 vs 5.21). Overall (abstract, §5.2): "3.3 to 5.3 bpe,
depending on the graph, 4.22 for graph UK" among techniques that answer both
direct and reverse queries, and 2–15 µs per neighbour. Table 3(f), against
Asano et al.'s "Smaller" on the small graphs, whole adjacency list per page:
CNR 2.34 ms against 0.048 ms, EU 28.72 ms against 0.099 ms, at comparable
space once Smaller is doubled for reverse (CNR 3.98 vs 4.46 bpe, EU 5.56 vs
5.21).

What was *not* compared: against WebGraph, direct neighbours only (Figure 3e),
the authors concede "alternatives RePair and WebGraph become preferable in most
cases" on time, with the k²-tree "still significantly less space than WebGraph"
on EU. The k²-tree's claim is about the **both-directions** case.

### Step 6 — extended functionality: tests, ranges, blocks

> **In:** Step 3's traversal. **Out:** what a matrix-shaped structure answers
> that a list-shaped one cannot (§6).

* **Is there a link `p → q`?** Descend one child per level: `O(log_k n)`, no
  listing. List-based formats must extract the neighbour list (or a prefix).
* **Neighbours of `p` within a range `[q1, q2]`:** enter only the children whose
  column range intersects it.
* **All links from a range of pages to another range** (for example all links
  between two domains): the general query, of which the others are special
  cases.

For a graph database these are existence checks, label-restricted expansions and
block extractions — the operations a masked `GrB_mxm` expresses. Whether the
tree could be the *storage* behind such masks is the paper's closing
generalisation ("binary relations … discrete grids of points"), and was not
evaluated there.

### Step 7 — what the vertex order does to a k²-tree

> **In:** Steps 1–5. **Out:** the dependence this topic tests, stated before
> the lane measures it.

A quadrant is pruned only if it is empty, so the tree is small when the 1s are
*clustered*: near the diagonal (locality) and in a few dense blocks (similar
rows). §2 says so directly — "Page identifiers are integers, which correspond
to their position in an array of alphabetically sorted URLs. This puts together
the pages of the same domains, and thus locality of reference translates into
closeness of page identifiers" — and nothing in the structure protects it from a
bad numbering: with random ids the 1s spread uniformly and the worst-case bound
of Step 4 (about 34 bits per link at `n = 10⁶`, `m = 8·10⁶`) is the right
expectation, not the 3.3–5.3 of the experiments. It is the same dependence as in
every other guide, here stated for a different structure; lane 3's contract does
not include a k²-tree, and no k²-tree number appears in this topic's measured
lanes (exercise 5 builds one).

## How to read the paper (with the concepts in hand)

§2 and Figure 1 give the construction; §3.1–3.3 the layout, Lemma 1 and
Figure 2's two procedures — implement `Direct` before reading on. §3.2's
"Analysis" gives the bounds, §5 and Figure 3 the measurements (the figures are
plots; the numbers quoted above are from Table 3 and the text). §6 is a page on
queries lists cannot answer.

## Where each step lives in this topic

| step | here |
|---|---|
| matrix view of a graph | `Csr`; GraphBLAS in [reading-graphblas-bytes.md](reading-graphblas-bytes.md) |
| the dependence on ids | lane 1 (ids) — every ordering row |
| a k²-tree to build | README exercise 5 |
| lists vs matrices | [reading-webgraph-framework.md](reading-webgraph-framework.md) |

## Questions (answer in notes.md)

1. Build the `k = 2` tree of the 4 × 4 matrix in Step 1 with row 1 set to `0 0 0 1`
   too. What are `T` and `L`, and what does `Direct` return for row 1?
2. For a matrix with `n = 2²⁰` and `m = 2²²` links in a single dense `2¹¹ × 2¹¹`
   block, how many bits does the tree need at `k = 2`, to first order?
3. Why can the k²-tree answer reverse neighbours with no second copy, while the
   list formats of the other guides need the transpose stored separately?
4. FalkorDB keeps a relation matrix and a transposed copy (see
   [reading-graphblas-bytes.md](reading-graphblas-bytes.md)). What would replacing the
   pair by one k²-tree change in the bytes per edge, in the time of a point
   lookup, and in the cost of one edge insert?

## Done when

Answer each before unfolding it.

- [ ] You can construct `T` and `L` for a small matrix and find a node's
  children.

  <details><summary>Answer</summary>

  Split recursively into k² quadrants, 1 if non-empty and 0 if empty (no
  further split); write levels in order, all but the last into T and the last
  into L. The children of the node at position x of T (a 1) start at
  rank(T, x)·k², where rank counts the 1s in T[0..x]. For the 4 × 4 example,
  T = 1001, L = 11001011, and the second 1 (position 3) has rank 2 so its
  children start at 8, which is L[4..7] = 1011.

  </details>

- [ ] You can run `Direct` and `Reverse` by hand and say why they are symmetric.

  <details><summary>Answer</summary>

  Direct enters the k children of a node that lie in one row of the child grid,
  Reverse the k that lie in one column; only the child-index formula differs
  (`k·⌊p/(n/k)⌋ + j` against `⌊q/(n/k)⌋ + j·k`). Row 3 of the example gives
  columns 2 and 3 by pruning the empty bottom-left quadrant with one bit.

  </details>

- [ ] You can state the worst-case space bound and its meaning for random
  graphs.

  <details><summary>Answer</summary>

  k²·m·(log_{k²}(n²/m) + O(1)) bits; for k = 2 that is 2m·log₂(n²/m) + O(m),
  twice the information-theoretic minimum. For n = 10⁶, m = 8·10⁶ it is about
  33.9 bits per link, so a k²-tree on a structureless graph is no better than
  32-bit CSR; the 3.3–5.3 bits of the experiments come from clustering.

  </details>

- [ ] You can say what the k²-tree is and is not better at than WebGraph.

  <details><summary>Answer</summary>

  It gives both direct and reverse neighbours from one structure in equal time,
  point link tests in O(log n), range and block queries, and the best space among
  both-direction structures in the paper (3.3–5.3 bpe; 4.22 on UK). Direct-only,
  the authors concede RePair and WebGraph are preferable on time in most
  cases (§5.2); on EU the k²-tree still used significantly less space, at
  1.5–2.6 µs per neighbour (Table 3b).

  </details>

## References

- Brisaboa, Ladra & Navarro, *k²-trees for Compact Web Graph Representation*,
  SPIRE 2009, LNCS 5721 — §2 (construction), §3 (layout, Lemma 1, Figure 2,
  analysis), §4 (hybrid), §5 (Table 3, Figure 3), §6.
- González, Grabowski, Mäkinen & Navarro, *Practical implementation of rank and
  select queries*, WEA 2005 — the rank structure; not read.
- Brisaboa, Ladra & Navarro, *Compact Representation of Web Graphs with Extended
  Functionality*, Information Systems 2014 — the journal version; abstract only.
- [reading-webgraph-framework.md](reading-webgraph-framework.md) — the list-based
  alternative.
