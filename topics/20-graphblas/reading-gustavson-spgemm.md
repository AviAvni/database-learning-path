# Gustavson SpGEMM: one output row at a time

Every modern sparse-times-sparse multiply — saxpy3, cuSPARSE, our
M20 stub — is still Gustavson's 1978 row-wise algorithm with a
different accumulator. This chapter builds the problem from zero —
why the dense loops die, what "work-optimal" means for a sparse
multiply, what the SPA is — then uses Buluç & Gilbert's survey to
map the whole design space onto one question: what data structure
is the SPA?

A note on sourcing before anything else. Gustavson's 1978 paper is
behind the ACM paywall and could not be fetched for this repo, so
**nothing below is attributed to it directly**. The complexity
statement comes from Buluç & Gilbert 2012 §3, which restates it;
the workspace and masking behaviour come from Davis's TOMS '19
§4.2.1; the shipped code comes from SuiteSparse:GraphBLAS at
`1fd5475`. Where those three disagree, the disagreement is the
interesting part and is called out.

## The problem in one sentence

Multiply two sparse matrices when the output's size is unknown
until you compute it — for the 10M-id-space matrix this topic
measures (`notes.md:41-42`), the naive one-dot-product-per-output-
cell view is 100 *trillion* candidate cells, while the
multiplications that actually exist number in the millions.

## The concepts, step by step

### Step 1 — SpGEMM, and why the dense loops die

> **In:** two sparse matrices A and B, and the textbook definition
> of matrix multiply.
> **Out:** the definition of *flops* for a sparse multiply, and the
> ratio between it and the dense candidate count.

**SpGEMM** (sparse general matrix-matrix multiply, C = A*B where
both inputs are sparse) is dense matmul's three nested loops with
two of them killed by sparsity. The **inner-product view** — for
every output cell, C(i,j) = A(i,:)·B(:,j), a dot product of a row
of A with a column of B — is how the math is written, but computed
literally it does an intersection test per *output cell*: n²
candidate cells, and for a graph matrix nearly all of those dot
products intersect to nothing. You would spend almost all your time
proving zeros are zero.

The number to hold is **flops**. Davis defines it precisely for a
semiring (TOMS '19 §4.2.1): "f is the number of 'multiply-adds'
computed (in the semiring)". Concretely, for C = A*B in the
column-wise form, it is Σ over (k,j) ∈ B of nnz(A(:,k)) — one
multiply-add per pair of entries that both exist. Note what it is
*not*: it is not nnz(C). Several flops can land in the same output
cell and be summed.

This repo measures both, so the gap is not theoretical:

```
 inputs: RMAT scale 14, edge factor 8   (notes.md:26)
         n = 16,384, nnz(A) = 120K, flops = 17.1M, nnz(C) = 8.9M

 dense candidate cells      = n²        = 16,384² = 2.68e8
 flops that actually exist  =           = 1.71e7
 ratio                                  = 15.7×  wasted, at scale 14

 now scale 20 (notes.md:14): n = 1.05e6
 dense candidate cells      = 1.10e12
 flops (extrapolating the ×8 growth per +2 scale) ≈ 2.7e8
 ratio                                  ≈ 4,100× wasted

 the waste grows linearly in n. That is the whole reason the
 inner-product view is unusable, and it gets worse with scale.

 flops / nnz(C) = 1.71e7 / 8.9e6 = 1.92     (notes.md:28-31)
   ⇒ the average output cell receives ~2 contributions. The
   accumulator is doing real work on about half its updates and
   pure insert on the other half.
```

Why it matters: an SpGEMM algorithm's quality is how close its
total work gets to flops. Everything below is measured against that
one number.

### Step 2 — the row-wise (or column-wise) formulation

> **In:** the flop count from Step 1.
> **Out:** the loop that achieves it, and the two names the
> literature gives the same loop.

Gustavson's move: compute C one *vector* at a time, driven by an
input's pattern instead of by output coordinates. Buluç & Gilbert
§3, Algorithm 1, give it column-wise:

```
 Algorithm 1  Column-wise formulation of serial matrix multiplication
 1: procedure Columnwise-SpGEMM(A, B, C)
 2:   for j ← 1 to n do
 3:     for k where B(k,j) ≠ 0 do
 4:       C(:,j) ← C(:,j) + A(:,k) · B(k,j)
```

That is a *saxpy* — scalar times a vector, added into an
accumulator — which is where SuiteSparse's engine gets its name.
The row-wise form in this chapter's title is the same algorithm
read through a transpose: for row i, each entry A(i,k) contributes
A(i,k) × (row k of B). SuiteSparse says so outright:

```c
// GB_AxB_saxpy3.c — the duality, in one line, 20
    20  // all matrices are in CSC format, but the algorithm is CSR/CSC agnostic.
```

Keep both names, and always say which orientation you mean, because
the *storage* has to match the loop: the column-wise form wants
CSC for A and B, the row-wise form wants CSR. SuiteSparse's default
is CSR (`GB_Global.c:203` sets `.is_csc = false`), so its saxpy3
comment's "vectors of B" are rows.

The complexity, from Buluç & Gilbert §3:

> "That algorithm, shown in Figure 3.1, runs in **O(flops + nnz + n)**
> time, which is **optimal for flops ≥ max{nnz, n}**. It uses the
> popular compressed sparse column (CSC) format for representing
> its sparse matrices."

Three terms, and each earns its place: `flops` is the useful work,
`nnz` is reading the inputs once, and `n` is *constructing the
output's pointer array* `C.p` of length n+1 — you pay for a slot
per column whether or not that column has anything in it. Davis
makes the same point in TOMS '19 §4.2.1: "Constructing C takes
Ω(n) time and space if it is stored in standard compressed
sparse-column form with a pointer array C.p of size n + 1."

Why it matters: the optimality condition `flops ≥ max{nnz, n}` is
not decoration. Step 7 shows this repo's own measured case where it
fails by two orders of magnitude.

### Step 3 — the SPA: the accumulator that makes scattering O(1)

> **In:** the loop of Step 2, which produces contributions to the
> same output cell from different k, in no order.
> **Out:** the SPA, its three operations, its cost in bytes, and
> the trick that makes clearing it free.

Gustavson's **SPA** (sparse accumulator) is a dense array of size m
— one slot per possible output index — plus a record of which slots
are occupied. Buluç & Gilbert's Figure 3.1 caption:

> "Columns of A are accumulated as specified by the non-zero
> entries in a column of B using a **sparse accumulator or SPA**.
> The contents of the SPA are stored into a column of C once all
> required columns are accumulated."

Three operations:

- **scatter**: `SPA[j] += v` is one array write — O(1), no probing;
- **mark**: a parallel array records first touches and appends j to
  the occupied list;
- **gather**: walk the occupied list to emit the finished vector,
  then reset only those slots.

The clever part is the reset. Davis describes SuiteSparse's version
in TOMS '19 §4.2.1, and it is the design our own stub is asked to
copy (`notes.md:59` — "stamp-marked SPA"):

> "The other is an initialized integer array, `mark`… When
> initialized, `mark[i]<flag` holds for all i; to set `mark[i]` as
> true, `mark[i]=flag` is done, and clearing the entire `mark` array
> simply requires `flag` to be incremented. **The entire space is
> cleared in constant time.**"

A generation counter, not a memset. Without it, each output vector
would cost O(m) to clear and the whole `O(n + f)` claim would
collapse to `O(nm)`.

Price the array, using this topic's own configuration
(`notes.md:50-51` sizes our stub's SPA at 12 bytes per slot — an
f64 value plus a 4-byte stamp):

```
 inputs: SPA slot = 12 bytes (f64 value + u32 stamp), notes.md:50
         M3 Pro, L2 ≈ 16 MB shared     (notes.md:3, topic 13)

 scale 14: m = 16,384   → 16,384 × 12 B = 196,608 B = 192 KB   fits L2 easily
 scale 16: m = 65,536   → 786 KB                                fits
 scale 18: m = 262,144  → 3.1 MB                                fits, but 8 threads want 25 MB
 scale 20: m = 1,048,576→ 12.6 MB                               one thread nearly fills L2

 so single-threaded, the SPA survives to scale 20; at 8 threads it
 falls out of cache somewhere around scale 17-18, which is the
 prediction notes.md:50-51 asks you to make.

 the crossover argument, in one line: the SPA costs 12·m bytes
 regardless of how many entries the vector has, while a hash table
 costs ~16 bytes × 2·flmax. They cost the same when
   12·m = 32·flmax  ⇒  flmax = 0.375·m
 — which is why every implementation's real threshold (SuiteSparse
 uses m/12 at GB_AxB_saxpy3_slice_balanced.c:94) sits far BELOW
 that break-even: cache residency, not byte count, is the criterion.
```

Why it matters: the SPA is dense-workspace thinking — pay memory to
make per-element search cost zero. Every argument about when to
abandon it is an argument about cache, not about asymptotics.

### Step 4 — the design space is "what data structure is the SPA"

> **In:** the SPA of Step 3.
> **Out:** the three accumulators the literature and the code
> actually ship, with the shipped selection rule.

Everything since 1978 keeps the vector-at-a-time loop and swaps the
accumulator:

```
 SPA = dense array + occupied list   (Gustavson '78)
       O(1) scatter, O(m) alloc, gather via occupied list
 SPA = hash table                    (saxpy3 hash task)
       O(1)-ish scatter, O(flops) alloc — wins for huge m
 SPA = heap / sorted-list merge      (merge k sorted vectors of B)
       output comes out SORTED — no gather/sort pass
```

All three are in SuiteSparse's history, and Davis names them in
TOMS '19 §4.2.1 as the library's three methods at version 2.3.3:
"(1) a variant of Gustavson's algorithm, (2) a heap-based method,
and (3) a dot-product formulation." The selection rule in that
paper is a sentence: "**If m is large compared with |A| + |B|,
Gustavson's method is not used, and the heap-based method is used
instead.**"

The modern library's rule is different in every respect — the heap
method is gone, replaced by a hash, and the threshold is a number:

```c
// GB_AxB_saxpy3_slice_balanced.c — the shipped rule, 83-95 (elided)
    82          // hash_size = 2 * (smallest power of 2 >= flmax)
    83          hash_size = ((uint64_t) 2) << (GB_FLOOR_LOG2 (flmax) + 1) ;
    ...
    92              // default: auto selection:
    93              // use Gustavson's method if hash_size is too big
    94              use_Gustavson = (hash_size >= cvlen/12) ;
```

Note the direction: *bigger* hash table means *use Gustavson*. A
vector with many flops needs a big table; once that table
approaches the size of the dense SPA, the dense SPA's O(1) scatter
is strictly better. The crossover is worked exactly in
[reading-suitesparse-internals.md](reading-suitesparse-internals.md)
Step 6 — at m = 2²⁰ it lands at flmax = 32,768.

Why it matters: "which accumulator" is not a research question, it
is a runtime branch on one number, and both the number and the
comparison direction are checkable in fifteen lines of source.

### Step 5 — the unknown-output-size problem: symbolic then numeric

> **In:** the loop and an accumulator.
> **Out:** why the pattern gets walked twice, and what the second
> walk buys.

nnz(C) is unknown before you compute C, so how big do you allocate
the output arrays? The answer is two phases: a **symbolic phase**
runs the same loop on patterns only (no values, no arithmetic) to
compute each output vector's nnz and allocate exactly, then a
**numeric phase** fills the values. Davis, TOMS '19 §4.2.1:

> "In the first method, when no mask is present, the work is split
> into a symbolic analysis phase that finds the pattern of C and a
> numerical phase that computes its values… **both phases take
> only O(n + f) time**, assuming all matrices are in CSC format, and
> assuming the O(m) workspace is already allocated and
> initialized."

Read the assumption clause twice. The `O(n + f)` bound — better
than Buluç & Gilbert's `O(flops + nnz + n)` because it drops the
input-reading term into f — holds **only if the O(m) workspace is
already there**. Davis spends the next paragraph on why that matters
for BFS: "Assuming the workspace of size O(m) has already been
allocated and initialized, the time to compute this set is simply
O(f)… The O(m) work appears just once in the entire breadth-first
search algorithm."

Every system in this curriculum that meets sparse output
rediscovers the two-phase shape: saxpy3's flopcount pre-pass
(`GB_AxB_saxpy3_flopcount.c:44-48`, `O(nnz(B)+n)`), cudf's
size/retrieve (topic 18), Gunrock's degree scan. The alternative is
guess-and-grow (topic 17's simdjson over-allocate answer) — cheaper
when vectors are small and uniform, disastrous under skew. Our stub
does symbolic+numeric; the HashMap reference does guess-free
accumulation and pays for it in allocator traffic, which is
measurable:

```
 inputs: notes.md:22-26, HashMap reference (guess-and-grow, per-row alloc)

 scale 10:    298K flops /   3.9 ms = 76 Mflop/s = 13.1 ns/flop
 scale 12:   2.27M flops /  33.0 ms = 69 Mflop/s = 14.5 ns/flop
 scale 14:  17.10M flops / 279.4 ms = 61 Mflop/s = 16.3 ns/flop

 a multiply-add is ~1 ns of arithmetic at best. So 13-16 ns/flop
 means ~93% of the time is NOT arithmetic — it is hashing, probing,
 per-row allocation and the final sort.

 and the per-flop cost DEGRADES 24% from scale 10 to 14 even though
 the flop count grew 57×: the accumulator is falling out of cache,
 exactly the effect Step 3's byte arithmetic predicts.
```

Why it matters: the two-phase design is not fastidiousness. At
16 ns/flop the arithmetic is invisible; what you are optimizing is
allocation and memory traffic, and knowing the size up front is how
you remove both.

### Step 6 — masking: where the mask can and cannot save work

> **In:** the two formulations (Step 2's saxpy, Step 1's discarded
> inner product).
> **Out:** which one a mask actually prunes, in the source's own
> words.

In row/column-wise saxpy, the flops happen *before* the mask can
reject them. Davis is unusually blunt about this, TOMS '19 §4.2.1:

> "If the mask is present (and not complemented), only the subset
> of entries appearing in the mask are computed. This greatly
> reduces the time and memory usage. In this method, the symbolic
> analysis is skipped. A matrix T = AB is computed whose pattern is
> assumed to be a subset of the mask matrix M. **Entries in AB
> outside the mask need not be computed, and are discarded if they
> are computed.**"

"Discarded if they are computed" is the honest half of the
sentence. The mask does buy something real — the symbolic phase is
skipped entirely, and `GB_AxB_saxpy3_flopcount.c:53` skips whole
vectors whose mask vector is empty — but within a vector that the
mask keeps, the multiply-adds still run and the losers are thrown
away.

In the inner-product formulation driven *by the mask*, masked-out
cells cost nothing at all, because the mask is the loop bound. The
proof is allocation: `GB_AxB_dot3.c:126` computes `mnz = GB_nnz(M)`
and `:171` sets `cnz = mnz`.

Davis's triangle-counting example, §4.2.1, is the canonical case:

> "if L is the strictly lower triangular part of an unweighted
> graph A, then C⟨L⟩ = L² finds the number of triangles in the
> graph… Not all of L² is computed or stored, but only the entries
> corresponding to entries in the mask, L. This greatly reduces the
> time and memory complexity of the masked matrix multiply, as
> compared with computing all of L² first and then applying the
> mask, as would be done in the MATLAB expression `C=(L^2).*L`."

Which is why LAGraph's triangle counting ships six formulations
(`LAGr_TriangleCount.c:31-37`) and its own performance note
(`:43-47`) refuses to name one winner — it says the dot-based
Sandia_LUT is usually fastest on the largest graphs *except* on
GAP-urand, where the saxpy-based LL wins. Question 5.

Why it matters: "masks are free performance" is a half-truth whose
other half is a discarded multiply-add, and knowing which half you
are getting requires knowing which engine ran.

### Step 7 — where O(flops + nnz + n) fails: hypersparsity

> **In:** the complexity of Step 2 and its optimality condition.
> **Out:** the measured case in this repo where the `n` term wins,
> and the data structure that removes it.

Buluç & Gilbert's condition was `optimal for flops ≥ max{nnz, n}`.
Check it against this topic's own measurements:

```
 inputs: notes.md:22-26 (RMAT ladder) and notes.md:41-42 (hypersparse)

 RMAT scale 14:  flops = 1.71e7, nnz = 1.20e5, n = 1.64e4
   max{nnz, n} = 1.20e5
   flops / max  = 143×      → comfortably optimal, the n term is 0.1%

 RMAT scale 10:  flops = 2.98e5, nnz = 6.7e3, n = 1.02e3
   flops / max  = 44×       → still optimal

 hypersparse case: 10M-node id space, 100K edges
   n = 1.0e7, nnz = 1.0e5, flops for A² ≈ nnz × mean-degree ≈ 1e6
   max{nnz, n} = 1.0e7
   flops / max  = 0.1×      → the condition FAILS by 10×

   Gustavson would spend O(n) = 1.0e7 just building C.p, against
   1e6 flops of real work: 91% of the runtime is allocating and
   walking pointer slots for columns that are empty.
```

That is not a hypothetical — it is the measurement in `FINDINGS.md`
row 20 seen from the algorithm's side. The same 10M-id-space graph
costs **80.4 MB of CSR index versus 1.59 MB hypersparse (50×)**,
and a full sweep takes **11,312 µs versus 66 µs (171×)**
(`notes.md:41-42`). The 171× is the `n` term of
`O(flops + nnz + n)` being deleted.

Buluç & Gilbert say exactly this, §3.1:

> "any algorithm whose complexity depends on matrix dimension, such
> as Gustavson's serial SpGEMM algorithm, is **asymptotically too
> wasteful** to be used as a computational kernel for multiplying
> the hypersparse submatrices. Our HyperSparseGEMM, on the other
> hand, operates on the strictly O(nnz) doubly compressed sparse
> column (DCSC) data structure, and its time complexity does not
> depend on the matrix dimension."

Their replacement is an outer-product formulation with complexity
`O(nzc(A) + nzr(B) + flops·lg nᵢ)` and memory
`O(nnz(A) + nnz(B) + nnz(C))` — note the `lg nᵢ` factor, which
they attribute to "the priority queue that is used to merge nᵢ
outer products on the fly". You trade the dimension term for a log
factor on the flops. **DCSC** is CSC with the repetitions in the
column-pointer array removed: "Only columns that have at least one
nonzero are represented, together with their column indices" (§3.2)
— which is the same idea as SuiteSparse's hypersparse `A.h`.

Davis's version of the same argument, TOMS '19 §4.2.1: "A
hypersparse format need only operate on the non-empty columns of B
and C, however, so the time complexity drops to O(n̄_B + f) where
n̄_B < n is the number of non-empty columns of B." And the trigger:
"SuiteSparse:GraphBLAS stores its matrices in hypersparse format if
n̄ < n/16" — which matches `GB_defaults.h:20`'s 0.0625 exactly.

Why it matters: this is the one place where an asymptotic term that
looks like bookkeeping turns into the entire runtime, and this
repo has the measurement.

### Step 8 — skew: the cost intuition to carry

> **In:** the row-wise loop, which looks embarrassingly parallel.
> **Out:** why it is not, with this repo's measured degree
> distribution.

For RMAT/power-law A², flops concentrate in hub vectors: vector i's
cost is Σ of the degrees of i's neighbours — a degree-squared
weighting. Measured, on the graph generator this curriculum uses:

```
 inputs: RMAT scale 16, topics/24-graph-algorithms/notes.md:5-7
         n = 65,536, m = 1,819,338, max degree = 9,751
         uniform graph, same n and m: max degree = 59

 mean degree      = 1,819,338 / 65,536      = 27.8
 max / mean       = 9,751 / 27.8            = 351×
 uniform max/mean = 59 / 27.8               = 2.1×

 now the SpGEMM cost, which is degree-SQUARED-ish:
   a mean row's flops  ≈ 27.8 × 27.8        =    773
   the hub row's flops ≈ 9,751 × 27.8       = 271,078
   ratio                                    = 351×

 total flops at scale 16 are not in notes.md (the ladder stops at
 14), so extrapolate: the measured ladder grows 298K → 2.27M →
 17.1M, i.e. ×7.6 then ×7.5 per +2 scale, so scale 16 ≈ 1.28e8.

 static partition over 8 threads, 65,536 rows, 8,192 rows each:
   fair share = 1.28e8 / 8                  = 1.60e7 flops
   one hub row = 271,078 flops = 1.7% of a thread's ENTIRE fair
   share, in a single indivisible unit if you only split by row —
   and a hub's neighbours are hubs too, so the 8,192-row block
   containing it can easily carry a multiple of that.
```

A few rows are hundreds of times the median, so static row
partitioning dies — seven threads finish and one grinds a hub row —
which is why every real implementation has the fine-task path
(`GB_AxB_saxpy3.c:22-27`). Whatever accumulator you pick, the load
balancer must be designed for the tail, not the median.

Why it matters: the 351× is measured on the same generator our
benchmarks use, so the load-imbalance argument in this curriculum is
not borrowed from a paper about someone else's graph.

## How to read the paper (with the concepts in hand)

- **Gustavson '78** — the ACM version is paywalled; if you have
  institutional access, read it whole (it is short). The row-wise
  algorithm is Step 2, the SPA is Step 3, and the symbolic/numeric
  two-phase is Step 5 — it is also where the "permuted
  transposition" half of the title lives, since the same two-phase
  shape builds a transpose. If you cannot get it, read its two
  restatements instead: Buluç & Gilbert §3 (Algorithm 1, Figure
  3.1) and Davis TOMS '19 §4.2.1, both of which are open.
- **Buluç & Gilbert 2012** — read §3 first: it is two pages and
  contains the complexity claim, Algorithm 1, and the SPA figure.
  Then §3.1-3.2 for the hypersparse argument (Step 7) and DCSC.
  The distributed-memory experiments in §4 onward are skimmable;
  the axes are the payload. Map each system you have met — saxpy3's
  Gustavson task, saxpy3's hash task, our stub, the HashMap
  reference — onto a point in their space as you read.
- **Davis TOMS '19 §4.2.1** — the third statement of the same
  algorithm, and the only one that tells you what the *workspace*
  does. Read the four paragraphs from "In the first method" to
  "cleared in constant time"; they are the specification for our
  stamp-marked SPA stub. Remember its version caveat: everything in
  that paper is single-threaded.

### What transfers to M20

- `spgemm_spa` (`notes.md:59`) is Step 3 verbatim: dense array,
  stamp array, occupied list, generation counter. The stamp trick
  is what makes the reset O(nnz of this vector) instead of O(m).
- The symbolic/numeric split of Step 5 is what lets M20 allocate
  `C.i`/`C.x` exactly once. The HashMap reference deliberately does
  not, which is what the 13-16 ns/flop in Step 5's arithmetic is
  measuring.
- Step 7's arithmetic is the argument for M20's hypersparse index:
  the `n` term is the whole cost at 10M ids, and deleting it is the
  171× in `notes.md:42`.

## Questions for notes.md

1. Derive: why is Gustavson's total work exactly
   Σ_{(k,j)∈B} nnz(A(:,k)) (equivalently Σ_{(i,k)∈A} nnz(B(k,:))
   in the row-wise orientation), and why can no SpGEMM do fewer
   multiplications — each is a necessary term, *unless* the
   semiring short-circuits. ANY_PAIR reachability can stop early:
   find where, and say what property of the monoid licenses it.
2. The dense SPA costs 12 bytes per slot (`notes.md:50`) per
   thread. Compute the crossover vector density where hash beats
   SPA using topic 13's cache numbers — SPA touches nnz_out random
   cells of a 12·m array; hash touches nnz_out cells of a
   2×flops table that fits L2 — and compare your answer to
   SuiteSparse's shipped `cvlen/12`
   (`GB_AxB_saxpy3_slice_balanced.c:94`).
3. Symbolic+numeric does the pattern walk TWICE. When is
   guess-and-grow cheaper? Make the variance argument — flops per
   vector small and uniform — and connect it to cudf's
   retrieve-skip answer (topic 18).
4. Outer-product SpGEMM produces k rank-1 updates that must be
   merged — which topic 3 structure is that (LSM: sorted runs +
   merge), and why does it win out-of-core / distributed
   (sequential I/O, no random SPA)? Buluç & Gilbert §3.1's
   `flops·lg nᵢ` is the priority-queue cost of that merge; price it
   against the SPA's O(1) scatter at scale 14's flop count.
5. Masked saxpy discards; masked dot never computes. Show it on
   triangle counting `C<L>=L*L`: what does each formulation do per
   wedge, and reconcile that with LAGraph shipping both Sandia_LL
   (saxpy) and Sandia_LUT (dot), with `LAGr_TriangleCount.c:43-47`
   naming different winners on different graphs.

## Done when

Answer each before unfolding it.

- [ ] You can define flops for a sparse multiply, and say why it is not nnz(C).

  <details><summary>Answer</summary>

  Davis, TOMS '19 §4.2.1: "f is the number of 'multiply-adds'
  computed (in the semiring)" — one per pair of input entries that
  both exist, Σ over (k,j) ∈ B of nnz(A(:,k)).

  It is not nnz(C) because several flops can land in the same
  output cell and be summed. This topic measures the ratio:
  17.1M flops against 8.9M output entries at scale 14
  (`notes.md:26`), so flops/nnz(C) = 1.92. `notes.md:28-31` reads
  that as RMAT A² producing mostly-distinct pairs — the accumulator
  "rarely accumulates", which is the hash's worst case because
  almost every update is an insert rather than a merge.

  </details>

- [ ] You can state Gustavson's complexity with all three terms and its optimality condition, and say what each term pays for.

  <details><summary>Answer</summary>

  Buluç & Gilbert §3: "**O(flops + nnz + n)** time, which is
  **optimal for flops ≥ max{nnz, n}**."

  `flops` is the useful work; `nnz` is reading the inputs once; `n`
  is building the output's pointer array `C.p` of length n+1 — one
  slot per column whether or not the column is empty. Davis makes
  the same point independently: "Constructing C takes Ω(n) time and
  space if it is stored in standard compressed sparse-column form
  with a pointer array C.p of size n + 1" (TOMS '19 §4.2.1).

  Davis's tighter `O(n + f)` for both phases is not a contradiction
  — it assumes the O(m) workspace is already allocated and
  initialised, which folds the input-reading term into f. State the
  assumption whenever you quote it.

  </details>

- [ ] You can explain what the SPA does, why scattering is O(1), and how it is cleared.

  <details><summary>Answer</summary>

  A dense array of size m — one slot per possible output index —
  plus a marker array and an occupied list. Scatter is `SPA[j] += v`,
  a single array write with no probing. Gather walks the occupied
  list to emit the vector.

  Clearing is the interesting part. Davis, TOMS '19 §4.2.1: the
  marker array `mark` satisfies `mark[i] < flag` when clear; setting
  entry i is `mark[i] = flag`; and "clearing the entire mark array
  simply requires flag to be incremented", so "the entire space is
  cleared in constant time". Without the generation counter, each
  output vector would cost O(m) to reset and the O(n + f) bound
  would become O(nm).

  Buluç & Gilbert's Figure 3.1 caption is the other half:
  "The contents of the SPA are stored into a column of C once all
  required columns are accumulated."

  </details>

- [ ] You can state the design space in one sentence, and connect it to this topic's measured SpGEMM.

  <details><summary>Answer</summary>

  Keep the vector-at-a-time loop; change what the SPA is — dense
  array, hash table, or heap/merge. Davis's TOMS '19 §4.2.1 lists
  the library's three methods of that era as Gustavson, heap-based,
  and dot-product, with the rule "If m is large compared with
  |A| + |B|, Gustavson's method is not used, and the heap-based
  method is used instead." The modern code replaced the heap with a
  hash and the prose rule with a number:
  `use_Gustavson = (hash_size >= cvlen/12)`
  (`GB_AxB_saxpy3_slice_balanced.c:94`).

  Measured here (`notes.md:22-26`), the HashMap reference runs
  **279.4 ms on 17.1M flops at scale 14** — 61 Mflop/s, or
  16.3 ns/flop, degrading 24% from the 13.1 ns/flop it manages at
  scale 10. That degradation with no change of algorithm is the
  accumulator falling out of cache, which is the whole argument for
  having two accumulators.

  </details>

- [ ] You can explain the unknown-output-size problem and when symbolic-then-numeric beats guessing.

  <details><summary>Answer</summary>

  nnz(C) is unknowable before computing C, so either walk the
  patterns twice (symbolic sizes the allocation exactly, numeric
  fills it) or guess and grow. Symbolic+numeric wins when growth is
  expensive or vector sizes vary wildly — which is every power-law
  graph, per Step 8's 351× max/mean.

  Guess-and-grow wins when flops per vector are small and uniform,
  because the second pattern walk is pure overhead you cannot
  amortise. The measured price of guessing is in `notes.md:24-26`:
  13-16 ns/flop against roughly 1 ns of actual arithmetic, so about
  93% of the HashMap reference's time is hashing, probing,
  per-row allocation and sorting.

  </details>

- [ ] You can show, on an example, why masked saxpy cannot skip work but masked dot can.

  <details><summary>Answer</summary>

  Davis, TOMS '19 §4.2.1, on the masked saxpy: the symbolic phase is
  skipped and "a matrix T = AB is computed whose pattern is assumed
  to be a subset of the mask matrix M. Entries in AB outside the
  mask **need not be computed, and are discarded if they are
  computed**." The flops happen; the losers are thrown away. The
  only whole-vector saving is at
  `GB_AxB_saxpy3_flopcount.c:53`, which skips a column whose mask
  column is empty.

  In the dot formulation the mask is the loop bound, so masked-out
  cells cost literally nothing: `GB_AxB_dot3.c:126` takes
  `mnz = GB_nnz(M)` and `:171` sets `cnz = mnz`, sizing C to the
  mask exactly.

  Davis's own example is triangle counting: C⟨L⟩ = L² where "Not
  all of L² is computed or stored, but only the entries
  corresponding to entries in the mask, L", against MATLAB's
  `C=(L^2).*L`, which materialises L² first. LAGraph ships both
  formulations for the same reason and `LAGr_TriangleCount.c:43-47`
  names different winners on different graphs.

  </details>

- [ ] You can name the case where O(flops + nnz + n) is *not* optimal, and quote this repo's measurement of it.

  <details><summary>Answer</summary>

  When `flops < max{nnz, n}` — i.e. hypersparsity, where the matrix
  dimension dwarfs the entry count. This repo's case: a 10M-node id
  space with 100K edges (`notes.md:41-42`), where n = 1.0e7 against
  nnz = 1.0e5. The `n` term is 100× the entry count, so almost all
  the work is walking pointer slots for empty columns.

  Measured: **80.4 MB of CSR index versus 1.59 MB hypersparse
  (50×)**, and a full sweep of **11,312 µs versus 66 µs (171×)**.
  (`FINDINGS.md` row 20 quotes 175× for the sweep; `notes.md:42`
  quotes 171×. Cite one, and say which — this answer uses
  `notes.md`, the measured baseline for this topic.)

  Buluç & Gilbert §3.1 predict it: Gustavson's algorithm is
  "asymptotically too wasteful" for hypersparse blocks, and their
  DCSC-based HyperSparseGEMM runs in
  `O(nzc(A) + nzr(B) + flops·lg nᵢ)` — dimension-free, at the cost
  of a `lg nᵢ` factor from the merge priority queue. Davis's
  equivalent is `O(n̄_B + f)` with the trigger "hypersparse format if
  n̄ < n/16" (TOMS '19 §4.2.1), matching `GB_defaults.h:20`'s
  0.0625.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the dense-SPA memory cost per thread.

  <details><summary>Answer</summary>

  The number to have: 12 bytes per slot (`notes.md:50` — f64 value
  plus a 4-byte stamp), so 12·m bytes per thread, independent of how
  many entries any vector holds. At scale 14 that is 192 KB; at
  scale 20 it is 12.6 MB; at 8 threads scale 18 already wants
  25 MB against an L2 of about 16 MB.

  For SuiteSparse's own coarse Gustavson task the figure is 16
  bytes per row (`GB_AxB_saxpy3.c:68` — `uint64_t Hf[m]` plus
  `double Hx[m]`), and its fine variant is 9 (`:66` uses
  `int8_t Hf`). Do not mix the three numbers; say which
  implementation each belongs to.

  </details>

## References

**Papers**

- Gustavson, F. G. — "Two Fast Algorithms for Sparse Matrices:
  Multiplication and Permuted Transposition", ACM TOMS 4(3), 1978,
  250-269,
  [doi:10.1145/355791.355796](https://doi.org/10.1145/355791.355796).
  The row-wise algorithm, the SPA, and the symbolic/numeric
  two-phase. **Paywalled**; nothing in this chapter is quoted from
  it. It is reference [1] in `GB_AxB_saxpy3.c:78-80` and reference
  [7] in Davis's CSC '20 paper.
- Buluç, A. & Gilbert, J. R. — "Parallel Sparse Matrix-Matrix
  Multiplication and Indexing: Implementation and Experiments",
  SIAM J. Sci. Comput. 34(4), 2012,
  [arXiv:1109.3739](https://arxiv.org/abs/1109.3739). §3 is the
  complexity claim, Algorithm 1 (column-wise) and Figure 3.1 (the
  SPA); §3.1 is the hypersparse argument and HyperSparseGEMM's
  `O(nzc(A) + nzr(B) + flops·lg nᵢ)`; §3.2 is DCSC.
- Davis, T. A. — "Algorithm 1000: SuiteSparse:GraphBLAS", ACM TOMS
  45(4), 2019. §4.2.1 is the third statement of Gustavson's
  algorithm, and the only one that specifies the workspace, the
  generation-counter clear, and the masked variant's
  discard-if-computed behaviour. Cited from the author's accepted
  manuscript (titled "Algorithm 9xx", describing version 2.3.3, and
  **single-threaded**). Walked in
  [reading-davis-toms19.md](reading-davis-toms19.md).
- Nagasaka, Y., Matsuoka, S., Azad, A., Buluç, A. —
  "High-Performance Sparse Matrix-Matrix Products on Intel KNL and
  Multicore Architectures", ICPP '18, Article 34,
  [doi:10.1145/3229710.3229720](https://doi.org/10.1145/3229710.3229720)
  — the hash accumulator that replaced the heap; reference [2] in
  `GB_AxB_saxpy3.c:82-86`.

**Code**

- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  at `1fd5475` — `Source/mxm/GB_AxB_saxpy3.c:20` (the CSR/CSC
  duality), `:62-70` (workspace),
  `Source/mxm/GB_AxB_saxpy3_slice_balanced.c:56-99` (the shipped
  accumulator choice), `Source/mxm/GB_AxB_saxpy3_flopcount.c:44-69`
  (the symbolic phase), `Source/mxm/GB_AxB_dot3.c:126`, `:171`
  (C sized to the mask), `Source/include/GB_defaults.h:20`
  (`hyper_switch` = 1/16). Walked in
  [reading-suitesparse-internals.md](reading-suitesparse-internals.md).
- [LAGraph](https://github.com/GraphBLAS/LAGraph) at `e2539e2` —
  `src/algorithm/LAGr_TriangleCount.c:31-37` (the six
  formulations), `:43-47` (which wins on which graph).

**Measured, in this repo**

- `topics/20-graphblas/notes.md:22-31` — the SpGEMM ladder:
  17.1M flops in 279.4 ms at scale 14, 61 Mflop/s, flops/nnz(C)
  ≈ 2. Steps 1, 5 and the crossover arithmetic all run on these.
- `topics/20-graphblas/notes.md:41-42` — the hypersparse headline:
  80.4 MB → 1.59 MB, 11,312 µs → 66 µs. Step 7's failure of the
  optimality condition, measured.
- `topics/20-graphblas/notes.md:50-51` — the two SPA prediction
  rows, sized at 12 bytes per slot. Fill them before you implement.
- `topics/24-graph-algorithms/notes.md:5-7` — RMAT scale 16 max
  degree 9,751 against a mean of 27.8. Step 8's 351×.
