# Worst-case optimal joins: intersect, don't enumerate

For cyclic patterns, binary join plans are asymptotically wrong — they
can overshoot the true output size by a √m factor, and no join order
fixes it, because the operator SET is the problem. This chapter covers
the AGM bound that proves it, the Generic Join algorithm that fixes it,
and the intersection kernels that make the fix fast. Pure paper
material — the code anchor is kuzu's Intersect operator
([reading-kuzu.md](reading-kuzu.md)) and FalkorDB's masked matrix
multiply ([reading-graphblas-internals.md](reading-graphblas-internals.md)).

## 1. Why binary joins are asymptotically wrong

Triangle query: `Q(a,b,c) = R(a,b) ⋈ S(b,c) ⋈ T(a,c)`, each relation m
edges. ANY pairwise plan first joins two relations:

```
 R ⋈ S  →  all paths a->b->c  →  can be Θ(m²) rows
                                 (star: hub connects everyone)
 …then filter by T             →  output was ≤ m^1.5 all along
```

**AGM bound**: |output| ≤ product of relation sizes raised to a
fractional edge cover. For the triangle: m^(3/2). Binary plans can
overshoot the bound by √m — on a 16M-edge graph that's 4000×
intermediates you didn't need. No join ORDER fixes it (topic 10's
optimizer is innocent; the operator SET is the problem).

## 2. Generic Join: intersect one variable at a time

```
 for a in R.a ∩ T.a:            # values for variable a
   for b in R[a].b ∩ S.b:       # b's consistent with this a
     for c in S[b].c ∩ T[a].c:  # ← THE intersection
       emit (a,b,c)
```

Runtime O(m^1.5) — matches AGM (worst-case optimal). The whole trick:
never enumerate (a,b,c-candidates) pairs that a later relation kills;
intersect FIRST. Requirement: each relation accessible sorted/hashed
by any prefix — i.e. sorted adjacency = CSR slices. Intersection of
two sorted lists sized d1 ≤ d2: merge O(d1+d2) or galloping
O(d1 log d2) — skew (supernodes) decides which.

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

## 3. EmptyHeaded and the matrix connection

EmptyHeaded compiled queries to set intersections over a trie/CSR-like
layout and picked intersection algorithm by density: **uint arrays vs
bitsets** — SIMD both ways (topic 17 preview). Its lesson: WCOJ is
only fast if the intersection kernel is hardware-conscious; the
asymptotics get you in the door, bandwidth wins the fight.

FalkorDB's spelling: masked matrix multiply. `C<A> = A²` computes, for
every EXISTING edge (a,b), |N(a) ∩ N(b)| — the mask prevents the O(m²)
blowup exactly like Generic Join's intersect-first. Same algorithm,
three syntaxes:

```
 kuzu:        Intersect(N(a), N(b)) operator in the plan
 EmptyHeaded: compiled SIMD set intersection
 GraphBLAS:   C<A> = A·A  with a PAIR/AND semiring
```

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
