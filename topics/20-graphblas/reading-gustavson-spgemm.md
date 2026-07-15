# Gustavson SpGEMM: one output row at a time

Every modern sparse-times-sparse multiply — saxpy3, cuSPARSE, our
M20 stub — is still Gustavson's 1978 row-wise algorithm with a
different accumulator. This chapter builds the problem from zero —
why the dense loops die, what "work-optimal" means for a sparse
multiply, what the SPA is — then uses Buluç & Gilbert's survey to
map the whole design space onto one question: what data structure
is the SPA?

## The problem in one sentence

Multiply two sparse matrices when the output's size is unknown
until you compute it — for a 10M×10M matrix, the naive
one-dot-product-per-output-cell view is 100 *trillion* dot
products, while the multiplications that actually exist may number
a few hundred million.

## The concepts, step by step

### Step 1 — SpGEMM, and why the dense loops die

**SpGEMM** (sparse general matrix-matrix multiply, C = A*B where
both inputs are sparse) is dense matmul's three nested loops with
two of them killed by sparsity. The **inner-product view** — for
every output cell, C(i,j) = A(i,:)·B(:,j), a dot product of a row
of A with a column of B — is how the math is written, but computed
literally it does an intersection test per *output cell*: n²
candidate cells, and for a graph matrix nearly all of those dot
products intersect to nothing. You'd spend almost all your time
proving zeros are zero.

The number to hold: the useful work is the **flops** — the scalar
multiplications between pairs of entries that both exist,
Σ over (i,k) ∈ A of nnz(B(k,:)) (nnz = number of stored entries).
For A² on a 100M-edge graph that's typically 10⁸–10⁹ flops —
against 10¹⁴ candidate output cells. An algorithm's quality is how
close its total work gets to the flops.

### Step 2 — the row-wise formulation: one output row at a time

Gustavson's move: compute C one *row* at a time, driven by A's
pattern instead of by output coordinates. For row i, each entry
A(i,k) contributes A(i,k) × (row k of B) into the output row —
scaled row additions instead of dot products:

```
 for i in rows(A):                      # one output row at a time
   for k where A(i,k) ≠ 0:              # A's row pattern
     for j where B(k,j) ≠ 0:            # B's row k
       SPA[j] += A(i,k) * B(k,j)        # scatter-accumulate
   C(i,:) = gather nonzeros of SPA      # then reset SPA
```

Work = flops = Σᵢₖ nnz(B(k,:)) over A's entries — *optimal*: every
multiplication performed is a term that exists in the answer, and
none can be skipped (unless the semiring short-circuits — question
1). No zero is ever inspected. Bonus: both A and B are consumed
row-by-row, so CSR (row-major sparse storage) serves both inputs
sequentially. This is why 1978's algorithm is still the one in
every library.

### Step 3 — the SPA: the accumulator that makes scattering O(1)

The one data structure the loop needs is somewhere to accumulate a
row's scattered contributions — entries for the same output column
j arrive from different k's, in no order. Gustavson's **SPA**
(sparse accumulator) is a dense array of size m (one slot per
possible output column) plus a list of which slots are occupied:

- scatter: `SPA[j] += v` is one array write — O(1), no probing;
- a marker array (or generation counter) records first touches and
  appends j to the occupied list;
- gather: walk the occupied list to emit the finished row, then
  reset only those slots.

The cost profile: O(1) per flop, but m slots of memory per thread
— for m = 10M that's an 80 MB array touched at random points,
i.e. cold DRAM per row (question 2 makes you compute the
crossover). The SPA is dense-workspace thinking: pay memory for
zero per-element search cost.

### Step 4 — the design space is "what data structure is the SPA"

Everything since 1978 keeps the row-wise loop and swaps the
accumulator:

```
 SPA = dense array + occupied list   (Gustavson '78)
       O(1) scatter, O(m) alloc, gather via occupied list
 SPA = hash table                    (saxpy3 hash task)
       O(1)-ish scatter, O(flops) alloc — wins for huge m
 SPA = heap / sorted-list merge      (merge k sorted rows of B)
       output comes out SORTED — no gather/sort pass
```

The selection logic: dense SPA wins when the output row fills
enough of m to amortize the cold array; hash wins when m is huge
and the row sparse (the table is sized by flops, so it stays in
cache); heap/merge wins when you need sorted output for free.
saxpy3's m/16 rule (previous chapter) is exactly this decision,
automated per task.

### Step 5 — the unknown-output-size problem: symbolic then numeric

nnz(C) is unknown before you compute C, so how big do you allocate
the output arrays? Gustavson's answer is two phases: a **symbolic
phase** runs the same loop on patterns only (no values, no
arithmetic) to compute each output row's nnz and allocate exactly,
then a **numeric phase** fills the values. Every system in this
curriculum that meets sparse output rediscovers this: saxpy3's
flopcount pre-pass, cudf's size/retrieve (topic 18), Gunrock's
degree scan. The alternative is guess-and-grow (topic 17's
simdjson over-allocate answer) — cheaper when rows are small and
uniform, disastrous under skew. Our stub does symbolic+numeric;
the HashMap reference does guess-free accumulation and pays for it
in allocator traffic.

### Step 6 — Buluç & Gilbert's axes: the survey's map

The survey organizes every SpGEMM as a point in a small space:

- **formulation**: row-wise (Gustavson) / outer-product (column of
  A × row of B → rank-1 updates, needs merging) / inner-product
- **accumulator**: SPA / hash / heap / merge — pick by density of
  the output row and size of m
- **parallelism**: rows are independent (row-wise ⇒ embarrassingly
  parallel over i) BUT power-law graphs make row costs wildly
  unequal ⇒ saxpy3's coarse/fine split, Gunrock's merge_path — the
  same load-balance problem at every layer of this curriculum
- **compression**: masked SpGEMM (`C<M>=A*B`) can skip work only in
  dot formulation; Gustavson's mask only prunes writes

The last axis is worth dwelling on: in row-wise, the flops happen
before the mask can reject them; in dot (inner-product driven *by
the mask*), masked-out cells cost nothing — which is why LAGraph's
triangle counting ships both formulations (question 5).

### Step 7 — skew: the cost intuition to carry

For RMAT/power-law A², flops concentrate in hub rows: row i's cost
is Σ of the degrees of i's neighbors — a degree-squared weighting.
A few rows are 1000× the median, so static row partitioning dies
(7 threads finish, 1 grinds a hub row), which is why every real
implementation has the fine-task path. Whatever accumulator you
pick, the load balancer must be designed for the tail, not the
median.

## How to read the paper (with the concepts in hand)

- **Gustavson '78** — short and readable; read it whole. The
  row-wise algorithm is step 2, the SPA is step 3, and the
  symbolic/numeric two-phase is step 5 (it's also where the
  "permuted transposition" half of the title lives — the same
  two-phase builds a transpose). Notice how little the 1978 prose
  differs from saxpy3's header comment.
- **Buluç & Gilbert 2012** — read §1-3 for the design-space
  framing (step 6): formulation × accumulator × parallelism. Skim
  the distributed-memory experiments; the axes are the payload.
  Map each system you've met (saxpy3 Gustavson task, saxpy3 hash
  task, our stub, the HashMap reference) onto a point in their
  space as you read.

## Questions for notes.md

1. Derive: why is Gustavson's total work exactly
   Σ_{(i,k)∈A} nnz(B(k,:)) and why can no SpGEMM do fewer
   multiplications (each is a necessary term — unless the SEMIRING
   short-circuits: ANY_PAIR reachability can stop early — where?).
2. The dense SPA costs m bytes×2 (value + mark) per thread. For
   m = 10M that's cold DRAM per row. Compute the crossover row
   density where hash beats SPA using topic 13's cache numbers
   (SPA touches nnz_out random cells of an 80 MB array; hash
   touches nnz_out cells of a 2×flops table that fits L2).
3. Symbolic+numeric does the pattern walk TWICE. When is
   guess-and-grow cheaper (flops/row small and uniform — the
   variance argument; connect to topic 16's ddmin determinism
   requirement... no wait, to cudf's retrieve-skip answer)?
4. Outer-product SpGEMM produces k rank-1 updates that must be
   merged — which topic 3 structure is that (LSM: sorted runs +
   merge), and why does it win out-of-core / distributed
   (sequential I/O, no random SPA)?
5. Masked Gustavson can't skip work; masked dot can. Show it on
   triangle counting `C<L>=L*L`: what does each formulation compute
   per wedge, and reconcile with LAGraph shipping BOTH Sandia_LL
   (saxpy) and Sandia_LUT (dot) as the fastest per-graph choices.

## References

**Papers**
- Gustavson — "Two Fast Algorithms for Sparse Matrices:
  Multiplication and Permuted Transposition" (ACM TOMS 1978) — the
  row-wise algorithm + the symbolic/numeric two-phase; short and
  readable
- Buluç, Gilbert — "Parallel Sparse Matrix-Matrix Multiplication
  and Indexing: Implementation and Experiments" (SIAM J. Sci.
  Comput. 2012, [arXiv:1109.3739](https://arxiv.org/abs/1109.3739))
  — the design-space framing: formulation × accumulator ×
  parallelism
