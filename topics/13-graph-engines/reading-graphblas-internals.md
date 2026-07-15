# GraphBLAS & Delta_Matrix: the graph as matrices

FalkorDB stores the graph AS matrices; every Cypher expand becomes a
GraphBLAS call. Two things make that fast rather than academic:
SuiteSparse picks storage format and mxm algorithm per matrix at
runtime, and FalkorDB layers a delta overlay on top so single-edge
writes don't rebuild CSR. Before touching either codebase, this
chapter builds the machine step by step — graph-as-matrix,
traversal-as-multiply, the format menu, the two multiply algorithms,
masks, the write problem, and the delta overlay that solves it — then
hands you the file anchors. It's also the topic-20/M20 preview: read
for the shape now, the kernels later.

## The problem in one sentence

Answer "who are the neighbors of these 10,000 nodes?" as one streaming
operation instead of 10,000 pointer walks — and then survive a single
edge insert without rewriting the read-optimized structure that made
the streaming possible.

## The concepts, step by step

### Step 1 — the graph is a boolean matrix; traversal is multiplication

A directed graph on n nodes can be stored as an n×n **adjacency
matrix** A where `A[i][j] = true` iff there is an edge i→j; row i then
IS node i's outgoing neighbor list. The GraphBLAS move: once the graph
is a matrix, traversals become linear algebra. Put a set of source
nodes into a boolean vector x (the **frontier**); then `y = xA`
(**SpMV** — sparse matrix-vector multiply) computes, in one operation,
the union of all their neighbors — a whole BFS step. Two hops = `xA²`,
triangles = `A ⊙ A²` (elementwise AND of A with A²):

```
 BFS frontier expansion = SpMV:  y<¬visited> = A^T x
 2-hop = A²,  triangles = A ⊙ A²
```

Why it matters: one engine — the sparse-multiply kernels — serves
every traversal, so every kernel optimization (SIMD, parallelism,
format tricks) speeds up every query. That's FalkorDB's whole
architectural bet.

### Step 2 — CSR: the matrix is stored as offsets + neighbors

Storing n² booleans is absurd for a real graph (1M nodes, 16M edges:
n² = 10¹² cells, 99.998% empty), so sparse matrices store only present
entries. The standard format is **CSR** (compressed sparse row — one
`offsets` array of n+1 positions plus one `neighbors` array of m
column indices; row i is the slice between its offsets):

```
 CSR for 0->{5,9}, 1->{5}, 2->{}:
 offsets: [0, 2, 3, 3]
 targets: [5, 9, 5]
 neighbors(i) = targets[offsets[i] .. offsets[i+1]]   one slice, zero chase
```

16M edges as CSR: 4 MB offsets + 64 MB targets — contiguous,
prefetchable arrays (topic 0's sequential-beats-random, structurally
guaranteed). Why it matters: "sparse matrix" and "read-optimized
adjacency" are the same object; the algebra of Step 1 runs over
exactly this layout.

### Step 3 — four sparsity formats, switched by density at runtime

SuiteSparse doesn't commit to CSR: it keeps each matrix in one of four
formats and switches automatically when the matrix's density (fraction
of present entries) crosses thresholds:

- `GxB_HYPERSPARSE` — offsets stored only for NON-empty rows (graphs
  where most node IDs have no edges of a given type — e.g. a rare
  relationship type touching 1K of 1M nodes)
- `GxB_SPARSE` — plain CSR/CSC
- `GxB_BITMAP` — dense bitmap of present entries + values array
  (fast random writes, no structure to shift)
- `GxB_FULL` — every entry present, no index arrays at all

```
 density →  hypersparse | sparse (CSR) | bitmap | full
             ~n rows      m ≈ O(n)       m/n²>τ   m = n²
```

Crossing a threshold flips the format on the next wait/computation.
This is the same menu as topic 12's encodings: representation follows
data shape, chosen by measurement, invisible above the API. Why it
matters: a label matrix with 3 labels and a supernode-heavy adjacency
matrix get different physical layouts for free.

### Step 4 — dot vs saxpy: two ways to multiply, picked per call

Sparse matrix multiply has two classic algorithms, and SuiteSparse
picks per operation. **dot** computes each output entry C(i,j) as an
inner product of a row of A' with a column of B — good when the output
is small or masked, because you compute *only the entries you need*.
**saxpy** (Gustavson's algorithm) scatters each input entry's
contributions into a per-row accumulator — good when the output is big
and dense-ish; a hash-based accumulator variant covers the
too-sparse-for-a-dense-scratch-row case.

BFS mapping: frontier × adjacency with a small frontier wants dot
guided by the mask (compute only unvisited candidates); a huge
frontier wants saxpy (stream everything). Why it matters: the SAME
`GrB_mxm` call is executed by different algorithms at frontier size 10
vs 10⁶ — the engine re-plans per step, which hand-written BFS code
never does.

### Step 5 — masks: the predicate pushed into the kernel

A **mask** is a boolean matrix/vector passed alongside any GraphBLAS
operation that restricts WHERE output may be produced —
`C<M> = A · B` computes A·B only at positions where M is true, and
never materializes the rest. In BFS, the `¬visited` complement mask
does the dedup/visited check inside the multiply; in triangle
counting, `C<A> = A²` evaluates A² only at positions where an edge
already exists — never building the full (potentially enormous) A².
Masks are how GraphBLAS fuses `filter ∘ compute` into one pass — no
materialized intermediate. Why it matters: this is topic 10's
predicate pushdown, one level down — the filter reaches the innermost
loop of the kernel, and it's the mechanism behind the WCOJ
equivalence in [reading-wcoj.md](reading-wcoj.md).

### Step 6 — the write problem: CSR hates single-edge inserts

CSR's strength — everything contiguous — is exactly why it can't
absorb writes: inserting one edge i→j means shifting the tail of the
`targets` array and bumping every offset after row i — O(m) work,
~64 MB of memmove on the 16M-edge graph, *per edge*. A graph database
takes single-edge writes constantly, so raw CSR is unusable as the
live structure. The generic fix (topic 4's LSM idea, applied to
adjacency): keep the read-optimized structure immutable, buffer
changes in a small mutable overlay, merge in the background. Every
system in this topic grows this mechanism — kuzu's CSR buffers,
GraphBLAS's own internal "pending tuples" — and FalkorDB builds its
own explicit one. Why it matters: the overlay design decides write
latency, read overhead, AND when the expensive merge happens.

### Step 7 — Delta_Matrix: main + additions + deletions

FalkorDB wraps every graph matrix in a `Delta_Matrix` — THREE
GraphBLAS matrices (+ optionally the same trio transposed): the main
matrix M (read-optimized, CSR inside), `delta_plus` DP (pending adds,
kept in the write-friendly bitmap/hypersparse world), and
`delta_minus` DM (pending deletes — deleting from CSR in place would
be Step 6's problem again, so deletes are *recorded*, not applied):

```
 M           main matrix   (read-optimized, CSR inside)
 delta_plus  pending adds
 delta_minus pending deletes
 read(i,j) = (M(i,j) OR DP(i,j)) AND NOT DM(i,j)
```

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

An entry may be in M, in DP, or in M+DM (deleted but not yet flushed)
— never in both DP and DM; the ASCII state diagrams in the header
enumerate the legal states. `wait()` is the compaction: M = (M ∪ DP)
∖ DM, a whole-matrix rebuild, deliberately deferred behind a sync
policy so FalkorDB controls WHEN it pays. Even matrix multiply has a
delta-aware variant that accounts for pending changes without
flushing. And this IS topic 4's LSM: DP the memtable, M the SST, DM
the tombstones, `wait()` the compaction. Why it matters: this overlay
is what makes "graph as matrices" viable as a *database* rather than
an analytics batch tool — reads stay algebraic, writes stay O(1)-ish,
and the rebuild bill is paid on FalkorDB's schedule.

## Where each step lives in the code

**GraphBLAS** ([SuiteSparse](https://github.com/DrTimothyAldenDavis/GraphBLAS),
shallow clone):

- **Step 3** — `Include/GraphBLAS.h`: `GxB_HYPERSPARSE` (`:1664`),
  `GxB_BITMAP` (`:1666`), plus `GxB_SPARSE`/`GxB_FULL` nearby; switch
  thresholds `GxB_HYPER_SWITCH` (`:1556`), `GxB_BITMAP_SWITCH`
  (`:1559`).
- **Step 4** — `Source/mxm/GB_AxB_meta.c:20-21`, the header comment IS
  the algorithm menu:

  > generic: for any semiring; dot2/dot3: does `C=A'*B`, `C<M>=A'*B` ...
  > saxpy: Gustavson + Hash

  The `dot2/dot3/dot4` files sit in the same `Source/mxm/` directory;
  dot3 is the masked variant driven BY the mask.
- **Step 5** — `Source/mask/GB_masker.c:2,10` — computes
  `R = masker(C, M, Z)`, i.e. `R<M> = Z`: entries of Z where M is
  true, entries of C elsewhere.

**FalkorDB** ([repo](https://github.com/FalkorDB/FalkorDB), local at
`~/repos/FalkorDB`):

- **Step 1** — `src/graph/graph.h:48-52` — the graph IS matrices:

```c
Delta_Matrix adjacency_matrix;  // all connections
Delta_Matrix *labels;           // one boolean matrix per label
Delta_Matrix node_labels;       // node id → label id mapping
Tensor *relations;              // one matrix per relation type
```

- **Step 7** — `src/graph/delta_matrix/delta_matrix.h:17-22` (the
  trio), `:26-80` (the ASCII state diagrams — the spec);
  `delta_set_element_bool.c` (writes go to DP, or clear DM if
  re-adding); `delta_remove_element.c` (deletes set DM, or clear DP);
  `delta_wait.c` / `delta_will_wait.c` (the flush, triggered by the
  sync policy — `graph.h:46` `SyncMatrixFunc`); `delta_mxm.c` (mxm
  that accounts for pending deltas without flushing).

Read order: `graph.h` (30 lines tell you the whole architecture) →
`delta_matrix.h` state diagrams → the four delta C files → then
GraphBLAS's format/algorithm anchors as the layer below.

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
  companion; the code comments above cover the same ground

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
