# LAGraph: graph algorithms as executable linear algebra

LAGraph is the "standard library" of GraphBLAS — and each of the
three algorithms read here (BFS, triangle counting, PageRank) is a
few GrB calls whose entire performance story lives in which
engine/format/mask they trigger underneath. This chapter builds
each algorithm as a concept first — the loop, the semiring trick,
the six-spellings observation, the bandwidth argument — then hands
you the file:line anchors to watch the previous chapters' machinery
get exercised end to end. This is also where M20's parity targets
come from.

Every anchor below is **LAGraph at commit `e2539e2`** (the pin in
`resources/codebases.md`), quoted with the line numbers the code
occupies at that commit. The pinned tree's `src/algorithm/`
directory contains exactly these algorithms: `LAGr_Betweenness`,
`LAGr_BreadthFirstSearch`, `LAGr_ConnectedComponents`,
`LAGr_PageRank`, `LAGr_PageRankGAP`,
`LAGr_SingleSourceShortestPath`, `LAGr_TriangleCount`,
`LAGraph_TriangleCount`, `LG_BreadthFirstSearch_SSGrB`,
`LG_BreadthFirstSearch_vanilla`, `LG_CC_Boruvka`, `LG_CC_FastSV6`
and `LG_CC_FastSV7` — plus the two BFS templates. If you go looking
for k-truss or Louvain at this pin, they are not there.

## The problem in one sentence

Once traversal is matrix algebra, a whole graph algorithm collapses
to a handful of library calls — LAGraph's direction-optimizing BFS
does its per-level work in exactly **two** lines of algebra
(`:307` and `:313`) — and its performance is decided entirely by
*which* call, *which* semiring, and *which* mask each line picks.

## The concepts, step by step

### Step 1 — an algorithm is a loop around one line of algebra

> **In:** the GraphBLAS operations from
> [reading-davis-toms19.md](reading-davis-toms19.md).
> **Out:** the four dials — call, semiring, mask, descriptor — that
> every later step turns, and what each one means.

LAGraph's algorithms all have the same skeleton: some scalar
bookkeeping, then one GrB call per iteration that does all the
real work. Four things are chosen at each such call, and they are
the whole vocabulary of this chapter:

- **The call.** `GrB_vxm` is sparse row-vector × matrix — an
  **SpMSpV** (sparse-vector times sparse-matrix) when the vector is
  sparse. `GrB_mxv` is matrix × column vector — an **SpMV**
  (sparse-matrix times vector). They are the same product in
  different index orders, so which one you write decides which
  engine runs.
- **The semiring.** A pair (additive monoid, multiplicative
  operator). It decides *what data moves*: `PLUS_TIMES` moves
  values, `ANY_SECONDI` moves indices, `LAGraph_any_one_bool` moves
  nothing but structure.
- **The mask.** A vector or matrix whose *structure* scopes the
  output. With the complement flag it means "write only where the
  mask has no entry".
- **The descriptor.** `GrB_DESC_RSC` = **R**eplace the output +
  use the **S**tructure of the mask (ignore its values) + **C**
  omplement it. `Include/GraphBLAS.h:666` in SuiteSparse defines it
  as exactly `GrB_REPLACE + GrB_STRUCTURE + GrB_COMP`. So
  `GrB_DESC_RSC` on a BFS frontier reads: *write only where the
  vertex has not yet been visited, and discard whatever was in the
  frontier before.*

Everything the previous chapters built — engine dispatch, format
switching, masks as outer loops — fires inside that one line,
invisibly. Reading LAGraph is learning to see it.

Why it matters: the four dials are the entire performance surface.
Nothing else in an LAGraph algorithm costs anything.

### Step 2 — the BFS level, in two lines

> **In:** the four dials (Step 1) and the direction switch from
> [reading-beamer-sc12.md](reading-beamer-sc12.md).
> **Out:** the two lines that do all the work, and the format hint
> that decides which engine each one reaches.

Here is the payload of the whole algorithm — everything else in the
343-line template is bookkeeping around it:

```c
// LG_BreadthFirstSearch_SSGrB_template.c — the level, 302-314.
// The comment on 305 and 311 is LAGraph's own naming of the engines.
   302          // mask is pi if computing parent, v if computing just level
   303          if (do_push)
   304          {
   305              // push (saxpy-based vxm):  q'{!mask} = q'*A
   306              GRB_TRY (LG_SET_FORMAT_HINT (q, LG_SPARSE)) ;
   307              GRB_TRY (GrB_vxm (q, mask, NULL, semiring, q, A, GrB_DESC_RSC)) ;
   308          }
   309          else
   310          {
   311              // pull (dot-product-based mxv):  q{!mask} = AT*q
   312              GRB_TRY (LG_SET_FORMAT_HINT (q, LG_BITMAP)) ;
   313              GRB_TRY (GrB_mxv (q, mask, NULL, semiring, AT, q, GrB_DESC_RSC)) ;
   314          }
```

Read `:306` and `:312` carefully, because this is where a common
claim is wrong. **The frontier's format is not left to
SuiteSparse's auto-conform heuristic — LAGraph sets it
explicitly**, sparse before every push, bitmap before every pull.
That hint is not decoration: it is what makes the engine dispatch
come out right. `GB_AxB_dot2_control.c:26-30` returns "use dot"
immediately if either operand is bitmap or full, so `LG_BITMAP` at
`:312` is what steers the pull into the dot engine.

And note what the pull does *not* reach.
`GB_AxB_dot3_control` (`GB_mxm.h:235-243`) requires
`M != NULL && !Mask_comp && (M sparse or hypersparse)`. The pull's
descriptor is `GrB_DESC_RSC`, so `Mask_comp` is **true** — dot3 is
structurally ineligible here. A complemented-mask product on a
bitmap operand is dot2's case, which is what
`GB_AxB_dot2_control.c:10` documents. (Guides that say "pull uses
dot3" have skipped the descriptor.)

The mask itself is chosen once, at `:200`:
`GrB_Vector mask = (compute_parent) ? pi : v` — the output vector
*is* the visited set. There is no separate `visited` bitmap,
because "has a parent assigned" and "has been visited" are the same
predicate.

Why it matters: two lines, four dials each, and the format hint on
the line *before* the call is what decides which of SuiteSparse's
engines the call lands in.

### Step 3 — the semiring trick: ANY_SECONDI, and its cheaper sibling

> **In:** the semiring dial (Step 1).
> **Out:** two semirings, what each moves per level, and the
> algebraic property that lets both skip a comparison.

`GxB_ANY_SECONDI_INT32/64` computes the parent vector with zero
comparisons:

```c
// LG_BreadthFirstSearch_SSGrB_template.c — semiring selection, 130-165
   130      // determine the semiring type
   131      GrB_Type int_type = (n > INT32_MAX) ? GrB_INT64 : GrB_INT32 ;
   ...
   135      bool many_expected = (nvals >= n) ;
   ...
   138      if (compute_parent)
   139      {
   140          // use the ANY_SECONDI_INT* semiring: either 32 or 64-bit depending on
   141          // the # of nodes in the graph.
   142          semiring = (n > INT32_MAX) ?
   143              GxB_ANY_SECONDI_INT64 : GxB_ANY_SECONDI_INT32 ;
   ...
   147          if (many_expected)
   148          {
   149              GRB_TRY (LG_SET_FORMAT_HINT (pi, LG_BITMAP + LG_FULL)) ;
   150          }
   ...
   158      else
   159      {
   160          // only the level is needed, use the LAGraph_any_one_bool semiring
   161          semiring = LAGraph_any_one_bool ;
```

The multiply op **SECONDI** returns the *index* of the second
operand's entry — the parent's id — rather than any stored value.
The additive monoid **ANY** is a reduction allowed to keep
whichever value arrives first; since any parent is a valid BFS
tree, no min, no compare, no tie-break is needed. This is
Gunrock's benign CAS race (topic 18) expressed as algebra rather
than as a data race you argue is harmless: ANY is associative,
commutative and idempotent, so the nondeterminism is
*definitionally* fine.

The level-only path at `:161` uses a different semiring, and the
name is worth getting right. `LAGraph_any_one_bool` is not
"ANY_PAIR". `include/LAGraph.h:825-829` documents the family:

> "`LAGraph_any_one_T`: using the `GrB_MIN_MONOID_T` for
> non-boolean types or `GrB_LOR_MONOID_BOOL` for boolean, and the
> `GrB_ONEB_T` multiplicative op. These semirings are very useful
> for unweighted graphs, or for algorithms that operate only on the
> sparsity structure of unweighted graphs."

So for booleans it is **(OR, true)**. The multiply produces the
constant `true` regardless of operands, which means the matrix's
*values are never read* — only its pattern. That is the
"structure-only" optimization Yang, Buluç & Owens measured at
**1.62×** standalone (ICPP '18, Table 2).

Price what each semiring moves per level, on this topic's RMAT
scale-18 graph (`notes.md:13`: n = 262,144, nnz = 2.0M):

```
 inputs: n = 262,144 < INT32_MAX, so :131 and :142 pick the 32-bit forms
         a level with nq = 40,000 frontier vertices, mean degree
         2.0e6 / 262,144 = 7.63

 parent BFS, ANY_SECONDI_INT32:
   values written per level  = nq_next entries × 4 bytes
   values READ from A        = 0  (SECONDI reads the index, not A.x)
   → A.x need never be touched; only A.p and A.i are streamed

 level BFS, LAGraph_any_one_bool (LOR, ONEB):
   values written per level  = nq_next entries × 1 byte
   values read from A        = 0
   → the same saving, and a 4× smaller output vector

 the edges streamed are the same either way:
   40,000 × 7.63 × 4 bytes of column indices = 1.22 MB per level
```

The semiring choice does not change how many edges you touch. It
changes whether you touch `A.x` at all — and on an unweighted graph
`A.x` can be *iso* (one stored value for the whole matrix), so
touching it is pure waste.

Why it matters: the semiring moved a correctness argument ("is the
race benign?") into the algebra, and it decides what data moves:
indices, not values.

### Step 4 — triangle counting: six spellings of one mask

> **In:** the mask dial (Step 1) and the engine split (Step 2).
> **Out:** six algebraically identical expressions with six cost
> profiles, and LAGraph's own statement of which wins where.

```c
// LAGr_TriangleCount.c — the six formulations, 27-47 (comment block)
    27  // One of 6 methods are used, defined below where L and U are the strictly
    28  // lower and strictly upper triangular parts of the symmetrix matrix A,
    29  // respectively.  Each method computes the same result, ntri:
    30  //
    31  //  0:  default:    use the default method (currently method Sandia_LUT)
    32  //  1:  Burkhardt:  ntri = sum (sum ((A^2) .* A)) / 6
    33  //  2:  Cohen:      ntri = sum (sum ((L * U) .* A)) / 2
    34  //  3:  Sandia_LL:  ntri = sum (sum ((L * L) .* L))
    35  //  4:  Sandia_UU:  ntri = sum (sum ((U * U) .* U))
    36  //  5:  Sandia_LUT: ntri = sum (sum ((L * U') .* L)).  Note that L=U'.
    37  //  6:  Sandia_ULT: ntri = sum (sum ((U * L') .* U)).  Note that U=L'.
    ...
    43  // The Sandia_* methods all tend to be faster than the Burkhardt or Cohen
    44  // methods.  For the largest graphs, Sandia_LUT tends to be fastest, except for
    45  // the GAP-urand matrix, where the saxpy-based Sandia_LL method (L*L.*L) is
    46  // fastest.  For many small graphs, the saxpy-based Sandia_LL and Sandia_UU
    47  // methods are often faster that the dot-product-based methods.
```

All six compute the same count. They differ only in which mxm
engine runs and how much the mask prunes:

- **`.* L` masks the OUTPUT to the lower triangle.** In
  SuiteSparse, a non-complemented sparse mask on `C = A*B'` is
  precisely dot3's case (`GB_mxm.h:235-243`), and dot3 allocates
  `C` with exactly `nnz(M)` entries —
  `GB_AxB_dot3.c:126` computes `mnz = GB_nnz(M)` and `:171` sets
  `cnz = mnz`. The mask is not a filter applied afterwards; it is
  the loop bound.
- **`L*L` versus `L*U'`** is saxpy versus dot, and the comment at
  `:43-47` gives LAGraph's own measured verdict rather than a rule:
  LUT (dot) usually wins on the largest graphs, LL (saxpy) wins on
  GAP-urand, and LL/UU win on many small graphs. Uniform-random
  degrees flatten the hub problem — exactly the Gustavson-vs-hash
  tradeoff of
  [reading-gustavson-spgemm.md](reading-gustavson-spgemm.md).
- **Burkhardt divides by 6 and Cohen by 2**, because they count
  each triangle from every orientation; the Sandia forms divide by
  nothing, because the triangular masks already fix an orientation.
  That factor is the whole reason the Sandia forms are faster.

Note what the pinned source does *not* say. The comment claims a
performance ordering; it does not quantify it, and this repo has
not measured triangle counting through GraphBLAS. Do not carry a
"dot3 is 3× faster" number out of this step — carry the ordering
and go measure. (This topic's `notes.md:62` lists masked SpGEMM as
a *stretch* stub for exactly that reason.)

Why it matters: at this level "algorithm choice" has become "which
algebraic spelling triggers the best engine for this graph's degree
distribution" — six mathematically equal expressions, six cost
profiles, and the library ships all six because none of them wins
everywhere.

### Step 5 — PageRank (GAP variant): no mask, all bandwidth

> **In:** the mask dial (Step 1) and the six-spellings lesson
> (Step 4).
> **Out:** the algorithm with *nothing* to prune, and this topic's
> own measured bandwidth ladder as its cost model.

```c
// LAGr_PageRankGAP.c — prescale then iterate, 109-142
   109      // prescale with damping factor, so it isn't done each iteration
   110      // d = d_out / damping ;
   111      GRB_TRY (GrB_Vector_new (&d, GrB_FP32, n)) ;
   112      GRB_TRY (GrB_apply (d, NULL, NULL, GrB_DIV_FP32, d_out, damping, NULL)) ;
   ...
   119      GRB_TRY (GrB_eWiseAdd (d, NULL, NULL, GrB_MAX_FP32, d1, d, NULL)) ;
   ...
   126      for ((*iters) = 0 ; (*iters) < itermax && rdiff > tol ; (*iters)++)
   127      {
   128          // swap t and r ; now t is the old score
   129          GrB_Vector temp = t ; t = r ; r = temp ;
   130          // w = t ./ d
   131          GRB_TRY (GrB_eWiseMult (w, NULL, NULL, GrB_DIV_FP32, t, d, NULL)) ;
   132          // r = teleport
   133          GRB_TRY (GrB_assign (r, NULL, NULL, teleport, GrB_ALL, n, NULL)) ;
   134          // r += A'*w
   135          GRB_TRY (GrB_mxv (r, NULL, GrB_PLUS_FP32, LAGraph_plus_second_fp32,
   136              AT, w, NULL)) ;
   ...
   142          GRB_TRY (GrB_reduce (&rdiff, NULL, GrB_PLUS_MONOID_FP32, t, NULL)) ;
   143      }
```

Count the `NULL`s on line 135. The mask argument is `NULL`. The
descriptor is `NULL`. Every vertex contributes every iteration, so
there is nothing for masks or sparsity to skip. PageRank is the
SpMV bandwidth benchmark — `gb_bench`'s spmv lane *is* this
algorithm's inner loop. It measures your memory system, not your
cleverness.

Which makes this topic's own measured ladder its cost model. Use
`notes.md:9-14` for the per-scale numbers:

| scale | n | nnz | µs | GB/s |
|---|---|---|---|---|
| 14 | 16K | 120K | 146 | 19.1 |
| 16 | 65K | 495K | 617 | 18.6 |
| 18 | 262K | 2.0M | 2547 | 18.3 |
| 20 | 1.05M | 8.2M | 11958 | 15.8 |

Now predict a PageRank iteration from first principles and check
it against that table:

```
 inputs: scale 18 — n = 262,144, nnz = 2.0e6 (notes.md:13)
         CSR with 4-byte column indices, 4-byte f32 values

 bytes touched by ONE mxv at :135:
   A.p    4 × (n+1)                     =   1.05 MB
   A.i    4 × nnz                       =   8.00 MB
   A.x    4 × nnz                       =   8.00 MB
   w gathers: nnz random 4-byte reads   =   8.00 MB (worst case, no reuse)
   r writes  4 × n                      =   1.05 MB
                                          ----------
   compulsory (A.p + A.i + A.x + r)     =  18.10 MB

 predicted time at notes.md:13's measured 18.3 GB/s:
   18.10 MB / 18.3 GB/s                 =   0.99 ms

 measured for the SpMV lane at scale 18: 2547 µs = 2.55 ms
   → 2.6× the compulsory-traffic prediction

 the gap IS the gather: notes.md:16-18 attributes the ~16-19 GB/s
 (against topic 0/13's ~30 GB/s streaming baseline) to the random
 x-gathers, "RMAT colidx sprays across the vector"
```

That 2.6× is the thing to remember. A PageRank iteration is not
"one pass over the matrix"; it is one pass over the matrix plus a
random walk through the vector, and on a scale-free graph the
second term dominates.

One honesty note before you quote a headline number. This topic
reports the SpMV decay two ways: `FINDINGS.md` row 20 says
**20.7 → 12.3 GB/s**, and `notes.md:11-14`'s table says
**19.1 → 15.8 GB/s** over the same scale 14 → 20 span. They are not
the same measurement and this guide does not blend them: the
per-scale arithmetic above uses `notes.md`, and any headline
citation should say `FINDINGS.md:38`. Reconciling the two is real
work someone should do; see the note in the report at the end of
this topic.

Finally, what the GAP variant deliberately gets wrong — read the
header before you trust the output:

```c
// LAGr_PageRankGAP.c — the disclaimer, 20-29
    20  // PageRank for the GAP benchmark (only).  Do not use in production.
    ...
    24  // ...  The GAP specification
    25  // ignores dangling nodes (nodes with no outgoing edges, also called sinks),
    26  // and thus shouldn't be used in production.  This method is for the GAP
    27  // benchmark only.  See LAGr_PageRank for a method that
    28  // handles sinks correctly.  This method does not return a centrality metric
    29  // such that sum(centrality) is 1, if sinks are present.
```

A benchmark-vs-correctness tension to remember for topic 22: the
fastest published implementation of an algorithm is sometimes
computing a slightly different function.

Why it matters: the algorithm with no mask is the one whose runtime
you can predict from bytes, which makes it the only one in this
chapter you can hold the memory system accountable for.

### Step 6 — API design: pull is the caller's bill to pay

> **In:** pull's transpose requirement
> ([reading-beamer-sc12.md](reading-beamer-sc12.md), Step 5).
> **Out:** where LAGraph puts that decision, and what changes when
> the storage layer has already paid it.

```c
// LG_BreadthFirstSearch_SSGrB_template.c — the optional inputs, 18-22 and 128
    18  // This is an Advanced algorithm.  G->AT and G->out_degree are required for
    19  // this method to use push-pull optimization.  If not provided, this method
    20  // defaults to a push-only algorithm, which can be slower.  This is not
    21  // user-callable (see LAGr_BreadthFirstSearch instead).  G->AT and
    22  // G->out_degree are not computed if not present.
    ...
   128      bool push_pull = (Degree != NULL && AT != NULL) ;
```

Read `:22` twice: "**are not computed if not present**". The
library will not quietly build a transpose to make your call
faster. One boolean at `:128` and the whole Beamer machinery is
either armed or gone, decided entirely by what the caller handed
in.

(A footnote on provenance: the file's own reference block at
`:24-32` cites Yang, Buluç & Owens ICPP '18 and *"The GAP Benchmark
Suite", arXiv:1508.03619, 2015* — the latter is a **different**
Beamer/Asanović/Patterson paper from the SC '12 one that
[reading-beamer-sc12.md](reading-beamer-sc12.md) reads. The α/β
machinery comes from SC '12; the graph suite comes from the 2015
report.)

`LAGr_PageRankGAP.c:31-33` states the same policy from the other
side, with a shortcut: "The `G->AT` and `G->out_degree` cached
properties must be defined for this method. If G is undirected or
`G->A` is known to have a symmetric structure, then `G->A` is used
instead of `G->AT`." On an undirected graph the transpose is free
because it is the same matrix — which is exactly Beamer §IV's
"performing the bottom-up approach requires no modification to the
graph data structures".

This transfers directly. FalkorDB always *has* the transpose — the
delta trio keeps a transposed twin in lockstep
(`delta_matrix.h:17-24`, walked in
[reading-falkordb-delta-matrix.md](reading-falkordb-delta-matrix.md))
— so pull is always on the menu. That is a storage-layer decision
made once, paid for on every write, and it unlocks an
algorithm-layer option forever.

Why it matters: "optional input" is an architectural choice, not an
API convenience. It puts the memory-doubling decision at the only
layer that knows the workload.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| `…SSGrB_template.c:18-22`, `:128` | 6 | optional `AT`/`out_degree`; `push_pull` armed by one boolean |
| `…SSGrB_template.c:131`, `:142-143` | 3 | 32- vs 64-bit index type and semiring, by `n > INT32_MAX` |
| `…SSGrB_template.c:135`, `:147-150`, `:173-176` | 3 | `many_expected`; output vectors hinted `LG_BITMAP + LG_FULL` |
| `…SSGrB_template.c:161` | 3 | level-only semiring is `LAGraph_any_one_bool`, **not** ANY_PAIR |
| `…SSGrB_template.c:183-188` | 2 | α = 8, β₁ = 8, β₂ = 512 and the two derived bounds |
| `…SSGrB_template.c:200` | 2 | the mask *is* the output vector: `pi` or `v` |
| `…SSGrB_template.c:243-294` | 2 | the push↔pull switch — three exclusive branches, not one test |
| `…SSGrB_template.c:268-275` | 2 | `edges_unexplored` maintained: masked assign, reduce, subtract |
| `…SSGrB_template.c:306-307` | 2 | push: hint `LG_SPARSE`, then `GrB_vxm(…, GrB_DESC_RSC)` |
| `…SSGrB_template.c:312-313` | 2 | pull: hint `LG_BITMAP`, then `GrB_mxv(…, GrB_DESC_RSC)` |
| `…SSGrB_template.c:335`, `:340` | 2 | `pi{q} = q` and `v{q} = k`, both with `GrB_DESC_S` |
| `LAGr_TriangleCount.c:27-47` | 4 | the six masked-mxm formulations and which wins where |
| `LAGr_PageRankGAP.c:20-29` | 5 | "Do not use in production" — sinks are ignored |
| `LAGr_PageRankGAP.c:112`, `:119` | 5 | prescale `d = d_out/damping`, then `d = max(1/damping, d)` |
| `LAGr_PageRankGAP.c:126-143` | 5 | the iteration: eWiseMult, assign, unmasked `mxv`, reduce |
| `include/LAGraph.h:825-829` | 3 | the `LAGraph_any_one_T` family, documented |
| `LG_CC_FastSV7.c` | — | components via hooking/shortcutting; M24 material |

Navigation advice: read the BFS template first, top to bottom —
355 lines of which maybe 70 are payload, and every line is now
familiar. Then read just the comment block of
`LAGr_TriangleCount.c` (`:27-47`), then `LAGr_PageRankGAP.c`'s
loop. Leave `LG_CC_FastSV7.c` until M24. Note the pin ships both
`LG_CC_FastSV6.c` and `LG_CC_FastSV7.c`; read 7.

### What transfers to M20/M24

- M20's BFS parity target: match the template's switch behaviour
  with our α/β on LDBC graphs; the per-level trace in `gb_bench` is
  the debugging tool.
- FastSV (`LG_CC_FastSV7.c`) is M24 material: components via
  min-semiring hooking — read after this topic settles.
- The "optional AT" API design transfers directly: FalkorDB always
  HAS the transpose (delta trio), so pull is always on the menu,
  unlike LAGraph's caller-supplied AT.

## Questions for notes.md

1. Read the switch block (`:243-294`) and list every input the
   heuristic consumes. Which are O(1) to maintain and which need a
   reduction over the frontier (the masked degree assign at
   `:268-269` plus the `GrB_reduce` at `:273-274`)? Note that the
   three branches consume *different* inputs.
2. What format does `q` take at the peak level — and who decided?
   Check `:306` and `:312` before reasoning about SuiteSparse's
   conform rules, then say what `GB_conform.c:150`'s switch would
   have done if the hint were absent.
3. Sandia_LUT uses `L*U'` with `U' = L` — so it is `L*L` with the
   second operand transposed, turning saxpy into dot. Spell out why
   dot3 plus a lower-triangular mask visits each wedge exactly once,
   using `GB_AxB_dot3.c:126` and `:171` as the evidence.
4. PageRankGAP vs textbook PR: what does prescaling `d/damping`
   (`:112`) save per iteration, and why is the teleport handled as
   a scalar assign (`:133`) rather than a vector add?
5. For M20: our engine's BFS needs parent AND level variants. Which
   semiring per variant (`GxB_ANY_SECONDI_INT32` vs
   `LAGraph_any_one_bool`), and what does each move per level
   (indices vs nothing — iso!)?

## Done when

Answer each before unfolding it.

- [ ] You can write the BFS level as two lines of algebra and say which dial differs between them.

  <details><summary>Answer</summary>

  `:307` `GrB_vxm(q, mask, NULL, semiring, q, A, GrB_DESC_RSC)` and
  `:313` `GrB_mxv(q, mask, NULL, semiring, AT, q, GrB_DESC_RSC)`.

  Same mask, same semiring, same descriptor. Two dials differ: the
  **call** (`vxm` vs `mxv`) and the **operand** (`A` vs `AT`). A
  third thing differs on the line before — the format hint,
  `LG_SPARSE` at `:306` against `LG_BITMAP` at `:312` — and that is
  what steers the two calls into different engines.

  </details>

- [ ] You can say who chooses the frontier's format, and name the engine each choice reaches.

  <details><summary>Answer</summary>

  LAGraph chooses, explicitly, on the line before each product:
  `LG_SET_FORMAT_HINT(q, LG_SPARSE)` at `:306` and
  `LG_SET_FORMAT_HINT(q, LG_BITMAP)` at `:312`. It is not left to
  SuiteSparse's auto-conform.

  Bitmap steers pull into the **dot2** engine:
  `GB_AxB_dot2_control.c:26-30` returns true immediately when
  either operand is bitmap or full. It cannot be dot3 — dot3
  requires a *non*-complemented sparse mask
  (`GB_mxm.h:235-243`), and `GrB_DESC_RSC` sets `Mask_comp`.
  Sparse steers push into **saxpy3** (`GB_AxB_saxpy3.c`), which is
  what LAGraph's own comment at `:305` calls it.

  </details>

- [ ] You can explain what `ANY_SECONDI` computes, name the level-only semiring correctly, and say what each moves.

  <details><summary>Answer</summary>

  `SECONDI` returns the *index* of the second operand's entry — the
  parent id — so the product moves indices and never reads `A.x`.
  `ANY` keeps whichever witness arrives; since any parent gives a
  valid BFS tree, no comparison or tie-break is needed. ANY is
  associative, commutative and idempotent, so the nondeterminism is
  algebraic rather than a race you have to argue about. `:131` and
  `:142-143` pick the 32- or 64-bit form by `n > INT32_MAX`.

  The level-only semiring at `:161` is **`LAGraph_any_one_bool`**,
  not ANY_PAIR. `include/LAGraph.h:825-829` documents it as
  `GrB_LOR_MONOID_BOOL` with `GrB_ONEB_BOOL` — (OR, true). The
  multiply is a constant, so the matrix's values are never read;
  only its pattern is. That is Yang's "structure only" optimization,
  measured standalone at 1.62× (ICPP '18, Table 2).

  </details>

- [ ] You can give more than one masked spelling of triangle counting, say which LAGraph defaults to, and say what the source does *not* claim.

  <details><summary>Answer</summary>

  Six, at `LAGr_TriangleCount.c:31-37`: Burkhardt
  `sum(sum((A²).*A))/6`, Cohen `sum(sum((L*U).*A))/2`, Sandia_LL
  `sum(sum((L*L).*L))`, Sandia_UU, Sandia_LUT
  `sum(sum((L*U').*L))`, Sandia_ULT. `:31` says the default is
  **Sandia_LUT**.

  `:43-47` gives the ordering: Sandia_* beat Burkhardt and Cohen;
  LUT (dot) is usually fastest on the largest graphs *except*
  GAP-urand, where saxpy-based LL wins; LL and UU are often faster
  on many small graphs.

  What the source does *not* give is a ratio. There is no measured
  speedup in that comment and this repo has not benchmarked masked
  SpGEMM through GraphBLAS — `notes.md:62` lists it as a stretch
  stub. Carry the ordering, not a number.

  </details>

- [ ] You can explain why PageRank needs no mask, and predict one iteration's time from bytes.

  <details><summary>Answer</summary>

  Because every vertex contributes every iteration. `:135`'s
  `GrB_mxv` passes `NULL` for both the mask and the descriptor —
  there is nothing to skip, so there is nothing a mask could scope.

  At scale 18 (`notes.md:13`: n = 262,144, nnz = 2.0e6, 4-byte
  indices and f32 values) the compulsory traffic is
  A.p 1.05 MB + A.i 8.00 MB + A.x 8.00 MB + r 1.05 MB = 18.10 MB.
  At `notes.md:13`'s measured 18.3 GB/s that predicts 0.99 ms; the
  lane actually takes 2547 µs — **2.6×** the prediction. The gap is
  the gather: `notes.md:16-18` attributes the shortfall against
  topic 0/13's ~30 GB/s streaming baseline to the random x-gathers,
  because RMAT column indices spray across the vector.

  Note the topic reports the decay two ways — `FINDINGS.md:38` says
  20.7 → 12.3 GB/s, `notes.md:11-14` says 19.1 → 15.8 — and they
  should not be blended. Cite whichever you used.

  </details>

- [ ] You can say what LAGraph makes the caller decide, and what changes when the storage layer has already decided it.

  <details><summary>Answer</summary>

  Whether pull exists at all. `:18-22` documents `G->AT` and
  `G->out_degree` as required "for this method to use push-pull
  optimization", says that without them "this method defaults to a
  push-only algorithm, which can be slower", and — the load-bearing
  clause — that they "are not computed if not present". `:128` is
  the one boolean that arms the machinery. The library will not
  spend the memory doubling on the caller's behalf.

  `LAGr_PageRankGAP.c:31-33` adds the shortcut: on an undirected or
  structurally symmetric graph, `G->A` is used instead of `G->AT`,
  so the transpose costs nothing.

  FalkorDB has already paid: the delta trio maintains a transposed
  twin on every write (`delta_matrix.h:17-24`), so pull, and
  incoming-edge traversal generally, is always available. The
  decision moved from the algorithm's caller to the storage
  engine's designer, and became a per-write cost instead of a
  per-query one.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including which inputs the direction switch actually reads.

  <details><summary>Answer</summary>

  The switch's inputs are not one set — they differ per branch, and
  that is question 1's real content. `:246` reads `nq` and
  `last_nq` (both O(1), maintained at `:320-321`). `:248` reads
  `edges_unexplored`, which is only valid until the first pull.
  `:261` reads `nq` against the precomputed `n_over_beta1` (`:187`,
  computed once). `:268-275` is the expensive one: a masked
  `GrB_assign` of the degree vector, then a `GrB_reduce` — O(nq)
  work per level, which is why `:275` maintains a running total
  rather than recomputing from scratch, and why `:255-260` explains
  that after a pull the total is abandoned rather than repaired.

  </details>

## References

**Code**

- [LAGraph](https://github.com/GraphBLAS/LAGraph) at `e2539e2`,
  `src/algorithm/` — `template/LG_BreadthFirstSearch_SSGrB_template.c`
  (355 lines; the whole Beamer paper in ~70 of them),
  `LAGr_TriangleCount.c` (362 lines; `:27-47` lists all six masked
  formulations), `LAGr_PageRankGAP.c` (152 lines),
  `LG_CC_FastSV7.c` (M24 material — read later), and
  `include/LAGraph.h` for the semiring families.
- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  at `1fd5475` — the engine-dispatch anchors this guide points at
  (`Source/mxm/GB_AxB_dot2_control.c`, `GB_AxB_dot3.c`,
  `GB_mxm.h`) are walked in
  [reading-suitesparse-internals.md](reading-suitesparse-internals.md).

**Papers**

- Davis — "Parallel GraphBLAS with OpenMP", CSC '20. §4.3 states
  the push = `vxm` / pull = `mxv` correspondence these two lines
  implement; §3.1 says which engine the library picks for each.
- Yang, Buluç, Owens — "Implementing Push-Pull Efficiently in
  GraphBLAS", ICPP '18,
  [doi:10.1145/3225058.3225122](https://doi.org/10.1145/3225058.3225122).
  Table 2's ablation is the source of the 1.62× attributed to
  structure-only in Step 3.

**Measured, in this repo**

- `topics/20-graphblas/notes.md:9-18` — the SpMV ladder Step 5 does
  arithmetic on, and the gather explanation for the shortfall.
- `topics/20-graphblas/notes.md:35` — the BFS scalar oracle,
  rmat18 3308 µs.
- `FINDINGS.md:38` — the topic headline. Note it and `notes.md`
  report the SpMV decay differently (20.7 → 12.3 versus
  19.1 → 15.8); cite one, do not average them.
