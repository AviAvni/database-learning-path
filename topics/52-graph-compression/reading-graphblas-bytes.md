# Bytes per edge in FalkorDB's matrices: counting, measuring, and what is left to win

The other guides in this topic quote bits per edge from papers. This one asks
the question for the engine you work on: how many bytes does one edge cost in a
SuiteSparse:GraphBLAS matrix as FalkorDB builds it, how far is that from what a
codec could reach, and which of the two levers — a better codec or better
vertex ids — has anything to work with inside GraphBLAS at all? Everything here
is either read from FalkorDB's or GraphBLAS's source, or measured by code in
`experiments/probe/` (a C probe linked against FalkorDB's own `libgraphblas.a`,
and a bulk-load recipe for the module). Two of the three numbers do not agree
with each other, and the guide says so.

Anchors: FalkorDB (C engine) at `ccb449a9a`, SuiteSparse:GraphBLAS at the pin
`7701390417` (v10.5.2; `resources/codebases.md`) for the header; the probe links
the `libgraphblas.a` of the FalkorDB checkout's own build, which is v10.5.0
(`deps/native-deps.lock`, `GxB_IMPLEMENTATION_MAJOR/MINOR/SUB` =
10/5/0 in its header).

## The problem in one sentence

What does a FalkorDB relationship cost in memory per edge, in bytes, and is the
number set by the codec (how a matrix is encoded) or by something upstream of it
(how vertices are numbered)?

## The concepts, step by step

### Step 1 — counting a sparse matrix by hand

> **In:** a sparse matrix with `n` rows and `m` entries, stored by row.
> **Out:** a formula for its bytes, and the three places it can be shrunk.

A GraphBLAS matrix in **sparse** format is CSR: an offsets array `Ap` of
`n + 1` entries, a column-index array `Ai` of `m` entries, and a values array
`Ax` of `m` entries (header, `GraphBLAS.h` the sparse description; see also
[reading-suitesparse-internals.md](../20-graphblas/reading-suitesparse-internals.md)
Step 1). Bytes = `(n + 1)·|Ap| + m·|Ai| + m·|Ax|`, where `|·|` is the byte
width of the element. Three shrinkable terms:

* `|Ai|`: 8 bytes with 64-bit indices, 4 with 32-bit;
* `|Ax|`: the value width, or **zero** for an **iso-valued** matrix — one whose
  entries all hold the same value, so GraphBLAS stores a single copy (the
  `GxB_ISO` flag);
* `|Ap|` and `n`: the offsets array is amortised over the edges — `n / m` times
  its width.

Worked for a 1,000,000-row matrix with 8,000,000 entries, 64-bit everything and
8-byte values: `8·1,000,001 + 8·8,000,000 + 8·8,000,000` = 8.0 MB + 64 MB + 64
MB = 136 MB, **17.0 bytes per edge**. With 32-bit indices and an iso boolean:
`4·1,000,001 + 4·8,000,000 + 1` = 36.0 MB = **4.5 bytes per edge**. Note the
second line: the index array is 32 of the 36 bits.

### Step 2 — what GraphBLAS actually chooses (probe)

> **In:** Step 1's formula. **Out:** the measured bytes per edge for the three
> matrix kinds FalkorDB creates, using FalkorDB's own library.

Since v10, GraphBLAS can store `Ap`, `Ah` and `Ai` as 32-bit or 64-bit integers
per matrix; the header exposes the choice as hints and as readable bit widths
(`GraphBLAS.h:1559-1561` define `GxB_ROWINDEX_INTEGER_HINT`,
`GxB_COLINDEX_INTEGER_HINT`, `GxB_OFFSET_INTEGER_HINT`; the `*_INTEGER_BITS`
options read the result). `GxB_Matrix_memoryUsage` (`GraphBLAS.h:4066`) returns
the bytes. `experiments/probe/graphblas_bytes.c` builds a 1M × 1M matrix from
8,000,000 uniform random edge draws and reports all of it. Run on this
machine against the FalkorDB build's `libgraphblas.a` (build line is in the
file's header):

```
GrB_BOOL as built      nvals=7999964 bytes=36000108 bytes/edge=4.500 offset_bits=32 index_bits=32 iso=1 sparsity_status=2
GrB_BOOL, ISO off      nvals=7999964 bytes=44000064 bytes/edge=5.500 offset_bits=32 index_bits=32 iso=0 sparsity_status=2
GrB_UINT64 (edge ids)  nvals=7999964 bytes=99999812 bytes/edge=12.500 offset_bits=32 index_bits=32 iso=0 sparsity_status=2
```

(Duplicate draws collapse: 8,000,000 draws give 7,999,964 distinct entries.)
Reading it against Step 1:

* A boolean matrix whose entries are all `true` is iso, and the library
  detects and keeps that: 36,000,108 bytes = `4·1,000,001` (`Ap`) + `4·7,999,964`
  (`Ai`) + a few header bytes: **4.5 B/edge, 36 bits**. The values array costs
  nothing.
* Clearing the iso flag adds one byte per edge: 5.5.
* A `GrB_UINT64` matrix with a distinct value per entry (an edge id): `4 + 8 =
  12` plus the offsets, **12.5**.
* The library chose **32-bit** offsets and indices for all three without being
  asked (`offset_bits=32 index_bits=32`): 1M rows is far below 2³². The
  per-edge cost is therefore *already half* of the textbook 64-bit CSR.

### Step 3 — which matrices FalkorDB builds per edge

> **In:** Step 2's three kinds. **Out:** the per-edge bytes implied by what the
> engine actually allocates, as arithmetic from Step 2.

`Graph_AddRelationType` builds each relationship type as a `Tensor`, and
`Tensor_new` is one line (`src/graph/tensor/tensor.c:50`):

```c
// src/graph/tensor/tensor.c — Tensor_new, 44-53
  44  Tensor Tensor_new
  45  (
  46  	GrB_Index nrows,  // # rows
  47  	GrB_Index ncols   // # columns
  48  ) {
  49  	Tensor T;
  50  	Delta_Matrix_new(&T, GrB_UINT64, nrows, ncols, true);
  51  
  52  	return T;
  53  }
```

Line 50 says: a `GrB_UINT64` matrix (an edge id per entry, or a pointer to a
vector of ids when the pair has several parallel edges — topic 20's delta-matrix
guide), and the final `true` asks for a **transposed copy**.
`Delta_Matrix_new` (`src/graph/delta_matrix/delta_new.c:73-76`) then builds a
second full delta matrix of type `GrB_BOOL` with the dimensions swapped, so
reverse traversals are as fast as forward ones. The graph's `adjacency_matrix`
(`src/graph/graph.c:346`) is a `GrB_BOOL` matrix with a transposed copy too. The
module sets every matrix to CSR (`src/module.c:132-133`: `GxB_BY_ROW`) and
initialises GraphBLAS with no index-width hints
(`src/module.c:120-146`).

Arithmetic from Step 2, for a settled graph (nothing pending in the delta
matrices) with 32-bit indices: the relation `R` costs **12.5 B per edge**, its
boolean transpose **4.5–5.5** (iso or not), together **17.0–18.0 B/edge**, about
136–144 bits. Each distinct *connected pair* also appears in the adjacency
matrix and its transpose: another 9–11 B per pair. A graph with no parallel
edges therefore costs 26–29 B per edge before attributes and the edge
datablock.

### Step 4 — what FalkorDB actually reports (measured, and not explained)

> **In:** Step 3's 17–18 B per edge prediction. **Out:** FalkorDB's own number,
> and the disagreement.

`experiments/probe/falkordb_memory_gen.py` writes two seeded bulk-load CSVs
(200,000 nodes; 1,000,000 and 4,000,000 random edges). Loading each through the
bulk loader into a FalkorDB module started with `redis-server` on a spare port
and asking `GRAPH.MEMORY USAGE`, on this machine (module version 999999, a
development build of the C engine, `bin/macos-arm64v8-release/falkordb.so`):

| edges | `relation_matrices_sz_mb` | `total_graph_sz_mb` |
|---|---|---|
| 1,000,000 | 26 | 36 |
| 2,000,000 | 49 | 68 |
| 4,000,000 | 94 | 131 |

The numbers are whole MB, so use the slope. From 1 M to 4 M edges the
relation matrices grow `94 − 26 = 68 MB` for 3 M edges: 22.7 MB per million, or
**about 23.8 bytes per edge** (taking MB as 2²⁰ bytes), and the whole graph
`131 − 36 = 95 MB`, **33.2 B/edge** including the edge datablock (about 8.5–9
B/edge: 17 MB at 2 M edges).

Step 3 predicted 17–18. The measurement is 6–7 B/edge more. Candidates, none
tested: the matrices are not in the settled state the probe measures (the delta
matrices hold pending entries, which are hypersparse and cost about 20 B per
entry: `Ah`, `Ap`, `Ai` and an 8-byte value), or the library's integer width
decision differs inside the module from the standalone probe, or the memory
report counts the transposed matrix differently. The honest statement: the
probe's number is what a *settled* matrix costs; FalkorDB's own accounting for
a freshly bulk-loaded graph is about 40% higher, and which of the three
explanations is right is an exercise ([README](README.md), exercise 6).

### Step 5 — what a codec could reach, and what ids could

> **In:** Steps 2–4. **Out:** the floor for a graph with no structure, and the
> bits per edge this topic's lanes put next to it.

A random graph is the *worst* case for compression: with `m` edges chosen
among the `n²` possible, the information needed is `log₂ C(n², m) ≈
m·(log₂(n²/m) + log₂ e)` bits. For the probe's `n = 1,000,000`, `m = 8,000,000`
that is `log₂(10¹²/8·10⁶) + 1.443 = 16.93 + 1.44 =` **18.4 bits per edge**,
against 36 bits in GraphBLAS: even with no structure at all, a codec that stores
the sorted edges near their entropy has 2× to win. For structured graphs the
floor falls far below that — the web-graph papers report 3–5 bits per edge
([reading-webgraph-framework.md](reading-webgraph-framework.md) Step 5), and this
topic's lane 1 measures 13–24 on a graph where the generator plants communities.
The point of Step 7 of that guide stands here too: the gap between 18.4 and 3
is not the codec but the numbering.

### Step 6 — where each lever would plug into GraphBLAS

> **In:** Step 5. **Out:** the honest list of what the library lets you do.

* **Vertex ids** are the one lever that is free to pull: GraphBLAS knows nothing
  about them, and renumbering a graph (a permutation applied to rows and
  columns) leaves every algorithm's result unchanged up to relabelling. It does
  not shrink a sparse matrix — `Ai` is a fixed-width integer array — but it
  changes how the `Ai` gathers of a multiply hit the cache, which is lane 1's
  `CSR ns/e` column. It is also the *precondition* for the next lever.
* **A codec** has no home inside the sparse format: `Ai` must be random-access.
  The compression options in the header (`GxB_COMPRESSION_NONE`, `LZ4`,
  `LZ4HC`, `ZSTD`, `GraphBLAS.h:2806-2810`) are the ones for serializing a
  matrix to a blob — useful for the RDB, not for traversal. A compressed
  *traversal* format would have to live next to the matrix, as Ligra+ does with
  its byte-coded edge array
  ([reading-ligra-plus.md](reading-ligra-plus.md)), for read-mostly relationships.
* **Smaller values** are a different lever: the iso flag on a boolean costs
  nothing and saves a byte per edge (5.5 → 4.5); a `UINT64` edge id per entry is
  the single largest term (8 of 12.5 bytes). Where a relationship type has no
  parallel edges and no per-edge attributes, an iso boolean carrying "this pair is
  connected" plus an id recovered from the position would be 4.5 B/edge instead
  of 12.5 — a design question for the Rust rewrite, not a library change.

## How to read the code (with the concepts in hand)

Read `src/module.c:120-146` (what the engine tells GraphBLAS: CSR, no index
hints), then `src/graph/tensor/tensor.c:44-53`, `src/graph/delta_matrix/delta_new.c:10-83`
(the trio and its transpose) and `src/graph/graph.c:345-347`. Then
`experiments/probe/graphblas_bytes.c` for how little it takes to ask the library
for its own byte count.

## Where each step lives in this topic

| step | here |
|---|---|
| bytes = Ap + Ai + Ax | `Csr::plain_bits_per_edge`; the probe |
| the measured library numbers | `experiments/probe/graphblas_bytes.c` |
| FalkorDB's own number | `experiments/probe/falkordb_memory_gen.py` |
| id order vs codec | lane 1 (ids) and lane 3 (codec) |

## Questions (answer in notes.md)

1. A relationship type has no parallel edges. What does an iso `GrB_BOOL` matrix
   plus a position-derived id save per edge against the `UINT64` tensor, and
   what must stay true for the id to be recoverable?
2. Compute the bytes per edge of the `UINT64` tensor at `n = 5·10⁹` vertices
   (past 2³²). What changes?
3. The measurement and the arithmetic disagree by 6–7 B/edge. Design the
   smallest experiment (using the probe plus `GRAPH.MEMORY USAGE`) that
   separates "pending deltas" from "64-bit indices".
4. Lane 1 prints the plain CSR's bits per edge with the offset array (32 plus
   `32·n/m`). Reconcile that figure with the probe's 36 bits per edge.

## Done when

Answer each before unfolding it.

- [ ] You can compute a CSR matrix's bytes per edge from `n`, `m` and the
  element widths.

  <details><summary>Answer</summary>

  Bytes = (n + 1)·|Ap| + m·|Ai| + m·|Ax| (no |Ax| term if iso). n = 1 M,
  m = 8 M, 64-bit everything with 8-byte values: 8 + 64 + 64 = 136 MB, 17.0
  B/edge. With 32-bit indices and an iso boolean: 4 MB + 32 MB = 36 MB, 4.5
  B/edge.

  </details>

- [ ] You can state the probe's three results and what each tells you.

  <details><summary>Answer</summary>

  GrB_BOOL all-true 4.5 B/edge (iso: the values array is one entry), the same
  with iso off 5.5, GrB_UINT64 edge ids 12.5; offsets and indices were 32-bit
  in every case. The index array is 4 of the 4.5 bytes: the library's default
  32-bit width already halves the textbook 64-bit CSR, and the 8-byte edge id
  dominates the relation matrix.

  </details>

- [ ] You can list the matrices FalkorDB allocates per relationship edge and
  predict the total.

  <details><summary>Answer</summary>

  A UINT64 tensor (12.5 B/edge) and a boolean transposed copy (4.5–5.5), 17–18
  B/edge; the adjacency matrix and its transpose add 9–11 B per distinct
  pair (`tensor.c:50`, `delta_new.c:73-76`, `graph.c:346`). FalkorDB's own
  `GRAPH.MEMORY USAGE` slope on a freshly bulk-loaded graph is about 23.8
  B/edge for relation matrices, 6–7 above the settled-matrix arithmetic, and the
  gap is unexplained here.

  </details>

- [ ] You can say why ids are a lever GraphBLAS lets you pull and a codec is
  not.

  <details><summary>Answer</summary>

  Renumbering is a permutation of rows and columns, invisible to every
  algorithm and free of format changes; it improves cache behaviour of gathers
  and is the precondition for gap coding to work. A codec needs variable-length
  data that cannot be random-accessed, which the sparse format's fixed-width
  Ai forbids; the compression options in the header are for serialization.

  </details>

## References

- SuiteSparse:GraphBLAS v10.5.2 (pin `7701390417`) `Include/GraphBLAS.h`:
  integer hints `1559-1561`, `GxB_Matrix_memoryUsage` `4066`, compression
  constants `2806-2810`; v10.5.0 `libgraphblas.a` from the FalkorDB build for
  the probe.
- FalkorDB (C engine) at `ccb449a9a`: `src/module.c:120-146`,
  `src/graph/tensor/tensor.c:44-53`, `src/graph/delta_matrix/delta_new.c`,
  `src/graph/graph.c:345-347`.
- [reading-falkordb-delta-matrix.md](../20-graphblas/reading-falkordb-delta-matrix.md)
  — the M / DP / DM trio.
- Cover & Thomas, *Elements of Information Theory* — the `log₂ C(N, m)`
  approximation (not read here; the arithmetic is standard Stirling).
