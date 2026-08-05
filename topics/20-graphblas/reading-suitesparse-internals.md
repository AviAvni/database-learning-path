# Inside SuiteSparse: format switching and the saxpy3 scheduler

The code walk behind the TOMS papers
([reading-davis-toms19.md](reading-davis-toms19.md)): where format
switches are decided, and the saxpy3 scheduler — the most
database-executor-like piece of code in the library. This chapter
builds the concepts each file implements, step by step, then hands
you the anchors into `Source/` of the SuiteSparse:GraphBLAS repo to
watch them happen.

Every anchor below is **SuiteSparse:GraphBLAS at commit
`1fd5475`** (the pin in `resources/codebases.md`), quoted with the
line numbers the code occupies at that commit. Two of the numbers
most often repeated about this library — the bitmap threshold and
the `m/16` hash rule — are *stale documentation* at this pin, and
this chapter says where the shipped code actually decides.

## The problem in one sentence

Inside one `GrB_mxm`, per-column costs can vary by three orders of
magnitude (this repo's own RMAT scale-16 graph has a max degree of
**9,751** against a mean of **27.8** —
`topics/24-graph-algorithms/notes.md:5-7`), the right accumulator
depends on output density, and the right *format* for the result
depends on how dense it came out — so the library must make
cost-based decisions per matrix, per multiply, and per task, and
every one of those decisions is a readable number in the code.

## The concepts, step by step

### Step 1 — the four formats, and the bitmask that gates them

> **In:** nothing; this fixes the vocabulary the rest of the
> chapter switches between.
> **Out:** the four data formats, and where a matrix records which
> of them it is allowed to become.

SuiteSparse stores a matrix in one of **four** formats, not two:

| format | what it is | index space |
|---|---|---|
| **full** | every entry present; just the values array | none needed |
| **bitmap** | values array of size m·n plus a byte-per-cell present flag | none needed |
| **sparse** | CSR/CSC: a pointer array `A.p` of length n+1, plus `A.i`, `A.x` | O(n + nnz) |
| **hypersparse** | `A.p` itself becomes sparse: `A.h` lists the non-empty vectors | O(nnz) |

Davis's TOMS '19 paper (§4.1) describes only the last two — at
version 2.3.3 "a GraphBLAS matrix is stored in one of four
different formats: compressed-sparse column (standard CSC),
compressed-sparse row (standard CSR), and hypersparse versions of
these two". **Bitmap and full postdate that paper.** Any claim
about them has to come from the source or from the later parallel
paper; a guide that cites TOMS '19 for the bitmap format is citing
the wrong document.

Each matrix carries a `sparsity_control` bitmask saying which of
the four it is *allowed* to be, plus two floats (`hyper_switch`,
`bitmap_switch`) saying *when* to move. `GB_conform` runs at the
end of every operation and dispatches on the bitmask —
`GB_conform.c:150` switches over
`GB_sparsity_control(A->sparsity_control, A->vdim)` into fifteen
cases, of which these three are the ones you will meet:

| case | anchor | what it does |
|---|---|---|
| `GxB_HYPERSPARSE` alone | `GB_conform.c:157-160` | `GB_convert_any_to_hyper` unconditionally — no test is run |
| `GxB_SPARSE` alone | `GB_conform.c:166-169` | convert to sparse unconditionally |
| `GxB_HYPERSPARSE + GxB_SPARSE` | `GB_conform.c:175-184` | run the hyper test; the bitmap test never executes |

That table is the answer to question 1 before you go looking:
pinning a matrix's `sparsity_control` does not bias a heuristic, it
*removes the branch that would have run it*.

Why it matters: "which format is this matrix in" is a question with
four answers and a per-matrix policy, and the policy is enforced by
which case of a switch statement you land in.

### Step 2 — the two switches, and the hysteresis in each

> **In:** the four formats and the two float thresholds (Step 1).
> **Out:** four one-line predicates with their exact constants, and
> the width of the band in which nothing happens.

The bitmap boundary is two functions, and the asymmetry between
them is deliberate. `GB_convert_bitmap_to_sparse_test.c:13-16`
states the policy in the library's own words:

```c
// GB_convert_bitmap_to_sparse_test.c — the policy comment, 13-16
    13  // If A is m-by-n and A->sparsity_control is GxB_ANY_SPARSITY with b =
    14  // A->bitmap_switch, the matrix switches to bitmap if nnz(A)/(m*n) > b.  A
    15  // bitmap matrix switches to sparse if nnz(A)/(m*n) <= b/2.  A matrix whose
    16  // density is between b/2 and b remains in its current state.
```

And the two predicates that implement it:

```c
// GB_convert_sparse_to_bitmap_test.c — sparse → bitmap, 31-38
    31      // current number of entries in the matrix or vector
    32      float nnz = (float) anz ;
    33
    34      // maximum number of entries in the matrix or vector
    35      float nnz_dense = ((float) vlen) * ((float) vdim) ;
    36
    37      // A should switch to bitmap if the following condition is true:
    38      return (nnz > bitmap_switch * nnz_dense && nnz_dense < (float) GB_NMAX) ;
```

```c
// GB_convert_bitmap_to_sparse_test.c — bitmap → sparse, 43-44
    43      // A should switch to sparse if the following condition is true:
    44      return (nnz <= (bitmap_switch/2) * nnz_dense) ;
```

**Hysteresis** — the switch-up and switch-down thresholds differ,
so a matrix hovering near the boundary does not convert back and
forth on every operation — is exactly the `b` versus `b/2` gap. The
same instinct as topic 3's LSM compaction triggers, and the cost of
getting it wrong is that each conversion is an O(nnz) rebuild, so
ping-ponging turns every operation into a copy.

The hyper boundary has the same shape with a factor of 2 on the
other side, where `k` is the number of non-empty vectors:

```c
// GB_convert_sparse_to_hyper_test.c — sparse → hyper, 33
    33      return (n > 1 && (((float) k) <= n * hyper_switch)) ;
```

```c
// GB_convert_hyper_to_sparse_test.c — hyper → sparse, 33
    33      return (n <= 1 || (((float) k) > n * hyper_switch * 2)) ;
```

Now the constants, and the first correction. `hyper_switch`
defaults to `GB_HYPER_SWITCH_DEFAULT` = **0.0625** = 1/16
(`Source/include/GB_defaults.h:20`) — which matches TOMS '19
§4.2.1 exactly: "SuiteSparse:GraphBLAS stores its matrices in
hypersparse format if n̄ < n/16."

`bitmap_switch` does **not** default to a small percentage, and it
does not depend on the operation. It is a table indexed by the
matrix's *minimum dimension*:

```c
// GB_Global.c — the bitmap_switch table, 181-189
   181      // min dimension                    density
   182      #define GB_BITMAP_SWITCH_1          ((float) 0.04)
   183      #define GB_BITMAP_SWITCH_2          ((float) 0.05)
   184      #define GB_BITMAP_SWITCH_3_to_4     ((float) 0.06)
   185      #define GB_BITMAP_SWITCH_5_to_8     ((float) 0.08)
   186      #define GB_BITMAP_SWITCH_9_to_16    ((float) 0.10)
   187      #define GB_BITMAP_SWITCH_17_to_32   ((float) 0.20)
   188      #define GB_BITMAP_SWITCH_33_to_64   ((float) 0.30)
   189      #define GB_BITMAP_SWITCH_gt_than_64 ((float) 0.40)
```

```c
// GB_Global.c — which row of the table a matrix gets, 486-497
   486  float GB_Global_bitmap_switch_matrix_get (int64_t vlen, int64_t vdim)
   487  {
   488      int64_t d = GB_IMIN (vlen, vdim) ;
   489      if (d <=  1) return (GB_Global.bitmap_switch [0]) ;
   ...
   495      if (d <= 64) return (GB_Global.bitmap_switch [6]) ;
   496      return (GB_Global.bitmap_switch [7]) ;
   497  }
```

For any graph matrix — anything with more than 64 rows and columns
— the answer is row 7: **b = 0.40**. Work out what that means:

```
 inputs: a graph adjacency matrix, n × n, n = 262,144 (notes.md:13)
         nnz = 2.0e6
         b = 0.40 (GB_Global.c:189, since min(vlen,vdim) = 262,144 > 64)

 density = 2.0e6 / (262,144)²  = 2.0e6 / 6.87e10 = 0.0000291 = 0.0029%

 bitmap needs density > 0.40                      → 40%
 the graph is 13,700× below the threshold
 nnz that WOULD trigger bitmap = 0.40 × 6.87e10   = 2.75e10 entries
   ... which at 1 byte of flag each is 27.5 GB of presence bits alone

 hysteresis band: density between b/2 = 20% and b = 40% keeps the
 current format. On this matrix the band is 1.37e10 to 2.75e10
 entries wide — utterly unreachable.
```

**A real graph adjacency matrix will never become bitmap.** The
bitmap format exists for the *vectors* and small dense blocks in a
computation, which is why LAGraph has to ask for it explicitly
(`LG_SET_FORMAT_HINT(q, LG_BITMAP)` at the BFS template's `:312`)
rather than waiting for the heuristic. Anyone who tells you the
threshold is "about 4–8%, operation-dependent" is reading
`bitmap_switch[0..3]`, which apply only to matrices with a
dimension of 8 or less.

Why it matters: the number that gets quoted for this switch is
wrong by an order of magnitude and wrong in kind (it is not
per-operation), and the correct number explains why the pull BFS
must set its format by hand.

### Step 3 — count the work before allocating: the flopcount pre-pass

> **In:** a multiply that has not started yet.
> **Out:** two numbers — total flops and per-vector flops — that
> size every subsequent decision, and their cost.

For sparse matrix multiply, **flops** means the number of scalar
multiply-add operations that actually exist: for C = A*B, one per
(A(i,k), B(k,j)) pair of present entries. Davis's own definition
(TOMS '19 §4.2.1) is "f is the number of 'multiply-adds' computed
(in the semiring)". Unlike dense matmul you can *count* them
cheaply before doing any of them, because the count depends only on
the patterns:

```c
// GB_AxB_saxpy3_flopcount.c — the algorithm, in the header comment, 50-69
    50  //      Bflops = zeros (1,n)         % (set to zero in the caller)
    51  //      for each column j in B:
    52  //          if (B (:,j) is empty) continue
    53  //          if (M is present and M (:,j) is empty and not Mask_comp) continue
    54  //          for each k where (B (k,j) != 0):
    55  //              aknz = nnz (A (:,k))
    56  //              if (aknz == 0) continue
    57  //              Bflops (j) += aknz          % A(:,k)*B(k,j) requires aknz flops
```

And its complexity, stated in the same file:

```c
// GB_AxB_saxpy3_flopcount.c — the complexity claim, 44-48
    44  // The complexity of this function is O(nnz(B)+n) if A and M are not
    45  // hypersparse.  If A and/or M are hypersparse, then the complexity can
    46  // increase to O(nnz(B)*log(h)) where h is the # of non-empty vectors in
    47  // A (or M).  The log(h) factor is due to the binary search of A->h or M->h
    48  // for each entry in B.
```

Read `:53` twice — the pre-pass already applies the mask, skipping
columns whose mask column is empty. The mask is priced in before a
single multiply happens.

Those two numbers then size *everything*: how many threads, how to
slice the work, and how big each hash table should be. The same
two-phase shape recurs across the curriculum — cudf's
size/retrieve (topic 18), Gunrock's degree-scan — because sparse
output size is the recurring villain: you cannot allocate the
output until you have measured the work.

Why it matters: this pre-pass is not an optimization, it is the
precondition for every decision in Steps 4 through 6. Everything
downstream is arithmetic on `total_flops` and `Bflops`.

### Step 4 — the thread count, computed from flops

> **In:** `total_flops` from Step 3.
> **Out:** the actual number of threads this multiply will use, on
> this repo's own measured workloads.

```c
// GB_AxB_saxpy3_slice_balanced.c — flopcount, then thread count, 308-310 and 418
   308      GB_OK (GB_AxB_saxpy3_flopcount (&Mwork, Bflops, M, Mask_comp, A, B,
   309          &total_flops, &axbflops, Werk)) ;
   ...
   418      (*nthreads) = GB_nthreads (total_flops, chunk, nthreads_max) ;
```

```c
// GB_nthreads.h — the whole rule, 17-32
    17  // If work < 2*chunk, then only one thread is used.
    18  // else if work < 3*chunk, then two threads are used, and so on.
    ...
    27      work  = GB_IMAX (work, 1) ;
    28      chunk = GB_IMAX (chunk, 1) ;
    29      int64_t nthreads = (int64_t) floor (work / chunk) ;
    30      nthreads = GB_IMIN (nthreads, nthreads_max) ;
    31      nthreads = GB_IMAX (nthreads, 1) ;
```

`chunk` defaults to `GB_CHUNK_DEFAULT` = **64·1024 = 65,536**
(`Source/include/GB_defaults.h:24`). Now run this repo's own
measured SpGEMM flop counts (`notes.md:22-26`) through it, on an
8-performance-core machine:

```
 rule: nthreads = clamp(floor(total_flops / 65536), 1, nthreads_max)
       with nthreads_max = 8

 scale 10:    298,000 flops / 65,536  = floor(4.5)   =  4 threads
 scale 12:  2,270,000 flops / 65,536  = floor(34.6)  = 34 → clamped to 8
 scale 14: 17,100,000 flops / 65,536  = floor(260.9) = 260 → clamped to 8

 so: the smallest of the three benchmarks does not even use the
 whole machine, and the other two are clamped long before the
 divide matters.
```

That is the whole story of `chunk`: it is a floor on how much work
justifies waking a thread, not a slicing parameter. Below
2 × 65,536 = 131,072 flops a multiply is single-threaded no matter
how many cores you own.

Why it matters: the first two rows explain why a small SpGEMM does
not scale — it was never asked to. Before blaming the scheduler,
check whether `GB_nthreads` handed it one thread.

### Step 5 — saxpy3's task taxonomy: coarse and fine

> **In:** `Bflops` per vector (Step 3) and a thread budget (Step 4).
> **Out:** the four task kinds, the workspace each costs in bytes,
> and the reason one of them needs atomics.

The header comment is the scheduler spec, and it is worth reading
in full:

```c
// GB_AxB_saxpy3.c — the task taxonomy, 22-48
    22  // The matrix B is split into two kinds of tasks: coarse and fine.  A coarse
    23  // task computes C(:,j1:j2) = A*B(:,j1:j2), for a unique set of vectors j1:j2.
    24  // Those vectors are not shared with any other tasks.  A fine task works with a
    25  // team of other fine tasks to compute C(:,j) for a single vector j.  Each fine
    26  // task computes A*B(k1:k2,j) for a unique range k1:k2, and sums its results
    27  // into C(:,j) via atomic operations.
    28
    29  // Each coarse or fine task uses either Gustavson's method [1] or the Hash
    30  // method [2].  There are 4 kinds of tasks:
    31
    32  //      fine Gustavson task
    33  //      fine hash task
    34  //      coarse Gustason task
    35  //      coarse hash task
    36
    37  // Each of the 4 kinds tasks are then subdivided into 3 variants, for C=A*B,
    38  // C<M>=A*B, and C<!M>=A*B, giving a total of 12 different types of tasks.
    ...
    42  // ... Coarse tasks are
    43  // prefered since they require less synchronization, but fine tasks allow for
    44  // better parallelization when B has only a few vectors.  If B consists of a
    45  // single vector (for GrB_mxv if A is in CSC format and not transposed, or
    46  // for GrB_vxm if A is in CSR format and not transpose), then the only way to
    47  // get parallelism is via fine tasks.
```

Note `:44-47`. A **matrix-vector** product — the BFS pull step —
has B with exactly one vector, so *the only available parallelism
is fine tasks*, with their atomics. That is a structural fact about
SpMV, and it is the mechanism behind the parallel-scaling gap in
[reading-openmp-vs-rayon.md](reading-openmp-vs-rayon.md).

The workspace bill, also from the header:

```c
// GB_AxB_saxpy3.c — workspace per task kind, 62-70
    62  // The workspace allocated depends on the type of task.  Let s be the hash
    63  // table size for the task, and C is m-by-n (assuming all matrices are CSC; if
    64  // CSR, then m is replaced with n).
    65  //
    66  //      fine Gustavson task (shared):   int8_t   Hf [m] ; ctype Hx [m] ;
    67  //      fine hash task (shared):        uint64_t Hf [s] ; ctype Hx [s] ;
    68  //      coarse Gustavson task:          uint64_t Hf [m] ; ctype Hx [m] ;
    69  //      coarse hash task:               uint64_t Hf [s] ; ctype Hx [s] ;
    70  //                                      uint64_t Hi [s] ;
```

Price those four rows for an f64 output at scale 20 (m = 2²⁰ =
1,048,576, `notes.md:14`):

```
 inputs: m = 1,048,576 ; ctype = double (8 bytes)
         a hash task with flmax = 4,096 gets s = 16,384 (Step 6)

 fine Gustavson (shared) : 1·m + 8·m  =  9 bytes/row × 1,048,576 =  9.4 MB
 coarse Gustavson        : 8·m + 8·m  = 16 bytes/row × 1,048,576 = 16.8 MB
 fine hash               : 8·s + 8·s  = 16 bytes/slot × 16,384   = 262 KB
 coarse hash             : 8·s ×3     = 24 bytes/slot × 16,384   = 393 KB

 per-thread, on 8 threads, coarse Gustavson:
   8 × 16.8 MB = 134 MB of workspace, none of it in any cache
 the M3 Pro's L2 is ~16 MB shared; ONE coarse Gustavson task
 already exceeds it, and eight of them thrash.

 the same 8 threads on coarse hash tasks: 8 × 393 KB = 3.1 MB,
 which fits in L2 with room to spare.
```

Note the fine Gustavson row is 9 bytes per row, not 16: `:66` uses
`int8_t Hf` rather than `uint64_t`, because a shared flag only
needs to hold a state, while a coarse task's `Hf` doubles as a
64-bit stamp. Nine versus sixteen is a 44% saving on the biggest
array in the library.

Why it matters: this is where the hash-versus-Gustavson decision
comes from. It is not about collisions — it is about 16.8 MB versus
393 KB, and topic 13's blocking argument.

### Step 6 — each task picks its accumulator, by a rule that is not m/16

> **In:** the per-vector flop maxima (Step 3) and the workspace
> bills (Step 5).
> **Out:** the shipped selection rule, the flop threshold it
> implies, and the stale comment it contradicts.

**Gustavson** here means the classic dense-workspace method: a
**SPA** (sparse accumulator — a dense array of size m, one slot per
possible output row, plus a marker of which slots are occupied)
gives O(1) scatter but costs m slots of possibly-cold memory per
task. The **hash** alternative sizes a table by the flop estimate
instead of by m — small and cache-resident when the vector is
sparse, no matter how big m is.

The header comment states the rule as `m/16`:

```c
// GB_AxB_saxpy3.c — the DOCUMENTED rule, 50-60. Read it, then read the code.
    50  // To select between the Hash method or Gustavson's method for each task, the
    51  // hash table size is first found.  ...
    53  // ... It is set to twice the smallest power of 2 that
    54  // is greater than the flop count to compute that vector (plus the # of entries
    55  // in M(:,j) for tasks that compute C<M>=A*B or C<!M>=A*B).  This size ensures
    56  // the results will fit in the hash table, and with ideally only a modest
    57  // number of collisions.  If the hash table size exceeds a threshold (currently
    58  // m/16 if C is m-by-n), then Gustavson's method is used instead, and the hash
    59  // table size is set to m, to serve as the gather/scatter workspace for
    60  // Gustavson's method.
```

The shipped code says something else:

```c
// GB_AxB_saxpy3_slice_balanced.c — GB_hash_table_size, the real rule, 56-99
    56  static inline uint64_t GB_hash_table_size
    57  (
    58      int64_t flmax,      // max flop count for any vector computed by this task
    59      int64_t cvlen,      // vector length of C
    60      const int AxB_method     // Default, Gustavson, or Hash
    61  )
    62  {
    63      uint64_t hash_size ;
    64
    65      if (AxB_method == GxB_AxB_GUSTAVSON || flmax >= cvlen/2)
    ...
    72          hash_size = cvlen ;
    ...
    82          // hash_size = 2 * (smallest power of 2 >= flmax)
    83          hash_size = ((uint64_t) 2) << (GB_FLOOR_LOG2 (flmax) + 1) ;
    84          bool use_Gustavson ;
    85          if (AxB_method == GxB_AxB_HASH)
    86          {
    87              // always use Hash method, unless the hash_size >= cvlen
    88              use_Gustavson = (hash_size >= cvlen) ;
    89          }
    90          else
    91          {
    92              // default: auto selection:
    93              // use Gustavson's method if hash_size is too big
    94              use_Gustavson = (hash_size >= cvlen/12) ;
    95          }
    96          if (use_Gustavson)
    97          {
    98              hash_size = cvlen ;
    99          }
```

**The threshold is `cvlen/12`, at `:94` — not `m/16`.** The
comment at `GB_AxB_saxpy3.c:57-58` has not been updated. There is
also an earlier cutoff the comment never mentions: `:65` sends any
vector with `flmax >= cvlen/2` straight to Gustavson without
computing a hash size at all.

Work out where the crossover actually falls, at scale 20:

```
 inputs: cvlen = m = 2^20 = 1,048,576   (notes.md:14, scale 20)
         default auto-selection, so :94 applies

 Gustavson is chosen when hash_size >= cvlen/12 = 87,381

 hash_size = 2 << (floor_log2(flmax) + 1) = 2^(floor_log2(flmax) + 2)

 smallest power of two >= 87,381 is 2^17 = 131,072
   → need floor_log2(flmax) + 2 >= 17
   → floor_log2(flmax) >= 15
   → flmax >= 2^15 = 32,768

 so: a vector with 32,768 or more flops gets Gustavson;
     fewer than that gets a hash table.

 as a fraction of m:  32,768 / 1,048,576 = 3.1%  (i.e. m/32)

 sanity-check against the DOCUMENTED m/16 rule:
   hash_size >= m/16 = 65,536 → 2^(k+2) >= 65,536 → k >= 14
   → flmax >= 16,384 = m/64
   the comment's rule would switch to Gustavson at HALF the flop
   count the code does. The code is more willing to hash.
```

And a corollary worth its own line: because `hash_size` is at least
2× `flmax` (`:83` gives `2^(k+2)` where `2^k ≤ flmax`), and `flmax`
is an *exact* upper bound on the number of distinct entries the
vector can produce, **the hash table cannot overflow**. There is no
resize path in saxpy3 because there is nothing to resize for — the
pre-pass of Step 3 makes the size provable. That is question 3's
answer, and it is a genuinely different design from SwissTable's
grow-on-load-factor (topic 8): SuiteSparse buys exactness with a
whole extra pass over the patterns.

Why it matters: two of the three numbers you would have quoted from
the header comment are wrong at this pin. The `m/16` rule is stale;
the `flmax >= cvlen/2` cutoff is undocumented; only the "twice the
smallest power of 2" sizing survives.

### Step 7 — dot3: the mask as the outer loop

> **In:** the mask, so far only a filter (Steps 3, 5).
> **Out:** the engine where the mask is the loop bound, and the
> line of code that proves it.

The other engine inverts control entirely:

```c
// GB_AxB_dot3.c — the contract, 10-13
    10  // This function only computes C<M>=A'*B.  The mask must be present, and not
    11  // complemented, and can be either valued or structural.  The mask is always
    12  // applied.  C and M are both sparse or hypersparse, and have the same sparsity
    13  // structure.
```

Four preconditions in three lines: mask present, mask not
complemented, C and M both sparse-or-hypersparse, **same sparsity
structure**. The last one is not a hint — it is a promise the code
keeps literally:

```c
// GB_AxB_dot3.c — C is allocated with exactly nnz(M) entries, 126 and 171
   126      const int64_t mnz = GB_nnz (M) ;
   ...
   171      int64_t cnz = mnz ;
```

`C` gets `nnz(M)` slots because it will have at most `nnz(M)`
entries, one candidate per mask entry. Work is nnz(M) sparse dot
products; the mask is not a filter applied afterwards, it is the
outer loop. Compare `GB_AxB_dot3.c:244`, where the thread count is
`GB_nthreads(cnz, chunk, nthreads_max)` — even the parallelism is
sized by the mask.

The shape of the loop, since the real one is templated across
twelve type combinations:

```rust
// ILLUSTRATION — not quoted from SuiteSparse. The real loop is generated
// from templates; the structural claims are GB_AxB_dot3.c:10-13 (the
// contract), :126 and :171 (C sized to nnz(M)), and :244 (threads sized
// to cnz).
fn dot3(m: &Pattern, a_t: &Csr, b: &Csc, semiring: &Semiring) -> Coo {
    let mut c = Coo::with_capacity(m.nnz());   // :171 — cnz = mnz
    for (i, j) in m.entries() {                // one dot per MASK entry
        // sparse dot = two-pointer intersect of the two patterns
        if let Some(v) = sparse_dot(a_t.row(i), b.col(j), semiring) {
            c.push(i, j, v);                   // ANY monoid ⇒ sparse_dot
        }                                      //   may stop at the first hit
    }
    c
}
```

If M is triangle counting's lower-triangular L, that is one dot per
candidate wedge — nothing is computed for output cells the mask
excludes. Contrast saxpy3, where the mask only prunes *writes*: the
flops still happen. Davis says the same thing about the masked case
in TOMS '19 §4.2.1: "If the mask is present (and not complemented),
only the subset of entries appearing in the mask are computed…
In this method, the symbolic analysis is skipped."

Price the asymmetry on a masked triangle-count product:

```
 inputs: C<L> = L*L on RMAT scale 16 (topics/24-graph-algorithms/notes.md:5)
         n = 65,536, m_directed = 1,819,338, so nnz(L) ≈ 909,669
         mean degree d̄ = 1,819,338 / 65,536 = 27.8

 dot3 work  = nnz(L) dot products
            = 909,669 × (intersect two rows of avg length 27.8)
            ≈ 909,669 × 55.6 pointer steps    = 5.06e7 steps

 saxpy work = Σ over L's entries of nnz(L(k,:))
            ≈ 909,669 × 27.8                  = 2.53e7 multiply-adds
            ... of which only the entries landing inside L survive

 so saxpy does FEWER raw operations here, and dot3 still often wins,
 because dot3's 5.06e7 steps are two sequential streams while
 saxpy's 2.53e7 are scatters into a 65,536-slot SPA plus a
 discard pass. Which is exactly why LAGr_TriangleCount.c:43-47
 refuses to pick a winner and ships both.
```

Why it matters: "masks are free performance" is true only when the
dispatcher picks a mask-as-outer-loop engine, and the four
preconditions at `:10-13` are what decide that.

### Step 8 — dispatch: a cost-based optimizer decision per multiply

> **In:** the two engines (Steps 5-7) and their preconditions.
> **Out:** the exact control function that chooses, and the reason
> the BFS pull step lands where it does.

For the `C = A'*B` shape, the decision lives in one function:

```c
// GB_AxB_meta_adotb_control.c — the auto-selection, 60-88 (elided)
    60      else if (AxB_method == GxB_DEFAULT)
    61      {
    62          // auto selection for A'*B
    ...
    72          if (GB_AxB_dot4_control (C_out_iso, can_do_in_place ? C_in : NULL,
    73              M, Mask_comp, accum, semiring))
    75              // C+=A'*B can be done with dot4
    76              (*axb_method) = GB_USE_DOT ;
    78          else if (GB_AxB_dot3_control (M, Mask_comp))
    80              // C<M>=A'*B uses the masked dot product method (dot3)
    81              (*axb_method) = GB_USE_DOT ;
    83          else if (GB_AxB_dot2_control (A, B))
    85              // C=A'*B or C<!M>=A'B* can efficiently use the dot2 method
    86              (*axb_method) = GB_USE_DOT ;
    88      }
```

Three `else if`s in order, and no `else` — falling off the end at
`:88` leaves `*axb_method` at the default set at `:36`, which is
`GB_USE_SAXPY`. **Saxpy is what you get when nothing else claims
the multiply.**

with the two predicates it consults:

```c
// GB_mxm.h — when dot3 is eligible, 235-243
   235  static inline bool GB_AxB_dot3_control
   ...
   241      return (M != NULL && !Mask_comp &&
   242          (GB_IS_SPARSE (M) || GB_IS_HYPERSPARSE (M))) ;
```

```c
// GB_AxB_dot2_control.c — the first and decisive test, 23-30
    23      // C = A'*B is very efficient if A and/or B are full or bitmap
    ...
    26      if (GB_IS_FULL (A) || GB_IS_BITMAP (A) ||
    27          GB_IS_FULL (B) || GB_IS_BITMAP (B))
    28      {
    29          return (true) ;
    30      }
```

Trace the BFS pull step through it and you get a result that
contradicts the usual summary. LAGraph's pull is
`GrB_mxv(q, mask, NULL, semiring, AT, q, GrB_DESC_RSC)`
(`LG_BreadthFirstSearch_SSGrB_template.c:313`). `GrB_DESC_RSC`
includes `GrB_COMP` (`Include/GraphBLAS.h:666`), so `Mask_comp` is
true, so `GB_AxB_dot3_control` at `GB_mxm.h:241` returns **false**.
**Pull cannot use dot3.** What makes a dot engine eligible is the
line before the call — `LG_SET_FORMAT_HINT(q, LG_BITMAP)` at
`:312` — which makes `GB_AxB_dot2_control.c:26-30` return true on
its first test. Pull is **dot2**, and Davis's CSC '20 §3.1 agrees
in words: "By default, GraphBLAS selects the masked-dot-product
method for… the pull phase of the push/pull BFS."

The consequence for API users: the *same* `GrB_mxm` line runs a
different algorithm depending on your mask's density and your
operands' formats — which is exactly how the BFS push/pull switch
is implemented in
[reading-beamer-sc12.md](reading-beamer-sc12.md) and
[reading-lagraph.md](reading-lagraph.md).

Why it matters: the dispatcher reads *four* things — mask presence,
mask complement, operand format, and output format — and getting a
prediction right means checking all four, not just "is there a
mask".

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| `Source/convert/GB_conform.c:150` | 1 | the 15-case switch on `sparsity_control` |
| `Source/convert/GB_conform.c:157-160`, `:166-169`, `:175-184` | 1 | hyper-only, sparse-only, hyper+sparse cases |
| `Source/convert/GB_conform_hyper.c:44-57` | 1-2 | `nvec_nonempty`, then the two hyper tests |
| `Source/convert/GB_convert_bitmap_to_sparse_test.c:13-16` | 2 | the b / b/2 hysteresis policy, in the library's words |
| `Source/convert/GB_convert_sparse_to_bitmap_test.c:32-38` | 2 | `nnz > bitmap_switch × nnz_dense` |
| `Source/convert/GB_convert_bitmap_to_sparse_test.c:44` | 2 | `nnz <= (bitmap_switch/2) × nnz_dense` |
| `Source/convert/GB_convert_sparse_to_hyper_test.c:33` | 2 | `n > 1 && k <= n × hyper_switch` |
| `Source/convert/GB_convert_hyper_to_sparse_test.c:33` | 2 | `n <= 1 \|\| k > n × hyper_switch × 2` |
| `Source/global/GB_Global.c:181-189`, `:486-497` | 2 | the bitmap_switch table, **indexed by min dimension** — 0.40 for graphs |
| `Source/include/GB_defaults.h:20`, `:24` | 2, 4 | `hyper_switch` 0.0625, `chunk` 65,536 |
| `Source/mxm/GB_AxB_saxpy3_flopcount.c:44-48`, `:50-69` | 3 | complexity and the pre-pass algorithm |
| `Source/mxm/GB_AxB_saxpy3_flopcount.c:219-221` | 3 | `schedule(dynamic,1)` over pre-sliced tasks |
| `Source/mxm/GB_AxB_saxpy3_slice_balanced.c:308-310`, `:418` | 3-4 | flopcount call, then `GB_nthreads(total_flops, …)` |
| `Source/omp/include/GB_nthreads.h:17-32` | 4 | `clamp(floor(work/chunk), 1, nthreads_max)` |
| `Source/mxm/GB_AxB_saxpy3.c:22-48` | 5 | coarse/fine, the 4 kinds, the 12 variants |
| `Source/mxm/GB_AxB_saxpy3.c:62-70` | 5 | the workspace table — 9 vs 16 bytes per row |
| `Source/mxm/GB_AxB_saxpy3_slice_balanced.c:56-99` | 6 | `GB_hash_table_size` — the **`cvlen/12`** rule |
| `Source/mxm/GB_AxB_saxpy3.c:57-58` | 6 | the stale `m/16` comment — do not quote it |
| `Source/mxm/GB_AxB_dot3.c:10-13`, `:126`, `:171`, `:244` | 7 | mask required and uncomplemented; C sized to nnz(M) |
| `Source/mxm/GB_AxB_meta_adotb_control.c:36`, `:60-93` | 8 | default saxpy, then dot4 / dot3 / dot2 in order |
| `Source/mxm/GB_mxm.h:235-243` | 8 | `GB_AxB_dot3_control` — the four-term predicate |
| `Source/mxm/GB_AxB_dot2_control.c:26-30`, `:68-79` | 8 | bitmap/full operand ⇒ dot2; the degree heuristic |
| `Include/GraphBLAS.h:666` | 8 | `GrB_DESC_RSC = REPLACE + STRUCTURE + COMP` |

Navigation advice: start with the saxpy3 header comment
(`GB_AxB_saxpy3.c:22-86`) — it is the scheduler spec, and
everything else in `Source/mxm/` is an implementation of that
comment — but read it *next to*
`GB_AxB_saxpy3_slice_balanced.c:56-99`, because the comment's
selection rule is out of date. Then read `GB_conform.c` top to
bottom (391 lines, mostly a switch), then
`GB_AxB_meta_adotb_control.c` for the dispatch conditions.

### What transfers to M20

- Our stub SpGEMM = one coarse Gustavson task (dense SPA). The
  HashMap reference = the hash task. `gb_bench` measures the
  crossover directly — and Step 6 says where to expect it.
- Masked-SpMV pull BFS = a dot engine's idea specialized: iterate
  the UNVISITED set (the complemented mask), early-exit each dot at
  the first frontier hit (ANY monoid ⇒ short-circuit legal).
- M20's kernel core needs only: saxpy-SpMSpV (push), masked
  dot-SpMV (pull), one SPA SpGEMM, conform-lite (hyper↔sparse). The
  bitmap arithmetic in Step 2 is the argument for *not* building a
  bitmap matrix format at all.

## Questions for notes.md

1. Find FalkorDB's `GxB_set` calls pinning formats
   (`src/graph/delta_matrix/delta_new.c` at the pin). Which matrices
   allow bitmap, and why not the adjacency ones? Use Step 2's
   arithmetic and Step 1's `GB_conform.c` case table for the
   answer — the interesting part is that pinning removes the
   branch, not that it biases it.
2. Why does a fine Gustavson task need atomics on the SPA but a
   coarse one does not — and what is the topic 11 analogue (shared
   hash aggregation vs per-thread pre-aggregation)? Use
   `GB_AxB_saxpy3.c:24-27` and the two `Hf` widths at `:66` and
   `:68`.
3. The hash task's table is sized at `:83`. What happens on an
   underestimate — collision pile-up, degrade, or rebuild? Find the
   resize path in `GB_AxB_saxpy3*.c`, then explain why what you find
   is what it is, and compare SwissTable's resize story (topic 8).
4. dot3 vs saxpy3 crossover: for `C<L> = L*U'` triangle counting on
   an RMAT graph, estimate both costs (nnz(L) dots of average length
   d̄ versus Σ flops) using Step 7's arithmetic — which wins, and
   why does LAGraph still ship both
   (`LAGr_TriangleCount.c:43-47`)?
5. Run `gb_bench`: at what RMAT scale does our dense-SPA Gustavson
   lose to the HashMap version? Predict it first from Step 5's
   workspace table (coarse Gustavson = 16 bytes/row) and your
   machine's L2 size, then measure.

## Done when

Answer each before unfolding it.

- [ ] You can name all four formats and say which two the TOMS '19 paper does *not* describe.

  <details><summary>Answer</summary>

  full, bitmap, sparse, hypersparse. TOMS '19 §4.1 describes only
  the sparse pair — at version 2.3.3 the four formats it names are
  "standard CSC, standard CSR, and hypersparse versions of these
  two". Bitmap and full arrived later, so any claim about them must
  be sourced to the code or a later paper.

  A matrix's `sparsity_control` bitmask says which of the four it
  may become, and `GB_conform.c:150` switches on it — fifteen
  cases. Pinning a matrix to `GxB_HYPERSPARSE` sends it to
  `:157-160`, which converts unconditionally: the test never runs.

  </details>

- [ ] You can state both format switches with their hysteresis, and give the default constants.

  <details><summary>Answer</summary>

  Bitmap: sparse → bitmap when `nnz > b × m·n`
  (`GB_convert_sparse_to_bitmap_test.c:38`); bitmap → sparse when
  `nnz <= (b/2) × m·n` (`GB_convert_bitmap_to_sparse_test.c:44`).
  The library's own summary is at `:13-16`: "A matrix whose density
  is between b/2 and b remains in its current state."

  Hyper: sparse → hyper when `n > 1 && k <= n × h`
  (`GB_convert_sparse_to_hyper_test.c:33`); hyper → sparse when
  `n <= 1 || k > n × h × 2`
  (`GB_convert_hyper_to_sparse_test.c:33`), where k is the number
  of non-empty vectors. So h versus 2h.

  Constants: `h` = `GB_HYPER_SWITCH_DEFAULT` = 0.0625 = 1/16
  (`GB_defaults.h:20`), matching TOMS '19 §4.2.1's "hypersparse
  format if n̄ < n/16". `b` is a table indexed by
  `min(vlen, vdim)` (`GB_Global.c:486-497`) running 0.04 → 0.40;
  for any dimension above 64 it is **0.40**, and it does not depend
  on the operation.

  </details>

- [ ] You can say whether a graph adjacency matrix ever becomes bitmap, with the arithmetic.

  <details><summary>Answer</summary>

  No. At scale 18 (`notes.md:13`) the matrix is 262,144 × 262,144
  with 2.0e6 entries, so its density is
  2.0e6 / 6.87e10 = 0.0029%. The threshold for a matrix with
  min dimension > 64 is b = 0.40 = 40% (`GB_Global.c:189`), which
  is 13,700× away. It would take 2.75e10 entries — 27.5 GB of
  presence flags alone — to trigger.

  This is why LAGraph sets `LG_SET_FORMAT_HINT(q, LG_BITMAP)` by
  hand at the BFS template's `:312` instead of letting the
  heuristic decide: bitmap is for the *vector*, and the heuristic
  would never choose it for the matrix.

  </details>

- [ ] You can explain why the flopcount pre-pass exists, and what its output sizes.

  <details><summary>Answer</summary>

  Because the output's size is not knowable without it, and every
  downstream decision needs a number. The pre-pass walks patterns
  only — `GB_AxB_saxpy3_flopcount.c:50-57`'s
  `Bflops(j) += nnz(A(:,k))` — at O(nnz(B)+n) when A and M are not
  hypersparse, or O(nnz(B)·log h) when they are (`:44-48`). It also
  applies the mask while counting (`:53`).

  Its two outputs then size: the thread count
  (`GB_nthreads(total_flops, chunk, …)` at
  `GB_AxB_saxpy3_slice_balanced.c:418`), the task slicing
  (`target_task_size = total_flops / ntasks_initial` at `:456`),
  and each task's hash table (`GB_hash_table_size(flmax, …)` at
  `:56-99`). Same two-phase shape as cudf's size/retrieve
  (topic 18).

  </details>

- [ ] You can compute how many threads a multiply gets, on this topic's own SpGEMM flop counts.

  <details><summary>Answer</summary>

  `GB_nthreads.h:29-31`:
  `clamp(floor(work/chunk), 1, nthreads_max)`, with `chunk` = 65,536
  (`GB_defaults.h:24`). From `notes.md:24-26`, on 8 cores:

  scale 10, 298K flops → floor(4.5) = **4 threads** — half the
  machine idle. Scale 12, 2.27M → 34, clamped to 8. Scale 14,
  17.1M → 260, clamped to 8.

  The comment at `:17-18` gives the rule in words: "If work <
  2*chunk, then only one thread is used." So below 131,072 flops a
  multiply is single-threaded regardless of core count. Check this
  before blaming the scheduler for poor scaling.

  </details>

- [ ] You can describe the four task kinds, price their workspace, and say which one SpMV is forced into.

  <details><summary>Answer</summary>

  `GB_AxB_saxpy3.c:22-38`: coarse (owns whole vectors of B, private
  workspace) × fine (a team splits one vector, shares workspace,
  sums via atomics), each × Gustavson or hash — four kinds, further
  × 3 mask variants = twelve.

  Workspace, from `:66-70`, for an f64 output at m = 2²⁰:
  fine Gustavson `int8_t Hf[m] + ctype Hx[m]` = 9 bytes/row =
  9.4 MB; coarse Gustavson `uint64_t Hf[m] + ctype Hx[m]` =
  16 bytes/row = 16.8 MB; a hash task with s = 16,384 is 262 KB
  (fine) or 393 KB (coarse, because `:70` adds `Hi[s]`). Eight
  coarse Gustavson tasks want 134 MB and will not fit any cache;
  eight coarse hash tasks want 3.1 MB and will.

  SpMV is forced into fine tasks. `:44-47`: "If B consists of a
  single vector… then the only way to get parallelism is via fine
  tasks." A matrix-vector product therefore pays atomics on every
  scatter, which is the structural reason BFS scales worse than
  SpGEMM.

  </details>

- [ ] You can state the shipped Gustavson-vs-hash rule and say why the header comment disagrees.

  <details><summary>Answer</summary>

  Shipped, at `GB_AxB_saxpy3_slice_balanced.c:56-99`: first, if
  `flmax >= cvlen/2`, Gustavson immediately (`:65`) — undocumented
  in the header. Otherwise size the table as
  `2 << (floor_log2(flmax) + 1)` (`:83`) and take Gustavson if
  `hash_size >= cvlen/12` (`:94`).

  `GB_AxB_saxpy3.c:57-58` still says the threshold is "m/16". It is
  stale. At m = 2²⁰ the shipped `cvlen/12` rule puts the crossover
  at flmax = 32,768 (= m/32, 3.1% of m); the documented m/16 rule
  would put it at flmax = 16,384. The code is twice as willing to
  hash as its own comment claims.

  The corollary at `:83`: `hash_size` is always at least 2× `flmax`,
  and `flmax` is an exact bound on the vector's distinct outputs, so
  the table cannot overflow. There is no resize path because there
  is nothing to resize for — SuiteSparse buys that with the extra
  pattern pass of Step 3, where SwissTable (topic 8) buys
  amortisation with a growth policy.

  </details>

- [ ] You can explain dot3's mask-as-outer-loop, cite the line that proves it, and say why the BFS pull step is *not* dot3.

  <details><summary>Answer</summary>

  dot3 computes `C<M> = A'*B` with one sparse dot product per mask
  entry. The proof is allocation, not documentation:
  `GB_AxB_dot3.c:126` computes `mnz = GB_nnz(M)` and `:171` sets
  `cnz = mnz`, so C is sized to the mask exactly; `:244` sizes the
  thread count from `cnz` too. Contrast saxpy3, where a mask prunes
  writes but the flops still happen.

  Pull is not dot3 because `GB_AxB_dot3.c:10-11` requires the mask
  to be "present, and not complemented", and `GB_mxm.h:241` encodes
  that as `M != NULL && !Mask_comp && (M sparse or hypersparse)`.
  LAGraph's pull passes `GrB_DESC_RSC`
  (`…SSGrB_template.c:313`), and `GrB_DESC_RSC` includes
  `GrB_COMP` (`Include/GraphBLAS.h:666`), so `Mask_comp` is true
  and the predicate fails.

  What pull actually reaches is **dot2**, because
  `LG_SET_FORMAT_HINT(q, LG_BITMAP)` on the line before (`:312`)
  makes `GB_AxB_dot2_control.c:26-30` return true on its first
  test. Davis's CSC '20 §3.1 says the same in prose: the library
  "selects the masked-dot-product method for… the pull phase of the
  push/pull BFS".

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the RMAT scale at which the dense SPA stops fitting.

  <details><summary>Answer</summary>

  Predict before you measure. A coarse Gustavson task's workspace is
  16 bytes per row of C (`GB_AxB_saxpy3.c:68`, `uint64_t Hf[m]` +
  `double Hx[m]`). On a machine with an L2 of size S, the SPA stops
  fitting at m ≈ S/16 per thread — and with T threads each holding
  its own, at m ≈ S/(16·T).

  On the M3 Pro of `notes.md:3`, with a shared L2 around 16 MB and
  8 threads, that is m ≈ 16 MB / (16 × 8) = 131,072 rows — RMAT
  scale 17. Above that, every SPA scatter is a DRAM round trip and
  the hash version, whose table is sized by flops rather than by m,
  should overtake. `notes.md:50-51`'s two prediction rows are asking
  for exactly this number at scales 14 and 20, which bracket it.

  </details>

## References

**Papers**

- Davis — "Algorithm 1000: SuiteSparse:GraphBLAS", ACM TOMS 45(4),
  2019. §4.1 is the data structure (two sparse formats, zombies,
  pending tuples); §4.2.1 is the multiply, including the "n̄ < n/16"
  hyper rule this chapter checks against `GB_defaults.h:20`. Cited
  here from the author's accepted manuscript, which is titled
  "Algorithm 9xx". Walked in
  [reading-davis-toms19.md](reading-davis-toms19.md).
- Davis — "Parallel GraphBLAS with OpenMP", CSC '20. §3.1 names the
  engine chosen for each algorithm, including the pull BFS. The
  parallelism this chapter reads does not exist in the TOMS '19
  paper's version of the library.
- Gustavson, F. G. — "Two Fast Algorithms for Sparse Matrices:
  Multiplication and Permuted Transposition", ACM TOMS 4(3), 1978,
  250-269,
  [doi:10.1145/355791.355796](https://doi.org/10.1145/355791.355796)
  — reference [1] in `GB_AxB_saxpy3.c:78-80`. Read in
  [reading-gustavson-spgemm.md](reading-gustavson-spgemm.md).
- Nagasaka, Matsuoka, Azad, Buluç — "High-Performance Sparse
  Matrix-Matrix Products on Intel KNL and Multicore Architectures",
  ICPP '18, Article 34,
  [doi:10.1145/3229710.3229720](https://doi.org/10.1145/3229710.3229720)
  — reference [2] in `GB_AxB_saxpy3.c:82-86`; the hash method
  saxpy3 implements.

**Code**

- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  at `1fd5475`. Read `Source/mxm/GB_AxB_saxpy3.c:22-86` first (the
  scheduler spec), with
  `Source/mxm/GB_AxB_saxpy3_slice_balanced.c:56-99` open beside it
  because the spec's selection rule is out of date. The full anchor
  table is above.
- [LAGraph](https://github.com/GraphBLAS/LAGraph) at `e2539e2` —
  `LG_BreadthFirstSearch_SSGrB_template.c:312-313` and
  `LAGr_TriangleCount.c:43-47` are the two callers this chapter
  traces through the dispatcher.

**Measured, in this repo**

- `topics/20-graphblas/notes.md:13-14`, `:22-26` — the SpMV and
  SpGEMM ladders Steps 4, 5 and 6 do arithmetic on.
- `topics/24-graph-algorithms/notes.md:5-7` — RMAT scale 16: max
  degree 9,751 against a mean of 27.8. The skew that fine tasks
  exist for.
