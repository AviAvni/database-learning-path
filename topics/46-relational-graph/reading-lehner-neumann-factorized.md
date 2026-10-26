# The Data World Is Not Flat: factorized joins inside a compiling relational engine

Lehner & Neumann (TUM, VLDB'26) ask the question this topic is built around:
if graph patterns are joins, why not run them inside a relational engine — and
what has to change in that engine so a many-to-many join stops multiplying its
input? Their answer is **factorized execution** done the Umbra way: the shape
of every intermediate result is fixed at compile time and turned into
generated C++ structs, the join gains a second, mirror-image insertion mode
(the **bottom insert**), and parallelism and counting are rebuilt so they work
on nested results rather than flat rows. This chapter builds the vocabulary
from zero, works every formula the paper uses on its own example, and then
reads the evaluation — including the parts where factorization loses, and two
headline numbers in the abstract that the evaluation section does not
reconstruct.

Everything quoted is from the PVLDB 19(11) version, pages 3006–3019. Lane 1
of this topic's bench measures the same effect on a seeded graph:
[README](README.md#the-problem-measured-bench-lane-1-provided--runs-today).

## The problem in one sentence

When every vertex joins with many others, a flat join result repeats the same
values over and over — 2,272,724,241 tuples for a 3-hop pattern on a
313,512-edge graph in lane 1 — so how can a relational engine keep the
*information* of that result while storing and processing only the
47,997,149 values that are not repeats?

## The concepts, step by step

### Step 1 — the many-to-many join and why its result explodes

> **In:** nothing yet — this step is the motivation.
> **Out:** the term *m:n join* and the quantity the rest of the paper attacks,
> the *duplication* inside a flat join result.

A **join** combines rows of two tables that agree on a **join key**. A
**foreign-key join** (1:n) pairs each row with at most one partner on one
side — every order has one customer — so its result is no bigger than the
larger input. A **many-to-many join** (written **m:n**) has repeated keys on
*both* sides, and its result for one key value is the **Cartesian product**
(every pairing) of the matching rows. A graph's edge table joined with itself
on "this edge's destination = that edge's source" is m:n by construction:
a vertex with 1,000 in-edges and 1,000 out-edges contributes 1,000 × 1,000 =
1,000,000 two-hop paths.

The paper's Figure 1 shows the effect on a bike catalogue. Joining frame
materials {Carbon, Aluminum} with colours {Black, White} for one model `r1`
gives 2 × 2 = **4** intermediate tuples; joining those with gear options
{105, Force} gives 4 × 2 = **8**, and `r1` appears in all eight. The
**duplication** of a value is how many times it is repeated across the result;
here `r1` is duplicated 8 times and every material 4 times. Relational engines
"are optimized for selective joins … where tuples usually join with at most one
other tuple" (§1), which is the assumption graph workloads break. The paper
cites this as the diamond problem (§1, reference [9]).

Why it matters: an operator above the join pays per tuple, so it re-evaluates
`r1` eight times when once would do. The question is how to stop repeating.

### Step 2 — f-representations: write the result as unions of products

> **In:** a flat join result, from Step 1.
> **Out:** an *f-representation* of it and its size in values — the number
> every later step tries to shrink.

An **f-representation** (factorized representation) writes a set of tuples
as an algebraic expression built from three things (§2.1): **atomic values**
(`a1`, `b1`, …), **Cartesian products** `×` that glue values into tuples, and
**unions** `∪` that list alternatives. Its **size** is the number of atomic
values it contains.

Figure 2 works it on four tuples over columns A, B, C, D:

```text
Figure 2a, flat:            a1 b1 c1 d1      a1 b1 c2 d1      a1 b1 c1 d2      a1 b1 c2 d2
as products and a union:    (a1×b1×c1×d1) ∪ (a1×b1×c2×d1) ∪ (a1×b1×c1×d2) ∪ (a1×b1×c2×d2)
                                                                       → 16 values
factor out the shared prefix a1×b1 (Figure 2c):
                            a1 × b1 × ((c1×d1) ∪ (c2×d1) ∪ (c1×d2) ∪ (c2×d2))
                                                                       → 2 + 8 = 10 values
the four suffixes are all pairs of {c1,c2} and {d1,d2} (Figure 2d):
                            a1 × b1 × (c1 ∪ c2) × (d1 ∪ d2)
                                                                       → 2 + 2 + 2 = 6 values
```

The rule used at each step is **distributivity**, the same algebra as
`x·y + x·z = x·(y + z)` with `×` for multiplication and `∪` for addition.
The paper's §2.2 then states the payoff: storing 6 values instead of 16
"reduces the memory requirements for materialization by 62.5%". Checking the
arithmetic: 6 / 16 = 0.375, and 1 − 0.375 = 0.625. ✓

Two properties make this safe to build an engine on. It is **lossless** —
multiplying the expression back out gives exactly the original tuples. And it
is never worse than flat: "it guarantees not to exceed the flat redundancy"
(§1), because the flat table is itself a (trivial) f-representation.

Why it matters: the size of the f-representation, not the number of tuples,
is what an engine that keeps results factorized has to store and touch.

### Step 3 — f-trees: one shape for every tuple, and the path constraint

> **In:** f-representations from Step 2.
> **Out:** the *f-tree* that fixes their shape, and the *path constraint* that
> says which shapes are legal.

An **f-tree** is the shape shared by every f-representation of a result: one
node per attribute (or, in this paper, per base-table row group — §1 calls it
table-level factorization), where a child node's values are stored once per
value of its parent and **siblings** are independent of each other given the
parent (§2.2). Figure 3's f-tree has A at the root, B and D as A's children, and
C under B. Read it as: for each `a`, a list of `b`s and, separately, a list of
`d`s; for each `(a, b)`, a list of `c`s.

Figure 3's running example has ten flat tuples:

```text
a1: B = {b1: C = {c1, c2},  b2: C = {c1}}      D = {d1, d2}
a2: B = {b3: C = {c3}}                          D = {d3}
a3: B = {b4: C = {c4, c5, c6}}                  D = {d4}
```

Two attributes are **dependent** if a predicate (a join condition or a filter)
links them. The **path constraint** (§2.2, citing FDB) says dependent
attributes must lie on one root-to-leaf path of the f-tree: if the query joins
on `A.a = B.b`, B must be A's descendant (or the other way round), because a
child is stored *per parent value* and that is exactly how the join's matches
are grouped. Siblings — B and D here — are stored independently, which is only
correct if nothing in the query relates them.

Why it matters: the f-tree is a compile-time object. Once it is fixed, the
code that builds, scans and counts a result can be generated for exactly that
shape — Step 6.

### Step 4 — counting without flattening

> **In:** an f-representation over a known f-tree (Step 3).
> **Out:** the number of flat tuples it represents, computed from subtree
> sizes; this is how `count(*)` runs in §4.5 and how lane 1 counts.

To **flatten** is to multiply an f-representation back out into flat tuples.
`count(*)` does not need the tuples, only how many there are, and
distributivity gives that directly. §4.5's formula for Figure 3:

```text
#flatTuples = Σ_{a ∈ A}  ( Σ_{b ∈ B_a} |C_{a,b}| ) · |D_a|
```

where `B_a` is the list of `b`s under `a`, `C_{a,b}` the list of `c`s under
`(a, b)`, `D_a` the list of `d`s under `a`, and `|·|` a list's length. Siblings
multiply (every `c` pairs with every `d`), children of one node add.

Worked on Figure 3:

```text
a1:  (|C_{a1,b1}| + |C_{a1,b2}|) · |D_{a1}| = (2 + 1) · 2 = 6
a2:  (|C_{a2,b3}|)               · |D_{a2}| = 1 · 1       = 1
a3:  (|C_{a3,b4}|)               · |D_{a3}| = 3 · 1       = 3
                                                     total = 10
```

Ten, the number of rows in Figure 3's flat table — and the paper's own load
balancing example agrees: "one thread has to handle six tuples (a1), whereas
another processes only one tuple (a2)" (§5.1).

This topic's lane 1 uses the same identity for paths. For the 2-hop pattern
grouped by the middle vertex `b`, `a` and `c` are siblings, so the count is
`Σ_b in(b) · out(b)` — `in(b)` is how many edges enter `b`, `out(b)` how many
leave it. The quoted code is this topic's crate:

```rust
// topics/46-relational-graph/experiments/src/fsize.rs — lines 38–48
38  pub fn two_hop(g: &Graph) -> FSize {
39      let (mut tuples, mut fact) = (0u64, 0u64);
40      for b in 0..g.n() as u32 {
41          let (i, o) = (g.inc.degree(b), g.out.degree(b));
42          if i > 0 && o > 0 {
43              tuples += i * o;                 // siblings multiply: the count
44              fact += 1 + i + o;               // values stored: b once, then each list
45          }
46      }
47      FSize { flat_tuples: tuples, flat_values: 3 * tuples, fact_values: fact }
48  }
```

The lines to compare are 43 and 44: the flat result grows with the *product*
of the two degrees, the factorized one with their *sum*.

Why it matters: an aggregate over a factorized result costs time proportional
to its size, not to the number of tuples — lane 1's timing table puts the two
side by side for the same count (see the README's lane 1 output).

### Step 5 — top inserts and bottom inserts: two ways to nest a join

> **In:** two f-trees about to be joined (each input of a hash join is an
> f-representation, Step 3).
> **Out:** the two insertion modes and the identity that makes them
> interchangeable, `L ⋈_TI R ≡ R ⋈_BI L`.

A **hash join** builds a hash table on one input (the **build side**) and
looks up each row of the other (the **probe side**). A factorized join has to
decide *where in the output f-tree* each side goes, because the side placed
higher is stored once per value and the side placed lower once per match
(§4.2.3):

- **Top insert (TI):** the probe side becomes the upper part of the output
  tree, and the set of build-side matches for each probe tuple is hung under
  it. This is how an ordinary hash join already thinks — "the engine remembers
  … that there is this set of matching values" — and prior systems implement
  it (§4.2.3 cites [16, 22]).
- **Bottom insert (BI):** the build side becomes the upper part, and probe
  tuples are inserted *below* the build-side node they matched. The paper
  says "to our knowledge, we are the first to propose bottom-insert joins"
  (§4.2.3).

Since the two differ only in which input ends up on top, §4.2.3 derives

```text
L ⋈_TI R  ≡  R ⋈_BI L
```

— a top insert can be rewritten as a bottom insert by swapping build and probe.
Why have both, then? Cost. A hash table should be built on the *smaller*
input (§4.2.3, citing [10]). Take a small table `B` with unique keys and a
large table `P` with many duplicates. With a top insert, the probe side `P`
becomes the upper tree, each `P` row matches at most one `B` row, and nothing
is factorized. To get the shared `B` value on top you would have to swap
sides and build the hash table on the large `P` — expensive. A bottom insert
keeps the hash table on `B` and still puts `B` on top: "the natural choice is
a bottom-insert" (§4.2.3).

The price is pipelining. A bottom insert cannot know a build-side node is
complete until every probe row has arrived, so "we implement bottom-inserts
as full pipeline breakers, which materialize the joined tuples immediately"
(§4.4). A **pipeline breaker** is an operator that must consume all its input
before emitting anything.

Why it matters: the identity decouples join *order* (which table to build on)
from f-tree *shape* (which attribute sits on top). The paper's optimizer can
enumerate join orders with top inserts only and decide insert positions in a
second pass (§4.2.3) — though §5.5 says the optimizer is "left for future
work" and the evaluation uses **true cardinalities** (exact row counts, not
estimates) for every decision.

### Step 6 — compiled f-trees: why the tree costs less when the code knows its shape

> **In:** an f-tree fixed at compile time (Step 3).
> **Out:** the generated container layout of Figure 5, and the inlining
> optimization of §5.3.

A **code-generating** (compiling) engine such as Umbra emits machine code
specialised to one query instead of interpreting a generic operator tree.
§4.1 names two costs of tree-shaped results: indirections (pointer chasing,
"high memory fragmentation and suboptimal prefetching"), and data-dependent
shape — if child counts are discovered at run time, "the child counts appear
for the CPU almost at random", so branch prediction fails. Code generation
removes the second entirely: "we do this once and embed the structure into
the executed code".

Figure 5 shows the generated container for Figure 3's f-tree (reproduced here
as the paper prints it, its own line numbers in the gutter):

```text
Figure 5 (Lehner & Neumann, §4.1)
 1   struct NodeD { D d; };
 2   struct NodeC { C c; };
 3   struct NodeB {
 4      B b;               NodeC frontC;
 5      vector<NodeC> cs;
 6   };                    vec<NodeC> tailC;
 7   struct NodeA {
 8      A a;                                  NodeD frontD;
 9      vector<NodeD> ds; NodeB frontB;
10      vector<NodeB> bs; vec<NodeC> tailD;
11   };                                       vec<NodeB> tailB;
12   struct FRep {
13      vector<NodeA> root;
14   };
```

Line 9–10 is the point: `NodeA` holds *two* separate lists, `ds` and `bs`,
because B and D are independent siblings — one fixed struct, one level of
indirection per child list. The right-hand column is §5.3's **inlining**: the
first child of each list (`frontC`, `frontD`, `frontB`) is stored inside the
parent, and only the rest goes to the `tail` vector. When a tuple joins with
exactly one other tuple — the foreign-key case — no indirection happens at
all, and "the storage layout resembles a traditional flat, row-based format
and can be interpreted as adaptive factorization" (§5.3).

Why it matters: this is the paper's answer to "factorization only pays off
under high duplication" (§1) — make the non-duplicated case as cheap as flat.
Step 8 shows how far that goes.

### Step 7 — parallel enumeration: give every flat tuple an ID

> **In:** an f-representation to be enumerated by many threads (Step 4's
> sizes).
> **Out:** the ID scheme of §5.1 and its worked example.

**Morsel-driven parallelism** (Leis et al., cited as [26]) splits input into
small chunks ("morsels") that worker threads take from a queue. Splitting a
factorized result by root node does not balance: in Figure 3, `a1` holds six
tuples and `a2` one (§5.1). The paper instead numbers every tuple that the
iterator *would* produce in a sequential pass, and hands out ranges of these
**IDs**. Finding where ID `k` lives takes a descent: at each node, scale child
subtree sizes by the product of the later siblings' sizes, and pick the child
whose range contains `k`.

The paper's worked example finds ID 2 when the pipeline needs only B and D
(C is not required, so `b1` and `b2` count once each):

```text
root a1 holds |B_a1| · |D_a1| = 2 · 2 = 4 tuples (IDs 0–3)   → ID 2 is inside a1
inside a1, each b is scaled by |D_a1| = 2:  b1 → IDs 0–1,  b2 → IDs 2–3   → b2
inside b2, D is the last child, no scaling:  d1 → ID 2,    d2 → ID 3      → d1
ID 2 = a1 b2 × {c1} × d1
```

For the root list the subtree sizes are prefix-summed once, so the start of a
chunk is found "in O(log n) using binary search" (§5.1); inner nodes are small
and searched linearly. Materialising *into* a factorized result in parallel
needs a lock per inner node and thread-local root lists (§5.2), accepted
because "a factorized tuple can represent multiple flat tuples".

Why it matters: a factorized engine must stay a parallel engine. The ablation
in Step 8 credits parallel execution with an average 18× improvement.

### Step 8 — the evaluation, including where factorization loses

> **In:** the engine of Steps 3–7.
> **Out:** the measured results (§6) with their caveats, and the two abstract
> claims the evaluation does not reconstruct.

Setup (§6): Intel Xeon Gold 6430, 64 cores, 256 GB; generated C++ compiled
with GCC 14.2.0 `-O3`; 5 warm-up runs, mean of 10; compilation time excluded;
true cardinalities for both the factorized engine and the flat baseline built
on the same infrastructure; comparisons with DuckDB 1.4.2 and Umbra.

**Microbenchmarks (§6.1, Figure 13).** Three-relation left-deep joins, 100,000
rows per relation (uniform) or 10,000 (Zipfian). Bottom inserts are 1.9× more
expensive than top inserts because of the extra materialization, "but they can
still outperform our flat implementation by over 13ms (1.4×)". The
**break-even** points — the duplication at which factorization stops losing —
are the useful numbers: top inserts match flat at 10 duplicates per key on
average and win from 12; bottom inserts win from 36. Below that, the tree costs
more than it saves. (The figure is log-scale; these are the prose numbers.)

**CE benchmark (§6.2).** 2,557 graph-pattern queries. Over *all* acyclic
queries the factorized engine is a **15% geometric-mean slowdown** against
its own flat baseline, "mainly driven by short-running queries". Restricted to
the 895 acyclic queries whose flat run takes over 10 ms it is a 6.9% speedup,
over 100 ms a 52% speedup, and 10 queries gain more than an order of
magnitude. Cyclic queries are **32% slower** (the f-tree must be flattened to
close a cycle), with 35 queries faster by up to 3.3×. Against Umbra: a 58%
geometric-mean slowdown overall, 294 acyclic queries faster, Umbra "up to 16×
longer" on some. Against DuckDB: faster on over 97% of acyclic queries. The
improved root-to-leaf transformation of §4.3 costs a 2.4% geometric-mean
slowdown over the naïve one, with speedups above 6.2× where it applies.

**SSB (§6.3).** Scale factor 10, queries cut down to joins plus a count
(group-by and order-by dropped). Slightly better than the flat baseline,
slightly worse than Umbra, comparable to DuckDB.

**Ablation (§6.4, Figure 15).** Bottom inserts were used in 49% of the queries
and 2,091 joins; 46 queries used only bottom inserts; caching subtree counts
helps by 30% where d-representations branch heavily; parallel execution
"improves runtime by 18×" on average; single-threaded left-deep factorized
plans already beat the *parallel* bushy flat engine on 185 queries.

**What does not reconstruct.** The abstract and §1 claim "speedups of over
25× and outperforms traditional RDBMSs on over 19% of benchmarked CE queries".
Neither number appears in §6's prose or in a table. "Over 25×" can only be
read off the log-scale axis of Figure 15. And 19% of 2,557 is 486 queries,
which matches none of §6.2's counts: 294 acyclic queries faster than Umbra is
11.5%, 559 faster than Umbra with lookup-expand joins is 21.9%, and the DuckDB
figure is a percentage of acyclic queries only. This guide does not quote
either abstract number as a result.

Why it matters: the honest reading is narrower than the abstract. Factorization
inside a relational engine wins on long-running m:n queries and loses a little
almost everywhere else — which is why §5.3's inlining and §6.4's "use the naïve
transformation unless the factorized attributes are used later"
recommendation exist.

### Step 9 — where this lands in a graph engine

> **In:** Steps 2–8.
> **Out:** the mapping from the paper's mechanisms to this topic's lanes and
> to a Cypher engine.

- Lane 1 measures Step 2's quantity — the f-representation size — on a
  power-law graph, with Step 4's counting identity as the third way to get the
  count. Lane 2's [`extend`](experiments/src/factorized.rs) is a bottom insert
  (Step 5): the third edge relation goes *under* the `c` level, so `a` is not
  repeated per `d`.
- A graph engine's `Expand` already behaves like a top insert into an
  adjacency list — §7 says GES "applies factorization in its Expand operator"
  and "currently supports only top-inserts". What a CSR-backed engine lacks is
  the second mode and the f-tree as a compile-time object.
- The m:n join is not only a graph phenomenon: §6.3 finds factorization helps
  slightly even on the star-schema benchmark, because "foreign key joins can
  also include some duplication".

## How to read the paper (with the concepts in hand)

1. **§1** and **Figure 1** for the problem; skip the related-work paragraph on
   first pass — §7 repeats it with more detail.
2. **§2** is Steps 2–3. Redo Figure 2's transformations on paper before reading
   §2.2's 62.5%.
3. **§3** is a narrated plan (Figure 4): read it after §4.2, not before — the
   numbered circles only make sense once top and bottom inserts are defined.
4. **§4.1** (containers, Figure 5), **§4.2** (TI/BI and the identity),
   **§4.4** (Figures 9–11, the generated probe code), **§4.5** (counting).
   §4.3's root-to-leaf merging (Figures 7–8) can wait for a second pass.
5. **§5.1–5.3**: parallel IDs, synchronization, inlining.
6. **§6** with Step 8 beside you; treat log-scale figures as qualitative.

## Where each step lives in the code

The paper's engine is published at `github.com/lehners/FactDB` (artifact
statement on page 1); it is not cloned for this topic. The steps map onto this
topic's crate instead:

| step | file | what to look at |
|---|---|---|
| 2, 4 | `experiments/src/fsize.rs:38–48`, `:72–93` | f-representation size and count from degrees, 2-hop and 3-hop |
| 3 | `experiments/src/fsize.rs:52–70` | the deep f-tree `a → b → c` (no sibling branching) |
| 5 | `experiments/src/factorized.rs:174` | `extend` — your bottom insert (lane 2) |
| 4 | `experiments/src/factorized.rs:154` | `ThreeHop::count` — counting without flattening (lane 2) |

## Questions (answer in notes.md)

1. Rewrite Figure 3's f-representation with D as B's child instead of A's.
   Does the size change? Does the count? Which predicate would force this
   shape?
2. Lane 1's 2-hop row at |V| = 40,000 stores 656,622 values against
   47,900,847 tuples. Using Step 4's identity, which single vertex property
   would you look at to predict the ratio, and why does the uniform control
   row show only 11×?
3. The break-even for top inserts is 10–12 duplicates per key (§6.1). What is
   the mean number of 2-hop paths per middle vertex in lane 1's 40,000-vertex
   graph, and is it above or below that break-even? Is the mean the right
   statistic for a power-law graph?
4. Why must a bottom insert be a pipeline breaker, and why does §4.4 say it
   could be a "half-pipeline breaker" when the flat columns are a subset of
   what later operators need?
5. §6.2 says cyclic queries are 32% slower because the tree must be merged to
   close a cycle. Which lane 1 pattern would become cyclic if you added one
   edge atom, and what would the path constraint force the f-tree to become?

## Done when

Answer each before unfolding it.

- [ ] You can turn Figure 2a into Figure 2d step by step and say how many values each step stores.

  <details><summary>Answer</summary>

  Flat, four tuples of four values: 16. Factoring out the shared prefix
  `a1 × b1` leaves `a1 × b1 × (four 2-value suffixes)`: 2 + 8 = 10. The four
  suffixes are exactly the pairs of `{c1, c2}` with `{d1, d2}`, so they become
  `(c1 ∪ c2) × (d1 ∪ d2)`: 2 + 2 + 2 = 6. The rule each time is distributivity
  of `×` over `∪`. 6 / 16 = 0.375, the 62.5% saving §2.2 reports.

  </details>

- [ ] You can count Figure 3's tuples from its f-representation without listing them, and say why siblings multiply.

  <details><summary>Answer</summary>

  `Σ_a (Σ_{b ∈ B_a} |C_{a,b}|) · |D_a|` (§4.5): a1 gives (2 + 1)·2 = 6, a2
  gives 1·1 = 1, a3 gives 3·1 = 3, total 10. Siblings (B's subtree and D under
  the same `a`) are independent given `a`, so every combination of a C-value
  path and a D-value is a tuple — a product. Children of one node are
  alternatives, so they add.

  </details>

- [ ] You can state the top-insert/bottom-insert identity and give the case where a bottom insert is the only cheap way to get a good f-tree.

  <details><summary>Answer</summary>

  `L ⋈_TI R ≡ R ⋈_BI L` (§4.2.3): they differ only in which input becomes the
  upper part of the output tree. With a small, unique-keyed `B` and a large,
  duplicate-heavy `P`, you want `B` on top (stored once) and the hash table on
  `B` (the small side). A top insert puts the *probe* side on top, so it would
  need the hash table on `P`; a bottom insert keeps the table on `B` and still
  puts `B` on top. The price is that a bottom insert is a full pipeline breaker
  (§4.4).

  </details>

- [ ] You can say what code generation buys a factorized engine that an interpreter cannot get.

  <details><summary>Answer</summary>

  The f-tree's shape is fixed when the query is compiled, so the generated
  code has one struct per f-tree node (Figure 5) and no run-time dispatch on
  "how many children does this node have" — the source of the
  near-random branches §4.1 describes. It also allows one level of indirection
  per child list, and §5.3's inlining of the first child, which makes the
  foreign-key case look like a flat row. The paper notes the techniques "are
  not inherent to code generation" (§4) — an interpreter can use the same
  containers but pays the dispatch.

  </details>

- [ ] You can give the evaluation's result in one honest sentence, including a case where factorization loses.

  <details><summary>Answer</summary>

  On the CE benchmark the factorized engine is 15% slower (geometric mean)
  than its own flat baseline over all acyclic queries and 32% slower on cyclic
  ones, but 6.9% faster on acyclic queries over 10 ms, 52% faster over 100 ms,
  and over 10× faster on 10 queries (§6.2); microbenchmarks put the break-even
  at 10–12 duplicates per key for top inserts and 36 for bottom inserts (§6.1).

  </details>

- [ ] You can explain why this guide does not repeat the abstract's "over 25×" and "over 19%".

  <details><summary>Answer</summary>

  Neither appears in §6's prose or in a table. "Over 25×" is visible only on
  Figure 15's log-scale axis. 19% of 2,557 CE queries is 486, which matches no
  count in §6.2 — 294 acyclic queries beat Umbra (11.5%), 559 beat Umbra with
  lookup-expand joins (21.9%), and the DuckDB figure (over 97%) is of acyclic
  queries only. A number that cannot be traced to a section or table does not
  go into this repo.

  </details>

## References

- Stefan Lehner, Thomas Neumann, **"The Data World Is Not Flat: Efficient
  Factorized Execution for Relational Systems"**, PVLDB 19(11): 3006–3019, 2026,
  [pdf](https://www.vldb.org/pvldb/vol19/p3006-lehner.pdf). §1 + Figure 1
  (problem), §2.1–2.2 (f-representations, f-trees, path constraint), §4.1
  (Figure 5), §4.2.3 (top/bottom insert and the identity), §4.4 (Figures 9–11),
  §4.5 (counting), §4.7 (d-representations), §5.1–5.3 (parallel IDs,
  synchronization, inlining), §5.5 (true cardinalities), §6.1–6.4, §7.
- Dan Olteanu, Jakub Závodný, **"Size Bounds for Factorised Representations
  of Query Results"**, ACM TODS 40(1), 2015 — the theory the paper cites as [36].
- Nurzhan Bakibayev, Dan Olteanu, Jakub Závodný, **"FDB: A Query Engine for
  Factorised Relational Databases"**, PVLDB 5(11), 2012 — the path constraint.
- Next chapter: [reading-ffx-factorized-vectorized.md](reading-ffx-factorized-vectorized.md),
  the same idea in a vectorized engine.
- Topic 11's [README](../11-execution-models/README.md) for compiled vs
  vectorized execution, and its "Newer work" entry on FFX.
