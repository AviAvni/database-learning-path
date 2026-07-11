# GraphBLAS & Delta_Matrix: the graph as matrices

FalkorDB stores the graph AS matrices; every Cypher expand becomes a
GraphBLAS call. Two things make that fast rather than academic:
SuiteSparse picks storage format and mxm algorithm per matrix at
runtime, and FalkorDB layers a delta overlay on top so single-edge
writes don't rebuild CSR. This chapter walks both codebases — it's
also the topic-20/M20 preview: read for the shape now, the kernels
later.

## 1. Four sparsity formats, chosen automatically

`Include/GraphBLAS.h`:

- `:1664` `GxB_HYPERSPARSE` — offsets only for NON-empty rows (graphs
  where most node IDs have no edges of a given type)
- `GxB_SPARSE` — plain CSR/CSC
- `:1666` `GxB_BITMAP` — dense bitmap of present entries + values array
  (fast random writes, no structure to shift)
- `GxB_FULL` — every entry present, no index arrays at all

Switch thresholds: `:1556` `GxB_HYPER_SWITCH`, `:1559`
`GxB_BITMAP_SWITCH` — density crossing a threshold flips the format on
the next wait/computation.

```
 density →  hypersparse | sparse (CSR) | bitmap | full
             ~n rows      m ≈ O(n)       m/n²>τ   m = n²
```

This is the same menu as topic 12's encodings: representation follows
data shape, chosen by measurement, invisible above the API.

## 2. Dot vs saxpy — two mxm algorithms

`Source/mxm/GB_AxB_meta.c:20-21`:

> generic: for any semiring; dot2/dot3: does `C=A'*B`, `C<M>=A'*B` ...
> saxpy: Gustavson + Hash

- **dot** (`dot2/dot3/dot4` files in `Source/mxm/`): C(i,j) =
  A(:,i)'·B(:,j) — good when C is small/masked (compute only needed
  entries; dot3 is the masked variant driven BY the mask).
- **saxpy/Gustavson**: scatter each A(i,k)·B(k,:) row into an
  accumulator — good when C is big and dense-ish; the hash variant when
  the accumulator would be too sparse to justify a dense scratch row.

BFS mapping: frontier vector × adjacency = one SpMV; the `visited`
complement mask makes dot3 only compute unvisited entries. The
**mask is a predicate pushed INTO the kernel** — topic 10's pushdown,
one level down.

## 3. Masks

`Source/mask/GB_masker.c:2,10` — computes `R = masker(C, M, Z)`, i.e.
`R<M> = Z`: entries of Z where M is true, entries of C elsewhere.
Masks are how GraphBLAS fuses `filter ∘ compute` into one pass — no
materialized intermediate. Triangle counting `C<A> = A²` never builds
A², it only evaluates A² at positions where A has an edge.

## 4. FalkorDB's Delta_Matrix

[`~/repos/FalkorDB/src/graph/graph.h:48-52`](https://github.com/FalkorDB/FalkorDB) — the graph IS matrices:

```c
Delta_Matrix adjacency_matrix;  // all connections
Delta_Matrix *labels;           // one boolean matrix per label
Delta_Matrix node_labels;       // node id → label id mapping
Tensor *relations;              // one matrix per relation type
```

`src/graph/delta_matrix/delta_matrix.h:17-22` — a Delta_Matrix is
THREE GraphBLAS matrices (+ optionally the same trio transposed):

```
 M           main matrix   (read-optimized, CSR inside)
 delta_plus  pending adds
 delta_minus pending deletes
 read(i,j) = (M(i,j) OR DP(i,j)) AND NOT DM(i,j)
```

The header's ASCII state diagrams (`delta_matrix.h:26-80`) enumerate
legal states: an entry may be in M, in DP, or in M+DM (deleted but not
yet flushed) — never in both DP and DM.

The whole contract in three functions:

```rust
// read = (M ∪ DP) ∖ DM — three probes, never a flush
fn get(g: &DeltaMatrix, i: u64, j: u64) -> bool {
    (g.m.get(i, j) || g.dp.get(i, j)) && !g.dm.get(i, j)
}

fn set(g: &mut DeltaMatrix, i: u64, j: u64) {
    if g.dm.remove(i, j) { return; }            // re-add of a pending delete
    if !g.m.get(i, j) { g.dp.insert(i, j); }    // never touch the CSR
}

fn wait(g: &mut DeltaMatrix) {                  // the LSM compaction:
    g.m = (&g.m | &g.dp) - &g.dm;               // whole-matrix rebuild —
    g.dp.clear();                               // expensive, so DEFERRED
    g.dm.clear();                               // behind a sync policy
}
```

- `delta_set_element_bool.c` — writes go to DP (or clear DM if
  re-adding a deleted edge)
- `delta_remove_element.c` — deletes set DM (or clear DP)
- `delta_wait.c` / `delta_will_wait.c` — the flush: M = (M ∪ DP) ∖ DM,
  triggered by sync policy (`graph.h:46` `SyncMatrixFunc`)
- `delta_mxm.c` — mxm that accounts for pending deltas without flushing

This is topic 4's LSM applied to adjacency: read-optimal main
structure + small mutable overlay + background merge. GraphBLAS itself
has the same idea internally ("pending tuples" merged on
`GrB_wait`) — FalkorDB adds its own layer to control WHEN the
(expensive, whole-matrix) wait happens and to make deletes cheap.

## Questions (answer in notes.md)

1. Why does FalkorDB need delta_minus at all — why not delete directly
   from M? (What does deleting one entry from CSR cost?)
2. dot3 vs saxpy for a BFS step at frontier size 10 vs 10⁶ on a 1M-node
   graph — which algorithm and why?
3. When is BITMAP the right format for a label matrix? Relate to the
   density thresholds.
4. The `read = (M ∪ DP) ∖ DM` identity means every read touches three
   matrices. Why is this still a win vs flushing on every write?
5. Map Delta_Matrix states to LSM vocabulary: what's the memtable, the
   SST, the tombstone, the compaction?

## References

**Papers**
- Davis — "Algorithm 1000: SuiteSparse:GraphBLAS: Graph Algorithms in
  the Language of Sparse Linear Algebra" (ACM TOMS 2019) — optional
  companion; the code comments below cover the same ground

**Code**
- [GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  (SuiteSparse, shallow clone) — `Include/GraphBLAS.h` for the four
  formats and switch thresholds, `Source/mxm/GB_AxB_meta.c` (the
  header comment is the algorithm menu), `Source/mask/GB_masker.c`
- [FalkorDB](https://github.com/FalkorDB/FalkorDB) —
  `src/graph/graph.h`, `src/graph/delta_matrix/delta_matrix.h` (the
  ASCII state diagrams in the header are the spec), plus
  `delta_set_element_bool.c`, `delta_remove_element.c`,
  `delta_wait.c`, `delta_mxm.c`
