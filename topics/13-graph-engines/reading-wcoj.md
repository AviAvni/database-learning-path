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

## The problem in one sentence

Counting triangles on a 16M-edge graph with pairwise joins can
materialize ~4000× more intermediate rows than the answer contains —
and no join order fixes it, because every pairwise plan must first
build a two-edge intermediate the third edge would have filtered.

## The concepts, step by step

### Step 1 — the triangle query breaks every pairwise plan

A **binary (pairwise) join plan** combines relations two at a time —
join R with S, then join the result with T — which is how every
relational optimizer since System R builds plans. On the triangle
query `Q(a,b,c) = R(a,b) ⋈ S(b,c) ⋈ T(a,c)` (each relation the same m
edges), any pairwise plan must first materialize a two-relation
intermediate:

```
 R ⋈ S  →  all paths a->b->c  →  can be Θ(m²) rows
                                 (star: hub connects everyone)
 …then filter by T             →  output was ≤ m^1.5 all along
```

The star graph is the killer: a hub with degree 1M makes R ⋈ S produce
10¹² two-edge paths, of which the final result keeps a vanishing
fraction. Why it matters: the waste is *structural* — the plan commits
to enumerating pairs before the third relation gets a say — and topic
10's optimizer is innocent; reordering the joins just picks which Θ(m²)
intermediate to build.

### Step 2 — the AGM bound: how big can the output actually be?

The **AGM bound** (Atserias–Grohe–Marx) gives the maximum possible
output size of a join query as a product of relation sizes raised to a
**fractional edge cover** — an assignment of weights to relations such
that every variable is "covered" by total weight ≥ 1 across the
relations containing it. For the triangle, weights (½, ½, ½) cover
each of a, b, c (each variable appears in two relations, ½ + ½ = 1),
giving:

```
 |Q| ≤ |R|^½ · |S|^½ · |T|^½ = m^(3/2)
```

For m = 16M ≈ 2²⁴: output ≤ 2³⁶ ≈ 64G in theory, but the point is the
*gap* — binary plans can produce Θ(m²) = 2⁴⁸ intermediates, √m ≈ 4000×
above the bound. Why it matters: the bound is a target — an algorithm
whose runtime is O(AGM bound) is **worst-case optimal**, and Step 1
proved no pairwise plan can be.

### Step 3 — Generic Join: intersect one variable at a time

**Generic Join** meets the AGM bound by changing the unit of work from
"join two relations" to "bind one *variable*, by intersecting
everything known about it":

```
 for a in R.a ∩ T.a:            # values for variable a
   for b in R[a].b ∩ S.b:       # b's consistent with this a
     for c in S[b].c ∩ T[a].c:  # ← THE intersection
       emit (a,b,c)
```

For the triangle this runs in O(m^1.5) — worst-case optimal. The whole
trick in one line: never enumerate (a,b,c-candidate) pairs that a
later relation kills; **intersect FIRST**. The data-structure
requirement: each relation must be accessible sorted or hashed by any
prefix of the variable order — which for graphs means sorted adjacency
= CSR slices (compressed sparse row — offsets array + sorted neighbors
array), exactly what kuzu's build side guarantees. Why it matters:
this is a different *operator set*, not a smarter plan — the fix lives
below the optimizer.

### Step 4 — the intersection kernel: merge vs galloping

Everything now reduces to intersecting two sorted lists of sizes
d1 ≤ d2, and there are two algorithms: **merge** (walk both in
lockstep, O(d1+d2)) and **galloping** (for each element of the small
list, exponentially probe then binary-search the big list,
O(d1 log d2)). Galloping wins when d1 ≪ d2 — intersect a degree-20
node with a degree-100K supernode: merge does ~100K steps, galloping
~20 × 17 ≈ 340:

```rust
// the inner kernel of every WCOJ engine: sorted-set intersection.
// galloping wins when d1 ≪ d2 — on power-law graphs (leaf ∩ supernode)
// that's the common case, and skew is exactly what WCOJ defends against
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
case — fitting, since skew is exactly what WCOJ defends against
("Skew Strikes Back" is the survey's title for a reason). Why it
matters: the asymptotics of Step 3 are delivered or squandered right
here, in the inner loop.

### Step 5 — EmptyHeaded: the kernel must be hardware-conscious

EmptyHeaded compiled whole queries down to set intersections over a
trie/CSR-like layout and chose the intersection *representation* by
density: sorted uint arrays for sparse sets, bitsets for dense ones —
SIMD both ways (topic 17 preview). Its lesson: WCOJ is only fast if
the intersection kernel is hardware-conscious; **the asymptotics get
you in the door, bandwidth wins the fight**. A bitset intersection of
two dense neighborhoods is 64 comparisons per cycle-ish; a scalar
merge is 1. Why it matters: this is the topic-0 discipline applied to
a theory result — a 4000× asymptotic win can still lose to a 50×
constant-factor loss if the kernel ignores the machine.

### Step 6 — the matrix spelling: `C<A> = A²` is Generic Join

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

Why it matters: this equivalence is the deepest tie in the topic — the
relational world's WCOJ literature and the linear-algebra world's
masked-SpGEMM literature converged on the same computation from
opposite directions, and your M20 matrix core inherits worst-case
optimality without ever naming it.

## How to read the papers (with the concepts in hand)

1. **Ngo, Ré, Rudra — "Skew Strikes Back" (SIGMOD Record 2013)** —
   read THIS one, it's the readable survey. The triangle example is
   Steps 1–2; Generic Join is Step 3. Work their skew discussion
   against Step 4 — skew is both the villain (kills binary plans) and
   the reason galloping wins.
2. **AGM (FOCS 2008)** — dip in only for the fractional edge cover
   definition and the bound statement (Step 2); the proofs are
   optional. Try computing the cover for a 4-cycle (question 2).
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

## References

**Papers**
- Atserias, Grohe, Marx — "Size Bounds and Query Plans for Relational
  Joins" (FOCS 2008) — the AGM bound
- Ngo, Ré, Rudra — "Skew Strikes Back: New Developments in the Theory
  of Join Algorithms" (SIGMOD Record 2013,
  [arXiv:1310.3314](https://arxiv.org/abs/1310.3314)) — the readable
  survey; read THIS one
- Aberger et al. — "EmptyHeaded: A Relational Engine for Graph
  Processing" (SIGMOD 2016) — the hardware-conscious intersection
  kernels

**Code**
- No repo for this chapter — the code anchors are
  [kuzu](https://github.com/kuzudb/kuzu)'s Intersect operator
  ([reading-kuzu.md](reading-kuzu.md)) and FalkorDB's masked mxm
  ([reading-graphblas-internals.md](reading-graphblas-internals.md))
