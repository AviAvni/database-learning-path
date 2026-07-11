# SuiteSparse:GraphBLAS: a sparse-matrix executor in disguise

Davis's TOMS '19 system paper (plus the '23 v2 update) describes the
library under FalkorDB. Read it as an executor-design paper, not a
math paper: it's about lazy evaluation, format polymorphism, and
kernel dispatch — the same problems as topics 8-11, in matrix
clothing.

## 1. The object model (paper §3, code `Source/matrix/GB_matrix.h`)

```
 GrB_Matrix = opaque header
   ├─ format: hypersparse | sparse | bitmap | full   (×2: by row/col)
   ├─ pending tuples + zombies       (lazy mutation!)
   ├─ hyper_switch / bitmap_switch   (per-matrix knobs)
   └─ iso flag (all values equal — store ONE value)
```

Zombies (deleted-but-present entries) and pending tuples
(inserted-but-unsorted) are the library's OWN delta mechanism —
FalkorDB's delta matrices exist because these are flushed at
`GrB_wait` boundaries the engine can't always control. Question 2.

## 2. Non-blocking mode (the executor story)

The spec allows every operation to return before doing work.
SuiteSparse uses it for *mutation batching* (pending tuples get
sorted+merged once), not full lazy fusion (v2 paper discusses the
JIT changing this calculus). Compare topic 27's incremental view
maintenance: same "amortize small updates" shape.

The whole mechanism, distilled:

```rust
fn set_element(a: &mut Matrix, i: u64, j: u64, v: f64) {
    a.pending.push((i, j, v));       // O(1): append, don't restructure CSR
}

fn delete_element(a: &mut Matrix, i: u64, j: u64) {
    if let Some(e) = a.find_mut(i, j) {
        e.mark_zombie();             // flag in place — no O(nnz) splice
    }
}

fn wait(a: &mut Matrix) {            // the GrB_wait boundary
    a.prune_zombies();               // one sweep drops ALL zombies
    a.pending.sort_unstable();       // n inserts → one sort + one merge,
    a.merge_pending_into_csr();      //   not n binary-searched splices
    conform(a);                      // then maybe switch format
}
```

## 3. The v2 update (TOMS '23) — what changed

- the CPU JIT (topic 19's jitifyer) — user-defined types/semirings
  now run at factory speed
- 32/64-bit integer indices chosen per matrix (v10) — halves index
  memory for graphs under 4B edges, i.e. all of ours
- iso-valued matrices — unweighted graphs store ZERO bytes of
  values (A(i,j)=true for all: pattern-only + one scalar)

Iso + (ANY,PAIR) semiring is why BFS over an unweighted FalkorDB
relation matrix moves no value data at all — pattern in, pattern
out. Question 4.

## 4. Numbers to retain

- format switch defaults: bitmap when nnz > ~4-8% (op-dependent),
  hyper when non-empty vectors < hyper_switch × nrows (~1/16)
- saxpy3 hash→Gustavson threshold: hash table > m/16 ⇒ Gustavson
- mxm engines: dot3 work ∝ nnz(M); saxpy3 work ∝ flops — the mask
  changes the complexity CLASS, not a constant

## Questions for notes.md

1. Map GrB objects to executor concepts: semiring ↔ ?, mask ↔ ?,
   accum ↔ ?, descriptor ↔ ? (operator, semi-join filter, UPDATE
   expression, query hints — defend each).
2. Zombies+pending vs FalkorDB's DP/DM: why does FalkorDB need its
   OWN deltas when the library already has them (control over WHEN
   wait happens; transposed pair kept in lockstep; readers must see
   pre-wait state — which reason dominates)?
3. The iso optimization: which FalkorDB matrices are iso (adjacency
   bool — yes; relation with edge IDs as values — no). What does
   losing iso cost on mxm bandwidth (values move again — 8×?)?
4. Trace one BFS step through the v2 machinery: iso bool matrix,
   ANY_PAIR semiring, sparse frontier — which engine runs
   (saxpy3/SpMSpV), and what does the JIT specialize away?
5. 32-bit indices (v10): for a 10M-node 100M-edge graph, compute
   the CSR memory in v9 (64-bit) vs v10 — and where the same 2×
   shows up in our Rust CSR if we switch usize→u32.

## References

**Papers**
- Davis — "Algorithm 1000: SuiteSparse:GraphBLAS: Graph Algorithms
  in the Language of Sparse Linear Algebra" (ACM TOMS 2019) — the
  system paper; read §3 (object model) and the non-blocking-mode
  discussion closely
- Davis — "Algorithm 1037: SuiteSparse:GraphBLAS: Parallel Graph
  Algorithms in the Language of Sparse Linear Algebra" (ACM TOMS
  2023) — the v2 update: JIT, 32/64-bit indices, iso matrices

**Code**
- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  `Source/matrix/GB_matrix.h` — the object model in one header;
  the internals walk is
  [reading-suitesparse-internals.md](reading-suitesparse-internals.md)
