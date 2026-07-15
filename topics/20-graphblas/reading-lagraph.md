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

## The problem in one sentence

Once traversal is matrix algebra, a whole graph algorithm collapses
to a handful of library calls — LAGraph's direction-optimizing BFS
is ~70 lines, and its performance is decided entirely by *which*
call, *which* semiring, and *which* mask each line picks.

## The concepts, step by step

### Step 1 — an algorithm is a loop around one line of algebra

LAGraph's algorithms all have the same skeleton: some scalar
bookkeeping, then one GrB call per iteration that does all the
real work. The two workhorse calls are `GrB_vxm` (sparse
row-vector × matrix — a **SpMSpV**, sparse-vector times sparse
matrix, when the vector is sparse) and `GrB_mxv` (matrix × column
vector — a **SpMV**, sparse-matrix times vector). Add a mask to
scope the output and a descriptor flag like `GrB_DESC_RSC`
(Replace the output + use the Structural Complement of the mask —
i.e. "write only where the mask has NO entry, discarding what was
there") and one line expresses "advance the frontier, excluding
visited". Everything the previous chapters built — engine
dispatch, format switching, masks as outer loops — fires inside
that one line, invisibly. Reading LAGraph is learning to see it.

### Step 2 — the BFS loop: the whole Beamer paper in 40 lines

The template's loop (template :243-313) reads as a decision
procedure wrapped around one line of algebra per level:

```
 if push:  switching to pull if frontier growing AND
           (nq > n/8  OR  push-work estimate > unexplored/8)
 if pull:  switching back if frontier < n/512
 then ONE line does the level:  vxm (push) or mxv-on-AT (pull),
 mask = complemented visited (DESC_RSC: replace + structural
 complement), assign frontier into parent/level vectors (:335-340)
```

Everything in [reading-beamer-sc12.md](reading-beamer-sc12.md) is
these ~70 lines. The whole loop, transcribed:

```rust
loop {
    // the direction switch wraps ONE line of algebra per level
    if push && growing && (nq > n / 8 || push_work > unexplored / 8) {
        push = false;                            // frontier huge → pull
    } else if !push && nq < n / 512 {
        push = true;                             // tail → back to push
    }
    q = if push {
        vxm(&q, &visited, AnySecondi, a)         // q'<!visited> = q' * A
    } else {
        mxv(at, &q, &visited, AnySecondi)        // q<!visited> = AT * q
    };
    parent.assign_where(&q);                     // ANY: any parent will do
    nq = q.nvals();
    if nq == 0 { break; }
}
```

What it costs to notice: the heuristic's inputs (`push_work`,
`unexplored`) must themselves be cheap — the template maintains
`edges_unexplored` incrementally by subtracting frontier degrees
(:196, :261-277) rather than recomputing a reduction per level.
Question 1 makes you audit every input.

### Step 3 — the semiring trick: ANY_SECONDI

`ANY_SECONDI` (:140-143) computes the parent vector with zero
comparisons. The multiply op SECONDI returns the *index of the
second operand's entry* — i.e. the parent's id; the monoid ANY (a
reduction allowed to keep whichever value arrives — any witness is
acceptable) keeps one of them. No min, no compare, no tie-break —
any parent is a valid BFS tree. This is Gunrock's benign CAS race
(topic 18), expressed as algebra instead of as a data race you
argue is harmless. The 32- vs 64-bit variant is chosen by
n > INT32_MAX: the v10 index-size story at the algorithm level.

Why it matters: the semiring choice moved a correctness argument
(is the race benign?) into the algebra (ANY is associative and
idempotent — the race is *definitionally* fine), and it decides
what data moves — indices, not values.

### Step 4 — triangle counting: six spellings of one mask

LAGr_TriangleCount.c:31-46 — Burkhardt `sum((A²).*A)/6`, Cohen
`sum((L*U).*A)/2`, Sandia_LL `sum((L*L).*L)`, … Sandia_LUT
`sum((L*U').*L)` (L and U are the lower/upper triangles of A). All
compute the same count; they differ ONLY in which mxm engine runs
and how much the mask prunes:

- `.*L` masks the OUTPUT to the lower triangle — dot3 iterates
  only candidate wedges
- L*L vs L*U': saxpy vs dot formulation — the comment (:43-46)
  says LUT (dot) usually wins, but LL (saxpy) wins on GAP-urand:
  uniform-random degrees flatten the hub problem, exactly the
  Gustavson-vs-hash tradeoff
- there's also a presort by degree (relabeling!) that bounds wedge
  work — topic 13's "renumber for locality," used for algorithmic
  pruning

The lesson: at this level, "algorithm choice" has become "which
algebraic spelling triggers the best engine for this graph's
degree distribution" — six mathematically equal expressions, six
different cost profiles.

### Step 5 — PageRank (GAP variant): no mask, all bandwidth

LAGr_PageRankGAP.c:99-135 — prescale out-degrees by damping once
(:112), then each iteration is `r = teleport; r += AT'*(t/d)` via
one mxv with PLUS_SECOND (:135) + eWise ops. Dense vectors, full
sweep, no early exit: unlike BFS, *every* vertex contributes every
iteration, so there is nothing for masks or sparsity to skip —
PageRank is the SpMV bandwidth benchmark (gb_bench's spmv lane IS
this). It's the algorithm that measures your memory system, not
your cleverness. Note what's absent: GAP PR skips proper
dangling-node handling for speed (:comment near top) — a
benchmark-vs-correctness tension to remember for topic 22.

### Step 6 — API design: pull is the caller's bill to pay

The out_degree vector and AT (the transpose, needed for pull —
step 4 of the Beamer chapter) are *optional inputs* to the BFS
template — without them it silently degrades to push-only
(:18-22). The library refuses to decide whether pull's memory
doubling is worth it; the caller does. This transfers directly:
FalkorDB always HAS the transpose (the delta trio keeps M and Mᵀ
in lockstep), so pull is always on the menu — a storage-layer
decision made once, unlocking an algorithm-layer option forever.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| template/LG_BreadthFirstSearch_SSGrB_template.c:184-187 | 2 | α=8, β1=8, β2=512 — the Beamer thresholds |
| …template.c:243-292 | 2 | the push↔pull switch logic (growing/shrinking + thresholds) |
| …template.c:307 | 2 | push: `GrB_vxm(q, mask, …, q, A, GrB_DESC_RSC)` |
| …template.c:313 | 2 | pull: `GrB_mxv(q, mask, …, AT, q, GrB_DESC_RSC)` |
| …template.c:196, 261-277 | 2 | `edges_unexplored` maintained incrementally |
| …template.c:140-143 | 3 | `GxB_ANY_SECONDI_INT{32,64}` — parent BFS with zero comparisons |
| LAGr_TriangleCount.c:31-46 | 4 | all SIX masked-mxm triangle formulations + which wins where |
| LAGr_PageRankGAP.c:99-135 | 5 | GAP-style PR: prescaled degrees, `mxv` + PLUS_SECOND at :135 |
| …template.c:18-22 | 6 | optional AT / out_degree — silent degrade to push-only |
| LG_CC_FastSV7.c | — | connected components via hooking/shortcutting (min-semiring); M24 material |

Navigation advice: read the BFS template first, top to bottom —
it's ~70 lines of payload and every line is now familiar. Then
read just the comment block of LAGr_TriangleCount.c (:31-46), then
LAGr_PageRankGAP.c's loop. Leave LG_CC_FastSV7.c until M24.

### What transfers to M20/M24

- M20's BFS parity target: match the template's switch behavior
  with our α/β on LDBC graphs; the per-level trace in gb_bench is
  the debugging tool.
- FastSV (LG_CC_FastSV7.c) is M24 material: components via
  min-semiring hooking — read after this topic settles.
- The "optional AT" API design transfers directly: FalkorDB always
  HAS the transpose (delta trio) — so pull is always on the menu,
  unlike LAGraph's caller-supplied AT.

## Questions for notes.md

1. Read the switch block (:243-292) and list every input the
   heuristic consumes. Which are O(1) to maintain and which need
   a reduction over the frontier (degree sum — GrB_reduce on a
   masked degree vector)?
2. Why does the template keep BOTH `q` sparse and the visited
   `mask` as a full vector — what format does q take at the peak
   level (SuiteSparse auto-switches it to bitmap — verify via
   GxB_print in a scratch C program, or reason from the conform
   rules)?
3. Sandia_LUT uses L*U' with U'=L — so it's L*L with the SECOND
   operand transposed, turning saxpy into dot. Spell out why dot3
   + lower-triangular mask visits each wedge exactly once.
4. PageRankGAP vs textbook PR: what does prescaling d/damping save
   per iteration (one eWise divide over n), and why is the
   important-teleport handled as scalar assign not vector add?
5. For M20: our engine's BFS needs parent AND level variants.
   Which semiring per variant (ANY_SECONDI vs ANY_PAIR + level
   assign), and what does each move per level (indices vs nothing
   — iso!)?

## References

**Code**
- [LAGraph](https://github.com/GraphBLAS/LAGraph) `src/algorithm/`
  — `template/LG_BreadthFirstSearch_SSGrB_template.c` (the whole
  Beamer paper in ~70 lines), `LAGr_TriangleCount.c` (:31-46 lists
  all six masked formulations), `LAGr_PageRankGAP.c`,
  `LG_CC_FastSV7.c` (M24 material — read later)
