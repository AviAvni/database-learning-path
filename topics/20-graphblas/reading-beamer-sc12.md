# Direction-optimizing BFS: push until pull is cheaper

Beamer's SC '12 paper made BFS a two-algorithm problem: push
(frontier scans its out-edges) wins early, pull (unvisited vertices
scan their in-edges) wins at the peak, and a two-threshold switch
picks per level. This chapter builds the two algorithms and the
switch from zero — waste argument, early exit, thresholds, then the
linear-algebra translation — so you can read the paper with
LAGraph's template open ([reading-lagraph.md](reading-lagraph.md)):
the 2012 idea ships in the 2025 library, though the constants moved
and the switch is not shaped the way most summaries claim.

Every paper number below is quoted from **Beamer, Asanović &
Patterson, "Direction-Optimizing Breadth-First Search", SC '12**,
with the section or figure it came from; every code anchor is
**LAGraph @ `e2539e2`** (the pin in `resources/codebases.md`),
quoted with the line numbers it occupies at that commit. Where the
paper and LAGraph disagree — and they do, on both constants and the
shape of the test — this guide says which is which.

## The problem in one sentence

On a small-world graph the middle levels of a BFS contain most of
the graph, and there the classic algorithm spends nearly every edge
inspection on an already-visited vertex: on the paper's `kron27`
sample search, a top-down BFS performs about **67×** the edge
examinations that the BFS tree strictly requires (§III), and
flipping who scans whom recovers an **average speedup of 3.9, never
below 2.4**, across ten graphs on a 16-core machine (§VI-C,
Fig. 10).

## The concepts, step by step

### Step 1 — BFS, frontiers, and levels

> **In:** a graph and a source vertex.
> **Out:** the vocabulary — frontier, visited, level — and the single
> cost unit (the edge inspection) every later step is priced in.

**BFS** (breadth-first search) explores a graph outward from a
source vertex in waves: **level** 0 is the source, level k+1 is
every not-yet-seen vertex adjacent to level k. The set of vertices
discovered at the current level is the **frontier**; the set of
everything discovered so far is **visited**. Each iteration
consumes the frontier and produces the next one, and the outputs
people actually want are per-vertex *level* (distance) or *parent*
(the BFS tree).

An **edge inspection** (the paper calls it an *edge check*) is one
look at one endpoint of one edge to ask "has this been visited?"
That is the cost unit for the whole chapter. The paper's own
statement of the baseline (§III):

> "The total number of edge checks in the conventional top-down
> algorithm is equal to the number of edges in the connected
> component containing the source vertex, as on each step every
> edge in the frontier is checked."

Beamer classifies the *outcome* of each check into four categories
(§III, Fig. 3 and Fig. 4), and this taxonomy is the whole argument:

| outcome | meaning |
|---|---|
| **claimed child** | the neighbour was unvisited; the check did work |
| **failed child** | neighbour at depth d+1, already claimed by a rival |
| **peer** | neighbour at the same depth d |
| **valid parent** | neighbour at depth d−1 |

Only *claimed child* is productive. The other three are the waste.

Why it matters: the entire paper is one observation about the ratio
between those four categories as the frontier grows, so pricing
everything in edge inspections is not a simplification — it is the
paper's own metric.

### Step 2 — the size of the waste, measured

> **In:** the four outcome categories from Step 1.
> **Out:** the paper's own arithmetic on `kron27`, reproduced, giving
> the headroom that direction-optimization is trying to claim.

The paper's §III does the subtraction for you, on a Kronecker
graph it calls **kron27** — 128M vertices, 2B undirected edges,
input degree 16 (Fig. 3 caption):

> "For the example in Figure 3, only 63,036,116 vertices are in the
> BFS tree, so at least 63,036,115 edges need to be considered,
> which is about 1/67th of all the edge examinations that would
> happen during a top-down traversal."

Work that factor of 67 yourself; the paper explains it in the next
sentence and the two effects multiply:

```
 inputs (Beamer §III, Fig. 3 caption):
   undirected edges                 m  = 2.0e9
   vertices in the BFS tree         t  = 63,036,116
   tree edges strictly needed       t−1 = 63,036,115

 top-down inspections = every edge in the component, from BOTH ends:
   2 × m                            = 4.0e9

 ratio = 4.0e9 / 63,036,115         = 63.5×      (paper: "about 67")

 why 67 and not the input degree 16, in the paper's own two reasons:
   ×2   each undirected edge is checked from both endpoints
   ×2.0 zero-degree vertices are excluded from the component, which
        raises the effective degree of the vertices that remain
   16 × 2 × 2.0                     ≈ 64
```

The 63.5 from the nominal 2B and the paper's 67 differ because the
generator's realised edge count is not exactly 2.0e9; the point
survives either way. **Roughly 98.5% of a top-down BFS's edge
inspections on this graph are, in principle, avoidable.**

Fig. 4 says where in the search the waste sits: "During the first
few steps, the percentage of claimed children is high… As the
frontier reaches its largest size, the percentage of peer edges
dominates." Waste is not spread evenly — it is concentrated in
exactly the two or three middle levels that also hold most of the
runtime (§III: "The middle steps (2 and 3) consume the vast
majority of the runtime").

Why it matters: this is the number your own trace has to reproduce
(question 1), and it tells you the switch only has to be right in
the middle of the search — the tails cannot pay for themselves.

### Step 3 — push (top-down), and why it dies mid-search

> **In:** the frontier/visited pair (Step 1) and the waste profile
> (Step 2).
> **Out:** push's cost formula, and the reason its cost tracks the
> frontier rather than the discoveries.

The classic formulation is **push** (the paper says *top-down*):
each frontier vertex scans its out-edges and tries to claim
unvisited neighbours. Its work per level is the sum of the
frontier's out-degrees — call that quantity **m_f**, the paper's
name for "the number of edges to check from the frontier" (§V) —
whether or not those checks find anything new.

```
 level:      0    1     2       3        4      5
 |frontier|: 1    d̄     d̄²      ~n/2     ~n/4   tail
 push work:  d̄    d̄²    d̄³      HUGE     …      …
              ↑ at the apex, Fig. 4 says PEER edges dominate:
                both endpoints are already in the frontier
```

Two costs, not one. The wasted checks are the obvious one. The
paper names the second in §IV: "In the top-down approach, there
could be multiple parallel writers to the same child, so atomic
operations are needed to ensure mutual exclusion." Push's failed
children are also its contended CAS operations — the same
few remaining vertices, raced for by many threads.

Why it matters: push is not badly implemented; it is asked to do
the wrong thing. Its cost is m_f, and m_f peaks exactly where the
discoveries stop.

### Step 4 — pull (bottom-up): invert who scans whom, and exit early

> **In:** push's cost m_f (Step 3).
> **Out:** pull's cost, the early exit that produces it, and the
> paper's measured evidence that the exit — not the inversion — is
> where the speed comes from.

Beamer's inversion: each *unvisited* vertex scans its in-edges
asking "is any parent of mine in the frontier?" — and stops at the
**first** hit, because one frontier parent is all it needs to join
the next level. Fig. 5 is the paper's pseudocode, and the `break`
is the whole idea:

```
 Fig. 5 — Single Step of Bottom-Up Approach (Beamer §IV, verbatim)

 function bottom-up-step(vertices, frontier, next, parents)
   for v ∈ vertices do
     if parents[v] = -1 then
       for n ∈ neighbors[v] do
         if n ∈ frontier then
           parents[v] ← n
           next ← next ∪ {v}
           break                 ← the early exit
         end if
       end for
     end if
   end for
```

The paper's own summary of the two consequences (§IV):

> "The advantage of this approach is that once a vertex has found a
> parent, it does not need to check the rest of its neighbors."

> "With the bottom-up approach, only the child writes to itself,
> removing any contention."

Do not take the early exit on faith as *the* mechanism — Yang,
Buluç & Owens (ICPP '18) took Beamer's construction apart into
separable optimizations and measured each one on
`kron_g500-logn21` (their **Table 2**, in GTEPS, cumulative left to
right, speedups standalone):

| optimization | GTEPS | standalone speedup |
|---|---|---|
| baseline | 0.874 | — |
| structure only | 1.411 | 1.62× |
| **change of direction** | 1.527 | **1.08×** |
| masking | 3.932 | 2.58× |
| **early exit** | 15.83 | **4.02×** |
| operand reuse | 42.44 | 2.68× |

Read the two bold rows together. Flipping direction *by itself*
buys 1.08×. The early exit buys 4.02× — Yang's §5.3 says flatly
that it "yielded the greatest speed-up". If you implement pull
without the `break`, you have implemented the 8% and skipped the
302%.

The work comparison, then:

```
 push work per level ≈ m_f = Σ_{v ∈ frontier} out_degree(v)   (all of it)
 pull work per level ≈ Σ_{v unvisited} (probes until first frontier hit)

 as the frontier grows toward n:
   m_f  grows       — more frontier vertices, all their edges
   pull shrinks     — fewer unvisited vertices, each finding a hit sooner
```

They cross somewhere in the middle levels, which is the whole
paper.

Why it matters: the early exit is the load-bearing part, and it is
also the part with an algebraic precondition (Step 7) — you cannot
transplant it into a semiring that needs every contribution.

### Step 5 — what pull needs: the reverse graph and a dense frontier

> **In:** pull's inner loop (Step 4).
> **Out:** its two prerequisites priced in bytes, and the reason
> LAGraph makes one of them optional.

- **The reverse graph.** Pull scans *in*-edges, so it needs the
  transpose Aᵀ (equivalently CSC — the same edges indexed by
  destination instead of source). The paper is explicit about the
  bill (§IV): "If the graph is directed, the bottom-up step will
  require the inverse graph, **which could nearly double the
  graph's memory footprint**." (For an undirected graph, it says,
  "performing the bottom-up approach requires no modification to
  the graph data structures as both directions are already
  represented.") This is why LAGraph takes `G->AT` as an *optional*
  cached property and silently degrades to push-only without it —
  `LG_BreadthFirstSearch_SSGrB_template.c:18-22`, and the flag it
  computes at `:128`.
- **A dense frontier representation.** Pull tests "is u in the
  frontier?" once per probe, so membership must be O(1) — a
  **bitmap** (one bit per vertex) rather than the sparse list of
  vertex ids that push iterates. The paper (§V): "Different data
  structures are used since the frontiers are of radically
  different sizes, and the conversion costs are far less than the
  penalty of using the wrong data structure."

Price both on this topic's own RMAT scale-18 graph, whose figures
are in `topics/20-graphblas/notes.md:13` and `:35`
(n = 262,144, nnz = 2.0M):

```
 inputs: n = 262,144 vertices, m = 2.0e6 edges, 4-byte indices

 CSR alone      : 4·(n+1)  +  4·m   =  1.05 MB  +  8.0 MB  =  9.05 MB
 CSR + CSC (Aᵀ) : 2 × 9.05 MB                              = 18.10 MB
   → the paper's "nearly double", exactly

 sparse frontier at the apex (say 40% of n in it):
   4 bytes × 104,858                                       = 419 KB
 bitmap frontier, always:
   n / 8                                                   =  32 KB
   → and it fits in L2, so each membership test is a cache hit
     rather than a random 419 KB gather (topic 13's lesson)

 conversion cost per switch: one O(n) pass = 262,144 writes.
   At notes.md:35's measured 1.6 ns/edge for this graph's BFS, an
   O(n) pass is ≈ 0.4 ms against a 3.3 ms whole-search budget —
   about 13% of the search per switch. You can afford two switches.
   You cannot afford one per level.
```

Why it matters: the conversion is not free, and the size of that
13% is precisely why the switch heuristic needs hysteresis rather
than a single threshold (Step 6).

### Step 6 — the switch, as the paper actually states it

> **In:** push's m_f (Step 3), pull's shrinking cost (Step 4), the
> conversion tax (Step 5).
> **Out:** the paper's two thresholds with their exact constants and
> the section they come from — not the version most summaries give.

The paper defines three quantities in §V: **m_f** (edges to check
from the frontier), **n_f** (vertices in the frontier), and **m_u**
(edges to check from unexplored vertices). Fig. 7 is the control
algorithm, and it is a two-state machine, not a formula:

```
 Beamer §V, Fig. 7 — Control algorithm for hybrid algorithm

        m_f > C_TB & growing
   Top-Down ─────────────────────→ Bottom-Up
        ←─────────────────────
        n_f < C_BT & shrinking

   with, from §V:
     C_TB = m_u / α          (switch to bottom-up)
     C_BT = n   / β          (switch back to top-down)

   and, from §VI-B (the tuning sweeps, Figs. 8 and 9):
     α = 14        β = 24
```

Three things in that block are routinely gotten wrong, so read
them off the paper directly:

1. **α gates edges, β gates vertices.** m_f > m_u/α is a
   comparison between two *edge* counts. n_f < n/β is a comparison
   between two *vertex* counts. They are not the same test in
   different units.
2. **There is no `n_f > n/β₁` push→pull condition in the paper.**
   The push→pull test is the α test alone. (LAGraph adds a
   vertex-count test — Step 8 — but it is LAGraph's, not Beamer's.)
3. **`growing`/`shrinking` are separate conjuncts and they are
   worth measuring.** Fig. 7's caption: "Growing and shrinking
   refer to the frontier size, and although they are typically
   redundant, their inclusion yields a speedup of about 10%."

On the constants, §VI-B is candid about how much they matter:

> "Sweeping α across a wide range demonstrates that once α is
> sufficiently large (>12), BFS performance for many graphs is
> relatively insensitive to its value (Figure 8)… we select α = 14
> since it maximizes the average and minimum… even if a
> less-than-optimal α is selected, the hybrid-heuristic algorithm
> still executes within 15–20% of its peak performance on most
> graphs."

> "Tuning β is less important than tuning α. We select β = 24…
> The value of β has a smaller impact on overall performance
> because the majority of the runtime is taken by the middle steps
> when the frontier is at its largest."

And on what the whole thing buys, §VI-C, on the 16-core machine of
Table II, against two top-down baselines (Fig. 10):

> "The hybrid provides large speedups across all of the graphs,
> with an average speedup of 3.9 and a speedup no lower than 2.4.
> The on-line heuristic often obtains performance within 10% of the
> oracle."

Note the gap between §VI-C's 3.9× *speedup* and Step 2's 67×
*edge-check headroom*. §VI-D explains it and quantifies the
conversion: Fig. 13 plots speedup against edge-check reduction and
"the slope of a best-fit line is approximately 0.3", because "while
the bottom-up approach skips edges, it reduces the spatial locality
of the remaining memory accesses". Skipping an edge is worth about
a third of an edge.

Why it matters: 0.3 is the exchange rate between the algorithmic
win and the measured win, and it is the number that stops you
predicting a 67× speedup from a 67× work reduction.

### Step 7 — the linear-algebra translation: push = vxm, pull = mxv

> **In:** the two directions (Steps 3-4) and the switch (Step 6).
> **Out:** each direction as one GraphBLAS call, and the algebraic
> precondition the early exit needs to stay legal.

Davis states the correspondence in one paragraph
("Parallel GraphBLAS with OpenMP", CSC '20, §4.3), for matrices in
the default CSR format:

> "The basic operation of this algorithm computes Aᵀq where q is
> the queue of nodes in the current level. This can be done with
> `GrB_vxm(q,A)` = (qᵀA)ᵀ = Aᵀq, or by `GrB_mxv(B,q)` = Bq = Aᵀq,
> where B = Aᵀ is the explicit transpose of A. Both steps compute
> the same thing, just in a different way; **the first is a push
> step and the second is a pull step**."

Yang §4.1 gives the masked form of push: `f' = Aᵀf .* ¬v` — the
product, then filtered by "not yet visited". Yang §4.2 gives pull:
start *from* ¬v and look at each node's parents.

```
 push  = qᵀ · A     sparse vector × CSR  → SpMSpV, saxpy engine
 pull  = Aᵀ · q     CSR(Aᵀ) × vector     → masked SpMV, dot engine
 visited mask = the COMPLEMENTED structural mask (GrB_DESC_RSC)
 direction switch = which of the two calls you make this level
```

Which engine SuiteSparse actually picks for each, and why, is
`GB_AxB_dot2_control.c` and `GB_AxB_saxpy3.c` — walked in
[reading-suitesparse-internals.md](reading-suitesparse-internals.md).
Davis CSC '20 §3.1 states the intent directly: "By default,
GraphBLAS selects the masked-dot-product method for triangle
counting, LCC, **the pull phase of the push/pull BFS**… The
saxpy-based Gustavson or heap-based methods are used in the
K-truss, **the push phase of the push/pull BFS**…"

The algebraic precondition. Pull's `break` says: *having found one
witness, stop looking for more.* That is only sound if the additive
monoid is **idempotent and selective** — if combining more
witnesses cannot change the answer. LAGraph's level-only BFS uses
`LAGraph_any_one_bool`, which `LAGraph.h:825-829` documents as
"using the `GrB_MIN_MONOID_T` for non-boolean types or
`GrB_LOR_MONOID_BOOL` for boolean, and the `GrB_ONEB_T`
multiplicative op". For booleans that is (OR, true): once you have
a `true`, every further `true` is absorbed, so stopping is not an
approximation. Under PLUS you must visit every contribution, and
the same `break` silently computes the wrong number — which is why
PageRank (Step 8's `LAGr_PageRankGAP.c`) never gets an early exit.

Why it matters: "stop at the first hit" is a *semiring* property,
not a coding trick. Getting this wrong is how a fast BFS becomes a
wrong PageRank.

### Step 8 — what LAGraph actually shipped, and where it diverges

> **In:** the paper's Fig. 7 machine and constants (Step 6).
> **Out:** the shipped constants, the shipped control flow — which
> is not Fig. 7 — and the arithmetic that explains both changes.

Here is the whole switch as LAGraph ships it. Read it against
Fig. 7 and note where the shapes differ:

```c
// LG_BreadthFirstSearch_SSGrB_template.c — the constants, 183-188
   183      GrB_Index nq = 1 ;          // number of nodes in the current level
   184      double alpha = 8.0 ;
   185      double beta1 = 8.0 ;
   186      double beta2 = 512.0 ;
   187      int64_t n_over_beta1 = (int64_t) (((double) n) / beta1) ;
   188      int64_t n_over_beta2 = (int64_t) (((double) n) / beta2) ;
```

```c
// LG_BreadthFirstSearch_SSGrB_template.c — the switch, 243-294.
// Three mutually exclusive branches, not one disjunction.
   243              if (do_push)
   244              {
   245                  // check for switch from push to pull
   246                  bool growing = nq > last_nq ;
   247                  bool switch_to_pull = false ;
   248                  if (edges_unexplored < n)
   249                  {
   250                      // very little of the graph is left; disable the pull
   251                      push_pull = false ;
   252                  }
   253                  else if (any_pull)
   254                  {
   ...                  // (comment 255-260: after a pull phase the edge count is
   ...                  //  no longer tracked, so fall back on frontier size)
   261                      switch_to_pull = (growing && nq > n_over_beta1) ;
   262                  }
   263                  else
   264                  {
   ...                  // w<q> = Degree ; then sum it (268-274)
   275                      edges_unexplored -= edges_in_frontier ;
   276                      switch_to_pull = growing &&
   277                          (edges_in_frontier > (edges_unexplored / alpha)) ;
   278                  }
   ...
   285              else
   286              {
   287                  // check for switch from pull to push
   288                  bool shrinking = nq < last_nq ;
   289                  if (shrinking && (nq <= n_over_beta2))
   290                  {
   291                      do_push = true ;
   292                  }
   293              }
```

Four divergences from Fig. 7, each verifiable in the block above:

1. **Direction optimization is switched off entirely, not switched
   over, when the graph is nearly exhausted** — `:248-251`. Fig. 7
   has no such state.
2. **The α test and the β₁ test are in mutually exclusive
   branches** (`:253-262` versus `:263-278`), selected by whether a
   pull phase has already happened. It is *not*
   `α-test OR β₁-test`. The β₁ path exists only because
   `edges_unexplored` stops being maintained once pull runs — the
   comment at `:255-260` says exactly that.
3. **`edges_unexplored` is maintained, not recomputed.** `:268-269`
   masks the degree vector by the frontier, `:273-274` reduces it,
   `:275` subtracts. That is a masked assign plus a reduce per
   level — the paper's own §VI-D notes "conversion and m_f
   calculation take a non-negligible fraction of the runtime".
   Note the ordering: the subtraction at `:275` happens *before*
   the comparison at `:276-277`, so α is applied to the count that
   already excludes this level.
4. **The constants all moved.** Work out what they mean on this
   topic's RMAT scale-18 graph (n = 262,144, m = 2.0e6, from
   `notes.md:13`):

```
                        threshold          value at n = 262,144
 Beamer α = 14    m_f > m_u/14        =  7.1% of remaining edges
 LAGraph α = 8    m_f > m_u/8         = 12.5% of remaining edges
   → LAGraph sets a HIGHER bar, so it switches to pull LATER

 Beamer β  = 24   n_f < n/24          =  10,922 vertices
 LAGraph β₂= 512  n_q ≤ n/512         =     512 vertices
   → LAGraph waits until the frontier is 21× smaller before
     switching back to push

 LAGraph β₁= 8    n_q > n/8           =  32,768 vertices
   → only consulted on a second push→pull switch, which the
     comment at :258 calls "unlikely"
```

Why β₂ moved from 24 to 512 is question 5, and the arithmetic in
Step 5 is the hypothesis: switching back costs an O(n) frontier
rebuild (≈ 0.4 ms of a 3.3 ms search), so a switch that only saves
the last few hundred vertices' worth of pull scanning cannot pay
for itself. 512 buys about 21× more certainty that the tail is
genuinely over.

Why it matters: three published statements of "the" heuristic
(Beamer's Fig. 7, Yang's §6.3 with α = β = 0.01, LAGraph's
`:243-294`) disagree in shape as well as in constants. Any claim
about "the switch condition" has to name which one.

## How to read the paper (with the concepts in hand)

Read in this order; each text is the next one's input.

- **Beamer §III** — the waste argument and the kron27 arithmetic of
  Step 2. Fig. 3 (absolute) and Fig. 4 (percentage) are the same
  data; Fig. 4 is the one to internalise. Compare its per-level
  shape with `gb_bench`'s `--trace` output on an RMAT graph.
- **Beamer §IV** — pull in one page, Fig. 5's `break` included, plus
  the two costs (atomics removed, transpose required).
- **Beamer §V and Fig. 7** — the control machine of Step 6. Read
  Fig. 7's *caption* as carefully as the box: the 10% for
  growing/shrinking is in it.
- **Beamer §VI-B** (Figs. 8-9, the α/β sweeps) then **§VI-C**
  (Fig. 10, the speedups) then **§VI-D** (Fig. 13, the 0.3 slope) —
  in that order, because §VI-D is what stops you over-reading
  §VI-C.
- **Yang, Buluç & Owens, ICPP '18, §4 and §5** — the linear-algebra
  translation (§4.1 push, §4.2 pull) and the ablation of Step 4's
  Table 2. §6.3 restates Beamer's heuristic and then replaces it
  with α = β = 0.01 on a *vertex* ratio, which is a third distinct
  formulation.
- **LAGraph `LG_BreadthFirstSearch_SSGrB_template.c:183-188` and
  `:241-296`** — Step 8. Read it last, with Fig. 7 beside it, and
  find the four divergences yourself before re-reading Step 8.

## Questions for notes.md

1. Reproduce Beamer's waste argument from `gb_bench`'s per-level
   trace: at the peak level, what fraction of push's edge checks
   found an already-visited target (count them — add a counter to
   the stub)? Split the failures into the paper's categories
   (failed child / peer / valid parent, §III) and compare the shape
   against Fig. 4.
2. Why does pull's early exit require the ANY (or OR) monoid
   algebraically — what property (idempotent, any-witness-suffices)
   makes stopping sound, and which semirings BREAK it (PLUS: you
   need every contribution — BFS parent vs PageRank)?
3. Road network vs RMAT: predict which levels (if any) go pull on
   each, from diameter and degree distribution alone. Then check
   with `gb_bench --trace`. Use `notes.md:35`'s path-100K figure
   (2041 µs, ~20 ns/hop) as the road-like case.
4. The reverse graph doubles memory. FalkorDB keeps BOTH (the
   transposed delta trio, `delta_matrix.h:20-22`) — for which query
   shapes besides BFS pull is Aᵀ load-bearing (incoming-edge
   traversals `<-[]-`)?
5. LAGraph's β₂ = 512 (vs the paper's β = 24) makes the pull→push
   switch-back very late. Hypothesize why, using Step 5's
   conversion arithmetic (switch-back rebuilds a SPARSE frontier
   from a bitmap — an O(n) scan), and design the experiment that
   would confirm it.

## Done when

Answer each before unfolding it.

- [ ] You can state, with the paper's own number, how much of a top-down BFS's work is avoidable in principle.

  <details><summary>Answer</summary>

  About 98.5%. §III: on `kron27` (128M vertices, 2B undirected
  edges, Fig. 3 caption) the BFS tree contains 63,036,116 vertices,
  so at least 63,036,115 edges must be considered — "about 1/67th
  of all the edge examinations that would happen during a top-down
  traversal". The 67 decomposes as roughly degree 16 × 2 (each
  undirected edge is checked from both endpoints) × ~2 (zero-degree
  vertices are outside the component, raising the effective degree
  of those inside).

  The measured speedup is far smaller — §VI-C's average 3.9 — and
  §VI-D explains the gap: Fig. 13's best-fit slope of speedup
  against edge-check reduction is "approximately 0.3", because
  bottom-up "reduces the spatial locality of the remaining memory
  accesses". Skipping an edge is worth about a third of an edge.

  </details>

- [ ] You can explain why push dies mid-search, in terms of edges checked per useful discovery.

  <details><summary>Answer</summary>

  Push's cost per level is m_f, the sum of the frontier's
  out-degrees (§V), and it is paid whether or not a check
  discovers anything. On a small-world graph the frontier grows
  exponentially for two or three levels and then holds most of the
  graph, so m_f peaks exactly when there is almost nothing left to
  discover. Fig. 4 shows the composition at that peak: claimed
  children collapse and **peer** edges dominate — both endpoints
  are already in the frontier, so the check cannot succeed.

  There is a second cost §IV names: "there could be multiple
  parallel writers to the same child, so atomic operations are
  needed to ensure mutual exclusion". The failed children are also
  the contended CAS operations.

  </details>

- [ ] You can explain what pull inverts, and say which of pull's two ingredients the measurements say actually pays.

  <details><summary>Answer</summary>

  Pull inverts the direction of the scan: instead of each frontier
  vertex scanning its out-edges for unvisited children, each
  *unvisited* vertex scans its in-edges for a parent in the
  frontier, and stops at the first one (§IV, Fig. 5's `break`).

  The inversion alone is nearly worthless. Yang, Buluç & Owens
  Table 2, on `kron_g500-logn21`, ablates the construction:
  "change of direction" is a **1.08×** standalone speedup, while
  "early exit" is **4.02×** — and their §5.3 says early-exit
  "yielded the greatest speed-up" of the five optimizations. The
  `break` is the mechanism; the direction flip is what makes the
  `break` possible. An implementation with the flip and without the
  exit has kept 8% of the idea.

  </details>

- [ ] You can price pull's two prerequisites in bytes on this topic's scale-18 graph.

  <details><summary>Answer</summary>

  n = 262,144, m = 2.0e6 (`notes.md:13`), 4-byte indices.

  *Reverse graph*: CSR alone is 4·(n+1) + 4·m = 1.05 + 8.0 =
  9.05 MB; keeping Aᵀ as well is 18.10 MB — the paper's §IV "could
  nearly double the graph's memory footprint", exactly. LAGraph
  therefore treats `G->AT` as optional and falls back to push-only
  (`LG_BreadthFirstSearch_SSGrB_template.c:18-22`, flag at `:128`).

  *Dense frontier*: a bitmap is n/8 = 32 KB regardless of frontier
  size, against 419 KB for a 4-byte id list at a 40%-of-n apex.
  The bitmap's real win is not the 13× — it is that 32 KB stays in
  L2, so each membership test is a cache hit.

  *Conversion*: one O(n) pass per switch. At `notes.md:35`'s
  measured ~1.6 ns/edge on this graph, that is ≈ 0.4 ms against a
  3.3 ms search — about 13% per switch. Two switches are
  affordable; one per level is not, which is what the hysteresis in
  Fig. 7 exists to prevent.

  </details>

- [ ] You can write down Beamer's two thresholds exactly, with their constants and the section they come from — and say what the third conjunct in each is worth.

  <details><summary>Answer</summary>

  §V and Fig. 7. Top-down → bottom-up when **m_f > C_TB and
  growing**, where **C_TB = m_u/α**. Bottom-up → top-down when
  **n_f < C_BT and shrinking**, where **C_BT = n/β**. m_f is the
  edges to check from the frontier, m_u the edges to check from
  unexplored vertices, n_f the vertices in the frontier.

  Constants from §VI-B's sweeps: **α = 14**, **β = 24**. §VI-B adds
  that performance is "relatively insensitive" to α once α > 12,
  and a mistuned α still runs "within 15–20% of its peak".

  The `growing`/`shrinking` conjuncts are worth ~10%: Fig. 7's
  caption says they are "typically redundant" but "their inclusion
  yields a speedup of about 10%".

  Note what is *not* there: the paper has no `n_f > n/β₁` push→pull
  condition. α gates an edge-count comparison; β gates a
  vertex-count comparison.

  </details>

- [ ] You can write the algebraic translation — push is `vxm`, pull is `mxv` — and name the property that makes the early exit legal.

  <details><summary>Answer</summary>

  Davis, CSC '20 §4.3, for CSR matrices: `GrB_vxm(q,A)` = (qᵀA)ᵀ =
  Aᵀq is the **push** step; `GrB_mxv(B,q)` with B = Aᵀ is the
  **pull** step. Both compute Aᵀq. Yang §4.1 gives push's masked
  form as `f' = Aᵀf .* ¬v`; the "not yet visited" filter is a
  complemented structural mask, `GrB_DESC_RSC` in LAGraph
  (template `:307` for push, `:313` for pull).

  The early exit is legal when the additive monoid is idempotent
  and selective, so that additional witnesses cannot change the
  result. LAGraph's level-only BFS uses `LAGraph_any_one_bool`,
  documented at `LAGraph.h:825-829` as `GrB_LOR_MONOID_BOOL` with
  `GrB_ONEB_BOOL` — (OR, true). Once a `true` has been produced,
  every further `true` is absorbed. Under PLUS every contribution
  matters and the same `break` computes a wrong number, which is
  why `LAGr_PageRankGAP.c:135-136`'s `PLUS_SECOND` mxv is dense and
  unmasked.

  </details>

- [ ] You can state how LAGraph's shipped switch differs in *shape* — not just in constants — from Fig. 7.

  <details><summary>Answer</summary>

  Three shape differences, all in
  `LG_BreadthFirstSearch_SSGrB_template.c:243-294`.

  First, a state Fig. 7 does not have: at `:248-251`, if
  `edges_unexplored < n`, direction optimization is **disabled
  outright** (`push_pull = false`) rather than switched.

  Second, the α test and the β₁ test are in **mutually exclusive
  branches**, not a disjunction. `:253-262` (frontier-size test
  `growing && nq > n_over_beta1`) runs only when `any_pull` is
  already true; `:263-278` (the edge test
  `growing && edges_in_frontier > edges_unexplored/alpha`) runs
  only the first time. The comment at `:255-260` gives the reason:
  after a pull phase, `edges_unexplored` is no longer tracked, so
  the edge test has no valid input.

  Third, `edges_unexplored` is *maintained* — masked assign at
  `:268-269`, reduce at `:273-274`, subtract at `:275` — and the
  subtraction happens before the comparison at `:276-277`, so α is
  applied to a remainder that already excludes the current level.

  Constants: α 14 → 8 (a higher bar, so pull starts later: 12.5% of
  remaining edges rather than 7.1%), β 24 → β₂ 512 (n/512 = 512
  vertices at n = 262,144, against n/24 = 10,922 — a 21× later
  switch-back), plus a new β₁ = 8 used only on the unlikely second
  switch.

  </details>

- [ ] You wrote answers to all five questions in notes.md, and can reproduce Beamer's waste argument from `gb_bench`'s per-level trace once `bfs_diropt` runs.

  <details><summary>Answer</summary>

  The trace has to show, per level: frontier size, edges inspected,
  and the outcome split. The check on your implementation is that
  the peak level's *claimed child* fraction collapses toward zero
  while *peer* rises to dominate — Fig. 4's shape. If your trace
  shows failed children dominating at the apex rather than peers,
  the graph is not small-world enough for the paper's story, which
  is itself the finding (question 3's road-network case).

  On the two flat predictions in `notes.md:52-55`: pull should not
  fire at all on path-100K, because the frontier never grows past
  a handful of vertices, so `growing && edges_in_frontier >
  edges_unexplored/8` is never satisfied — and `:248-251` will
  disable direction optimization outright as the path drains.
  A pull-only run on that graph is the catastrophe case: n probes
  per level × 100K levels.

  </details>

## References

**Papers**

- Beamer, Asanović, Patterson — **"Direction-Optimizing
  Breadth-First Search"**, SC '12. Read §III (waste, Figs. 3-4),
  §IV (bottom-up, Fig. 5), §V (the hybrid, Fig. 7), §VI-B (α/β
  sweeps, Figs. 8-9), §VI-C (speedups, Fig. 10), §VI-D (Fig. 13's
  0.3 slope). Every paper figure in this guide is from that PDF.
- Yang, Buluç, Owens — **"Implementing Push-Pull Efficiently in
  GraphBLAS"**, ICPP '18,
  [doi:10.1145/3225058.3225122](https://doi.org/10.1145/3225058.3225122)
  (Article 89; preprint
  [arXiv:1804.03327](https://arxiv.org/abs/1804.03327)). §4 is the
  vxm/mxv translation, §5 the five-optimization ablation whose
  Table 2 is quoted in Step 4, §6.3 their own α = β = 0.01
  heuristic. LAGraph's template cites the DOI form at its `:26-29`.
- Davis — **"Parallel GraphBLAS with OpenMP"**, CSC '20 (SIAM
  Workshop on Combinatorial Scientific Computing). §4.3 is the
  one-paragraph push = `vxm` / pull = `mxv` statement quoted in
  Step 7; §3.1 says which engine SuiteSparse picks for each.

**Code**

- [LAGraph](https://github.com/GraphBLAS/LAGraph) at `e2539e2`,
  `src/algorithm/template/LG_BreadthFirstSearch_SSGrB_template.c`
  (355 lines) — the shipped algorithm, walked in
  [reading-lagraph.md](reading-lagraph.md).

| File | Lines | What |
|------|-------|------|
| `LG_BreadthFirstSearch_SSGrB_template.c` | 18-22 | `G->AT` and `G->out_degree` optional; degrade to push-only |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 128 | `push_pull` computed from what the graph offers |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 161 | `LAGraph_any_one_bool` — the level-only semiring |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 183-188 | α = 8, β₁ = 8, β₂ = 512 and the two derived bounds |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 248-251 | direction optimization disabled when `edges_unexplored < n` |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 253-262 | the β₁ branch, taken only after a pull phase |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 263-278 | the α branch: maintain `edges_unexplored`, then compare |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 288-289 | pull → push: `shrinking && nq <= n/β₂` |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 303-308 | push: `LG_SET_FORMAT_HINT(q, LG_SPARSE)` then `GrB_vxm` |
| `LG_BreadthFirstSearch_SSGrB_template.c` | 309-314 | pull: `LG_SET_FORMAT_HINT(q, LG_BITMAP)` then `GrB_mxv` |
| `include/LAGraph.h` | 825-829 | `LAGraph_any_one_bool` = (LOR, ONEB) for booleans |

**Measured, in this repo**

- `topics/20-graphblas/notes.md:13` — RMAT scale 18: n = 262,144,
  nnz = 2.0M, the graph Steps 5 and 8 do arithmetic on.
- `topics/20-graphblas/notes.md:35` — BFS scalar oracle: rmat18
  3308 µs (~1.6 ns/edge), path-100K 2041 µs (~20 ns/hop).
