# Inside SuiteSparse: format switching and the saxpy3 scheduler

The code walk behind the TOMS papers
([reading-davis-toms19.md](reading-davis-toms19.md)): where format
switches are decided, and the saxpy3 scheduler — the most
database-executor-like piece of code in the library. This chapter
builds the concepts each file implements, step by step, then hands
you the anchors into `Source/` of the SuiteSparse:GraphBLAS repo to
watch them happen.

## The problem in one sentence

Inside one `GrB_mxm`, per-column costs can vary 1000× (power-law
hub columns), the right accumulator depends on output density, and
the right *format* for the result depends on how dense it came out
— so the library must make cost-based decisions per matrix, per
multiply, and per task, and every one of those decisions is a
readable number in the code.

## The concepts, step by step

### Step 1 — format switching is a bitmask + two floats

Every matrix carries a `sparsity_control` bitmask saying which
formats (hypersparse/sparse/bitmap/full — the ladder from the
previous chapter) are *allowed*, plus two floats
(`hyper_switch`, `bitmap_switch`) saying *when* to move between
them. `GB_conform` runs at the end of every operation and applies
the tests:

```
 allowed?      test                              go to
 bitmap    nnz > bitmap_switch × n×m  (:32-38)   bitmap
 sparse    bitmap_to_sparse_test (reverse, with  sparse
           hysteresis — thresholds differ so it
           doesn't ping-pong)
 hyper     #non-empty vectors vs hyper_switch    hyper/sparse
           (GB_conform_hyper.c:52)
```

**Hysteresis** (the switch-up and switch-down thresholds differ,
so a matrix hovering near the boundary doesn't convert back and
forth on every operation) is the lesson — the same instinct as
topic 3's LSM compaction triggers. FalkorDB pins relation matrices
hypersparse+sparse via GxB_set — find where (question 1). Cost of
getting this wrong: each conversion is an O(nnz) rebuild, so
ping-ponging turns every op into a copy.

### Step 2 — count the work before allocating: the flopcount pre-pass

For sparse matrix multiply, **flops** means the number of scalar
multiply-add operations that actually exist — for C = A*B, that's
one per (A(i,k), B(k,j)) pair of present entries. Unlike dense
matmul, you can *count* them cheaply before doing any of them: walk
the patterns (the index structure, no values) and sum
`nnz(B(k,:))` over A's entries.

saxpy3 runs exactly this pre-pass (`GB_AxB_saxpy3_flopcount.c`)
before allocating anything, producing total flops and per-column
flops. Those two numbers then size *everything*: how many threads,
how to slice the work, and how big each hash table should be. The
same two-phase shape recurs across the curriculum — cudf's
size/retrieve (topic 18), Gunrock's degree-scan — because sparse
output size is the recurring villain: you can't allocate the
output until you've measured the work.

### Step 3 — saxpy3's task taxonomy: coarse and fine tasks

With per-column flops in hand, saxpy3 slices the multiply into
tasks of two kinds. The header comment (GB_AxB_saxpy3.c:22-60)
describes a two-level work division that IS morsel-driven
parallelism (topic 11):

```
 B's vectors (columns) → tasks:
   coarse task: owns ≥1 whole vectors, private workspace
   fine task:   teams up on ONE big vector (a hub column),
                shares workspace, needs atomics
```

A **coarse task** is one thread owning whole columns — no
coordination, its workspace is private. A **fine task** exists
because power-law graphs have hub columns whose flops exceed an
entire fair share: a *team* of threads splits one fat column, and
because they share one output workspace, they pay for atomics.
The cost gradient: coarse = zero coordination overhead; fine =
atomics on every scatter, bought only where skew forces it.

### Step 4 — each task picks its accumulator: Gustavson vs hash

Each task accumulates one output column's worth of scattered
contributions, and it independently picks the data structure to do
it in:

```
 each task independently picks its accumulator:
   Gustavson: dense f64[m] + pattern marker ("SPA") — O(1) scatter,
              wins when the column's flops fill enough of m
   hash:      open-addressing table 2×pow2(flops-estimate) — wins
              when m is huge and the column is sparse
   rule: hash size would exceed m/16 ⇒ just use Gustavson (:57)
```

**Gustavson** here means the classic dense-workspace method: a
**SPA** (sparse accumulator — a dense array of size m, one slot
per possible output row, plus a marker of which slots are
occupied) gives O(1) scatter but costs m slots of (possibly cold)
memory per task. The **hash** alternative sizes a table by the
flops estimate instead of by m — small and cache-resident when the
column is sparse, no matter how big m is. The shipped rule at :57:
if the hash table would exceed m/16, the dense SPA is cheaper —
just use Gustavson. This is topic 8's hash-vs-sort aggregation
choice, made per task from step 2's numbers.

### Step 5 — dot3: the mask as the outer loop

The other engine inverts control entirely. dot3 computes
`C<M> = A'*B` and *requires* the mask M (the "only produce outputs
here" matrix): it iterates over M's entries, and for each
(i,j) ∈ M computes one sparse dot product A(:,i)'·B(:,j). Work is
nnz(M) dot products — the mask isn't a filter applied afterwards,
it's the OUTER LOOP:

```rust
// dot3: the mask M is the outer loop — work ∝ nnz(M), a complexity
// CLASS below computing A'*B and filtering afterward
fn dot3(m: &Pattern, a_t: &Csr, b: &Csc, semiring: &Semiring) -> Coo {
    let mut c = Coo::new();
    for (i, j) in m.entries() {                    // one dot per MASK entry
        // sparse dot = two-pointer intersect of the two patterns
        if let Some(v) = sparse_dot(a_t.row(i), b.col(j), semiring) {
            c.push(i, j, v);                       // (ANY monoid ⇒ sparse_dot
        }                                          //  may stop at first hit)
    }
    c
}
```

If M is triangle counting's lower-triangular L, that's one dot per
candidate wedge — nothing is computed for output cells the mask
excludes. Contrast saxpy3, where the mask only prunes *writes*:
the flops still happen. This asymmetry is why "masks are free
performance" in FalkorDB — but only when the dispatcher picks dot3.

### Step 6 — dispatch: a cost-based optimizer decision per multiply

`GB_AxB_meta.c` chooses the engine for each multiply: dot3 when a
mask is present and C is sparse (work ∝ nnz(M)), saxpy3 for the
general case (work ∝ flops), bitmap/full variants (saxbit, dot2,
dot4) when operands or output are dense. The choice weighs nnz(M)
against predicted saxpy flops — a cost-based optimizer decision
(topic 10) made per multiply, using step 2's estimates as the cost
model. The consequence for API users: the *same* GrB_mxm line runs
a different algorithm depending on your mask's density — which is
exactly how the BFS push/pull switch will be implemented in the
Beamer and LAGraph chapters.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| Source/convert/GB_convert_sparse_to_bitmap_test.c:32-38 | 1 | THE bitmap heuristic: `nnz > bitmap_switch * nnz_dense` |
| Source/convert/GB_conform_hyper.c:52 | 1 | hyper→sparse test via `hyper_switch` |
| Source/convert/GB_conform.c:33-89 | 1 | conform runs after every op; sparsity_control bitmask |
| Source/mxm/GB_AxB_saxpy3_flopcount.c | 2 | the sizing pre-pass |
| Source/mxm/GB_AxB_saxpy3.c:22-60 | 3-4 | coarse/fine tasks × Gustavson/hash — read this header comment twice |
| Source/mxm/GB_AxB_saxpy3.c:57 | 4 | hash > m/16 ⇒ fall back to Gustavson |
| Source/mxm/GB_AxB_dot3.c:2-10 | 5 | `C<M>=A'*B` — mask REQUIRED, work ∝ nnz(M) |
| Source/mxm/GB_AxB_dot2.c / dot4.c | 6 | unmasked / C+=A'*B dense-output variants |
| Source/mxm/GB_AxB_meta.c | 6 | engine dispatch (dot vs saxpy vs saxbit) |

Navigation advice: start with the saxpy3 header comment
(GB_AxB_saxpy3.c:22-60) — it is the scheduler spec, and everything
else in `Source/mxm/` is implementation of that comment. Then read
`GB_conform.c` top to bottom (it's short), then skim
`GB_AxB_meta.c` for the dispatch conditions.

### What transfers to M20

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
   in [~/repos/FalkorDB](https://github.com/FalkorDB/FalkorDB)/src). Which matrices allow bitmap and why
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

## References

**Papers**
- Davis — "Algorithm 1000: SuiteSparse:GraphBLAS" (ACM TOMS 2019)
  — the companion paper; see
  [reading-davis-toms19.md](reading-davis-toms19.md)

**Code**
- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  `Source/convert/GB_conform.c`, `GB_conform_hyper.c`,
  `GB_convert_sparse_to_bitmap_test.c`; `Source/mxm/GB_AxB_meta.c`,
  `GB_AxB_saxpy3.c`, `GB_AxB_saxpy3_flopcount.c`, `GB_AxB_dot3.c` —
  read the saxpy3 header comment (:22-60) twice; it's the scheduler
  spec
