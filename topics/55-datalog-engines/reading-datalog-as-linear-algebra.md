# Datalog as linear algebra: the closure as a masked sparse matrix product

A Datalog rule is a join and a projection; over the boolean semiring a join
followed by a projection *is* a matrix product. This chapter builds that
dictionary, reads the one paper that turned it into an evaluation strategy
(Sato's dense, equation-solving one), and then takes the sparse, iterative
form — the one a GraphBLAS engine like FalkorDB's would run — and measures
where it wins over a hash-indexed semi-naive loop and where it loses. The
other chapters read engines; this one reads a *formulation* and the
data-structure choice hiding inside it.

Two earlier chapters supply the pieces. Topic 20's
[reading-lagraph.md](../20-graphblas/reading-lagraph.md) Steps 1–2 explain
`GrB_vxm`, masks and the `GrB_DESC_RSC` descriptor on a BFS level; this
chapter's loop is that BFS with the frontier promoted from a vector to a
matrix. And [reading-recstep.md](reading-recstep.md) Step 5 reads RecStep's
bit-matrix evaluation, which is the same computation with the loop order
inverted.

The paper is **"A Linear Algebraic Approach to Datalog Evaluation"** —
Taisuke Sato, *Theory and Practice of Logic Programming* (to appear at the
time of the arXiv v2, 24 Feb 2017;
[arXiv 1608.00139](https://arxiv.org/abs/1608.00139)). The sparse, masked
formulation in Steps 3–5 is not from that paper; it is what lane 3 of
`datalog_bench` implements, and its numbers are this repository's.

## The problem in one sentence

If a recursive rule is a matrix product and "not already derived" is a mask,
the whole fixpoint is a loop of one GraphBLAS call per round — so what does
that formulation do to the number of derivations, and what does it do to the
cost of each one?

## The concepts, step by step

### Step 1 — the dictionary

> **In:** the transitive-closure rule and a 4-vertex example. **Out:** the
> correspondence between Datalog operations and boolean-matrix operations,
> worked on numbers.

A binary relation over vertices 0..n−1 is an **n × n boolean matrix**: entry
(i, j) is 1 when the tuple (i, j) is present. The **boolean semiring** has
`or` for addition and `and` for multiplication, so the matrix product
`(P·Q)[i,j] = or over k of (P[i,k] and Q[k,j])` says "there is some k with
P(i,k) and Q(k,j)" — exactly the body `p(i,k), q(k,j)` with the join
variable k projected away. The dictionary:

| Datalog | boolean matrices |
|---|---|
| relation `r(x,y)` | matrix `R`, `R[x,y] = 1` iff `r(x,y)` |
| join on a shared variable, then project it out | matrix product `P·Q` over (or, and) |
| union of two rules with the same head | elementwise `or` |
| head already known: drop the tuple | mask: write only where the output matrix is empty (**complement mask**) |
| delta of semi-naive evaluation | the frontier matrix `D` |
| fixpoint | `D` has no entries |

Worked on the path 0 → 1 → 2 → 3, `A` has entries (0,1), (1,2), (2,3). The
closure by the loop `D' = (D·A)⟨¬T⟩; T = T ∨ D'` with `T = D = A`:

```
round 1:  D·A = {(0,2), (1,3)}            mask ¬T keeps both    →  D' = {(0,2),(1,3)},  T has 5 entries
round 2:  D·A = {(0,3)}                   (0,2)·(2,3)           →  D' = {(0,3)},        T has 6 entries
round 3:  D·A = {}                        no entry in D has an out-edge  → stop
```

Six tuples — the closure of a 4-vertex path (4·3/2) — in 3 rounds, the last
empty. Same accounting as lane 1: the multiply-accumulates attempted
(here 2 + 1 + 0 = 3) are the **derivations** of the recursive rule, and the
base rule's |arc| = 3 more are not counted by the matrix loop (the test
`flops + |arc| = lane 1's semi-naive derivations` in `src/algebra.rs` pins
exactly this).

### Step 2 — Sato: solve the equation instead of iterating

> **In:** the same dictionary. **Out:** the paper's idea, what it measured,
> and what that does and does not say about sparse semi-naive evaluation.

Sato's paper (§3–§4) takes a *linear* Datalog program, encodes each
relation as an adjacency matrix, and rewrites the program's if-and-only-if
completion as a **matrix equation**. For transitive closure that is
`R₂ = min₁(R₁ + R₁R₂)` (its equation (8), as §7.1 restates it), where `R₁` is the arc matrix, `R₂` the closure and
`min₁` clamps entries to 1. Instead of iterating it, the paper solves the
*linearized* equation directly: `trcl(R₁) = (I − εR₁)⁻¹ εR₁` with ε a
small constant derived from the row-sum norm ‖R₁‖∞ so that the series
converges, then thresholds entries at 0 (§7.1). The cost is a dense n × n matrix inverse, O(N³)
(abstract: "O(N³) time complexity irrespective of the number of variables in
a clause").

What it measured (§7, Octave 4.0 on an i7-3770, N = 1,000 constants,
random graphs with edge probability p_e; Tables 1–3):

```
closure, N = 1,000 (Table 2, seconds)
p_e        Matrix    B-Prolog     XSB      DLV       Clingo
0.001      0.094     0.004        0.003    0.293     0.038
0.01       0.117     2.520        1.746    10.657    14.618
0.1        0.105     18.382       16.296   75.544    125.993
1.0        0.100     188.280      137.903  483.380   1,073.301
```

The matrix method takes about 0.1 s whatever the density; the symbolic
systems scale with the number of tuples. "For p_e ≤ 0.001, Matrix runs
slower than the symbolic systems but for p_e ≥ 0.01, it overwhelms them, runs
15 ~ 10⁴ times faster" (§7.2); the same-generation program (Table 3) behaves
alike (Matrix 8.5–10.5 s for every p_e; B-Prolog 1,379 s at p_e = 0.01).
Against its own **iteration** baseline `R₂ ← min₁(R₁ + R₁R₂)`, Matrix is
faster on every real graph of Table 1 — for instance subelj-cora, N = 23,166,
17,893.7 s against 497.2 s (36×) and ego-twitter 5,481.9 s against 654.9 s
(8.4×).

Read that for what it is. The comparison is **dense linear algebra against
tuple-at-a-time symbolic engines** on graphs dense enough that the closure
is nearly the whole matrix, and the baseline "iteration" is a *dense*
product loop, not semi-naive evaluation. The paper contains no semi-naive
sparse evaluation, so it cannot say how the algebra fares against one — and
its own crossover (algebra loses at p_e ≤ 0.001) is the warning that the
interesting regime for graph databases, sparse graphs, is where the dense
formulation does not pay. The sparse iteration of the next steps is the
formulation that stays competitive there.

### Step 3 — the sparse form: multi-source BFS with a matrix frontier

> **In:** Step 1's loop. **Out:** why it is a BFS, which GraphBLAS call
> computes it, and the identity between its work and semi-naive's.

Run Step 1's loop on one row of `T` and it is a breadth-first search from
one source: the frontier `D[i,:]` is the set of vertices first reached last
round, `(D·A)[i,:]` is their out-neighbours, and the complement mask drops
the already-visited. With every row at once, `D` is the frontier of **n BFS
searches advanced in lockstep** and `T` is n visited sets. In GraphBLAS the
round is one call: `GrB_mxm(D', T, NULL, ANY_PAIR, D, A, GrB_DESC_RSC)` —
mask `T`, replace the output, use the mask's structure, complement it
(topic 20's four dials; `ANY_PAIR`/`LAGraph_any_one_bool` because only
structure matters). The vector form is LAGraph's BFS level at
`LG_BreadthFirstSearch_SSGrB_template.c:307`.

The identity: **the masked product attempts exactly the (frontier entry,
arc) pairs semi-naive evaluation enumerates.** Each closure tuple enters
`D` once — in the round it is first derived — and fires once per out-edge of
its target, so total flops = Σ over closure tuples (x,z) of outdeg(z).
Lane 1's power-law graph: 137,482 semi-naive derivations = 3,489 base + the
matrix loop's **133,993** flops (`datalog_bench`, lane 3, `flops` column);
grid 165,600 = 1,104 + 164,496; chain 124,750 = 499 + 124,251. The mask
changes what is *written*, never what is *multiplied* — so **the algebra form
cannot reduce the derivation count below semi-naive's**, and any speed
difference between lanes 1 and 3 is a difference in the cost of one
derivation, not in the number of them. That is the honest framing for the
next step.

### Step 4 — what one derivation costs in each formulation

> **In:** Step 3's identity. **Out:** the per-derivation costs, the three
> structures that differ, and the three places the matrix form pays more.

Per derivation lane 1 does: a hash probe on `tc` (`contains`), and for a
survivor an insert into the round's `seen` set and later into `tc` and
`all`. Lane 3 does: a bit test in `T` (`BitMatrix::get`, one cache line per
row segment), a stamp comparison in a per-row scratch array to skip
within-row duplicates, and for a survivor a push to a short list and later
one `set`. Three differences carry the speed:

1. **Membership is a bit, not a hash lookup**: an n²-bit matrix is 281 KB
   for n = 1,500, resident in L2, against a hash set of 65,289 entries.
2. **Adjacency is CSR rows**, contiguous, against an `FxMap<u32, Vec<u32>>`
   for `arc` in lane 1's nonlinear variants (the linear program's `arc` is a
   CSR in both, so this one is not a difference for TC-linear).
3. **No per-tuple allocation**: delta rows are appended to one `col` array.

Three places the matrix form costs more, all visible in the reference
numbers (notes.md): **(a) the mask is n² bits whatever the closure size** —
the `6000 chains × 10` dataset has n = 60,000 (450 MB of mask, mostly
empty) for a 270,000-tuple closure, and lane 3 loses to hash there;
**(b) two products per round for a three-atom rule**: same-generation is
`(Aᵀ·D)·A`, and the intermediate `Aᵀ·D` is materialized — the
pairwise-plan intermediate of
[reading-wcoj-gpu-distributed.md](reading-wcoj-gpu-distributed.md) Step 1 in
miniature — so on the power-law graph lane 3 attempts **85,124**
multiply-accumulates (base product included) against lane 1's **64,787**
nested-loop derivations; **(c) per-round fixed costs** — a long chain has
499 rounds of a handful of entries each, and every round pays the call
overhead; GraphBLAS libraries amortize it with hypersparse formats, the
pure-Rust reference here by iterating only non-empty rows.

### Step 5 — per source versus per round: PBME again

> **In:** Steps 3–4. **Out:** the second loop order and what it does and
> does not change.

[RecStep's PBME](reading-recstep.md) runs the same bit test with the loops
nested the other way: for each *row* i run its BFS to completion (Algorithm
2), instead of for each *round* advance every row. Derivations are identical
(every frontier entry, every out-edge). What changes is the working set and
the parallelism: per-source needs only row i of the bit matrix hot, and rows
are independent, so there is no synchronization (the paper's
"zero-coordination", §5.3); per-round touches the whole matrix every round
but expresses the round as *one* library call a GraphBLAS engine can
optimize and choose push or pull for (topic 20's
[reading-beamer-sc12.md](../20-graphblas/reading-beamer-sc12.md)). The notes.md
reference table puts per-source PBME next to the per-round matrix loop on
every dataset.

## How to read the paper (with the concepts in hand)

1. **§1–§3** for the encoding: how a rule becomes a matrix equation; §3's
   transitive-closure example is Step 1's dictionary.
2. **§4–§5** only for the statement of what is solvable — the linearization
   and the class of tail-recursive programs (the proofs are not needed here).
3. **§7 with Step 2's tables beside it**: note the hardware (a 2012 desktop,
   Octave), the density sweep, and the baselines.
4. **§8** for what the author lists as remaining problems.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1, 3 | `experiments/src/algebra.rs`, `tc_algebra` (stub) | the loop `D = (D·A)⟨¬T⟩` |
| 3 | `algebra.rs` tests | `flops + |arc| = semi-naive derivations`, exactly |
| 4 | lane 3 rows, `6000 chains × 10` | the mask's n² cost |
| 4 | lane 3, `sg_algebra` | 85,124 vs 64,787 on the power-law graph |
| 5 | notes.md, PBME rows | per-source against per-round |

## Questions (answer in notes.md)

1. Run Step 1's loop by hand on the 4-cycle 0→1→2→3→0. How many rounds, how
   many entries in `T` at the end, and how many multiply-accumulates?
2. Show `flops = Σ_{(x,z) in closure} outdeg(z)` for a path of n vertices:
   compute both sides for n = 5.
3. Sato's Matrix method is slower than B-Prolog at p_e = 0.001 and faster at
   p_e = 0.01 for N = 1,000. At what edge count does that put the crossover,
   and what is lane 3's equivalent of p_e on the `6000 chains × 10` graph?
4. Same-generation's step is `Aᵀ·D·A`, two products. It can be written as
   one product, `(Aᵀ ⊗ Aᵀ)ᵀ` applied to `vec(D)`, with ⊗ the Kronecker
   product. How many entries does the Kronecker matrix have, and why is that
   worse than two products?

## Done when

Answer each before unfolding it.

- [ ] You can state the dictionary from Datalog to boolean matrices and run the closure loop by hand.

  <details><summary>Answer</summary>

  Relation = matrix; join-and-project on a shared variable = product over
  (or, and); union = elementwise or; "drop tuples already known" = complement
  mask; delta = frontier matrix. On the path 0→1→2→3: `T = D = A` (3
  entries); round 1 `D·A` = {(0,2),(1,3)}; round 2 {(0,3)}; round 3 empty:
  6 entries, 3 rounds including the empty one, 3 multiply-accumulates.

  </details>

- [ ] You can say what Sato's method is, what it measured, and why it does not settle the sparse case.

  <details><summary>Answer</summary>

  It linearizes the program's matrix equation and solves it with a dense
  inverse, O(N³): `trcl(R₁) = (I − εR₁)⁻¹εR₁` (ε from the row-sum norm),
  thresholded. At N = 1,000 it takes
  about 0.1 s for any p_e, losing to Prolog and ASP engines at p_e ≤ 0.001
  and winning 15–10⁴× at p_e ≥ 0.01 (Table 2); against its own dense
  iteration it wins on all five real graphs of Table 1. Its baselines are
  tuple-at-a-time engines and dense iteration — never sparse semi-naive —
  and its own crossover shows the dense form loses on sparse inputs.

  </details>

- [ ] You can explain why the masked product does the same number of multiplications as semi-naive evaluation.

  <details><summary>Answer</summary>

  A closure tuple is in the frontier exactly once and fires once per
  out-edge of its target; the mask only discards results afterwards.
  Lane 1: 137,482 derivations on the power-law graph = 3,489 base + 133,993
  flops. So the algebra form changes the cost per derivation (a bit test
  against a hash probe), not their number.

  </details>

- [ ] You can name where the matrix form is slower, with the numbers.

  <details><summary>Answer</summary>

  The mask is n² bits regardless of closure size (n = 60,000: 450 MB for a
  270,000-tuple closure — lane 3 loses to lane 1 on `6000 chains × 10`);
  multi-atom rules need several products and materialize an intermediate
  (same-generation, power-law: 85,124 multiply-accumulates against lane 1's
  64,787); and a deep graph pays per-round overhead (chain: 499 rounds).

  </details>

- [ ] You can say how PBME relates to the per-round matrix loop.

  <details><summary>Answer</summary>

  PBME (RecStep Algorithm 2) runs each row's BFS to completion, rows in
  parallel with no coordination; the matrix loop advances all rows one round
  at a time in one product. Same bit test, same derivations; different
  working set and different parallel structure.

  </details>

## References

- Taisuke Sato, **"A Linear Algebraic Approach to Datalog Evaluation"**,
  TPLP (arXiv 1608.00139v2, Feb 2017) — §3–§5 (encoding), §7.1–§7.2
  (Tables 1–3), §8.
- Zhiwei Fan et al., **RecStep**, PVLDB 12(6), 2019, §5.3 — per-row
  bit-matrix evaluation ([reading-recstep.md](reading-recstep.md)).
- LAGraph at the topic-20 pin: `src/algorithm/template/LG_BreadthFirstSearch_SSGrB_template.c:307`
  (`GrB_vxm(…, GrB_DESC_RSC)`); SuiteSparse:GraphBLAS `Include/GraphBLAS.h:672`
  (the descriptor), both read in topic 20's
  [reading-lagraph.md](../20-graphblas/reading-lagraph.md).
- Related: [reading-gustavson-spgemm.md](../20-graphblas/reading-gustavson-spgemm.md)
  for the row-wise product the lane-3 kernel is.
