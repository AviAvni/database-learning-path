# Direction-optimizing BFS: push until pull is cheaper

Beamer's SC '12 paper made BFS a two-algorithm problem: push
(frontier scans its out-edges) wins early, pull (unvisited vertices
scan their in-edges) wins at the peak, and a two-threshold switch
picks per level. This chapter builds the two algorithms and the
switch from zero — waste argument, early exit, thresholds, then the
linear-algebra translation — so you can read the paper with
LAGraph's template open ([reading-lagraph.md](reading-lagraph.md)):
the 2012 idea ships verbatim in the 2025 library, thresholds and
all.

## The problem in one sentence

On a small-world graph, the middle levels of a BFS contain most of
the graph, and there the classic algorithm wastes nearly every edge
inspection on already-visited vertices — direction-optimizing BFS
cuts total edge inspections 3-8× on scale-free graphs by flipping
who scans whom.

## The concepts, step by step

### Step 1 — BFS, frontiers, and levels

**BFS** (breadth-first search) explores a graph outward from a
source vertex in waves: **level** 0 is the source, level k+1 is
every not-yet-seen vertex adjacent to level k. The set of vertices
discovered at the current level is the **frontier**; the set of
everything discovered so far is **visited**. Each iteration
consumes the frontier and produces the next one, and the outputs
people actually want are per-vertex *level* (distance) or *parent*
(the BFS tree). All the work is edge inspections — "is this
neighbor new?" — so the cost model is simply: how many edge
inspections does producing each next frontier take?

### Step 2 — push (top-down), and why it dies mid-search

The classic formulation is **push**: each frontier vertex scans its
out-edges and tries to claim unvisited neighbors. Its work per
level is the sum of the frontier's out-degrees — every out-edge of
every frontier vertex is inspected, whether or not it finds
anything new. On small-world graphs the frontier explodes — level
3-4 of an RMAT graph holds most of the graph:

```
 level:      0    1     2       3        4      5
 |frontier|: 1    d̄     d̄²      ~n/2     ~n/4   tail
 push work:  d̄    d̄²    d̄³      HUGE     …      …
              ↑ most edge checks FAIL: neighbor already visited
```

At the peak, nearly every edge inspection hits an already-visited
vertex — wasted claims (and wasted CAS on parallel hardware: many
threads race to claim the same few remaining vertices). Push is
perfect when the frontier is small and hopeless when it's huge.

### Step 3 — pull (bottom-up): invert who scans whom, and exit early

Beamer's bet is to invert the roles: each *unvisited* vertex scans
its in-edges asking "is any parent of mine in the frontier?" — and
stops at the FIRST hit, because one frontier parent is all it needs
to join the next level. That early exit is the entire speedup: when
the frontier is most of the graph, an unvisited vertex finds a
frontier parent in O(1) expected probes.

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

The work comparison:

```
 push work ≈ Σ_{v ∈ frontier} out_degree(v)      (all of it)
 pull work ≈ Σ_{v unvisited} (probes until first frontier hit)
             ≈ nnz-touched shrinks as frontier grows
```

Push's work grows with the frontier; pull's shrinks as the
unvisited set drains and hits come faster. They cross somewhere in
the middle levels — which is the whole paper.

### Step 4 — what pull needs: the reverse graph and a dense frontier

Pull's two prerequisites, each a real cost:

- **the reverse graph**: pull scans *in*-edges, so it needs the
  transpose AT (equivalently, CSC — the same edges indexed by
  destination instead of source). For a graph stored once in CSR,
  that's a second copy: memory ×2. This is the memory-doubling
  question from topic 13 and Gunrock, and it's why LAGraph makes
  AT an *optional* input.
- **a dense frontier representation**: pull tests "is u in the
  frontier?" once per probe, so membership must be O(1) — a
  **bitmap** (one bit per vertex; n/8 bytes total) rather than the
  sparse list of vertex IDs that push iterates. Converting between
  the two representations at each switch is itself an O(n) cost the
  switch heuristic must respect.

### Step 5 — the switch: two thresholds, with hysteresis

Per level, pick the cheaper direction. Beamer's heuristic compares
estimated push work against a slice of the unexplored edges, with
*asymmetric* thresholds so the algorithm doesn't oscillate:

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
never triggers and the machinery must cost ~nothing — a heuristic
is judged on the workload where it *doesn't* fire, too.

### Step 6 — the linear-algebra translation: push=vxm, pull=mxv

Yang/Buluç/Owens (ICPP '18) showed the whole construction is two
GraphBLAS calls plus the switch:

```
 push  = q' * A     sparse vector × CSR  = SpMSpV (saxpy engine)
 pull  = AT * q     CSR(AT) × vector w/ mask = masked SpMV (dot
                    engine, ANY monoid ⇒ early exit is LEGAL)
 visited mask = the complemented structural mask (GrB_DESC_RSC)
 direction switch = engine dispatch on frontier density
```

The ANY monoid (an accumulator that may keep *any one* of the
values combined into it, so the reduction may stop at the first)
is what makes pull's `break` algebraically sound — question 2. The
profound part: SuiteSparse's *format* switch (sparse↔bitmap
vector) and *engine* switch (saxpy↔dot) mirror the push↔pull
switch — the same decision at three abstraction levels. Our stub
implements all three explicitly in ~100 lines.

## How to read the paper (with the concepts in hand)

- **§3-4** — the two algorithms and the waste argument: steps 2-3
  in the authors' words, with measured per-level edge-inspection
  counts. Compare their per-level plots with gb_bench's `--trace`
  output on an RMAT graph.
- **§5** — the α/β tuning: step 5. This is the part LAGraph copied
  (with different constants — question 5 asks why β2 moved from 24
  to 512).
- **Yang, Buluç, Owens, §3** — the push=vxm / pull=mxv translation
  (step 6); read it after the Beamer paper, with the LAGraph
  template open — the three texts are one idea at three levels of
  abstraction.

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
