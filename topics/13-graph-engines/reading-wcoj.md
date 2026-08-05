# Worst-case optimal joins: intersect, don't enumerate

For cyclic patterns, binary join plans are asymptotically wrong — they
can overshoot the true output size by a √m factor, and no join order
fixes it, because the operator SET is the problem. Before the papers,
this chapter builds the theory step by step: the triangle query that
breaks pairwise plans, the AGM bound that proves the gap, the Generic
Join algorithm that closes it, the intersection kernels that make it
fast, and the matrix spelling that shows FalkorDB was already doing
it. Pure paper material — the code anchors are kuzu's Intersect
operator ([reading-kuzu.md](reading-kuzu.md)) and FalkorDB's masked
matrix multiply ([reading-graphblas-internals.md](reading-graphblas-internals.md)).

Every claim below is cited to a section, lemma or theorem of a paper
that is linked in the References and was read to check it. Two things
the previous version of this chapter got loose — who proved which half
of "the AGM bound", and what the bound is a bound *on* — are corrected
in place and flagged.

## The problem in one sentence

Counting triangles on a 16M-edge graph with pairwise joins can
materialize ~4000× more intermediate rows than the answer contains —
and no join order fixes it, because every pairwise plan must first
build a two-edge intermediate the third edge would have filtered.

## The concepts, step by step

### Step 1 — the triangle query breaks every pairwise plan

> **In:** the triangle query `Q(a,b,c) = R(a,b) ⋈ S(b,c) ⋈ T(a,c)` over
> a graph with m edges, and any plan built from two-relation joins.
> **Out:** a lower bound on what such a plan must materialize — and the
> conclusion that the join *order* is not the free variable.

A **binary (pairwise) join plan** combines relations two at a time —
join R with S, then join the result with T — which is how every
relational optimizer since System R builds plans. On the triangle
query (each relation the same m edges), any pairwise plan must first
materialize a two-relation intermediate:

```
 R ⋈ S  →  all paths a->b->c  →  can be Θ(m²) rows
                                 (star: hub connects everyone)
 …then filter by T             →  output was ≤ m^1.5 all along
```

The survey states the gap as a settled fact rather than an
observation:

> "A first bound is to say that there are at most N edges, and hence
> at most O(N³) triangles. A bit more thought suggests that every
> triangle is indexed by any two of its sides and hence there at most
> O(N²) triangles. However, the correct, tight, and non-trivial
> asymptotic is O(N^{3/2}). … In contrast, traditional databases
> evaluate joins pairwise, and as has been noted by several authors,
> this forces them to run in time Ω(N²) on some instance of the
> triangle query."
> — Ngo, Ré & Rudra, *Skew Strikes Back*, §1, p.1

Note the shape of that Ω(N²): it is a lower bound on *some instance*,
not on every one. The instance is a star. And this topic's own graph
is a mild version of one — count the two-edge paths it contains, which
is exactly what an `R ⋈ S` intermediate enumerates:

```
 two-edge paths through a node v = deg(v)²   (in × out, here both = deg)
 this graph (notes.md): 1e6 nodes, 16.0e6 directed edges,
                        p50 degree 11, max degree 6 565

 through the median node:   11²    =         121
 through the max node:      6 565² =  43 099 225
 ratio                                    356 192×
```

One node out of a million contributes 43 M intermediate rows on its
own. That is the same fact the topic headline measures from the other
end — the 101× two-hop slowdown from supernodes — and it is why the
waste is *structural*: the plan commits to enumerating pairs before
the third relation gets a say. Topic 10's optimizer is innocent;
reordering the joins just picks which Θ(m²) intermediate to build.

### Step 2 — the AGM bound: how big can the output actually be?

> **In:** a join query's hypergraph and the sizes |R| of its relations.
> **Out:** a provable ceiling on |Q(D)| for *every* database instance
> D, obtained by solving a small linear program — plus a matching
> instance proving the ceiling is not slack.

The **AGM bound** gives the maximum possible output size of a join
query as a product of relation sizes raised to a **fractional edge
cover**. The cover is not folklore; it is the feasible set of an
explicit LP, and the fractional edge cover number ρ*(Q) is its
optimum:

```
 LQ :  minimise    Σ_R x_R
       subject to  Σ_{R : a ∈ A_R} x_R ≥ 1   for every attribute a
                   x_R ≥ 0                   for every relation R
```
— Atserias, Grohe & Marx, §3.1, linear program (3.1)

The bound itself:

> **Lemma 2 ([10]).** Let Q be a join query with schema σ and let D be
> a σ-instance. Then for every fractional edge cover (x_R : R ∈ σ) of
> Q we have |Q(D)| ≤ ∏_{R∈σ} |R(D)|^{x_R}.
> — AGM §3.1

**Correction — attribution.** The previous version of this chapter
attributed the bound to "AGM (Atserias–Grohe–Marx)" without
qualification. The upper bound is not theirs: AGM's own paper cites it
as Lemma 2 **[10]**, i.e. Grohe & Marx, *Constraint solving via
fractional edge covers* (SODA 2006), and reproves it via Shearer's
lemma (AGM §3.1, "The proof of Lemma 2 is based on a combinatorial
lemma known as Shearer's lemma"). AGM's contribution is the *matching
lower bound*:

> **Lemma 4.** … for every N₀ ∈ ℕ there is a σ-instance D such that
> |D| ≥ N₀ and |Q(D)| ≥ ∏_{R∈σ} |R(D)|^{x_R}.
> — AGM §3.1, proved by LP duality against the dual program (3.2)

So "the AGM bound" names the *pair*: the Grohe–Marx ceiling plus the
AGM instance that reaches it. That is what makes it a target worth
building an algorithm to — a bound with slack would not be. The
survey's history agrees and reaches back further, to Friedgut–Kahn
(1990s), and to the Loomis–Whitney inequality of the 1940s
(*Skew Strikes Back* §1, p.1).

For the triangle, solve the LP by hand. Each of a, b, c appears in
exactly two of the three relations, so x = (½, ½, ½) is feasible
(½ + ½ = 1 for each variable) with cost 3/2:

```
 |Q| ≤ |R|^½ · |S|^½ · |T|^½ = m^(3/2)

 m = 16.0e6:
   AGM ceiling      m^1.5 = 16.0e6 × √(16.0e6) = 16.0e6 × 4 000 = 6.4e10
   pairwise plan    m²                                          = 2.56e14
   gap              m² / m^1.5 = √m                             = 4 000×
```

**Correction — what the bound bounds.** The previous version wrote
"output ≤ 2³⁶ ≈ 64G in theory". That is right as an arithmetic
statement but easy to misread: `m^1.5` is a *worst-case* ceiling over
all instances with m edges, not an estimate of this graph's triangle
count, which is far smaller. The number that matters is the ratio, not
either endpoint.

The gap is also not universal. Do the same LP for question 2's
4-cycle `R(a,b) S(b,c) T(c,d) U(d,a)`: take x_R = x_T = 1 and
x_S = x_U = 0 — a covers via R, b via R, c via T, d via T — for cost
2, so |Q| ≤ m². And the dual (3.2) certifies that 2 is optimal: set
y_a = y_c = 1, y_b = y_d = 0, and every relation's constraint
Σ_{a∈A_R} y_a ≤ 1 holds with equality, giving dual value 2 = ρ*. On
the 4-cycle the AGM ceiling *equals* the pairwise intermediate, so
worst-case optimality buys nothing asymptotically. Why it matters: the
bound is a target — an algorithm whose runtime is O(AGM bound) is
**worst-case optimal**, Step 1 proved no pairwise plan can be for the
triangle, and the LP is how you find out whether a given pattern is
one where that distinction pays.

### Step 3 — Generic Join: intersect one variable at a time

> **In:** the relations, pre-indexed consistently with one global
> attribute order.
> **Out:** the query answer, in time proportional to the AGM bound —
> by binding one *variable* at a time via intersection, never
> materializing a pair a later relation would kill.

**Generic Join** meets the AGM bound by changing the unit of work from
"join two relations" to "bind one variable, by intersecting everything
known about it". The survey gives it as Algorithm 3 (§4.2, p.14): if
the query has one variable left, return `⋂_{F∈E} R_F`; otherwise split
the variables into I and J, recurse on the projection onto I, and for
each tuple t_I recurse on the residual relations `R_F ⋈ t_I`. For the
triangle, unrolled with I taken one variable at a time:

```
 for a in R.a ∩ T.a:            # values for variable a
   for b in R[a].b ∩ S.b:       # b's consistent with this a
     for c in S[b].c ∩ T[a].c:  # ← THE intersection
       emit (a,b,c)
```

Two lines of the survey's analysis are the whole reason this works,
and they are worth memorizing because they reappear as code in Step 4:

> "Given the indices, when |V| = 1 computing ⋂_{F∈E} R_F can easily be
> done in time Õ(m · min |R_F|) = Õ(m · ∏_{F∈E} |R_F|^{x_F})."
> — *Skew Strikes Back* §4.2

**min**, not sum, not max. The base case is charged to the *smallest*
participating list — so the intersection kernel must never do work
proportional to the big side. That single word is the specification
that kuzu's `swapSmallestListToFront`
(`intersect.cpp:103-118`, [reading-kuzu.md](reading-kuzu.md) Step 4)
implements, and the property EmptyHeaded names the "min property"
(§1). Overall the algorithm runs in Õ(m·n·∏|R_F|^{x_F}), where Õ
hides a log factor of the input size — for the triangle, Õ(m^1.5).

The data-structure requirement is the other half:

> "Both NPRR and Leapfrog Triejoin algorithms do this by fixing a
> global attribute order and build a B-tree-like index structure for
> each input relation consistent with this global attribute order.
> NPRR also described an hash-based indexing structure so as to remove
> a log-factor from the final run time."
> — *Skew Strikes Back* §4.2

For graphs that means sorted adjacency = CSR slices (compressed sparse
row — offsets array + sorted neighbors array), exactly what kuzu's
build side guarantees with one overridden method. Note the honest
caveat about the log factor: Veldhuizen's own abstract says leapfrog
triejoin is worst-case optimal "**up to a log factor**, in the sense
of NPRR", and it exhibits a class of instances where LFTJ runs in
O(n log n) while NPRR runs in Θ(n^1.375) — the two algorithms are not
ordered, they are optimal against different granularities of
constraint. Why it matters: this is a different *operator set*, not a
smarter plan — the fix lives below the optimizer.

### Step 4 — the intersection kernel: merge vs galloping

> **In:** two sorted lists of node ids with sizes d1 ≤ d2.
> **Out:** their intersection, and a rule for which of two algorithms
> to use — the rule that decides whether Step 3's asymptotics survive
> contact with the machine.

Everything now reduces to intersecting two sorted lists, and there are
two algorithms: **merge** (walk both in lockstep, O(d1+d2)) and
**galloping** (for each element of the small list, exponentially probe
then binary-search the big list, O(d1 log d2)). Only the second one
satisfies Step 3's `min` requirement. Price both on this topic's
actual degree distribution — a median node meeting the supernode:

```
 d1 = 11      (p50 degree, notes.md)
 d2 = 6 565   (max degree, notes.md)

 merge:      d1 + d2        = 11 + 6 565            = 6 576 steps
 galloping:  d1 · log2(d2)  = 11 × 12.68            ≈   140 steps
 ratio                                              ≈    47×

 and when the two sides are the same size (d1 = d2 = 11):
 merge:       22 steps      galloping: 11 × 3.46 ≈ 38 steps  → merge wins
```

The crossover is real, which is why nobody ships only one kernel.

```rust
// ILLUSTRATION — not from any pinned repo. The production version of
// this decision is kuzu's Intersect operator, whose sorted-merge kernel
// is src/processor/operator/intersect/intersect.cpp:65-90 and whose
// smallest-list-first heuristic is intersect.cpp:103-118.
fn intersect(small: &[u32], big: &[u32], out: &mut Vec<u32>) {
    let mut lo = 0;
    for &x in small {                                  // O(d1 log d2)
        let mut step = 1;                              // exponential probe…
        while lo + step < big.len() && big[lo + step] < x { step *= 2; }
        let end = (lo + step + 1).min(big.len());
        match big[lo..end].binary_search(&x) {         // …binary-search the bracket
            Ok(i) => { out.push(x); lo += i + 1; }
            Err(i) => lo += i,
        }
    }
}
```

On power-law graphs (leaf ∩ supernode) the skewed case IS the common
case — fitting, since skew is exactly what WCOJ defends against. The
survey is explicit that this is the whole story, not a side remark:

> "Connections of join size to arcane geometric bounds may reasonably
> lead a practitioner to believe that the cause of suboptimality is a
> mysterious force wholly unknown to them—but it is not; it is the old
> enemy of the database optimizer, skew."
> — *Skew Strikes Back* §1, p.2

Why it matters: the asymptotics of Step 3 are delivered or squandered
right here, in the inner loop.

### Step 5 — EmptyHeaded: the kernel must be hardware-conscious

> **In:** neighborhood sets of wildly varying density on one machine
> with a fixed SIMD width.
> **Out:** a *choice of representation per set* — and a measured
> answer to how much that choice is worth.

EmptyHeaded compiled whole queries down to set intersections over a
trie/CSR-like layout. Its first measurement is the one that justifies
the rest of the paper:

> "For common graph queries over real data, we found that set
> intersection typically accounts for over 95% of the overall
> runtime."
> — Aberger et al., *EmptyHeaded*, §1, p.1-2

So it chose the intersection *representation* by density — defined as
"the cardinality of the set divided by its range" (§1) — using `uint`
arrays for sparse sets and a two-level `bitset` (blocks of, "say, 128
bits", each a bitvector) for dense ones, SIMD both ways (topic 17
preview). And it makes that choice at **three granularities**: graph
level, set level, and block level. The payoff for descending from the
first to the second is measured, and it is enormous where the data is
skewed and small where it is not:

```
 set-level vs graph-level representation choice (EmptyHeaded §1):
   Google+     (highly skewed):  13.4×
   LiveJournal (sparse):          1.6×
   ratio between the two datasets: 8.4×
```

Their optimizer lands within 2× of an infeasible oracle. The hardware
it was written for: "the current Intel Ivy Bridge architecture
supports CPUs with 12 cores and a SIMD register width of 256 which
execute a staggering 14.7 trillion bitwise comparisons per second when
running at 2.4GHz" (§1) — a 2015 number, quoted here because the
*ratio* it implies is the lesson: a bitset intersection moves 256 bits
per instruction where a scalar merge moves one comparison.

Its lesson: WCOJ is only fast if the intersection kernel is
hardware-conscious; **the asymptotics get you in the door, bandwidth
wins the fight**. Why it matters: this is the topic-0 discipline
applied to a theory result — a 4000× asymptotic win can still lose to
a constant-factor loss if the kernel ignores the machine, and
EmptyHeaded's own 13.4× says exactly how large that constant can get
on one representation decision.

### Step 6 — the matrix spelling: `C<A> = A²` is Generic Join

> **In:** the adjacency matrix A and the mask mechanism from the
> GraphBLAS chapter.
> **Out:** the observation that a masked SpGEMM computes precisely
> Step 3's innermost intersection — the same algorithm, arrived at
> from linear algebra instead of from relational theory.

FalkorDB never wrote an Intersect operator — because masked matrix
multiply already is one. `C<A> = A²` (compute A², but only at
positions where the mask A has an edge — the mask mechanism from
[reading-graphblas-internals.md](reading-graphblas-internals.md))
computes, for every EXISTING edge (a,b), the count |N(a) ∩ N(b)| —
each masked dot product IS the c-loop intersection from Step 3, and
the mask prevents the O(m²) blowup exactly like intersect-first does.
Same algorithm, three syntaxes:

```
 kuzu:        Intersect(N(a), N(b)) operator in the plan
 EmptyHeaded: compiled SIMD set intersection
 GraphBLAS:   C<A> = A·A  with a PAIR/AND semiring
```

The equivalence is only real if the mask is applied *early*, and
GraphBLAS has a specific method for that: `GB_AxB_dot3`, whose work is
Ω(nnz(M)) — proportional to the mask's nonzeros, i.e. to m, not to the
m² of the unmasked product (`Source/mxm/GB_AxB_dot.c:21-26`; see
[reading-graphblas-internals.md](reading-graphblas-internals.md) for
how dot3 gets selected and for the late-masking path that would
silently forfeit the whole argument). EmptyHeaded's abstract makes the
same identification from its side, calling out "the link between
general-purpose worst-case-optimal join algorithms and Boolean
algebra" (§1).

Why it matters: this equivalence is the deepest tie in the topic — the
relational world's WCOJ literature and the linear-algebra world's
masked-SpGEMM literature converged on the same computation from
opposite directions, and your M20 matrix core inherits worst-case
optimality without ever naming it.

## How to read the papers (with the concepts in hand)

| Step | Where to read it |
|---|---|
| 1 | *Skew Strikes Back* §1 p.1 (the N³ → N² → N^{3/2} paragraph) and §2 |
| 2 | AGM §3.1 — LP (3.1), Lemma 2 (upper, ← Grohe–Marx), Lemma 4 (lower, AGM's) |
| 2 | AGM §1 Theorem 1 for the four equivalent characterisations; §3.2 Theorems 6 and 7 for why join-project plans work and join-only plans do not |
| 3 | *Skew Strikes Back* §4.2, Algorithm 3 and the `Õ(m · min |R_F|)` base case |
| 3 | Veldhuizen, *Leapfrog Triejoin*, abstract + §1 for the "up to a log factor" caveat and the O(n log n) vs Θ(n^{1.375}) separation from NPRR |
| 4 | *Skew Strikes Back* §1 p.2 ("it is the old enemy … skew") and EmptyHeaded §1 on the min property |
| 5 | EmptyHeaded §1 (95% of runtime; the three granularities; 13.4× vs 1.6×) then its layout section |
| 6 | [reading-graphblas-internals.md](reading-graphblas-internals.md) on dot3 and masking, then [reading-kuzu.md](reading-kuzu.md) on `Intersect` |

1. **Ngo, Ré, Rudra — "Skew Strikes Back" (SIGMOD Record 2013)** —
   read THIS one, it's the readable survey. The triangle example is
   Steps 1–2; Generic Join is Step 3. Work their skew discussion
   against Step 4 — skew is both the villain (kills binary plans) and
   the reason galloping wins.
2. **AGM (FOCS 2008; SICOMP version)** — dip in only for the LP (3.1),
   Lemma 2 and Lemma 4 (Step 2); the proofs are optional, but read
   enough of Lemma 4's opening to see LP duality doing the work, since
   you need the dual anyway for question 2's 4-cycle.
3. **EmptyHeaded (SIGMOD 2016)** — read the layout section and the
   density-adaptive intersection (Step 5); skim the compiler
   machinery. Compare their array-vs-bitset crossover against your
   own intersect experiments.
4. Then re-read kuzu's operator ([reading-kuzu.md](reading-kuzu.md))
   and FalkorDB's masked mxm
   ([reading-graphblas-internals.md](reading-graphblas-internals.md))
   as two productions of Step 6's table.

## Questions (answer in notes.md)

1. Star graph, hub degree 1M: count R⋈S intermediates vs triangle
   output. Where did they go?
2. Fractional edge cover for the triangle is (½,½,½) → m^1.5. What's
   the bound for the 4-cycle `R(a,b)S(b,c)T(c,d)U(d,a)`?
3. Galloping search wins when d1 ≪ d2. Which real-graph fact makes
   this the common case?
4. Why does `C<A> = A²` with a boolean/PAIR semiring never materialize
   A²? Which GraphBLAS mechanism from reading-graphblas-internals.md
   does the work (dot3!)?
5. M10 planner question: how would YOUR optimizer decide binary-join
   vs intersect for a pattern — what's the detectable trigger?
   (Cyclicity of the pattern graph.)

## Done when

Answer each before unfolding it.

- [ ] You can explain why every pairwise plan loses on the triangle query, using intermediate sizes rather than intuition.

  <details><summary>Answer</summary>

  Any pairwise plan must build a two-relation intermediate first, and
  for the triangle every such intermediate is a set of two-edge paths.
  A node of degree d contributes d² of them, so on a star the
  intermediate is Θ(m²) while the output is O(m^1.5) — *Skew Strikes
  Back* §1 p.1 states the Ω(N²) lower bound for pairwise evaluation
  and the tight O(N^{3/2}) output asymptotic side by side.

  Reordering does not help: symmetry means every choice of "which two
  first" produces the same shape of intermediate. On this topic's
  graph the max-degree node alone contributes 6 565² = 43 099 225
  intermediate rows.

  </details>

- [ ] You can state the AGM bound, compute the fractional edge cover for the triangle, and say which half of it is due to whom.

  <details><summary>Answer</summary>

  |Q(D)| ≤ ∏_R |R(D)|^{x_R} for any feasible solution x of the LP
  (3.1): minimise Σ x_R subject to Σ_{R ∋ a} x_R ≥ 1 for every
  attribute a. For the triangle each variable sits in two relations,
  so (½,½,½) is feasible with cost ρ* = 3/2 and the bound is m^1.5.

  Attribution: the upper bound is AGM's **Lemma 2 [10]** — Grohe &
  Marx, SODA 2006, reproved via Shearer's lemma. AGM's own result is
  **Lemma 4**, the matching lower bound: an instance exists that
  attains the product, constructed by LP duality against (3.2). "The
  AGM bound" is shorthand for the pair.

  </details>

- [ ] You can narrate Generic Join as one variable at a time, say where the intersections happen, and quote its base-case cost.

  <details><summary>Answer</summary>

  Algorithm 3 of *Skew Strikes Back* §4.2: with one variable left,
  return the intersection of all relations; otherwise pick a variable
  subset I, recurse on the projections onto I, and for every tuple
  t_I recurse on the relations semi-joined with t_I. Unrolled for the
  triangle: intersect for a, then for b given a, then — the one that
  matters — c ∈ S[b].c ∩ T[a].c.

  Base case cost: Õ(m · **min** |R_F|). "min" is the whole point: the
  work is charged to the smallest list, which is what forces
  galloping in Step 4 and smallest-list-first ordering in kuzu.
  Overall Õ(m·n·∏|R_F|^{x_F}), Õ hiding a log factor.

  </details>

- [ ] You can say when galloping beats a merge intersection, in terms of the two list lengths, with a worked number.

  <details><summary>Answer</summary>

  Merge is O(d1+d2), galloping O(d1 log d2); galloping wins once
  d2 ≫ d1 log d2. On this topic's graph, d1 = 11 (p50) against
  d2 = 6 565 (max degree): merge 6 576 steps versus 11 × log2(6 565)
  ≈ 140, a 47× difference. At d1 = d2 = 11 it inverts — 22 versus
  ≈ 38 — which is why engines keep both kernels and pick per call.

  Power-law degree distributions make the skewed case the common one,
  which is the answer to question 3.

  </details>

- [ ] You can explain why `C<A> = A²` is the same algorithm in matrix spelling — and connect it to the masked-SpMV lane here.

  <details><summary>Answer</summary>

  Each entry of A² at position (a,b) is a dot product of A's row a
  with A's column b, i.e. |N(a) ∩ N(b)| — Step 3's innermost
  intersection. Restricting the computation to positions where the
  mask A is nonzero means only existing edges (a,b) are ever
  considered, which is intersect-first rather than
  enumerate-then-filter.

  The mechanism is `GB_AxB_dot3`, whose work is Ω(nnz(M)) — the mask's
  nonzeros, i.e. m — instead of the m² of an unmasked product
  (`Source/mxm/GB_AxB_dot.c:21-26`). It only holds if the mask is
  applied *early*; the late path through `GB_accum_mask`/`GB_masker`
  computes the full product first and then discards, forfeiting the
  argument entirely.

  </details>

- [ ] You wrote answers to all questions in notes.md.

  <details><summary>Answer</summary>

  Question 2's 4-cycle: ρ* = 2, so |Q| ≤ m². Primal certificate
  x_R = x_T = 1, x_S = x_U = 0 (a and b covered by R, c and d by T).
  Dual certificate for optimality, using AGM's program (3.2):
  y_a = y_c = 1, y_b = y_d = 0 satisfies Σ_{a ∈ A_R} y_a ≤ 1 with
  equality on all four relations, value 2.

  The lesson is the one Step 2 draws: m² is also what a pairwise plan
  materializes, so the 4-cycle has no polynomial gap and worst-case
  optimality is not automatically a win. That is the "detectable
  trigger" question 5 wants — cyclicity is necessary but the honest
  test is ρ*(pattern) versus the intermediate size the best binary
  plan would build.

  </details>

## References

**Papers**
- Atserias, Grohe, Marx — "Size Bounds and Query Plans for Relational
  Joins" (FOCS 2008; SICOMP,
  [arXiv:1711.03860](https://arxiv.org/abs/1711.03860)) — §3.1 has LP
  (3.1), Lemma 2 (upper bound, credited to Grohe–Marx SODA 2006) and
  Lemma 4 (AGM's matching lower bound); §3.2 Theorems 6 and 7 for
  join-project versus join-only plans
- Ngo, Ré, Rudra — "Skew Strikes Back: New Developments in the Theory
  of Join Algorithms" (SIGMOD Record 2013,
  [arXiv:1310.3314](https://arxiv.org/abs/1310.3314)) — the readable
  survey; read THIS one. §1 p.1 for N^{3/2} and the Ω(N²) pairwise
  lower bound, §1 p.2 for the skew thesis, §4.2 Algorithm 3 for
  Generic Join and its `Õ(m · min |R_F|)` base case
- Ngo, Porat, Ré, Rudra — "Worst-case Optimal Join Algorithms"
  (PODS 2012, [arXiv:1203.1952](https://arxiv.org/abs/1203.1952)) —
  NPRR, the first algorithm to match the bound
- Veldhuizen — "Leapfrog Triejoin: A Simple, Worst-Case Optimal Join
  Algorithm" ([arXiv:1210.0481](https://arxiv.org/abs/1210.0481)) —
  worst-case optimal *up to a log factor*, implementable on ordinary
  B-trees; the abstract's O(n log n) vs Θ(n^{1.375}) separation is
  worth knowing before you assume the algorithms are ordered
- Aberger et al. — "EmptyHeaded: A Relational Engine for Graph
  Processing" (SIGMOD 2016,
  [arXiv:1503.02368](https://arxiv.org/abs/1503.02368)) — the
  hardware-conscious intersection kernels; §1 for the 95%-of-runtime
  measurement, the uint/bitset choice at graph/set/block granularity,
  and the 13.4× (Google+) vs 1.6× (LiveJournal) payoff

**Code**
- No repo of its own for this chapter. The anchors live in the two
  chapters this one binds together:
  [kuzu](https://github.com/kuzudb/kuzu)'s `Intersect` operator —
  `src/processor/operator/intersect/intersect.cpp:65-90` (sorted
  merge) and `:103-118` (smallest list first),
  [reading-kuzu.md](reading-kuzu.md) — and GraphBLAS's masked SpGEMM,
  `Source/mxm/GB_AxB_dot.c:21-26` (dot3's Ω(nnz(M))),
  [reading-graphblas-internals.md](reading-graphblas-internals.md)
