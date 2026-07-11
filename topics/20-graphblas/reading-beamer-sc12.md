# Direction-optimizing BFS: push until pull is cheaper

Beamer's SC '12 paper made BFS a two-algorithm problem: push
(frontier scans its out-edges) wins early, pull (unvisited vertices
scan their in-edges) wins at the peak, and a two-threshold switch
picks per level. Read with LAGraph's template open
([reading-lagraph.md](reading-lagraph.md)) — the 2012 idea ships
verbatim in the 2025 library, thresholds and all.

## 1. Top-down's waste (why push dies mid-search)

Top-down (push): each frontier vertex scans its out-edges, tries to
claim unvisited neighbors. On small-world graphs the frontier
explodes — level 3-4 of an RMAT graph holds most of the graph:

```
 level:      0    1     2       3        4      5
 |frontier|: 1    d̄     d̄²      ~n/2     ~n/4   tail
 push work:  d̄    d̄²    d̄³      HUGE     …      …
              ↑ most edge checks FAIL: neighbor already visited
```

At the peak, nearly every edge inspection hits an already-visited
vertex — wasted claims (and wasted CAS on parallel hardware).

## 2. Bottom-up's bet (pull)

Invert: each UNVISITED vertex scans its in-edges asking "is any
parent in the frontier?" — and stops at the FIRST hit (early
exit). When the frontier is most of the graph, an unvisited vertex
finds a frontier parent in O(1) expected probes:

```
 push work ≈ Σ_{v ∈ frontier} out_degree(v)      (all of it)
 pull work ≈ Σ_{v unvisited} (probes until first frontier hit)
             ≈ nnz-touched shrinks as frontier grows
```

Pull needs: the REVERSE graph (CSC / AT — the memory-doubling
question from topic 13 and Gunrock), and a dense frontier
representation (bitmap — O(1) membership).

```rust
// pull: each UNVISITED vertex asks "is any of MY parents in the frontier?"
fn bfs_pull_level(at: &Csr, frontier: &Bitmap, visited: &Bitmap) -> Bitmap {
    let mut next = Bitmap::new(at.nrows);
    for v in 0..at.nrows {
        if visited.get(v) { continue; }
        for &u in at.row(v) {          // v's in-edges (a row of AT)
            if frontier.get(u) {
                next.set(v);
                break;                 // ANY monoid: first hit suffices —
            }                          //   the early exit IS the speedup
        }
    }
    next
}
```

## 3. The switch heuristic (the shipped numbers)

```
 push → pull:  m_frontier_out > m_unexplored / α     (α = 14 paper,
               or |frontier| > n/β1                    8 in LAGraph)
 pull → push:  |frontier| < n / β2                   (β = 24 paper,
                                                      512 in LAGraph)
```

Asymmetric thresholds = hysteresis (same instinct as SuiteSparse's
format switches). LAGraph adds a refinement: track
`edges_unexplored` incrementally by subtracting frontier degrees
(template :196, :261-277) — the heuristic input is maintained, not
recomputed. Result on scale-free graphs: 3-8× total edge
inspections saved; on high-diameter graphs (road networks) pull
never triggers and the machinery must cost ~nothing.

## 4. The linear-algebra translation (Yang/Buluç/Owens ICPP '18)

```
 push  = q' * A     sparse vector × CSR  = SpMSpV (saxpy engine)
 pull  = AT * q     CSR(AT) × vector w/ mask = masked SpMV (dot
                    engine, ANY monoid ⇒ early exit is LEGAL)
 visited mask = the complemented structural mask (GrB_DESC_RSC)
 direction switch = engine dispatch on frontier density
```

The profound part: SuiteSparse's *format* switch (sparse↔bitmap
vector) and *engine* switch (saxpy↔dot) mirror the push↔pull
switch — the same decision at three abstraction levels. Our stub
implements all three explicitly in ~100 lines.

## Questions for notes.md

1. Reproduce Beamer's waste argument from gb_bench's per-level
   trace: at the peak level, what fraction of push's edge checks
   found an already-visited target (count them — add a counter to
   the stub)?
2. Why does pull's early exit require the ANY (or OR) monoid
   algebraically — what property (idempotent, any-witness-suffices)
   makes stopping sound, and which semirings BREAK it (PLUS: you
   need every contribution — BFS parent vs PageRank)?
3. Road network vs RMAT: predict which levels (if any) go pull on
   each, from diameter and degree distribution alone. Then check
   with gb_bench --trace.
4. The reverse graph doubles memory. FalkorDB keeps BOTH (the
   transposed delta trio, delta_matrix.h:20-22) — for which query
   shapes besides BFS pull is AT load-bearing (incoming-edge
   traversals `<-[]-`)?
5. LAGraph's β2=512 (vs paper's 24) makes pull→push switch-back
   very late. Hypothesize why (switch-back cost includes rebuilding
   a SPARSE frontier from a bitmap — O(n) scan), and design the
   experiment that would confirm it.

## References

**Papers**
- Beamer, Asanović, Patterson — "Direction-Optimizing
  Breadth-First Search" (SC 2012) — §3-4 are the two algorithms and
  the waste argument; §5's α/β tuning is the part LAGraph copied
- Yang, Buluç, Owens — "Implementing Push-Pull Efficiently in
  GraphBLAS" (ICPP 2018,
  [arXiv:1804.03327](https://arxiv.org/abs/1804.03327)) — the
  push=vxm / pull=mxv translation in §3

**Code**
- [LAGraph](https://github.com/GraphBLAS/LAGraph)
  `src/algorithm/template/LG_BreadthFirstSearch_SSGrB_template.c` —
  the shipped thresholds (:184-187) and switch logic (:243-292);
  walked in [reading-lagraph.md](reading-lagraph.md)
