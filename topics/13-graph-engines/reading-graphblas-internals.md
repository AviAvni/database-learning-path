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

Every anchor below was read at the revisions pinned in
[`resources/codebases.md`](../../resources/codebases.md):
**SuiteSparse:GraphBLAS at `1fd5475`** and **FalkorDB at `ccb449a9a`**.
Line numbers move; check one with
`python3 tools/pinned-source.py show GraphBLAS Include/GraphBLAS.h -r 1664:1667`
before you trust it. Three claims in the previous version of this
chapter did not survive that check, and each is called out where it
used to be.

## The problem in one sentence

Answer "who are the neighbors of these 10,000 nodes?" as one streaming
operation instead of 10,000 pointer walks — and then survive a single
edge insert without rewriting the read-optimized structure that made
the streaming possible.

## The concepts, step by step

### Step 1 — the graph is a boolean matrix; traversal is multiplication

> **In:** a directed graph: a set of nodes, and a set of ordered pairs
> (edges) over them.
> **Out:** the same graph as an n×n boolean matrix, and every traversal
> rewritten as a sparse matrix product.

An **adjacency matrix** A for a graph on n nodes is the n×n boolean
matrix with `A[i][j] = true` exactly when there is an edge i→j. Row i
of A is therefore node i's outgoing neighbour list, spelled as a row
instead of as a linked list.

Two more definitions before the algebra means anything:

- A **frontier** is the set of nodes a breadth-first search is
  currently standing on, written as a boolean vector x of length n
  (`x[i] = true` iff node i is in the set).
- **SpMV** is sparse matrix–vector multiply. Over the boolean
  semiring — where "+" is OR and "×" is AND — the entry `(A^T x)[j]`
  is `OR over i of (A[i][j] AND x[i])`, which is true exactly when
  *some* frontier node i has an edge to j.

So one SpMV is one whole BFS level, for the entire frontier at once:

```
 BFS frontier expansion = SpMV:  y<¬visited> = A^T x
 2-hop = A²,  triangles = A ⊙ A²      (⊙ = elementwise AND)
```

`A²` is the two-hop reachability matrix: `A²[i][k]` is true iff there
is some j with i→j and j→k. `A ⊙ A²` keeps a two-hop pair only when
the two endpoints are also directly connected — which is the
definition of a triangle.

Why it matters: one engine — the sparse-multiply kernels — serves
every traversal, so every kernel optimization (SIMD, parallelism,
format tricks) speeds up every query. That's FalkorDB's whole
architectural bet, and it is the bet this topic's headline stresses:
the same two-hop query costs 4 914 ns from random sources and 495 378
ns from supernodes ([FINDINGS.md](../../FINDINGS.md) row 13). The
matrix spelling does not make that ratio go away — but it changes
*which* part of the machine you can attack.

### Step 2 — CSR: the matrix is stored as offsets + neighbors

> **In:** the n×n boolean matrix from Step 1, with m true entries out
> of n² cells.
> **Out:** two contiguous integer arrays that hold only the m true
> entries, and give row i's neighbours as one slice.

Storing n² booleans is absurd for a real graph. On this topic's own
bench graph — 1 M nodes, 16.0 M directed edges
([notes.md](notes.md), baseline table) — that is:

```
 cells  = n²  = 1e6 × 1e6      = 1e12
 filled = m   = 16.0e6
 density      = 16.0e6 / 1e12  = 1.6e-5  →  99.9984% of the matrix is empty
```

So sparse matrices store only the present entries. The standard format
is **CSR** (compressed sparse row): one `offsets` array of n+1
positions plus one `targets` array of m column indices, where row i's
neighbours are the slice `targets[offsets[i] .. offsets[i+1]]`.

```
 CSR for 0->{5,9}, 1->{5}, 2->{}:
 offsets: [0, 2, 3, 3]
 targets: [5, 9, 5]
 neighbors(i) = targets[offsets[i] .. offsets[i+1]]   one slice, zero chase
```

Work the size on the bench graph, with 4-byte indices (a 1 M-node
graph needs 20 bits per id, so 32-bit indices fit with room to spare):

```
 offsets: (n + 1) × 4 B = 1 000 001 × 4 B =  4.00 MB
 targets:  m      × 4 B = 16 000 000 × 4 B = 64.00 MB
 total                                     = 68.00 MB
 bytes per edge = 68.00 MB / 16.0e6 = 4.25 B/edge
```

Both arrays are contiguous and prefetchable — topic 0's
sequential-beats-random, structurally guaranteed rather than hoped
for. Why it matters: "sparse matrix" and "read-optimized adjacency"
are the same object; the algebra of Step 1 runs over exactly this
layout, and 4.25 B/edge is the number every other representation in
this topic gets compared against.

### Step 3 — four sparsity formats, switched by density at runtime

> **In:** a sparse matrix and its current occupancy — how many of its
> n vectors are non-empty, and how many entries it holds.
> **Out:** one of four physical layouts, chosen by SuiteSparse without
> the caller asking.

SuiteSparse doesn't commit to CSR. `Include/GraphBLAS.h:1664-1667`
names four **sparsity formats**, and a matrix may be told which
subset it is allowed to take:

```c
// GraphBLAS.h, the sparsity-control bitmask values
  1664  #define GxB_HYPERSPARSE 1   // store entries in a list of non-empty vectors
  1665  #define GxB_SPARSE      2   // store entries in a compressed form (CSR/CSC)
  1666  #define GxB_BITMAP      4   // store entries in a bitmap
  1667  #define GxB_FULL        8   // store all entries, no need for indices
```

Read those as: **hypersparse** keeps an explicit list of only the
non-empty rows (so a matrix with 1 000 non-empty rows out of 1 000 000
costs 1 000 offsets, not 1 000 001); **sparse** is plain CSR/CSC;
**bitmap** is one presence bit per cell plus a values array — random
writes cost a bit-flip because there is no structure to shift; **full**
drops the index arrays entirely because every cell is present.

The switch is not vibes. `GraphBLAS.h:1715-1728` states the rule, and
`Source/include/GB_defaults.h:20` gives the constant:

```c
// GraphBLAS.h:1715-1728 — the hyper_switch rule, paraphrasing the comment
  1715  // ... let k be the number of non-empty vectors, n the number of
  1716  // vectors, and h the hyper_switch:
  //  ... 1717-1722: elided — the same rule stated for GxB_Matrix_Option_set ...
  1723  //   hypersparse -> sparse:  if  n <= 1  ||  k > 2*n*h
  1724  //   sparse -> hypersparse:  if  n >  1  &&  k <= n*h
  //  ... 1725-1727: elided ...
  1728  //
```

```c
// Source/include/GB_defaults.h
    20  #define GB_HYPER_SWITCH_DEFAULT (0.0625)
```

h = 0.0625 = 1/16. Now put the bench graph's numbers in. The matrix
is 1 M × 1 M, so n = 1 000 000 and the sparse→hypersparse threshold is:

```
 n · h = 1 000 000 × 0.0625 = 62 500 non-empty vectors
```

- The full adjacency matrix has roughly 1 M non-empty rows (nearly
  every node has an out-edge). k ≈ 1 000 000 > 62 500 → stays
  **sparse** (CSR).
- A rare relationship type touching 1 000 of the 1 M nodes has
  k = 1 000 ≤ 62 500 → flips to **hypersparse**, and its offsets array
  costs 1 000 slots rather than 1 000 001. That is a 1 000× saving on
  the offsets array, for free, decided by the library.
- Note the two thresholds differ by a factor 2 (`k > 2*n*h` going
  back the other way). That gap is hysteresis: without it, a matrix
  sitting at exactly k = n·h would flip format on every insert and
  delete.

This is the same menu as topic 12's encodings: representation follows
data shape, chosen by measurement, invisible above the API. Why it
matters: a label matrix with 3 labels and a supernode-heavy adjacency
matrix get different physical layouts for free.

### Step 4 — dot vs saxpy: two ways to multiply, picked per call

> **In:** a `GrB_mxm` call — operands A and B, an optional mask M, and
> the sparsity formats each of them currently holds.
> **Out:** one of four kernels (saxpy, dot2, dot3, dot4), chosen by a
> per-call control function, with different asymptotic cost.

Sparse matrix multiply has two classic algorithm families.

- **dot** computes each output entry `C(i,j)` as an inner product of a
  row of A' with a column of B. You compute *only the entries you
  ask for* — so dot is the right shape when the output is small or
  masked.
- **saxpy** (Gustavson's algorithm) walks the input and *scatters*
  each entry's contribution into a per-row accumulator. It touches
  the output implicitly, so it is the right shape when the output is
  large.

**Correction.** The previous version of this chapter quoted
`Source/mxm/GB_AxB_meta.c:20-21` as "the algorithm menu", with the
text *"generic: for any semiring; dot2/dot3: does `C=A'*B`,
`C<M>=A'*B` … saxpy: Gustavson + Hash"*. At pin `1fd5475` those two
lines say something else entirely:

```c
// Source/mxm/GB_AxB_meta.c
    20  // The method is chosen automatically:  a gather/scatter saxpy method
    21  // (Gustavson), or a dot product method.
```

The real menu, with the real asymptotics, is at the top of
`Source/mxm/GB_AxB_dot.c`:

```c
// Source/mxm/GB_AxB_dot.c
    21  // The dot product method for C=A'*B, C<M>=A'*B, or C<!M>=A'*B computes
    22  // C(i,j) = A(:,i)'*B(:,j) for each entry C(i,j).  dot2 computes C=A'*B
    23  // and C<!M>=A'*B, taking Omega(m*n) time ...
    24  // ... dot3 computes C<M>=A'*B, and only examines entries in the
    25  // mask M, taking Omega(nnz(M)) time ...
    26  // ... dot4 computes C+=A'*B when C is full ...
```

and the saxpy side names its three variants in its own signature:

```c
// Source/mxm/GB_AxB_saxpy.c
    18  GrB_Info GB_AxB_saxpy               // C = A*B using Gustavson/Hash/Bitmap
```

The *choice* is made per call in
`Source/mxm/GB_AxB_meta_adotb_control.c`: saxpy is the default (`:36`
sets `GB_USE_SAXPY`), and dot4 (`:72-77`), dot3 (`:78-82`) and dot2
(`:83-87`) each override it under stated conditions. The dot3
condition is spelled out in `Source/mxm/GB_mxm.h:235-243`: dot3 is
eligible iff there is a mask, the mask is not complemented, and the
mask is sparse or hypersparse.

Work the Ω's on the triangle query over the bench graph, where the
mask is the adjacency matrix itself (`C<A> = A²`, Step 5):

```
 m = n = 1 000 000    (the matrix is 1 M × 1 M)
 nnz(M) = nnz(A) = 16 000 000

 dot2:  Omega(m · n)    = 1e6 × 1e6   = 1.0e12 cell visits
 dot3:  Omega(nnz(M))   =               1.6e7  cell visits
 ratio  = 1.0e12 / 1.6e7 = 62 500×
```

Sixty-two thousand times less work, from the same `GrB_mxm` call, for
no reason other than that a sparse mask was supplied and SuiteSparse
noticed. BFS mapping: a small frontier against a big adjacency matrix
gives a sparse mask and wants dot3; a huge frontier makes the mask
useless and wants saxpy. Why it matters: the SAME `GrB_mxm` call is
executed by different algorithms at frontier size 10 vs 10⁶ — the
engine re-plans per step, which hand-written BFS code never does.

### Step 5 — masks: the predicate pushed into the kernel

> **In:** an operation `C = A · B` plus a boolean matrix M of the same
> shape as C.
> **Out:** `C<M> = A · B` — output produced only where M is true, with
> the rest never computed rather than computed and discarded.

A **mask** is a boolean matrix or vector passed alongside any
GraphBLAS operation, restricting WHERE output may be produced. In BFS
the complement mask `¬visited` performs the visited check inside the
multiply. In triangle counting `C<A> = A²` evaluates A² only at
positions where an edge already exists, so the full A² — which on the
bench graph would be up to 10¹² cells — is never built.

There is a subtlety worth reading the source for, because it decides
whether masking actually saves work. There are two places a mask can
be applied:

1. **Inside the kernel**, by dot3, which walks the mask and computes
   nothing else. That is the Ω(nnz(M)) path from Step 4.
2. **After the fact**, by `GB_masker`, which computes Z = A·B in full
   and then merges. `Source/mask/GB_masker.c:2` and `:10` say what it
   does; `:14-15` says who calls it — only `GB_mask`, which is called
   only from `GB_accum_mask`. And `GB_AxB_meta.c:15-18` warns that the
   algorithm *may* choose this late path.

```c
// Source/mask/GB_masker.c
     2  // R = masker (C, M, Z):  compute C<M>=Z, returning the result in R.
  //  ... 3-9: elided — argument description ...
    10  // R, M, and Z can be sparse, hypersparse, bitmap, or full ... does R=C ; R<M>=Z
  //  ... 11-13: elided ...
    14  // GB_masker is only called by GB_mask, which itself is only called
    15  // by GB_accum_mask.
```

So "I passed a mask" and "the mask saved me work" are different
claims. Only path 1 saves the Ω. Why it matters: this is topic 10's
predicate pushdown, one level down — when the mask reaches the
innermost loop of the kernel, it is also the mechanism behind the
WCOJ equivalence in [reading-wcoj.md](reading-wcoj.md); when it does
not, you paid for the intermediate anyway.

### Step 6 — the write problem: CSR hates single-edge inserts

> **In:** a live CSR adjacency matrix and one `CREATE (a)-[:R]->(b)`.
> **Out:** the cost of applying that one edge in place — and the
> reason no graph database does it that way.

CSR's strength — everything contiguous — is exactly why it cannot
absorb writes. Inserting one edge i→j means shifting the tail of the
`targets` array and bumping every offset after row i:

```
 targets memmove: on average half of 64.00 MB = 32.0 MB
 offsets bump:    on average half of  4.00 MB =  2.0 MB
 per single-edge insert                       = 34.0 MB touched
```

At a generous 20 GB/s of achievable memmove bandwidth that is ~1.7 ms
of pure memory traffic *per edge*. A graph database takes single-edge
writes constantly, so raw CSR is unusable as the live structure.

The generic fix is topic 4's LSM idea applied to adjacency: keep the
read-optimized structure immutable, buffer changes in a small mutable
overlay, merge in the background. Every system in this topic grows
this mechanism. SuiteSparse has its own version — `GB_PENDING_INIT`
at `Source/include/GB_defaults.h:27` is the initial size of a
matrix's pending-tuple list, 256 entries — and FalkorDB builds an
explicit one on top. Why it matters: the overlay design decides write
latency, read overhead, AND when the expensive merge happens.

### Step 7 — Delta_Matrix: main + additions + deletions

> **In:** one logical graph matrix, and a stream of single-entry sets
> and removes.
> **Out:** three GraphBLAS matrices whose combination is the logical
> matrix, with writes O(1)-ish and the rebuild deferred behind a
> counted threshold.

FalkorDB wraps every graph matrix in a `Delta_Matrix`. The struct is
at `src/graph/delta_matrix/delta_matrix.h:108-115` — **not** at
`:17-22`, which is where the accessor macros live:

```c
// src/graph/delta_matrix/delta_matrix.h
   108  struct _Delta_Matrix {
   109      bool dirty;                    // Indicates if matrix requires sync
   110      GrB_Matrix matrix;             // Underlying GrB_Matrix
   111      GrB_Matrix delta_plus;         // Pending additions
   112      GrB_Matrix delta_minus;        // Pending deletions
   113      struct _Delta_Matrix *transposed;
   114      pthread_mutex_t mutex;         // Lock
   115  };
```

```
 M           matrix        (read-optimized, CSR inside)
 DP          delta_plus    pending adds
 DM          delta_minus   pending deletes
 read(i,j) = (M(i,j) OR DP(i,j)) AND NOT DM(i,j)
```

**Correction.** The previous version of this chapter said the deltas
are "kept in the write-friendly bitmap/hypersparse world". They are
not allowed to be bitmap. `delta_get_set.c:44-53` pins them:

```c
// src/graph/delta_matrix/delta_get_set.c, inside Delta_Matrix_setElement
    44  // Force delta matrices to be hypersparse
  //  ... 45: elided ...
    46      info = GxB_set(A->delta_plus, GxB_SPARSITY_CONTROL, GxB_HYPERSPARSE);
    47      info = GxB_set(A->delta_plus, GxB_HYPER_SWITCH, GxB_ALWAYS_HYPER);
  //  ... 48-51: elided — the hyper-hash is disabled on both deltas ...
    52      info = GxB_set(A->delta_minus, GxB_SPARSITY_CONTROL, GxB_HYPERSPARSE);
    53      info = GxB_set(A->delta_minus, GxB_HYPER_SWITCH, GxB_ALWAYS_HYPER);
```

M is left free to be `GxB_SPARSE | GxB_HYPERSPARSE`; DP and DM are
pinned to hypersparse with `GxB_ALWAYS_HYPER` (Step 3's constant,
forced). That is the right choice for the reason Step 3 gave: a delta
holding a few thousand entries has a few thousand non-empty rows out
of a million, so a CSR offsets array would be 1 000 001 slots of
almost entirely zeros.

**Correction.** The previous version's `set()` pseudocode keyed the
branch on DM ("if the entry is in DM, clear it"). The real code keys
on **M**:

```c
// src/graph/delta_matrix/delta_set_element_bool.c
  //  ... 1-30: elided — argument checks and matrix extraction ...
    31      bool in_m;
    32      info = GxB_Matrix_isStoredElement(m, i, j);
  //  ... 33-35: elided ...
    36          info = GrB_Matrix_removeElement(dm, i, j);   // re-add: drop the tombstone
  //  ... 37-38: elided ...
    39          info = GrB_Matrix_setElement_BOOL(dp, true, i, j);   // never touch M
```

The distinction matters: re-adding an entry that M already holds is a
DM removal, and adding a genuinely new entry is a DP insert. Neither
path touches M, which is the whole point.
`delta_remove_element.c:36-43` is the mirror image — in M means set
DM, not in M means remove from DP.

Reads are also not three probes. `delta_isStored.c` short-circuits
DP → DM → M (`:26`, `:32`, `:39`), and `delta_extract.c` does the same
at `:25`, `:31`, `:38`. An entry that lives in DP costs **one** probe,
not three.

**Correction.** The previous version described `wait()` as a single
`M = (M ∪ DP) ∖ DM` rebuild. `delta_wait.c` (218 lines) is two
independent flushes, each gated on its own counter:

```c
// src/graph/delta_matrix/delta_wait.c
    13  static void Delta_Matrix_sync_deletions(Delta_Matrix C) {
  //  ... 14-28: elided ...
    29      info = GrB_transpose(m, dm, NULL, m, GrB_DESC_RSCT0);  // M = M .* !DM
  //  ... 30-32: elided ...
    33      info = GrB_Matrix_clear(dm);
  //  ... 34-35: elided ...
    36  static void Delta_Matrix_sync_additions(Delta_Matrix C) {
  //  ... 37-50: elided ...
    51      info = GrB_Matrix_assign(m, dp, NULL, dp, GrB_ALL, nrows,
    52                               GrB_ALL, ncols, GrB_DESC_S);  // M |= DP
  //  ... 53-55: elided ...
    56      info = GrB_Matrix_clear(dp);
  //  ... 57-88: elided — Delta_Matrix_sync begins at :59 ...
    89          if(delta_minus_nvals >= delta_max_pending_changes) {
  //  ... 90-96: elided ...
    97          if(delta_plus_nvals >= delta_max_pending_changes) {
  //  ... 98-102: elided ...
   103      info = GrB_wait(m,  GrB_MATERIALIZE);
   104      info = GrB_wait(dm, GrB_MATERIALIZE);
   105      info = GrB_wait(dp, GrB_MATERIALIZE);
```

Deletions flush via a masked transpose, additions via an assign, and
with `force_sync == false` each side flushes **only when its own
pending count crosses a threshold**. The threshold is a config knob
with a default you can read:

```c
// src/configuration/config.h
    19  #define DELTA_MAX_PENDING_CHANGES_DEFAULT 10000
```

That number is the amortisation. Work it against Step 6's cost:

```
 flush-per-write:  34.0 MB touched per edge
 flush per 10 000: one rebuild touches O(nnz) ≈ 16.0e6 entries
                   amortised = 16.0e6 / 10 000 = 1 600 entries/insert
 vs a per-write CSR rebuild of 16.0e6 entries/insert
 improvement factor = 10 000×  (exactly the threshold, by construction)
```

Even the multiply is delta-aware. `delta_mxm.c:44` states the
identity `(A * (M + 'delta-plus'))<!'delta-minus'>` — but read `:47`
before believing that the deltas are free on both sides:

```c
// src/graph/delta_matrix/delta_mxm.c
    44  // C = A * (M + 'delta-plus') <!'delta-minus'>
  //  ... 45-46: elided ...
    47      ASSERT(Delta_Matrix_Synced(A));      // A must already be flushed
  //  ... 48-73: elided ...
    74      info = GrB_mxm(mask,  NULL, NULL, semiring, a, dm, NULL);   // mask  = A·DM
  //  ... 75-85: elided ...
    86      info = GrB_mxm(accum, NULL, NULL, semiring, a, dp, NULL);   // accum = A·DP
  //  ... 87-103: elided ...
   104      info = GrB_mxm(_C, mask, NULL, semiring, a, m, GrB_DESC_RSC);
  //  ... 105-106: elided ...
   107      info = GrB_eWiseAdd(_C, NULL, NULL, plus, _C, accum, NULL);
```

Only **B**'s deltas are handled without flushing; A is asserted
synced. An entry may be in M, in DP, or in M+DM (deleted but not yet
flushed) — never in both DP and DM; the ASCII state diagrams at
`delta_matrix.h:26-106` enumerate the legal states and are the real
specification. And this IS topic 4's LSM: DP the memtable, M the SST,
DM the tombstones, `Delta_Matrix_sync` the compaction, 10 000 the
compaction trigger. Why it matters: this overlay is what makes "graph
as matrices" viable as a *database* rather than an analytics batch
tool — reads stay algebraic, writes stay O(1)-ish, and the rebuild
bill is paid on FalkorDB's schedule.

## Where each step lives in the code

**GraphBLAS** ([SuiteSparse](https://github.com/DrTimothyAldenDavis/GraphBLAS),
pinned at `1fd5475`):

| Step | Anchor | What is there |
|---|---|---|
| 3 | `Include/GraphBLAS.h:1664-1667` | the four sparsity format constants |
| 3 | `Include/GraphBLAS.h:1556`, `:1559` | `GxB_HYPER_SWITCH`, `GxB_BITMAP_SWITCH` field ids |
| 3 | `Include/GraphBLAS.h:1715-1728` | the exact switch rule, both directions |
| 3 | `Include/GraphBLAS.h:1734` | `GxB_ALWAYS_HYPER` / `GxB_NEVER_HYPER` |
| 3 | `Source/include/GB_defaults.h:20` | `GB_HYPER_SWITCH_DEFAULT (0.0625)` |
| 4 | `Source/mxm/GB_AxB_dot.c:21-26` | dot2 Ω(m·n), dot3 Ω(nnz(M)), dot4 |
| 4 | `Source/mxm/GB_AxB_saxpy.c:18` | Gustavson / Hash / Bitmap |
| 4 | `Source/mxm/GB_AxB_meta_adotb_control.c:36`, `:72-87` | the per-call choice |
| 4 | `Source/mxm/GB_mxm.h:235-243` | `GB_AxB_dot3_control` — when dot3 is legal |
| 5 | `Source/mask/GB_masker.c:2`, `:10`, `:14-15` | the *late* mask path and its only caller |
| 5 | `Source/mxm/GB_AxB_meta.c:15-18` | the warning that masking may be deferred |
| 6 | `Source/include/GB_defaults.h:27` | `GB_PENDING_INIT 256` — GraphBLAS's own overlay |

**FalkorDB** ([repo](https://github.com/FalkorDB/FalkorDB), pinned at
`ccb449a9a`):

| Step | Anchor | What is there |
|---|---|---|
| 1 | `src/graph/graph.h:44` | `struct Graph` opens |
| 1 | `src/graph/graph.h:48-51` | the four matrix members (was cited as `:48-52`) |
| 7 | `src/graph/graph.h:42` | `SyncMatrixFunc` typedef (was cited as `:46`) |
| 7 | `.../delta_matrix/delta_matrix.h:17-22` | accessor **macros**, not the struct |
| 7 | `.../delta_matrix/delta_matrix.h:108-115` | `struct _Delta_Matrix` — the actual trio |
| 7 | `.../delta_matrix/delta_matrix.h:26-106` | the ASCII state diagrams (was cited as `:26-80`) |
| 7 | `.../delta_set_element_bool.c:31-39` | write path, branching on M |
| 7 | `.../delta_remove_element.c:36-43` | delete path, the mirror image |
| 7 | `.../delta_isStored.c:26,32,39` | short-circuiting DP → DM → M read |
| 7 | `.../delta_extract.c:25,31,38` | the same order, for range extraction |
| 7 | `.../delta_wait.c:13-34`, `:36-57`, `:89`, `:97`, `:103-105` | the two flushes and their thresholds |
| 7 | `src/configuration/config.h:19` | `DELTA_MAX_PENDING_CHANGES_DEFAULT 10000` |
| 7 | `.../delta_get_set.c:44-53` | DP/DM pinned hypersparse |
| 7 | `.../delta_mxm.c:44`, `:47`, `:74`, `:86`, `:104`, `:107` | delta-aware multiply, and the A-must-be-synced assert |

Read order: `graph.h:44-53` (ten lines tell you the whole
architecture) → `delta_matrix.h:26-106` state diagrams → `delta_wait.c`
(the only file that tells you *when* the price is paid) → the three
smaller delta C files → then GraphBLAS's format/algorithm anchors as
the layer below.

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

## Done when

Answer each before unfolding it.

- [ ] You can write the `read = (M ∪ DP) ∖ DM` identity, explain what each of the three matrices holds, and say how many probes a read actually costs.

  <details><summary>Answer</summary>

  `read(i,j) = (M(i,j) OR DP(i,j)) AND NOT DM(i,j)`. M is the
  read-optimized main matrix (CSR or hypersparse inside); DP holds
  pending additions; DM holds pending deletions — tombstones, because
  removing an entry from M in place is Step 6's problem again. Both
  deltas are pinned hypersparse at `delta_get_set.c:46-53`.

  A read is **not** three probes. `delta_isStored.c` tests DP first
  (`:26`), then DM (`:32`), then M (`:39`), returning early. An entry
  that was just written costs one probe; only an entry absent from
  both deltas pays all three.

  </details>

- [ ] You can explain why CSR is hostile to single-edge inserts, and compute the cost on this topic's 16 M-edge graph.

  <details><summary>Answer</summary>

  CSR stores row i's neighbours as a contiguous slice, so inserting
  one entry into row i means shifting every later element of
  `targets` and bumping every later element of `offsets`. On the
  bench graph (1 M nodes, 16.0 M edges, 4-byte indices) `targets` is
  16e6 × 4 B = 64.0 MB and `offsets` is 1 000 001 × 4 B = 4.0 MB; an
  average insert lands mid-array, so ≈ 32.0 MB + 2.0 MB = 34.0 MB is
  touched per edge.

  That is the argument for Delta_Matrix, and FalkorDB's threshold
  makes the saving explicit: `DELTA_MAX_PENDING_CHANGES_DEFAULT` is
  10 000 (`config.h:19`), so one rebuild is amortised over 10 000
  writes — a 10 000× reduction in rebuild work per write, by
  construction.

  </details>

- [ ] You can say when dot beats saxpy for a BFS step, in terms of frontier size against matrix dimension, and quote the two Ω's.

  <details><summary>Answer</summary>

  `GB_AxB_dot.c:22-25`: dot2 computes `C=A'*B` and `C<!M>=A'*B` in
  **Ω(m·n)**; dot3 computes `C<M>=A'*B` examining only entries of M,
  in **Ω(nnz(M))**. `GB_mxm.h:235-243` says dot3 is only eligible when
  a non-complemented, sparse-or-hypersparse mask exists.

  So a small frontier gives a small sparse mask and wants dot3: on
  the triangle query over the bench graph, dot3's Ω(nnz(A)) = 1.6e7
  against dot2's Ω(n²) = 1.0e12, a factor of 62 500. A frontier of
  10⁶ on a 10⁶-node graph makes the mask nearly full, the dot3
  eligibility test fails or stops paying, and saxpy — the default set
  at `GB_AxB_meta_adotb_control.c:36` — streams the whole thing
  instead.

  </details>

- [ ] You can explain what a mask pushes into the kernel, what it saves, and the one case where passing a mask saves nothing.

  <details><summary>Answer</summary>

  A mask restricts where output may be produced, so `C<A> = A²`
  computes only the 16 M positions where an edge already exists
  rather than the up-to-10¹² cells of A². That is predicate pushdown
  reaching the innermost loop, and it is the reason the masked-SpMV
  lane in this topic's bench has something to attack on supernodes —
  the oracle spends 6.28 ns per distinct node reached there against
  4.81 ns from random sources (78 907 against 1022 nodes per query),
  and a mask makes re-walking unrepresentable.

  The case where it saves nothing: `GB_masker.c` is the *late* path —
  it computes Z in full and then merges `R = C ; R<M> = Z`. It is
  reached from `GB_accum_mask` (`GB_masker.c:14-15`), and
  `GB_AxB_meta.c:15-18` warns the algorithm may choose to defer the
  mask to it. A deferred mask costs the intermediate you were hoping
  to avoid.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the Delta_Matrix-to-LSM vocabulary mapping.

  <details><summary>Answer</summary>

  DP is the memtable (small, mutable, hypersparse, absorbs writes);
  M is the SST/level (large, immutable in practice, read-optimized);
  DM is the tombstone set; `Delta_Matrix_sync` (`delta_wait.c:59`) is
  the compaction; `DELTA_MAX_PENDING_CHANGES_DEFAULT` = 10 000
  (`config.h:19`) is the compaction trigger.

  The one place the analogy is tighter than LSM: the two sides flush
  *independently* (`delta_wait.c:89` for deletions, `:97` for
  additions), so a delete-heavy workload can compact tombstones
  without rewriting for additions. The one place it is looser: there
  is no level hierarchy — one M, one DP, one DM, full stop.

  </details>

## References

**Papers**
- Davis — "Algorithm 1000: SuiteSparse:GraphBLAS: Graph Algorithms in
  the Language of Sparse Linear Algebra" (ACM TOMS 2019) — optional
  companion; the code comments cited above cover the same ground and
  are the ones that were actually checked for this chapter.

**Code** (all line numbers verified at the pins named at the top)

| Repo | File | Lines | What |
|---|---|---|---|
| GraphBLAS | `Include/GraphBLAS.h` | 1556, 1559, 1664-1667, 1715-1728, 1734 | format constants, switch fields, the switch rule |
| GraphBLAS | `Source/include/GB_defaults.h` | 20, 27 | hyper switch default 0.0625; pending-tuple init 256 |
| GraphBLAS | `Source/mxm/GB_AxB_dot.c` | 21-26 | dot2/dot3/dot4 and their Ω's |
| GraphBLAS | `Source/mxm/GB_AxB_saxpy.c` | 18 | Gustavson / Hash / Bitmap |
| GraphBLAS | `Source/mxm/GB_AxB_meta_adotb_control.c` | 36, 72-87 | which kernel, per call |
| GraphBLAS | `Source/mxm/GB_AxB_meta.c` | 15-18, 20-21 | saxpy-or-dot; mask may be deferred |
| GraphBLAS | `Source/mxm/GB_mxm.h` | 235-243 | `GB_AxB_dot3_control` |
| GraphBLAS | `Source/mask/GB_masker.c` | 2, 10, 14-15, 21-33 | the late mask path and its truth table |
| FalkorDB | `src/graph/graph.h` | 42, 44, 48-53 | `SyncMatrixFunc`; the graph as four matrices |
| FalkorDB | `src/graph/delta_matrix/delta_matrix.h` | 17-22, 26-106, 108-115 | macros; state diagrams; the struct |
| FalkorDB | `src/graph/delta_matrix/delta_set_element_bool.c` | 31-39 | write path |
| FalkorDB | `src/graph/delta_matrix/delta_remove_element.c` | 36-43, 50-81 | delete path, bulk delete |
| FalkorDB | `src/graph/delta_matrix/delta_isStored.c` | 26, 32, 39 | short-circuiting read |
| FalkorDB | `src/graph/delta_matrix/delta_extract.c` | 25, 31, 38 | same order for extraction |
| FalkorDB | `src/graph/delta_matrix/delta_wait.c` | 13-34, 36-57, 59-113, 89, 97, 103-105 | the two flushes and their gates |
| FalkorDB | `src/graph/delta_matrix/delta_get_set.c` | 44-53 | DP/DM pinned hypersparse |
| FalkorDB | `src/graph/delta_matrix/delta_mxm.c` | 44, 47, 74, 86, 104, 107 | delta-aware mxm |
| FalkorDB | `src/configuration/config.h` | 19, 33 | the flush threshold and its config enum |
