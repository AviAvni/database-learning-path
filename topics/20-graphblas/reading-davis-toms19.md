# SuiteSparse:GraphBLAS: a sparse-matrix executor in disguise

Davis's TOMS '19 system paper (plus the '23 v2 update) describes the
library under FalkorDB. Read it as an executor-design paper, not a
math paper: it's about lazy evaluation, format polymorphism, and
kernel dispatch — the same problems as topics 8-11, in matrix
clothing. Before you open it, this chapter builds the six concepts
the paper assumes, one at a time — then hands you a reading route
and the numbers worth retaining.

**Read the version numbers before you read anything else**, because
this chapter's single biggest hazard is quoting the 2019 paper about
a 2025 library:

| source | describes | status here |
|---|---|---|
| TOMS '19, "Algorithm 1000" | version **2.3.3**, **single-threaded**, **two** sparsity structures | quoted from the author's accepted manuscript, titled "Algorithm 9xx" |
| Davis, CSC '20, "Parallel GraphBLAS with OpenMP" | version **3.0.1**, the first parallel release | the citable source for every parallelism claim |
| TOMS '23, "Algorithm 1037" | version 7.x: JIT, iso, 32/64-bit indices | **could not be downloaded**; no claim below is attributed to it |
| the pinned source | **version 10.3.1** (`Include/GraphBLAS.h:290-292`), **four** sparsity structures × 2 orientations | where every v2-era claim below is verified instead |

## The problem in one sentence

One `GrB_mxm` call must behave well whether the operand is a
10M×10M matrix with 100K entries or with 1B entries — a density
range of four orders of magnitude — and must absorb millions of
single-entry mutations without restructuring a packed array each
time; this paper is the design that does both behind one opaque
handle.

## The concepts, step by step

### Step 1 — a graph is a sparse matrix; store only what exists

> **In:** a graph, and the definition of a matrix.
> **Out:** the single cost driver for this whole topic, and the
> order of magnitude it saves.

A **sparse matrix** is one where almost every cell is zero/absent,
so you store only the present entries — each one an
`(row, col, value)` fact. A graph maps onto this directly: the
**adjacency matrix** A has A(i,j) present iff there is an edge
i→j, so "the graph" and "the matrix" are the same object. The count
of present entries is **nnz** ("number of nonzeros") — the number
that drives every cost in this topic.

Davis is careful about what an absent entry *means*, and it is not
"zero" (TOMS '19 §4.1):

> "MATLAB drops its entries with a numerical value of zero, but
> this is never done in GraphBLAS. A 'zero' is simply an entry that
> is not stored in the data structure, and **the value of this
> implicit entry depends on the semiring**. If the matrix is used
> in the conventional plus-times semiring, the implicit value is
> zero. If used in max-plus, the implicit entry is −∞."

Concrete cost, on this topic's own configuration:

```
 inputs: 10M-node id space (notes.md:41)

 dense boolean:  10e6 × 10e6 = 1.0e14 cells
                 at 1 bit each          = 12.5 TB
 sparse, 100M edges, 32-bit indices
                 100e6 × 4 B            = 400 MB of column indices

 ratio                                  = 31,000×

 and the SPARSE representation still is not free: see Step 2, where
 the row-pointer array alone costs 80 MB before a single edge.
```

Why it matters: everything in this paper is machinery for
exploiting the zeros you did not store, and the interesting failures
are the places where a *structure* proportional to the dimension
sneaks the cost back in.

### Step 2 — the format ladder, and the two counts of it

> **In:** nnz and the matrix dimensions.
> **Out:** the four sparsity structures, why the source says
> "eight formats", and the byte cost of each rung.

The standard sparse format is **CSR** (compressed sparse row): a
`rowptr` array with one offset per row marking where that row's
column indices start, plus a `colidx` array with one entry per
edge. CSR is great at "give me row i" (one pointer lookup, then a
contiguous slice) — the core operation of graph traversal. But no
single format wins at every density, so a `GrB_Matrix` moves along
a ladder as its density changes:

```
 density →
 hypersparse ──► sparse (CSR/CSC) ──► bitmap ──► full
 (store only     (rowptr[n+1] +       (one byte  (no structure,
  the non-empty   colidx per edge)     per cell   just values)
  rows: h[] +                          + values)
  their ptrs)

 nvals ≪ nrows   nvals ~ O(nrows)     nvals >    every cell
 (10M×10M with   the graph default    40% of     present
  100K edges)                         n×m
```

Two corrections before you carry that picture into the paper.

**First: the paper does not describe this ladder.** TOMS '19 §4.1
says, in full:

> "In SuiteSparse:GraphBLAS (**version 2.3.3**), a GraphBLAS matrix
> (the `GrB_Matrix` object) is stored in one of **four different
> formats: compressed-sparse column (standard CSC),
> compressed-sparse row (standard CSR), and hypersparse versions of
> these two formats** (hyper CSR and hyper CSC)."

So the paper's "four formats" are *two* sparsity structures times
two orientations. Bitmap and full do not exist in it. The pinned
source counts differently again:

```c
// GB_Matrix_content.h — how the source counts formats, 52 and 76
    52  // The matrix can be held in one of 8 formats, each one consisting of a set of
    53  // vectors.  The vector "names" are in the range 0 to A->vdim-1.
    ...
    76  // The 8 formats:  (hypersparse, sparse, bitmap, full) x (CSR or CSC)
```

Four **sparsity structures** × two **orientations** = eight
formats. Say which you mean. And note the consequence TOMS '19 §4.2
draws from its own count — "the four matrices for C⟨M⟩ = AB give
rise to **256 variants** of the sparse matrix-matrix multiply" —
which at the pin's eight formats would be 8⁴ = 4,096. That
combinatorial explosion is the reason the JIT of Step 6 exists.

**Second: the density thresholds.** The bitmap rung is not "~4-8%".
The struct's own comment states the rule:

```c
// GB_Matrix_content.h — the bitmap rule, in the struct's comment, 450-457
   450  //          A->vdim can have at most anz_dense = (A->vlen)*(A->vdim) entries.
   451  //          If A is sparse/hypersparse with anz > A->bitmap_switch * anz_dense,
   452  //          then it switches to bitmap.  If A is bitmap and anz =
   453  //          (A->bitmap_switch / 2) * anz_dense, it switches to sparse.  In
   454  //          between those two regions, the sparsity structure is unchanged.
   ...
   456  float hyper_switch ;    // controls conversion hyper to/from sparse
   457  float bitmap_switch ;   // controls conversion sparse to/from bitmap
```

and `bitmap_switch` defaults from a table indexed by the matrix's
*minimum dimension*, not by the operation: `GB_Global.c:181-189`
runs 0.04 → 0.40, and `GB_Global.c:486-497` selects **0.40** for
any dimension above 64. A graph adjacency matrix never becomes
bitmap; the arithmetic is in
[reading-suitesparse-internals.md](reading-suitesparse-internals.md)
Step 2. `hyper_switch` defaults to 0.0625 = 1/16
(`GB_defaults.h:20`), which matches the paper's §4.2.1: "SuiteSparse:
GraphBLAS stores its matrices in hypersparse format if n̄ < n/16."

Now price the ladder against this topic's measurement, because the
numbers reproduce exactly:

```
 inputs: 10M-node id space, 100K edges  (notes.md:41-42)
         this repo's CSR: rowptr u64, colidx u32
         this repo's hyper: h u32, p u64, colidx u32

 CSR index bytes
   rowptr  (10,000,000 + 1) × 8   = 80,000,008
   colidx       100,000    × 4    =    400,000
   total                          = 80,400,008 B = 80.4 MB   ← measured: 80.4 MB

 hypersparse index bytes, with k distinct non-empty rows
   h            k × 4
   p        (k+1) × 8
   colidx  100,000 × 4            =    400,000
   at k = 100,000:  400,000 + 800,008 + 400,000 = 1.60 MB
                                                  ← measured: 1.59 MB
   the 0.01 MB gap is k slightly below 100,000: a few hundred of
   the 100,000 edges share a source row.

 ratio 80.4 / 1.59 = 50.6×        ← FINDINGS.md row 20 says "50x"

 and the term that vanished is exactly the O(n) rowptr: 80.0 of the
 80.4 MB, i.e. 99.5% of the CSR index, is pointers for rows that
 hold nothing.
```

**Hypersparse matters most to FalkorDB**: node IDs are a shared
namespace across all relation types, so most rows of any one
relation matrix are empty, and that 80 MB is *per relation type*
before storing a single edge. The switches between rungs are decided
by the two per-matrix knobs applied after every operation — the
internals chapter reads that code.

Why it matters: the topic's 50× headline is not a benchmark
artifact, it is `(n+1)×8` disappearing, and you can predict it to
within 1% with a pocket calculator.

### Step 3 — semiring, mask, accum: the GraphBLAS ops are executor concepts

> **In:** the matrix object of Steps 1-2.
> **Out:** the four operation parameters, each mapped onto a
> database-executor concept.

GraphBLAS operations are parameterized matrix products, and each
parameter maps onto a database-executor concept:

- A **semiring** (a pair of operations standing in for multiply
  and add, letting one matrix-multiply routine compute many
  different algorithms) is the inner loop's two ops: (+,×) gives
  numeric matmul, (min,+) gives shortest-path relaxation,
  (ANY,PAIR) gives boolean reachability with early exit.
- A **mask** (`C<M> = A*B`: only compute/keep outputs where M has
  entries) is a semi-join filter — and, in the right engine, it
  *drives* the iteration rather than filtering after, changing the
  complexity class.
- An **accum** operator (`C += A*B` instead of `C = A*B`) is an
  UPDATE expression — merge new results into existing ones.
- A **descriptor** (flags: transpose an input, complement the
  mask, replace C) is the query-hint block.

The paper's §3.1.9 lists exactly these four as one bundle:

> "Most GraphBLAS operations can be modified via transposing input
> matrices, using an accumulator operator, applying a mask or its
> complement, and by clearing all entries the matrix C after using
> it in the accumulator operator but before the final results are
> written back into it. All of these steps are optional, and are
> controlled by a **descriptor** object."

You can read one off the header. `GrB_DESC_RSC`
(`Include/GraphBLAS.h:666`) is `GrB_REPLACE + GrB_STRUCTURE +
GrB_COMP` — replace C, use only the mask's pattern and ignore its
values, and complement it. That single constant is what LAGraph's
BFS passes to mean "write only where I have not been yet", and
Step 8 of the internals chapter shows how it decides which engine
runs.

Why it matters: §3 reads like an API reference. Read it as an
*operator algebra* and question 1 answers itself.

### Step 4 — lazy mutation: zombies and pending tuples

> **In:** the packed CSR arrays of Step 2, which are expensive to
> splice.
> **Out:** the two deferral mechanisms, their exact encodings, and
> the complexity they buy.

CSR's packed arrays make single-entry mutation expensive: deleting
one edge means splicing `colidx` (O(nnz) memmove), inserting one
means the same. SuiteSparse's answer is to *not do it yet*. The
source defines both precisely:

```c
// GB_Matrix_content.h — the zombie, 367-373 and 391
   367  // A "zombie" is the opposite of a pending tuple.  It is an entry A(i,j) that
   368  // has been marked for deletion, but has not been deleted yet because it is
   369  // more efficient to delete all zombies all at once, rather than one (or a few)
   370  // at a time.  An entry A(i,j) is marked as a zombie by 'zombifying' its index
   371  // via GB_ZOMBIE (i).  A zombie index is negative, and the actual index can be
   372  // obtained by GB_UNZOMBIE (i).  GB_ZOMBIE (i) is a function that is its own
   373  // inverse: GB_ZOMBIE (GB_ZOMBIE (x))=x for all x.
   ...
   391  uint64_t nzombies ;     // number of zombies marked for deletion
```

TOMS '19 §4.1 gives the arithmetic the modern macro hides: "its row
index i is changed to **(-i-2)**, to accommodate zero-based row
indices". And the property that makes it worth the trouble:
"Zombies allow for fast deletion, and they also permit binary
searches of a sparse vector to performed, even if it contains
zombies" — the index is negated but the *ordering* is preserved
under the transform, so the sorted-array invariant survives.

The source adds a second reason the paper does not stress
(`:375-383`): "a zombie may be restored as a regular entry by a
subsequent update… Had the zombie not been there, the update would
have to be placed in the pending tuple list." Delete-then-reinsert
is O(1) in place, and never lengthens the pending list.

The pending tuple is the mirror image — TOMS '19 §4.1: "an entry
that has not yet been added to the compressed-sparse vector part of
the data structure. Pending tuples are held in an **unsorted list**
of row indices, column indices, and values. Duplicates may appear
in this list. The matrix also keeps track of a single operator to be
used to combine duplicate entries. A matrix can have both zombies
and pending tuples."

The whole mechanism, distilled:

```rust
// ILLUSTRATION — not SuiteSparse source. The structural claims are
// GB_Matrix_content.h:361 (the Pending list), :367-373 and :391 (zombies,
// GB_ZOMBIE, nzombies) and TOMS '19 §3.1.8 (the O(e log e) bound).
fn set_element(a: &mut Matrix, i: u64, j: u64, v: f64) {
    a.pending.push((i, j, v));       // O(1): append, don't restructure CSR
}

fn delete_element(a: &mut Matrix, i: u64, j: u64) {
    if let Some(e) = a.find_mut(i, j) {
        e.mark_zombie();             // negate the index in place — no splice
    }                                //   ordering survives, binary search still works
}

fn wait(a: &mut Matrix) {            // the GrB_wait boundary
    a.prune_zombies();               // one sweep drops ALL zombies
    a.pending.sort_unstable();       // e inserts → one sort + one merge,
    a.merge_pending_into_csr();      //   not e binary-searched splices
    conform(a);                      // then maybe switch format
}
```

The cost shape is the paper's headline claim, §3.1.8: e single
`GrB_Matrix_setElement` calls take **O(e log e)** in
SuiteSparse, where "the equivalent method in MATLAB takes **O(e²)**
time". Price it:

```
 inputs: e = 100,000 edges inserted one at a time (notes.md:41)

 eager (MATLAB shape):  e²        = 1.0e10 index-slot moves
 lazy  (SuiteSparse):   e log₂ e  = 1.0e5 × 16.6 = 1.66e6

 ratio                            = 6,000×

 at 1 ns per moved slot that is 10 seconds versus 1.7 milliseconds.

 and the paper's second claim: GrB_Matrix_build (all at once) is
 ALSO O(e log e) — "both methods below take O(e log e) time". So
 lazy incremental insertion costs the same asymptotically as
 batch construction. That equality is the point of the section.
```

This is the LSM memtable move (topic 3) inside a matrix library —
and it is the library's OWN delta mechanism, which makes FalkorDB's
delta matrices (this topic §5) look redundant until you ask who
controls the flush. Question 2.

Why it matters: `O(e log e)` for incremental inserts *equalling*
batch build is the sentence that licenses a graph database to be
built on a matrix library at all.

### Step 5 — non-blocking mode: the object model assembled

> **In:** Steps 2 and 4 — formats, zombies, pending tuples.
> **Out:** the full opaque handle, and who decides when the
> deferred work runs.

The GraphBLAS spec allows every operation to return before doing
work; the deferred state is reconciled at **`GrB_wait`**
boundaries — or forced implicitly by any operation that needs to
*read* the matrix. The source says so for zombies specifically
(`GB_Matrix_content.h:386-389`): "methods and operations in
GraphBLAS that cannot tolerate zombies in their input matrices can
check the condition (A->nzombies > 0), and then delete all of them
if they appear, via GB_wait."

Assembling steps 2-4 against the pinned struct, the opaque handle
looks like this — every line is a real field:

```
 GrB_Matrix = opaque header  (GB_Matrix_content.h)
   ├─ p, h, i, x, b            :223-228  the five arrays; h only if hypersparse,
   │                                     b only if bitmap, x only if not full-iso
   ├─ nvals, nvec, nvec_nonempty :213-229
   ├─ Y                        :241-274  hyper_hash: a hash over h[] replacing
   │                                     the binary search, load factor 2-4
   ├─ Pending                  :361      the unsorted insert list
   ├─ nzombies                 :391      deleted-in-place count
   ├─ hyper_switch/bitmap_switch :456-457  the two per-matrix knobs
   ├─ sparsity_control         :462      which of the four structures are allowed
   ├─ is_csc / jumbled         :497-498  orientation; "may be unsorted"
   ├─ iso                      :524      all values equal ⇒ store ONE  ← step 6
   └─ p_is_32/j_is_32/i_is_32  :534-536  per-array index width         ← step 6
```

Two of those are worth a second look. `jumbled` (`:498`) is a
*deferred sort*: the matrix admits it may be out of order, which is
a third deferral mechanism alongside zombies and pending tuples.
And `Y` (`:241-274`) is a hash table over the hypersparse row list,
because otherwise finding row j in `h[]` is a binary search — the
`lg h` term that appears in `GB_AxB_saxpy3_flopcount.c:44-48`'s
complexity. Its documented load factor: "the load factor is
normally in the range of 2 to 4, so ideally each bucket will
contain about 4 entries on average". FalkorDB turns it *off* for
its delta matrices (`delta_new.c:33`, `GxB_HYPER_HASH` false) —
worth asking why.

SuiteSparse uses non-blocking mode for *mutation batching*
(pending tuples get sorted and merged once), not full lazy fusion.
Compare topic 27's incremental view maintenance: same "amortize
small updates" shape. The cost to remember: **the flush point is
chosen by the library** — any read can trigger it — not by the
application. That single fact is what motivates FalkorDB's own
delta layer, which exists precisely to move the decision.

Why it matters: every deferral in this design is invisible until
something forces it, and the thing that forces it is not under the
caller's control. Hold that thought through
[reading-falkordb-delta-matrix.md](reading-falkordb-delta-matrix.md).

### Step 6 — what changed after the paper: parallelism, JIT, iso, 32-bit

> **In:** the 2019 design.
> **Out:** four changes, each attributed to a source that can be
> checked, and the one the paper flatly contradicts.

**Parallelism.** TOMS '19 does not have it. Its own §4.2.1 says
"while SuiteSparse:GraphBLAS is not yet multi-threaded, it is
thread-safe", and §7 calls the work "an efficient and highly
optimized **single-threaded** implementation". Davis's CSC '20 §3
dates the change: "Version 2.3.3 is to appear as a Collected
Algorithm… it does not exploit any parallelism at all. **Version
3.0.1 has been released (July 31, 2019), with exploitation of
multi-threaded parallelism expressed through OpenMP.**" Any
speedup number you attribute to "the TOMS paper" is wrong by
construction; use CSC '20's Table 2, which
[reading-openmp-vs-rayon.md](reading-openmp-vs-rayon.md) reads.

**The CPU JIT** (topic 19's jitifyer). Verified at the pin: the
source tree has `Source/jitifyer/` with one encoder per operation —
`GB_encodify_mxm.c`, `GB_encodify_ewise.c`, `GB_encodify_reduce.c`,
`GB_encodify_select.c`, and nine more. What it buys is the
combinatorial problem of Step 2: TOMS '19 §4.2 counted 256 mxm
variants over four formats, and the pin has eight formats. Compiling
the one variant you need beats shipping 4,096 of them or falling
back to a function pointer per element.

**Iso-valued matrices.** Verified at the pin, and the struct
explains itself:

```c
// GB_Matrix_content.h — why iso exists, 513-524
   513  // Instead, the common practice is to assign all entries present in the matrix
   514  // to be equal to a single value, typically 1 or true.  SuiteSparse:GraphBLAS
   515  // exploits this typical practice by allowing for iso matrices, where all
   516  // entries present have the same value, held as A->x [0].  The sparsity
   517  // structure is kept, so in an iso matrix, A(i,j) is either equal to A->x [0],
   518  // or not present in the sparsity pattern of A.
   ...
   521  // If A is full, A->x is the only component present, and thus a full iso matrix
   522  // takes only O(1) memory, regardless of its dimension.
   524  bool iso ;          // true if all entries have the same value and only a
```

An unweighted graph — A(i,j) = true for every edge — is exactly
this. `:520-521`'s corollary is startling: a full iso matrix is
O(1) memory *regardless of dimension*, so `GrB_Matrix` can
represent an all-ones 10M×10M matrix in a handful of bytes.

**32/64-bit indices per array.** Verified at the pin, and finer
than "per matrix":

```c
// GB_Matrix_content.h — three independent width flags, 531-536
   531  // A->p, A->h, and A->i can be either 32-bit or 64-bit integers.
   ...
   534  bool p_is_32 ;  // true if A->p is 32-bit, false if 64
   535  bool j_is_32 ;  // true if A->h and A->Y->[pix] are 32-bit, false if 64
   536  bool i_is_32 ;  // true if A->i is 32-bit, false if 64
```

Three flags, three arrays, chosen independently — plus
`p_control`/`j_control`/`i_control` at `:464-466` so an application
can force any of them. Note which array each governs: `p_is_32`
covers the *offsets* (bounded by nnz), `i_is_32` the *indices*
(bounded by the dimension). A graph with 5B edges but 100M nodes
wants 64-bit `p` and 32-bit `i`, and this design lets it have
both. Question 5's arithmetic:

```
 inputs: 10M nodes, 100M edges, CSR

 all-64-bit:  p (10e6+1)×8 =   80.0 MB
              i    100e6×8 =  800.0 MB
              total index  =  880.0 MB

 all-32-bit:  p (10e6+1)×4 =   40.0 MB
              i    100e6×4 =  400.0 MB
              total index  =  440.0 MB      → exactly 2×

 mixed (p 64-bit, i 32-bit), which is what a 5B-edge graph needs:
              p            =   80.0 MB
              i            =  400.0 MB
              total        =  480.0 MB      → still 1.83×

 legality check: i needs to address 10e6 < 2³¹, and p needs to
 reach 100e6 < 2³¹ — so all-32-bit is legal here. It stops being
 legal for p at 2.1e9 edges.
```

Iso plus the (ANY,PAIR) semiring is why BFS over an unweighted
FalkorDB relation matrix moves no value data at all — pattern in,
pattern out. Question 4 traces that path.

Why it matters: three of these four are checkable in the source in
under a minute, and the fourth (parallelism) is the one the paper
gets *actively wrong* if you read it as current.

## How to read the paper (with the concepts in hand)

- **TOMS '19, §3 (basic concepts)** — read §3.1.8 (non-blocking
  mode) and §3.1.9 (accumulator and mask) closely; they are Steps
  3 and 4 in the author's words. §3.1.8 is two pages and contains
  the `O(e log e)` versus `O(e²)` comparison whole.
- **TOMS '19, §4.1 (data structure)** — read closely against Step
  2, and keep the version caveat in view the entire time: it
  describes *two* sparsity structures, and its "four formats" are
  CSR/CSC × standard/hyper. The code counterpart at the pin is
  `Source/builtin/include/GB_Matrix_content.h`, which is 657 lines
  of comment-heavy struct and is genuinely readable top to bottom.
- **TOMS '19, §4.2.1 (matrix multiply)** — the three methods,
  Gustavson's complexity, the hypersparse `n̄ < n/16` rule, and the
  masked variant's "discarded if they are computed". Walked in
  [reading-gustavson-spgemm.md](reading-gustavson-spgemm.md).
- **TOMS '19, §6 (performance)** — Table 6 is 3-truss throughput in
  10⁶ edges/s against hand-written C: roadNet-TX 10.8 (GraphBLAS)
  vs 15.1 (sequential C) vs 56.6 (parallel C); cit-Patents 0.9 vs
  1.4 vs 11.5; g-1073643522 10.1 vs 38.6 vs 199.9. The paper's own
  summary — "rarely taking more than twice the time as the
  highly-optimized, sequential versions in pure C" — is the honest
  claim; the parallel column is the gap version 3.0.1 was written
  to close.
- **TOMS '23 (Algorithm 1037)** — not available to this repo. Do
  not cite it from memory. Everything you would want from it —
  JIT, iso, 32/64-bit indices, the hyper_hash — is verifiable in
  the pinned source, as Steps 5 and 6 do.

Numbers to retain while you read:

- format switch defaults: **`bitmap_switch` is a table indexed by
  `min(vlen, vdim)`, not by operation** — 0.04 for a dimension of
  1, rising to **0.40 for anything above 64**
  (`GB_Global.c:181-189`, `:486-497`), with hysteresis at b/2
  (`GB_Matrix_content.h:450-454`). `hyper_switch` is 0.0625
  (`GB_defaults.h:20`), matching the paper's n/16.
- saxpy3's Gustavson-vs-hash threshold: **`hash_size >= cvlen/12`**
  (`GB_AxB_saxpy3_slice_balanced.c:94`), plus an undocumented
  `flmax >= cvlen/2` shortcut at `:65`. The widely repeated "m/16"
  is a stale comment at `GB_AxB_saxpy3.c:57-58`.
- mxm engines: dot3's work is ∝ nnz(M) — provably, since
  `GB_AxB_dot3.c:126` and `:171` size C to exactly nnz(M) —
  while saxpy3's is ∝ flops. The mask changes the complexity
  class, not a constant.
- this topic's measured SpMV bandwidth: **19.1 GB/s at scale 14
  falling to 15.8 GB/s at scale 20** (`notes.md:9-14`), against a
  ~30 GB/s streaming baseline from topic 0/13. (`FINDINGS.md` row
  20 states the same decay as 20.7 → 12.3 GB/s. The two were
  measured on different runs; cite one and say which.)

## Questions for notes.md

1. Map GrB objects to executor concepts: semiring ↔ ?, mask ↔ ?,
   accum ↔ ?, descriptor ↔ ? (operator, semi-join filter, UPDATE
   expression, query hints — defend each, and check your answer
   against the four-item list in §3.1.9).
2. Zombies + pending vs FalkorDB's DP/DM: why does FalkorDB need
   its OWN deltas when the library already has them? Candidates:
   control over *when* wait happens; keeping the transposed pair in
   lockstep; readers must see pre-wait state. Decide which
   dominates, using Step 5's "the library chooses the flush point".
3. The iso optimization (`GB_Matrix_content.h:513-524`): which
   FalkorDB matrices are iso — adjacency bool, yes; a relation
   matrix holding edge IDs as values, no. What does losing iso cost
   on mxm bandwidth? Compute it: an iso bool matrix moves 0 value
   bytes per entry, a `uint64` relation matrix moves 8. Put that
   against this topic's measured 15.8-19.1 GB/s
   (`notes.md:9-14`).
4. Trace one BFS step through the v2 machinery: iso bool matrix,
   ANY_PAIR semiring, sparse frontier — which engine runs, and what
   does the JIT specialize away? Use
   [reading-suitesparse-internals.md](reading-suitesparse-internals.md)
   Step 8's dispatch trace, and remember that the pull step's
   descriptor complements the mask.
5. 32-bit indices: for a 10M-node 100M-edge graph, compute the CSR
   index memory with all-64-bit versus all-32-bit arrays, then
   redo it with `p_is_32 = false, i_is_32 = true`. Where does the
   same factor show up in our Rust CSR if we switch `usize` → `u32`,
   and at what edge count does 32-bit `p` become illegal?

## Done when

Answer each before unfolding it.

- [ ] You can say which version each of your claims about this library comes from.

  <details><summary>Answer</summary>

  TOMS '19 describes **version 2.3.3** and is **single-threaded**
  (§4.2.1: "while SuiteSparse:GraphBLAS is not yet multi-threaded,
  it is thread-safe"; §7: "an efficient and highly optimized
  single-threaded implementation"). Parallelism arrives in 3.0.1,
  per Davis's CSC '20 §3. The pin this repo reads is **10.3.1**
  (`Include/GraphBLAS.h:290-292`). TOMS '23 could not be obtained,
  so nothing here is attributed to it.

  The practical rule: performance claims → CSC '20 or the source;
  data-structure claims → check the source, because §4.1 is two
  sparsity structures behind.

  </details>

- [ ] You can name the four sparsity structures, say how the source counts formats, and give both switch rules.

  <details><summary>Answer</summary>

  hypersparse, sparse, bitmap, full. The source counts **eight
  formats** — `GB_Matrix_content.h:76`: "(hypersparse, sparse,
  bitmap, full) x (CSR or CSC)". TOMS '19 §4.1 counts four, but its
  four are CSC/CSR × standard/hyper: bitmap and full postdate it.

  Bitmap: switch up when `nnz > b × m·n`, back down when
  `nnz <= (b/2) × m·n`, unchanged in between
  (`GB_Matrix_content.h:450-454`). `b` comes from a table indexed
  by `min(vlen, vdim)` — **0.40 for any dimension above 64**
  (`GB_Global.c:189`, `:486-497`) — so it is neither "4-8%" nor
  operation-dependent.

  Hyper: up when `k <= n × h`, down when `k > n × h × 2`, with
  `h` = 0.0625 (`GB_defaults.h:20`). The paper agrees: "hypersparse
  format if n̄ < n/16" (§4.2.1).

  </details>

- [ ] You can predict this topic's 50× hypersparse index saving from the array sizes.

  <details><summary>Answer</summary>

  CSR index for a 10M id space holding 100K edges: rowptr
  (10,000,001 × 8) + colidx (100,000 × 4) = 80,400,008 B =
  **80.4 MB**, which is `notes.md:41`'s measured figure exactly.

  Hypersparse with k ≈ 100,000 non-empty rows: h (k × 4) + p
  ((k+1) × 8) + colidx (100,000 × 4) = 1.60 MB against a measured
  **1.59 MB** — within 1%, the gap being a few hundred edges
  sharing a source row.

  80.4 / 1.59 = 50.6×. And the term that disappeared is the
  `(n+1)×8` rowptr: 80.0 of the 80.4 MB, 99.5% of the CSR index,
  is pointers for rows that hold nothing.

  </details>

- [ ] You can map semiring, mask and accumulator onto executor concepts.

  <details><summary>Answer</summary>

  Semiring → the inner loop's two operators, i.e. a pluggable
  aggregate + combine, so one kernel computes matmul, shortest-path
  relaxation, or reachability. Mask → a semi-join filter, which in
  a dot engine becomes the *driving* iteration and changes the
  complexity class. Accum → an UPDATE expression, merging into the
  existing C. Descriptor → the query-hint block.

  §3.1.9 bundles all four in one sentence, and one constant proves
  it: `GrB_DESC_RSC` = `GrB_REPLACE + GrB_STRUCTURE + GrB_COMP`
  (`Include/GraphBLAS.h:666`) — three hints in a single opaque
  handle, which is exactly what LAGraph's BFS passes to mean "write
  only where I have not been".

  </details>

- [ ] You can explain zombies and pending tuples with their encodings, and give the complexity lazy mutation buys.

  <details><summary>Answer</summary>

  Zombie = an entry marked for deletion in place by negating its
  index: TOMS '19 §4.1 gives the transform as **i → (−i−2)**, and
  the source describes `GB_ZOMBIE`/`GB_UNZOMBIE` as mutually
  inverse (`GB_Matrix_content.h:370-373`). The transform preserves
  ordering, so binary search still works on a vector containing
  zombies — and a re-insert can *de-zombify* in place rather than
  lengthening the pending list (`:375-383`).

  Pending tuple = an insert appended to an **unsorted** side list
  of (i, j, value) with duplicates allowed and a combining operator
  recorded (§4.1, and `GB_Matrix_content.h:361`).

  The payoff, §3.1.8: e incremental `setElement` calls cost
  **O(e log e)** in SuiteSparse against **O(e²)** in MATLAB. At
  e = 100,000 that is 1.66e6 versus 1.0e10 — 6,000×. And
  `GrB_Matrix_build` is *also* O(e log e), so incremental costs the
  same as batch. That equality is why a graph database can sit on
  top of a matrix library.

  </details>

- [ ] You can say what non-blocking mode defers, what forces completion, and who decides.

  <details><summary>Answer</summary>

  Deferred: pending inserts, zombie deletions, and — a third one
  the paper does not stress — sortedness, via `jumbled`
  (`GB_Matrix_content.h:498`). Completion is forced by `GrB_wait`
  or implicitly by any operation that cannot tolerate the deferred
  state; the source spells out the zombie case at `:386-389`
  ("check the condition (A->nzombies > 0), and then delete all of
  them if they appear, via GB_wait").

  Who decides is the load-bearing part: **the library**, because
  any read can trigger it. The application cannot pin the flush to
  a transaction boundary. That is the gap FalkorDB's delta matrices
  exist to fill, and question 2 asks you to weigh it against the
  other candidate reasons.

  </details>

- [ ] You can explain the iso-value optimization and identify which FalkorDB matrices are iso.

  <details><summary>Answer</summary>

  An iso matrix keeps its full sparsity pattern but stores **one**
  value, `A->x[0]`, for every present entry
  (`GB_Matrix_content.h:513-518`, `:524`). The struct's own
  rationale is that GraphBLAS deliberately has no structure-only
  type — that "would result in a mathematical mismatch with all
  other objects" — so iso is the sanctioned way to express an
  unweighted graph. Corollary at `:520-521`: a *full* iso matrix is
  O(1) memory regardless of dimension.

  FalkorDB: the boolean adjacency and delta matrices are iso — and
  `delta_new.c:40-44` makes DM always `GrB_BOOL`. A relation matrix
  carrying edge IDs as values is not. Question 3 asks for the mxm
  bandwidth cost of losing it: 8 bytes per entry that iso would
  have moved zero of, against a measured 15.8-19.1 GB/s
  (`notes.md:9-14`).

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the 32-bit index memory computation.

  <details><summary>Answer</summary>

  For 10M nodes and 100M edges: all-64-bit CSR index is
  80.0 + 800.0 = **880 MB**; all-32-bit is 40.0 + 400.0 =
  **440 MB**, exactly 2×.

  The finer point is that the pin has **three independent flags**,
  not one — `p_is_32`, `j_is_32`, `i_is_32`
  (`GB_Matrix_content.h:534-536`), plus `p_control`/`j_control`/
  `i_control` at `:464-466` for forcing them. `p` indexes *offsets*
  (bounded by nnz) and `i` indexes *rows* (bounded by the
  dimension), so a 5B-edge graph over 100M nodes wants 64-bit `p`
  and 32-bit `i` — 480 MB here, still 1.83× better than all-64.
  32-bit `p` becomes illegal at about 2.1e9 edges.

  </details>

## References

**Papers**

- Davis, T. A. — "Algorithm 1000: SuiteSparse:GraphBLAS: Graph
  Algorithms in the Language of Sparse Linear Algebra", ACM TOMS
  45(4), Article 44, December 2019,
  [doi:10.1145/3322125](https://doi.org/10.1145/3322125). Read
  §3.1.8 (non-blocking mode), §3.1.9 (accumulator and mask), §4.1
  (data structure) and §4.2.1 (matrix multiply). Cited here from
  the author's accepted manuscript, which is titled "Algorithm 9xx"
  and describes **version 2.3.3, single-threaded**.
- Davis, T. A. — "Parallel GraphBLAS with OpenMP", CSC '20 (SIAM
  Workshop on Combinatorial Scientific Computing). §3 dates the
  arrival of parallelism to version 3.0.1; §3.1 names the engine
  chosen per algorithm; Table 2 is the 40-thread speedup table.
  The citable source for anything about threads. Read in
  [reading-openmp-vs-rayon.md](reading-openmp-vs-rayon.md).
- Davis, T. A. — "Algorithm 1037: SuiteSparse:GraphBLAS: Parallel
  Graph Algorithms in the Language of Sparse Linear Algebra", ACM
  TOMS 49(3), 2023. The v2 update: JIT, 32/64-bit indices, iso
  matrices. **Not obtainable for this repo** — no claim in this
  chapter is attributed to it; the same features are verified in
  the pinned source instead.

**Code**

- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  at `1fd5475`, version **10.3.1** (`Include/GraphBLAS.h:290-292`).
  `Source/builtin/include/GB_Matrix_content.h` is the object model
  in one 657-line file — `:52` and `:76` (eight formats),
  `:223-228` (the five arrays), `:241-274` (the hyper_hash Y),
  `:361` (Pending), `:367-391` (zombies), `:450-457` (the two
  switches), `:462-467` (sparsity and index-width controls),
  `:497-498` (`is_csc`, `jumbled`), `:513-524` (iso), `:531-536`
  (the three width flags). `Source/jitifyer/` is the JIT.
  `Include/GraphBLAS.h:666` is `GrB_DESC_RSC`. The `Source/mxm/`
  and `Source/convert/` walk is
  [reading-suitesparse-internals.md](reading-suitesparse-internals.md).
- [FalkorDB](https://github.com/FalkorDB/FalkorDB) at `ccb449a9a` —
  `src/graph/delta_matrix/delta_new.c:24-44` pins the sparsity and
  disables the hyper_hash. Read in
  [reading-falkordb-delta-matrix.md](reading-falkordb-delta-matrix.md).

**Measured, in this repo**

- `topics/20-graphblas/notes.md:41-42` — 80.4 MB → 1.59 MB and
  11,312 µs → 66 µs. Step 2 reproduces the first from array sizes
  to within 1%.
- `topics/20-graphblas/notes.md:9-18` — the SpMV ladder,
  19.1 → 15.8 GB/s, and why it sits below the ~30 GB/s streaming
  baseline. Question 3's bandwidth arithmetic runs on it.
