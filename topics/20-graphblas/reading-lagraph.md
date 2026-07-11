# Reading guide — LAGraph algorithms (`~/repos/LAGraph/src/algorithm/`)

Freshly cloned. The "standard library" of GraphBLAS — read three
algorithms as *executable linear algebra*: each is a few GrB calls
whose entire performance story lives in which engine/format/mask
they trigger underneath.

## Anchor map

| anchor | what it is |
|---|---|
| template/LG_BreadthFirstSearch_SSGrB_template.c:184-187 | α=8, β1=8, β2=512 — the Beamer thresholds |
| …template.c:243-292 | the push↔pull switch logic (growing/shrinking + thresholds) |
| …template.c:307 | push: `GrB_vxm(q, mask, …, q, A, GrB_DESC_RSC)` |
| …template.c:313 | pull: `GrB_mxv(q, mask, …, AT, q, GrB_DESC_RSC)` |
| …template.c:140-143 | `GxB_ANY_SECONDI_INT{32,64}` — parent BFS with zero comparisons |
| …template.c:196, 261-277 | `edges_unexplored` maintained incrementally |
| LAGr_TriangleCount.c:31-46 | all SIX masked-mxm triangle formulations + which wins where |
| LAGr_PageRankGAP.c:99-135 | GAP-style PR: prescaled degrees, `mxv` + PLUS_SECOND at :135 |
| LG_CC_FastSV7.c | connected components via hooking/shortcutting (min-semiring) |

## 1. BFS: the whole Beamer paper in 40 lines

The loop (template :243-313) reads as a decision procedure:

```
 if push:  switching to pull if frontier growing AND
           (nq > n/8  OR  push-work estimate > unexplored/8)
 if pull:  switching back if frontier < n/512
 then ONE line does the level:  vxm (push) or mxv-on-AT (pull),
 mask = complemented visited (DESC_RSC: replace + structural
 complement), assign frontier into parent/level vectors (:335-340)
```

Everything we said in reading-beamer-sc12.md is these ~70 lines.
The out_degree vector and AT are *optional inputs* — without them
it silently degrades to push-only (:18-22): the caller decides
whether pull's memory doubling is worth it, not the library.

## 2. The semiring trick: ANY_SECONDI

`ANY_SECONDI` (:140-143): multiply op SECONDI returns the *index of
the second operand's entry* — i.e. the parent's id; monoid ANY
keeps whichever arrives. No min, no compare, no tie-break — any
parent is a valid BFS tree (Gunrock's benign CAS race, expressed
as algebra). 32- vs 64-bit variant chosen by n > INT32_MAX: the
v10 index-size story at the algorithm level.

## 3. Triangle counting: six spellings of one mask

LAGr_TriangleCount.c:31-46 — Burkhardt `sum((A²).*A)/6`, Cohen
`sum((L*U).*A)/2`, Sandia_LL `sum((L*L).*L)`, … Sandia_LUT
`sum((L*U').*L)`. All the same count; they differ ONLY in which
mxm engine and how much the mask prunes:

- `.*L` masks the OUTPUT to the lower triangle — dot3 iterates
  only candidate wedges
- L*L vs L*U': saxpy vs dot formulation — the comment (:43-46)
  says LUT (dot) usually wins, but LL (saxpy) wins on GAP-urand:
  uniform-random degrees flatten the hub problem, exactly the
  Gustavson-vs-hash tradeoff
- there's also a presort by degree (relabeling!) that bounds wedge
  work — topic 13's "renumber for locality," used for algorithmic
  pruning

## 4. PageRank (GAP variant): no mask, all bandwidth

LAGr_PageRankGAP.c:99-135 — prescale out-degrees by damping once
(:112), then each iteration is `r = teleport; r += AT'*(t/d)` via
one mxv with PLUS_SECOND (:135) + eWise ops. Dense vectors, full
sweep, no early exit: PageRank is the SpMV bandwidth benchmark
(gb_bench's spmv lane IS this). Note what's absent: GAP PR skips
proper dangling-node handling for speed (:comment near top) — a
benchmark-vs-correctness tension to remember for topic 22.

## 5. What transfers to M20/M24

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
