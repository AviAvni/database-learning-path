# Reading guide — SuiteSparse:GraphBLAS internals (`~/repos/GraphBLAS/Source/`)

The code walk behind the TOMS papers: where format switches are
decided, and the saxpy3 scheduler — the most database-executor-like
piece of code in the library.

## Anchor map

| anchor | what it is |
|---|---|
| Source/convert/GB_convert_sparse_to_bitmap_test.c:32-38 | THE bitmap heuristic: `nnz > bitmap_switch * nnz_dense` |
| Source/convert/GB_conform_hyper.c:52 | hyper→sparse test via `hyper_switch` |
| Source/convert/GB_conform.c:33-89 | conform runs after every op; sparsity_control bitmask |
| Source/mxm/GB_AxB_meta.c | engine dispatch (dot vs saxpy vs saxbit) |
| Source/mxm/GB_AxB_saxpy3.c:22-60 | coarse/fine tasks × Gustavson/hash — read this header comment twice |
| Source/mxm/GB_AxB_saxpy3.c:57 | hash > m/16 ⇒ fall back to Gustavson |
| Source/mxm/GB_AxB_saxpy3_flopcount.c | the sizing pre-pass |
| Source/mxm/GB_AxB_dot3.c:2-10 | `C<M>=A'*B` — mask REQUIRED, work ∝ nnz(M) |
| Source/mxm/GB_AxB_dot2.c / dot4.c | unmasked / C+=A'*B dense-output variants |

## 1. Format switching is a bitmask + two floats

Every matrix carries `sparsity_control` (which formats are ALLOWED)
plus `hyper_switch`/`bitmap_switch` (when to move). `GB_conform`
(GB_conform.c) runs at the end of operations:

```
 allowed?      test                              go to
 bitmap    nnz > bitmap_switch × n×m  (:32-38)   bitmap
 sparse    bitmap_to_sparse_test (reverse, with  sparse
           hysteresis — thresholds differ so it
           doesn't ping-pong)
 hyper     #non-empty vectors vs hyper_switch    hyper/sparse
           (GB_conform_hyper.c:52)
```

Hysteresis is the lesson: switch-up and switch-down thresholds
differ, like topic 3's LSM compaction triggers. FalkorDB pins
relation matrices hypersparse+sparse via GxB_set — find where
(question 1).

## 2. saxpy3 — a query scheduler in one file

The header comment (GB_AxB_saxpy3.c:22-60) describes a two-level
work division that IS morsel-driven parallelism (topic 11):

```
 B's vectors (columns) → tasks:
   coarse task: owns ≥1 whole vectors, private workspace
   fine task:   teams up on ONE big vector (a hub column),
                shares workspace, needs atomics
 each task independently picks its accumulator:
   Gustavson: dense f64[m] + pattern marker ("SPA") — O(1) scatter,
              wins when the column's flops fill enough of m
   hash:      open-addressing table 2×pow2(flops-estimate) — wins
              when m is huge and the column is sparse
   rule: hash size would exceed m/16 ⇒ just use Gustavson (:57)
```

The flopcount pre-pass (saxpy3_flopcount.c) computes per-column
flops BEFORE allocating — exact same two-phase as cudf's
size/retrieve (topic 18) and Gunrock's degree-scan. Sparse output
size is the recurring villain of this whole curriculum.

## 3. dot3 — the mask as the driver

dot3 (GB_AxB_dot3.c) requires the mask and iterates over M's
entries: for each (i,j) ∈ M, compute A(:,i)'·B(:,j). Work is
nnz(M) dot products — if M is a triangle-counting L, that's one
dot per candidate wedge. The mask isn't a filter; it's the OUTER
LOOP. Contrast saxpy3, where the mask only prunes writes.
Dispatch between them (GB_AxB_meta.c) weighs nnz(M) vs predicted
saxpy flops — a cost-based optimizer decision (topic 10) made per
multiply.

## 4. What transfers to M20

- Our stub SpGEMM = one coarse Gustavson task (dense SPA). The
  HashMap reference = the hash task. gb_bench measures the m/16
  intuition directly.
- Masked-SpMV pull BFS = dot3's idea specialized: iterate the
  UNVISITED set (the mask), early-exit each dot at first frontier
  hit (ANY monoid ⇒ short-circuit legal).
- M20's kernel core needs only: saxpy-SpMSpV (push), masked
  dot-SpMV (pull), one SPA SpGEMM, conform-lite (hyper↔sparse).

## Questions for notes.md

1. Find FalkorDB's GxB_set calls pinning formats (grep GxB_SPARSITY
   in ~/repos/FalkorDB/src). Which matrices allow bitmap and why
   not the adjacency ones?
2. Why does a fine Gustavson task need atomics on the SPA but a
   coarse one doesn't — and what's the topic 11 analogue
   (shared hash aggregation vs per-thread pre-aggregation)?
3. The hash task's table is sized 2× next-pow2(estimated flops).
   What happens on underestimate (collision pile-up — degrade,
   or rebuild? find it in GB_AxB_saxpy3.c) — compare SwissTable's
   resize story (topic 8).
4. dot3 vs saxpy3 crossover: for `C<L>=L*U'` triangle counting on an
   RMAT graph, estimate both costs (nnz(L) dots of avg length d̄ vs
   Σ flops) — which wins and why does LAGraph still offer both
   (LAGr_TriangleCount.c:31-46)?
5. Run gb_bench: at what RMAT scale does our dense-SPA Gustavson
   lose to the HashMap version (SPA = m×8B cold bytes per row team
   — when m outgrows L2, topic 13's blocking argument bites)?
