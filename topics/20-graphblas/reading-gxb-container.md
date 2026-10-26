# GxB_Container: handing a matrix across the library boundary without copying it

Every other guide in this topic is about what SuiteSparse does *inside* a
`GrB_Matrix`. This one is about the door: how a matrix gets in and out. It
matters for the Rust rewrite because the rewrite will own its own CSR (capstone
M13/M20) and will call GraphBLAS for the operations it has not rewritten yet;
whether that call costs a copy of the whole matrix or a handful of pointer
assignments decides whether the two can coexist in one process. SuiteSparse
10.0.0 added the mechanism — **`GxB_Container`** — and this chapter reads it
in the source, then measures it against the copying interface it replaces.

Source: SuiteSparse:GraphBLAS, `DrTimothyAldenDavis/GraphBLAS`, read at the
pinned commit `7701390417` (version 10.5.2, `Include/GraphBLAS.h:290–292`)
through `python3 tools/pinned-source.py`. The announcement is
`Doc/ChangeLog:147–153` (10.0.0, "March 1, 2025"): "New object, the
GxB_Container added for fast import/export of matrices/vectors with arbitrary
integer content", and `:163–164` declares the older pack/unpack methods
"historical" ("they still work but use 64-bit integers only").

## The problem in one sentence

`GrB_Matrix_build` takes unsorted tuples and `GrB_Matrix_extractTuples` gives
tuples back, so moving a matrix that *already is* a CSR through them costs a
sort-and-compress on the way in and a full expansion on the way out — O(nnz)
work, and 2–3× the memory, to move something that was never in the wrong
shape.

## The concepts, step by step

### Step 1 — what a `GrB_Matrix` is made of

> **In:** this topic's README §1 (the format lattice). **Out:** the five
> arrays the Container will name.

A **sparse** (CSR/CSC-like) `GrB_Matrix` is a handful of arrays. In the
by-row orientation: `p`, the **row pointer** array of length `nrows + 1`
(entry `p[i]..p[i+1]` delimits row `i`); `i`, the **column index** array of
length `nvals` (the library calls it `i` whichever the orientation — it is the
index *within* each vector); and `x`, the **values**, length `nvals` (length 1
if the matrix is **iso**, all entries equal). A **hypersparse** matrix adds
`h`, the list of the non-empty vectors' ids, with `p` shortened to
`nvec + 1`, plus an optional hash `Y` over `h` (README §1). A **bitmap**
matrix has `b`, one byte per cell; a **full** one has only `x`.

Everything in the Container follows from that list: the Container is a
struct with a slot for each array.

### Step 2 — the copying interface, and why it is not free

> **In:** Step 1. **Out:** what `build` and `extractTuples` do per entry, so
> Step 7's table has a prediction to be compared with.

`GrB_Matrix_build_FP64(A, rows, cols, vals, n, dup)` takes three parallel
arrays of `n` tuples in *any order* and an operator `dup` that combines
duplicates. The library's own account of the work is the comment at the top
of `GB_builder.c`, which describes five steps. For unsorted input, **Step 2
sorts**: "O((e log e)/p) work to sort the tuples" (`GB_builder.c:63–66`, where
*e* is the entry count and *p* the thread count), then Step 3 merges
duplicates with `dup`, Step 4 computes `p` (and `h`), and Step 5 copies the
values. For input that is "already sorted with no duplicates, and no
typecasting needs to be done" (`:44–51`), Steps 2 and 3 are skipped, each
thread does O(e/p) reads of the index arrays, and the sorted index array is
"transplanted" into the matrix. So even the best case reads and copies every
tuple; the worst case adds an `e log e` sort.
`GrB_Matrix_extractTuples_FP64` does the reverse: it writes two `n`-entry
index arrays and a value array, expanding `p` into one row number per entry.
Both calls copy, so both allocate and touch memory proportional to `nnz`, and
the standard entry points take the indices as 64-bit integers (10.0.0 added
32-bit variants, `Doc/ChangeLog:150–152`). The cost model to carry into
Step 7: build ≈ copy for sorted input and copy + sort for shuffled input;
extract ≈ copy + expand; and neither can be faster than `memcpy` of the same
bytes.

### Step 3 — the Container struct

> **In:** Step 1's five arrays. **Out:** the Container's fields and which are
> in play for a CSR.

`GxB_Container` (`Include/GraphBLAS.h:6896–6935`) is a struct made of
scalars plus *GraphBLAS vectors that hold the arrays*:

```c
// Include/GraphBLAS.h at 7701390417 — lines 6896–6928, with gaps marked
6896  struct GxB_Container_struct
6897  {
6898      // 16 words of uint64_t / int64_t:
6899      uint64_t nrows ;
6900      uint64_t ncols ;
6901      int64_t nrows_nonempty ;
6902      int64_t ncols_nonempty ;
6903      uint64_t nvals ;
        // ... 6904–6906: future-expansion words ...
6907      int32_t format ;                // GxB_HYPERSPARSE, GxB_SPARSE, GxB_BITMAP,
6908                                      // or GxB_FULL
6909      int32_t orientation ;           // GrB_ROWMAJOR or GrB_COLMAJOR
        // ... 6910–6914: header_arena and future-expansion words ...
6915      GrB_Vector p ;
6916      GrB_Vector h ;
6917      GrB_Vector b ;
6918      GrB_Vector i ;
6919      GrB_Vector x ;
        // ... 6920–6926 ...
6927      bool iso ;
6928      bool jumbled ;
```

(Line numbers are `Include/GraphBLAS.h` at `7701390417`.) The array slots
`p`, `h`, `b`, `i`, `x` are ordinary dense `GrB_Vector`s, one per array, and
`Y` (line 6923) is a `GrB_Matrix`. The element type of each vector is the
element type of the array it carries: for a CSR, `p` and `i` are `GrB_UINT32`
or `GrB_UINT64`, `x` is the value type. `jumbled` (line 6928) says the
indices inside each vector are not yet sorted; **jumbled** matrices are legal
in GraphBLAS and sorted lazily.

The point of using vectors is the next step: the library already knows how to
move a dense vector's array in and out in O(1).

### Step 4 — `GxB_Vector_load`: ownership is one flag

> **In:** Step 3. **Out:** the two ownership modes and what O(1) means.

`GxB_Vector_load(V, &X, type, n, X_memsize, handling, desc)` makes the dense
vector `V` *be* the array `X` — it points `V->x` at it and sets the length.
`GB_vector_load` is the whole body (`Source/container/GB_vector_load.c`):

```c
// Source/container/GB_vector_load.c at 7701390417 — lines 34–62
34      GB_phybix_free ((GrB_Matrix) V) ;
35  
36      V->type = type ;
37      V->plen = -1 ;
38      V->vlen = n ;
        // ... 39–54: nvals, format flags, 32/64-bit flags ...
55      V->x = (*X) ;
56      V->x_shallow = (V->x == NULL) ? false : readonly ;
57      V->x_mem = X_mem ;      // memsize and data_arena
58      if (!readonly)
59      { 
60          // tell the caller that X has been moved into V
61          (*X) = NULL ;
62      }
```

Line 34 frees whatever `V` held; line 55 is the whole transfer — one pointer
assignment, no allocation, no loop; it is O(1) by construction, and the file
header at `GxB_Vector_load.c:10` says so: "guaranteed to take O(1) time and
space". The `handling` argument picks between two **ownership** modes:

- `GrB_DEFAULT` (0): GraphBLAS takes ownership. Line 61 nulls the caller's
  pointer; GraphBLAS will `free` the array later with its own allocator, so
  the array must have been allocated by GraphBLAS's `malloc` (a `Vec` is not).
- `GxB_IS_READONLY`: the array stays the caller's. Line 56 sets
  `x_shallow`, a **shallow** component is never written to and never freed by
  GraphBLAS (`GxB_Vector_load.c:14–18`: it "is not freed if V is freed ...
  (*X) need not be a malloc'd array at all"). The caller must keep the
  memory alive and unchanged for as long as the vector or any matrix loaded
  from it exists.

Readonly is the mode a Rust-owned `Vec` needs.

### Step 5 — `GxB_load_Matrix_from_Container`: O(1), and what it does not check

> **In:** Step 4's vectors, filled. **Out:** the contract between caller and
> library, and what a bad Container does.

The matrix load (`GB_load_from_container.c`) first frees the matrix's old
content (line 48), copies the scalar fields (lines 54–67), and then moves each
array out of its vector with `GB_vector_unload` — the same pointer move as
Step 4, in reverse. For a CSR (`case GxB_SPARSE`, lines 163–195) that is
`Container->p` into `A->p` (line 170) and `Container->i` into `A->i`
(line 180), then `Container->x` into `A->x` (line 249); each vector's
`readonly` flag becomes the matrix component's `*_shallow` flag, so a Rust
array stays shallow all the way through. The integer widths are read from
the arrays' types (lines 253–255), and `GB_type_ok` (lines 17–21) accepts
`INT32, UINT32, INT64, UINT64` for each of `p`, `h` and `i` *independently*: a
CSR with 64-bit row pointers and 32-bit column indices — exactly the
`Csr` struct of this topic's crate — loads without converting either array.

What the load does *not* do is stated in its own comment (lines 261–264):
"Loading a GrB_Matrix from a Container takes O(1) time, so there is not enough
time to test the entire content of the matrix to see if it's valid. The user
application can do that with GxB_Matrix_fprint with a print level of zero".
It checks only sizes — `Ax_len >= nvals` (line 275), `Ai_len >= nvals`
(line 287), and `A->p[plen] == nvals` (line 291) — so a CSR whose column
indices are out of range, or unsorted while `jumbled = false`, loads cleanly
and is wrong. That is the price of O(1), and it is the Rust side's
responsibility: a `Csr` built by counting sort (as this crate's is) satisfies
it by construction.

### Step 6 — `GxB_unload_Matrix_into_Container`: the way back

> **In:** Step 5's loaded matrix. **Out:** the cost of the reverse and its one
> exception.

`GB_unload_into_container.c` does the mirror image, and its header says it
all: "This method takes O(1) time and performs no mallocs at all, unless A has
pending work that must be finished" (lines 10–12). Line 39
(`GB_MATRIX_WAIT_IF_PENDING_OR_ZOMBIES (A)`) is that exception: a matrix with
pending tuples (set with `GrB_Matrix_setElement` and not yet assembled) or
**zombies** (entries deleted but not yet compacted out — the lazy updates that
FalkorDB's delta matrices, README §5, sit in front of) is assembled first, which
costs real work. Then lines 52–60 copy the scalars out, the `GxB_SPARSE` case
(lines 87–96) moves `A->p` and `A->i` into the Container's vectors and line
111 moves `A->x`, and lines 121–129 leave `A` an empty 0-by-0 matrix. After
that, `GxB_Vector_unload` per array hands the raw pointers to the caller
(`handling` returns `GxB_IS_READONLY` if the array is shallow, i.e. was lent
by the caller originally, 0 if the library allocated it).

Two properties follow that the measurements below confirm. The pointer that
comes out of an unload of a matrix that was loaded readonly *is the pointer
that went in*. And a GraphBLAS-built matrix can be read in place by its host:
unload, treat the three pointers as slices, load them back.

### Step 7 — what it costs, measured

> **In:** Steps 2–6 and the lane `cargo run --release --bin gb_container`
> (seeded RMAT, edge factor 8, seed 42 — the same matrices as the SpMV lane of
> `gb_bench`). **Out:** the table that backs "zero-copy".

**Lane A — moving the matrix.** Best of 5 (3 at scale 20; 20 and 12 iterations
for the O(1) rows, whose minimum is the figure), µs, Apple M3 Pro, libgraphblas
10.5.0 built by FalkorDB's native-deps cache (the pinned reading is 10.5.2; the
Container source did not differ in the files quoted), load average 24–26 while
other work ran, so quote the shape, not the digits:

| scale | nnz | CSR MB | `build` sorted tuples | `build` shuffled | `extractTuples` | Container load (Rust CSR) | Container unload | unload (GB-built) | reload (GB-built) | `memcpy` of the CSR |
|---|---|---|---|---|---|---|---|---|---|---|
| 14 | 120,128 | 1.6 | 349 | 9,384 | 118 | 0.17 | 0.12 | 0.17 | 0.12 | 21 |
| 16 | 494,634 | 6.5 | 540 | 8,164 | 251 | 0.12 | 0.12 | 0.12 | 0.12 | 97 |
| 18 | 2,018,190 | 26.3 | 1,557 | 29,497 | 1,320 | 0.17 | 0.12 | 0.12 | 0.12 | 550 |
| 20 | 8,175,704 | 106.5 | 6,822 | 146,676 | 4,039 | 0.12 | 0.12 | 0.12 | 0.12 | 2,355 |

How to read it:

- **The Container rows are flat.** From 120,128 to 8,175,704 entries (68×)
  load and unload stay at 0.12–0.17 µs. The timer's tick on this machine is
  about 0.04 µs, so those are three or four ticks: "a few pointer assignments
  and four library calls", not a precise figure. The pointers that come out of
  `unload` are asserted equal to the `Vec` pointers that went in, at every
  scale (the lane panics otherwise): nothing was copied.
- **Everything that copies grows with `nnz`.** `build` of tuples already in CSR
  order grows 19.5× (349 → 6,822 µs; sub-linear because 11 threads share the
  work at large sizes), `extractTuples` 34× (118 → 4,039), `memcpy` of the
  three arrays 112× (21 → 2,355 µs, the 21 µs being cache-resident). At
  scale 20 the Container load is at least 19,000× cheaper than even the
  `memcpy` of the same bytes (2,355 / 0.12).
- **Input order is the whole difference between the two `build` columns.** The
  shuffled tuples are the same 8,175,704 entries permuted by a seeded
  shuffle; `build` takes 146,676 µs against 6,822 µs sorted, 21.5×, which is
  the Step-2 prediction: sorted input skips the sort (`GB_builder.c:44–51`),
  shuffled input pays it (`:63–66`). Ingesting an edge list is the shuffled
  column.
- **`memcpy` is the floor of every copy.** `build` sorted (6,822) is 2.9× the
  `memcpy` of the CSR (2,355), `extractTuples` (4,039) 1.7×, and shuffled
  `build` 62×. No copying interface can approach the Container, because the
  Container is the only column that does not read the matrix.
- **The reverse direction costs the same.** A matrix GraphBLAS built itself
  (`build`) unloads and reloads in 0.12–0.17 µs, and the arrays it returns can
  be read in place: the lane asserts that GraphBLAS's own CSR — `Ap` and `Ai`
  both 32-bit, `iso = false`, format sparse by-row at every scale — equals
  the seeded `Csr` entry for entry, reading it through raw pointers.

Sanity checks the lane carries: `build` keeps all `nnz` entries (the seeded
graph has no duplicates after dedup), `extractTuples` returns tuples equal to
the seeded ones, and the `memcpy` column is the one figure to hold against the
hardware: 106.5 MB in 2,355 µs is 45 GB/s of copied bytes (twice that as
read-plus-write traffic), above the ~30 GB/s single-thread streaming figure
topic 0 records. A single memcpy can reach that on this chip, but it is the
number to re-measure on a quiet machine before leaning on it.

**Lane B — one product, two kernels** is in the topic's [notes.md](notes.md)
(Rust SpMV against `GrB_mxv` on the same matrix): with the JIT off the
library's generic kernel is 3.1–4.3× slower than the Rust loop on one
thread; with the JIT on it is within about 0.8–1.3× of it in time, and with all 11
threads 1.8–4.4× faster. Neither number is about the Container; the Container only
makes it possible to run the library on the Rust matrix without a copy.

### Step 8 — what it means for a Rust rewrite

> **In:** Steps 4–7. **Out:** the rules for handing a Rust CSR to GraphBLAS,
> and what the measurement does not cover.

1. **Own the arrays, lend them readonly.** Keep the `Vec`s; load with
   `GxB_IS_READONLY` (the lane's `Mat::load_csr_readonly`). GraphBLAS never
   frees or writes them. Rust must keep the `Vec`s alive and unmodified until
   the matrix is unloaded or freed, and the lifetime is not visible to the
   borrow checker — the safe wrapper has to hold the `Vec`s (or an `Arc` of
   them) inside the wrapper object.
2. **Mixed widths are free.** `u64` row pointers with `u32` column indices
   load as they are (Step 5); a Rust CSR with `u32` pointers loads too. No
   widening pass.
3. **Sorted, in range, `jumbled = false`.** The library will not check
   (Step 5). A debug build can call `GxB_Matrix_fprint` at print level 0 after
   each load, the check the source comment recommends.
4. **Hypersparse needs `p` of length `nvec + 1` and `h`.** FalkorDB's relation
   matrices are hypersparse (README §1); the hypersparse branch
   (`GB_load_from_container.c:104–161`) takes `p`, `h`, an optional prebuilt
   `Y` hash, and `i`. This lane measures plain sparse; a hypersparse
   hand-off has the same three pointer moves plus one, but it is not measured
   here.
5. **Results come back GraphBLAS-owned.** The output of `GrB_mxm` is a
   library-allocated matrix; unloading it gives `handling = 0` arrays that
   Rust must not `Vec::from_raw_parts` (different allocator). Read them as
   slices and `GxB_Vector_load` them back, or copy. Whether the library's
   allocator can be pointed at Rust's `GlobalAlloc` (`GxB_init` takes
   `malloc`/`free` function pointers, `Include/GraphBLAS.h:2837`) was not tested.
6. **Pending work defeats the O(1) unload** (Step 6), so a matrix that sits
   behind a delta layer should be `GrB_wait`ed before it is unloaded, outside
   the latency-critical path.

What this lane does not measure: any operation *on* a shallow matrix other
than `GrB_mxv` (does a `GrB_assign` into it copy the array first? the source
says components are "treated as readonly", but the cost of the conversion was
not timed); multi-threaded loads; and the hypersparse format.

## How to read the source (with the concepts in hand)

1. `Doc/ChangeLog:147–164` — what was added in 10.0.0 and what was demoted.
2. `Include/GraphBLAS.h:6890–7005` — the struct and the six entry points.
3. `Source/container/GB_vector_load.c` (67 lines) and `GB_vector_unload.c` —
   Step 4; read the readonly flag through both.
4. `Source/container/GB_load_from_container.c` — the `switch (format)` at
   line 101 is the whole format lattice of README §1; read the `SPARSE` case,
   then `HYPERSPARSE`.
5. `Source/container/GB_unload_into_container.c` — Step 6; note what is
   *absent* (no loop, no `memcpy`).
6. `Doc/UserGuide/GrB_objects_Container.tex` — the documented contract.
7. `Test/GB_mex_container.c` — the library's own use.

## Where each step lives in this topic

| step | where | what to look at |
|---|---|---|
| 3–6 | `experiments/shim/gb_shim.c` `gbs_load_csr`, `gbs_unload_csr` | the three vector loads plus one matrix load, and the reverse |
| 3–6 | `experiments/src/gbffi.rs` `Mat::load_csr_readonly` | the Rust side: `rowptr` as `u64`, `colidx` as `u32`, `vals` as `f64`, all readonly |
| 7 | `experiments/src/bin/gb_container.rs` | the two lanes; skipped, exit 0, when no GraphBLAS install is found |
| 8 | `experiments/build.rs` | how an install is located (`GRAPHBLAS_DIR`, then standard prefixes, then FalkorDB's `native-deps` cache) |

## Questions (answer in notes.md)

1. Step 4 says a readonly vector is "shallow". Read `GB_phybix_free` and say
   what it does with a shallow `x` when the matrix is freed. Which allocator
   must free an array loaded with `GrB_DEFAULT`?
2. A matrix is loaded readonly from Rust, then `GrB_assign` inserts one entry
   and the matrix is unloaded. Does the `x` pointer that comes out equal the
   one that went in? Design the experiment (hint: `handling` on unload).
3. Why does the Container use *vectors* for its arrays instead of raw
   pointers? Which sentence of `GB_vector_unload.c` (lines 73–76) depends on
   it?
4. `GrB_Matrix_build` of already-sorted tuples is much faster than of shuffled
   ones in Step 7's table. Name the step of Step 2's cost model that the sorted
   input skips.

## Done when

Answer each before unfolding it.

- [ ] You can say why `GxB_load_Matrix_from_Container` is O(1) and what it gives up for that.

  <details><summary>Answer</summary>

  It moves pointers: each array sits in a dense `GrB_Vector` whose `x`
  field is the array, and the matrix load unloads each vector into the
  matrix's `p`, `i`, `x` fields with no loop over entries
  (`GB_load_from_container.c:170`, `:180`, `:249`). It gives up validation:
  the source says "there is not enough time to test the entire content"
  (lines 261–264) and checks only lengths (lines 275, 287, 291), so unsorted
  or out-of-range indices are the caller's bug.

  </details>

- [ ] You can state the two ownership modes of `GxB_Vector_load` and which one a Rust `Vec` needs.

  <details><summary>Answer</summary>

  `GrB_DEFAULT` transfers ownership: the caller's pointer is nulled
  (`GB_vector_load.c:58–61`) and GraphBLAS frees the array with its own
  allocator later. `GxB_IS_READONLY` keeps ownership with the caller and
  marks the component shallow (line 56): never written, never freed. A
  `Vec` was allocated by Rust's allocator, so it needs readonly, and it must
  outlive every matrix built from it.

  </details>

- [ ] You can read the measured table: which operations scale with `nnz` and which do not.

  <details><summary>Answer</summary>

  Container load and unload took 0.12–0.17 µs at every size from 120,128 to
  8,175,704 entries: they do not touch the entries, so they do not scale.
  Everything that copies scales with `nnz`: `build` of sorted tuples
  349 → 6,822 µs, `extractTuples` 118 → 4,039 µs, `memcpy` of the CSR 21 →
  2,355 µs. Input order matters only to `build`: the same entries shuffled
  took 146,676 µs against 6,822 µs sorted (21.5×) because the sort is no
  longer skipped. The Container is therefore the right door for a matrix that
  already is a CSR, and `build` remains the right door for an edge list that
  is not.

  </details>

- [ ] You can list what a Rust CSR must satisfy before it is loaded.

  <details><summary>Answer</summary>

  Row pointers of length `nrows + 1`, non-decreasing, ending at `nvals`
  (line 291 checks only the last); column indices in `[0, ncols)` and
  strictly increasing within each row unless `jumbled` is set; value and
  index arrays at least `nvals` long (lines 275, 287); widths each 32 or 64
  bits (`GB_type_ok`, lines 17–21); the arrays alive and unmodified for the
  matrix's lifetime. None except the lengths and the last pointer is
  verified by the library.

  </details>

## References

- Timothy A. Davis, **SuiteSparse:GraphBLAS** v10.5.2 (pinned `7701390417`) —
  `Doc/ChangeLog` 10.0.0 entry; `Include/GraphBLAS.h` Container section;
  `Source/container/`; `Doc/UserGuide/GrB_objects_Container.tex`.
- This topic's [reading-suitesparse-internals.md](reading-suitesparse-internals.md)
  for the format lattice and [reading-falkordb-delta-matrix.md](reading-falkordb-delta-matrix.md)
  for the pending-work layer that Step 6's exception interacts with.
- Previous: [reading-openmp-vs-rayon.md](reading-openmp-vs-rayon.md).
