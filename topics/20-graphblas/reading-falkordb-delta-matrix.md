# Delta matrices: an LSM memtable over GraphBLAS

FalkorDB's answer to "GrB matrices are fast to read, slow to mutate
one edge at a time" — your own code, with this curriculum's eyes.
The delta matrix is topic 3's LSM memtable+tombstone pattern rebuilt
over GrB matrices. This chapter builds the design step by step —
the mutation problem, the three-matrix trio, its invariants, the
load-bearing "why not the library's own deltas" question, the
masked-multiply fold, and the compaction — then hands you the
anchors into `src/graph/delta_matrix/` to verify each piece.

Every anchor is **FalkorDB at commit `ccb449a9a`**, the pin in
`resources/codebases.md`. Read the code, not the blog posts: three
of the claims in the previous version of this chapter were wrong at
this commit, and each correction is called out where it lands.

## The problem in one sentence

Deleting or inserting one edge in a packed sparse matrix means
splicing contiguous arrays — O(nnz) work, potentially hundreds of
MBs moved, for a one-edge change — and a graph database takes
single-edge writes *continuously* while readers expect
multiply-speed reads.

## The concepts, step by step

### Step 1 — the mutation problem: packed arrays hate point writes

> **In:** a settled sparse matrix and a single-edge write.
> **Out:** the cost of doing it eagerly, in this repo's own units.

A settled `GrB_Matrix` in sparse/hypersparse form is CSR-like:
contiguous row-pointer and column-index arrays, packed with no
slack. That is exactly what makes reads and multiplies fast — and
exactly what makes one-edge mutation expensive: inserting edge
(i,j) means shifting every index after it, deleting means the
same splice in reverse.

```
 inputs: a 100M-edge relation matrix, colidx u32
         memmove bandwidth ~30 GB/s (topic 0/13 streaming baseline)

 one eager insert at the midpoint:
   bytes moved  = 50e6 entries × 4 B     = 200 MB
   time         = 200e6 / 30e9           = 6.7 ms

 a modest write burst of 10,000 edges, done eagerly:
   10,000 × 6.7 ms                       = 67 seconds

 the same 10,000 edges appended to a side matrix:
   10,000 × O(1)                         = microseconds
   plus ONE fold of 100M entries at the end (Step 6)
```

The generic fix, seen in topic 3 (LSM) and in SuiteSparse's own
zombies/pending tuples: do not restructure — *record the change
somewhere cheap and merge later*.

Why it matters: 67 seconds versus one fold is the entire
justification for the rest of this chapter, and the ratio is set by
the flush threshold you pick in Step 6.

### Step 2 — the trio: settled matrix plus two delta matrices

> **In:** the "record it somewhere cheap" idea.
> **Out:** the actual struct, its six matrices, and the read
> identity.

```c
// delta_matrix.h — the struct, 108-115
   108  struct _Delta_Matrix {
   109  	bool locked;
   110  	GrB_Matrix matrix;        // Underlying GrB_Matrix
   111  	GrB_Matrix delta_plus;    // Pending additions
   112  	GrB_Matrix delta_minus;   // Pending deletions
   113  	Delta_Matrix transposed;  // Transposed matrix
   114  	pthread_mutex_t mutex;    // Lock
   115  };
```

Note the recursion at `:113`: `transposed` is itself a
`Delta_Matrix`, so it carries its own trio. Six GrB matrices per
logical relation, reached through six macros:

```c
// delta_matrix.h — the accessors, 17-24
    17  #define DELTA_MATRIX_M(C)            ((C)->matrix)
    18  #define DELTA_MATRIX_DELTA_PLUS(C)   ((C)->delta_plus)
    19  #define DELTA_MATRIX_DELTA_MINUS(C)  ((C)->delta_minus)
    20  #define DELTA_MATRIX_TM(C)            ((C)->transposed->matrix)
    21  #define DELTA_MATRIX_TDELTA_PLUS(C)   ((C)->transposed->delta_plus)
    22  #define DELTA_MATRIX_TDELTA_MINUS(C)  ((C)->transposed->delta_minus)
    23
    24  #define DELTA_MATRIX_MAINTAIN_TRANSPOSE(C) ((C)->transposed != NULL)
```

A naming trap worth fixing now: the header's comment diagrams call
the settled matrix **`A`**, but the struct field is `matrix` and
the macro is `DELTA_MATRIX_M`. Throughout this chapter, **M** is
the settled matrix and **A** is the *logical* matrix a reader
sees.

M is big and packed; DP and DM are tiny (bounded by the write batch
since the last sync — Step 6 gives the bound as 10,000), so
mutating them is cheap. The logical matrix the rest of the engine
sees is defined algebraically: **A ≡ (M ∪ DP) \ DM** — everything
settled or pending-added, minus everything pending-deleted. Same
read algebra as an LSM point-read (memtable ∪ sstables minus
tombstones — DM's entries are exactly **tombstones**, deletion
markers that suppress a still-physically-present entry).

The three matrices are not configured alike, and `delta_new.c`
says so:

```c
// delta_new.c — the sparsity pins, 21-44 (elided)
    22  	// m, can be either hypersparse or sparse
    24  	GrB_OK(GrB_Matrix_new (&A->matrix, type, nrows, ncols));
    25  	GrB_OK (GrB_set (
    26  		A->matrix, GxB_SPARSE | GxB_HYPERSPARSE, GxB_SPARSITY_CONTROL));
    ...
    29  	// delta-plus, always hypersparse
    31  	GrB_OK (GrB_Matrix_new (&A->delta_plus, type, nrows, ncols));
    32  	GrB_OK (GrB_set (A->delta_plus, GxB_HYPERSPARSE, GxB_SPARSITY_CONTROL));
    33  	GrB_OK (GrB_set (A->delta_plus, (int32_t) false, GxB_HYPER_HASH));
    35  	GrB_OK (GxB_set (A->delta_plus, GxB_HYPER_SWITCH, GxB_ALWAYS_HYPER));
    ...
    38  	// delta-minus, always hypersparse
    40  	GrB_OK (GrB_Matrix_new (&A->delta_minus, GrB_BOOL, nrows, ncols));
    41  	GrB_OK (GrB_set (A->delta_minus, GxB_HYPERSPARSE, GxB_SPARSITY_CONTROL));
    42  	GrB_OK (GrB_set (A->delta_minus, (int32_t) false, GxB_HYPER_HASH));
    44  	GrB_OK (GxB_set (A->delta_minus, GxB_HYPER_SWITCH, GxB_ALWAYS_HYPER));
```

Four decisions in twenty lines, and each is answerable from
[reading-suitesparse-internals.md](reading-suitesparse-internals.md):

1. **M allows sparse or hypersparse, never bitmap or full.** In
   `GB_conform.c:150`'s switch that is case (3) at `:175-184`, the
   `GxB_HYPERSPARSE + GxB_SPARSE` arm — where the bitmap test is
   not merely unlikely, it *never executes*. Given Step 2 of the
   internals chapter (a graph matrix is 13,700× below the 0.40
   bitmap threshold), this pin costs nothing and removes a branch.
2. **DP and DM are pinned hypersparse and forced there** by
   `GxB_ALWAYS_HYPER`, which lands in `GB_conform.c:157-160` —
   `GB_convert_any_to_hyper` unconditionally, no test at all. A
   matrix holding 10,000 entries over a 10M id space *must* be
   hypersparse; the 50× index saving of `notes.md:41` is the
   reason.
3. **The hyper_hash is disabled** (`:33`, `:42`). That is the
   `A->Y` structure of `GB_Matrix_content.h:241-274`; without it,
   finding row j in `h[]` is a binary search costing `lg k`. With
   k ≤ 10,000 that is 14 comparisons — cheaper than building and
   maintaining a hash table that gets cleared at every flush.
4. **DM is always `GrB_BOOL`** (`:40`), whatever the matrix type
   is. A tombstone carries no value, so DM is iso by construction
   (`GB_Matrix_content.h:513-524`) and stores one byte total for
   its values.

And the type restriction, at `delta_new.c:64-65`: "supported
types: boolean and uint64", `ASSERT (type == GrB_BOOL || type ==
GrB_UINT64)`.

Why it matters: the trio is not three copies of the same thing.
Each matrix is configured for its own access pattern, and every
one of those four settings is a decision you can now trace into
SuiteSparse's own source.

### Step 3 — the invariants, and the read/write paths

> **In:** the trio and the identity A ≡ (M ∪ DP) \ DM.
> **Out:** the eight states the header enumerates, the actual read
> order, and the GrB-call cost of one mutation.

The header comment at `delta_matrix.h:26-106` walks every state
through a worked 3×3 example — it IS the design doc. (The previous
version of this chapter cited `:34-108`; the block runs `:26-106`,
and the struct that follows is `:108-115`.) It enumerates **four
legal states and four impossible ones**:

| state | lines | A | DP | DM |
|---|---|---|---|---|
| empty | `:32-38` | · | · | · |
| flushed, no pending changes | `:41-47` | 1 | · | · |
| single entry added | `:50-56` | · | 1 | · |
| single entry deleted | `:59-65` | 1 | · | 1 |
| **impossible** — "existing entry deleted and then added back" | `:67-74` | 1 | 1 | 1 |
| **impossible** — "marked none existing entry for deletion" | `:77-84` | · | · | 1 |
| **impossible** — "adding to an already existing entry" | `:87-95` | 1 | 1 | · |
| **impossible** — "deletion of pending entry should have cleared it DP[0,0]" | `:98-105` | · | 1 | 1 |

Read the four impossible rows as the invariants they encode:

```
 logical A  ≡  (M ∪ DP) \ DM

 DP ∩ M = ∅     additions are NEW entries          (:87-95 forbids the overlap)
 DM ⊆ M         only settled entries can be         (:77-84 forbids DM without M)
                pending-deleted
 DP ∩ DM = ∅    delete of a DP entry clears DP      (:98-105)
                directly — never passes through DM
 M ∩ DP ∩ DM = ∅  re-adding a deleted entry         (:67-74)
                resurrects it out of DM
```

Every entry is in exactly one state, so reads never need conflict
resolution. Now the actual read path — and **the order is not what
the LSM analogy predicts**:

```c
// delta_isStored.c — the read path, 25-40. DP first, then DM, then M.
    25  	// if dp[i,j] exists return it
    26  	info = GxB_Matrix_isStoredElement (DP, i, j) ;
    27  	if(info == GrB_SUCCESS) {
    28  		return info ;
    29  	}
    30
    31  	// if dm[i,j] exists, return no value
    32  	info = GxB_Matrix_isStoredElement (DM, i, j) ;
    33  	if (info == GrB_SUCCESS) {
    34  		// entry marked for deletion
    35  		return GrB_NO_VALUE ;
    36  	}
    37
    38  	// entry isn't marked for deletion, see if it exists in 'm'
    39  	info = GxB_Matrix_isStoredElement (M, i, j) ;
    40  	return info ;
```

**DP is probed first, not DM.** That is legal precisely because of
the `DP ∩ DM = ∅` invariant at `:98-105` — a tombstone can never
shadow a pending addition, so the order does not change the answer,
and probing the newest layer first is the cheapest ordering. An LSM
would have to check tombstones first because its layers *do*
overlap. Note also `:28`: the early return means an entry found in
DP costs **one** probe, not three.

The write path, both directions:

```c
// delta_set_element_bool.c — insert, 27-40
    27  	if (DELTA_MATRIX_MAINTAIN_TRANSPOSE (C)) {
    28  		GrB_OK (Delta_Matrix_setElement_BOOL (C->transposed, j, i)) ;
    29  	}
    30
    31  	GrB_OK (info = GxB_Matrix_isStoredElement (m, i, j)) ;
    32  	already_allocated = (info == GrB_SUCCESS);
    33
    34  	if (already_allocated) {
    35  		// unset delta-minus
    36  		GrB_OK (GrB_Matrix_removeElement (dm, i, j)) ;
    37  	} else {
    38  		// update entry to dp[i, j]
    39  		GrB_OK (GrB_Matrix_setElement_BOOL (dp, true, i, j)) ;
    40  	}
```

```c
// delta_remove_element.c — delete, 28-44
    28  	if (DELTA_MATRIX_MAINTAIN_TRANSPOSE (C)) {
    29  		GrB_OK (Delta_Matrix_removeElement (C->transposed, j, i)) ;
    30  	}
    ...
    36  	info = GxB_Matrix_isStoredElement (m, i, j) ;
    37  	in_m = (info == GrB_SUCCESS) ;
    38
    39  	if (in_m) {
    40  		// mark deletion in delta minus
    41  		GrB_OK (GrB_Matrix_setElement_BOOL (dm, (bool) true, i, j)) ;
    42  	} else {
    43  		GrB_OK (GrB_Matrix_removeElement (dp, i, j)) ;
    44  	}
```

Both are the same shape: recurse into the transposed twin **first**
(`:27-29` and `:28-30`), probe M once, then branch. Cost it:

```
 inputs: one logical edge insert on a delta matrix WITH a transposed twin

 per orientation:
   1 × GxB_Matrix_isStoredElement (M)             probe
   1 × GrB_Matrix_removeElement(DM)  OR
       GrB_Matrix_setElement_BOOL(DP)             mutation
   = 2 GrB calls

 two orientations (the recursion at :28)          = 4 GrB calls

 read path, worst case (entry lives in M):
   3 × GxB_Matrix_isStoredElement                 = 3 GrB calls
 read path, best case (entry lives in DP):
   1 × GxB_Matrix_isStoredElement                 = 1 GrB call   (:28)

 so: 4 calls to write, 1-3 to read, against ONE array splice
 avoided (Step 1: 6.7 ms on a 100M-edge matrix).
```

Note also which branch fires. Inserting a *brand-new* edge writes
to DP; re-inserting a *deleted* edge (`already_allocated` true)
removes from DM instead, which is the `:67-74` impossible state
being actively prevented rather than merely asserted.

Why it matters: the write path costs four library calls and the
read path costs one to three, and both numbers come from counting
lines, not from a blog post.

### Step 4 — why not SuiteSparse's own pending tuples? (the load-bearing question)

> **In:** the trio, and SuiteSparse's zombies + pending tuples.
> **Out:** three candidate reasons, each grounded in a specific
> line of one library or the other.

SuiteSparse already defers mutations (zombies + pending tuples,
[reading-davis-toms19.md](reading-davis-toms19.md) Step 4), and its
own bound is excellent: e incremental inserts in **O(e log e)**
against MATLAB's O(e²) (TOMS '19 §3.1.8). So why rebuild it?

1. **Flush control.** Any GrB read op can force an internal wait —
   the source says so for zombies at
   `GB_Matrix_content.h:386-389`. FalkorDB needs reads that do
   *not* flush. DP and DM are ordinary matrices the library never
   touches implicitly, and the flush decision lives in FalkorDB's
   own code at `delta_wait.c:89` and `:97` (Step 6).
2. **The transposed twin.** SuiteSparse maintains one matrix;
   FalkorDB needs M and Mᵀ synced under the same deltas. The
   recursion is right there in the struct (`delta_matrix.h:113`)
   and in every mutation (`delta_set_element_bool.c:27-29`), so
   pull traversals (`<-[]-` patterns) are always available without
   a transpose.
3. **Bounded sync cost.** A wait folds a *small* DP/DM — bounded by
   `DELTA_MAX_PENDING_CHANGES_DEFAULT` = **10,000**
   (`src/configuration/config.h:19`). Library pending tuples have
   no such bound and can degrade into a full rebuild inside an
   unrelated query.

Question 2 asks you to decide which dominates. A hint from the
code: reason 2 is the only one that is *impossible* to get from
the library at any threshold, because SuiteSparse has no concept of
a matrix pair kept in lockstep.

The general lesson: a lower layer's deferred-work mechanism is
only reusable if you control *when* it fires and *what invariants*
it maintains — otherwise you rebuild it one level up, which is
exactly what happened here.

Why it matters: this is the transferable design judgement in the
whole topic, and it recurs every time you sit a system on a library
that already has a cache, a log, or a scheduler.

### Step 5 — delta_mxm: algebra instead of a flush

> **In:** a multiply where one operand carries pending state.
> **Out:** the four GrB calls that avoid a flush, the exact
> masking the code performs, and where it differs from its own
> comment.

The expensive operation on a delta matrix is a multiply — must the
deltas be folded into M first? `delta_mxm.c` (121 lines) says no:
fold the pending state into the *algebra* of one multiply. Its own
statement of intent, and its preconditions:

```c
// delta_mxm.c — the contract, 40-50
    40  	// where A is fully synced!
    ...
    43  	// this operation performs: A * B by computing:
    44  	// (A * (M + 'delta-plus'))<!'delta-minus'>
    45
    46  	// validate A doesn't contains entries in either delta-plus or delta-minus
    47  	ASSERT(Delta_Matrix_Synced(A));
    48
    49  	// validate C doesn't contains entries in either delta-plus or delta-minus
    50  	ASSERT(Delta_Matrix_Synced(C));
```

**Only B carries deltas.** A and C must be synced, asserted at
`:47` and `:50`. That narrows the problem enormously: the pending
state appears on exactly one side.

The four calls, in order:

```c
// delta_mxm.c — what actually runs, 71-108 (elided)
    71  	if (dm_nvals > 0) {
    72  		// compute A * 'delta-minus'
    74  		GrB_OK (GrB_mxm (mask, NULL, NULL, GxB_ANY_PAIR_BOOL, _A, dm, NULL)) ;
    78  	}
    80  	if (dp_nvals > 0) {
    81  		// compute A * 'delta-plus'
    86  		GrB_OK (GrB_mxm (accum, NULL, NULL, semiring, _A, dp, NULL)) ;
    90  	}
    ...
    96  	if (deletions) {
    97  		desc = GrB_DESC_RSC ;
    ...
   103  	// compute (A * B)<!mask>
   104  	GrB_OK (GrB_mxm (_C, mask, NULL, semiring, _A, _B, desc)) ;
   105
   106  	if (additions) {
   107  		GrB_OK (GrB_eWiseAdd (_C, NULL, NULL, semiring, _C, accum, NULL)) ;
   108  	}
```

Line by line: `:74` builds `mask = A·DM` over `GxB_ANY_PAIR_BOOL`
(pattern only, no values, and ANY lets it stop at the first hit);
`:86` builds `accum = A·DP` over the real semiring; `:104`
computes the main product with `GrB_DESC_RSC` — which is
`REPLACE + STRUCTURE + COMP` (`Include/GraphBLAS.h:666`), so the
mask is *complemented*: write only where `A·DM` has **no** entry;
`:107` adds the additions in.

**Two things here are not what the comment at `:44` says**, and the
code is authoritative:

- The comment masks by `'delta-minus'`; the code masks by
  `A · DM` (`:74`), which is a different, coarser matrix.
- The comment applies the mask to `A*(M + DP)` — additions
  included. The code applies it only to `A*M` at `:104`, then adds
  `accum` **unmasked** at `:107`. So a cell killed by the mask can
  be revived by an addition, but only if a DP edge happens to
  produce it.

The over-masking is real and constructible. Here is the
counterexample question 3 asks for, entirely readable off `:74`,
`:97` and `:104`:

```
 A = one row, two live edges:      A(0,0) = 1,  A(0,1) = 1
 B's settled M:                    M(0,5) = 1,  M(1,5) = 1
 delete edge (0,5) from B      ⇒   DM(0,5) = 1, M unchanged

 truth:  logical B = (M ∪ DP) \ DM  has only B(1,5)
         A·B  ⇒  C(0,5) present, via A(0,1)·B(1,5)         ← LIVE

 code:   mask  = A·DM      ⇒ mask(0,5) present, via A(0,0)·DM(0,5)
         :104  = (A·M)<!mask>  ⇒ C(0,5) SUPPRESSED
         :107  accum = A·DP = empty ⇒ nothing restores it

 result: delta_mxm drops a live entry. The mask is structural —
 it kills the whole output cell if ANY contributing path used a
 deleted edge, even when other paths are alive.
```

How correctness is restored at the call sites is **not verified
here** — question 3 sends you to `graph/graph.c` for it. Do not
assume it is fixed inside `delta_mxm.c`; it is not.

Price the overhead, so you know what the algebra costs:

```
 inputs: main product A·B on an RMAT-scale-16-sized relation
         (topics/24-graph-algorithms/notes.md:5) — mean degree 27.8
         DP and DM each at the flush threshold, 10,000 entries
         main-product flops ≈ 1.28e8 (extrapolated from notes.md:22-26)

 A·DM flops = Σ over DM's 10,000 entries of nnz(A(:,k))
            ≈ 10,000 × 27.8                 = 278,000
 A·DP flops ≈ same                          = 278,000
 total extra                                = 556,000

 overhead = 5.56e5 / 1.28e8                 = 0.43%

 versus the alternative — flushing B first — which is
 Step 6's O(nnz(M)) fold on the critical path of the query.
```

Two extra *small* multiplies instead of one big compaction: the LSM
read-amplification-versus-compaction trade, chosen per multiply, at
under half a percent.

Why it matters: 0.43% is why this design is worth its correctness
hazard — and the hazard is the price, which you should be able to
construct on demand.

### Step 6 — wait: the two-sided compaction

> **In:** DP and DM, grown since the last fold.
> **Out:** the two GrB calls that fold them, the threshold that
> triggers it, and the amortized cost per mutation.

`Delta_Matrix_wait` is the compaction that folds the deltas into M
and resets the trio. Deletions first:

```c
// delta_wait.c — sync_deletions, 13-33 (elided)
    13  static GrB_Info Delta_Matrix_sync_deletions
    ...
    25  	if (nvals > 0) { //shortcut if no vals
    ...
    29  		GrB_RETURN_IF_FAIL (GrB_transpose (m, dm, NULL, m, GrB_DESC_RSCT0)) ;
    30  	}
    31
    32  	// clear delta minus
    33  	return GrB_Matrix_clear (dm) ;
```

Unpack `:29` carefully, because it is the cleverest line in the
directory. `GrB_transpose(C, Mask, accum, A, desc)` computes
`C<Mask> = Aᵀ`. Here C = m, Mask = dm, A = m, and the descriptor is
`GrB_DESC_RSCT0` — `REPLACE + STRUCTURE + COMP` plus `GrB_TRAN` on
input 0 (`Include/GraphBLAS.h:668`). The `T0` transposes the input,
so `Aᵀ` becomes `(mᵀ)ᵀ = m`; the `C` complements the mask; the `S`
uses only its pattern; the `R` replaces. Net effect: **copy m into
itself, keeping only the entries dm does not mark.** One library
call performs the whole tombstone sweep, and the transpose flags
cancel each other out.

Then additions:

```c
// delta_wait.c — sync_additions, 36-56 (elided)
    36  static GrB_Info Delta_Matrix_sync_additions
    ...
    48  	if (nvals > 0) { //shortcut if no vals
    ...
    51  		GrB_RETURN_IF_FAIL (GrB_Matrix_assign (m, dp, NULL, dp, GrB_ALL, 0,
    52  					GrB_ALL, 0, GrB_DESC_S)) ;
    53  	}
    54
    55  	// clear delta plus
    56  	return GrB_Matrix_clear (dp) ;
```

(The previous version of this chapter cited `delta_wait.c:36-46+`
for this function; it runs to `:57`.)

Now the trigger, and **the correction that matters most in this
chapter**. The policy is *not* in `delta_will_wait.c`:

```c
// delta_wait.c — the thresholds, 89-99
    89  		if (dm_nvals >= delta_max_pending_changes) {
    90  			GrB_RETURN_IF_FAIL (Delta_Matrix_sync_deletions (C)) ;
    91  		}
    ...
    97  		if (dp_nvals >= delta_max_pending_changes) {
    98  			GrB_RETURN_IF_FAIL (Delta_Matrix_sync_additions (C)) ;
    99  		}
```

Two independent thresholds against the same constant, deletions
tested first. `delta_max_pending_changes` comes from
`Config_DELTA_MAX_PENDING_CHANGES` (`delta_wait.c:126-128`),
defaulting to **10,000** (`src/configuration/config.h:19`). And
`force_sync` at `:71-73` bypasses both.

What `delta_will_wait.c` actually does is a different question
entirely — it asks *SuiteSparse* whether the library has its own
pending work:

```c
// delta_will_wait.c — asking the LIBRARY, not the delta layer, 34-44
    34  	// check if M contains pending changes
    35  	GrB_OK (GrB_Matrix_get_INT32 (M, &p, GxB_WILL_WAIT)) ;
    36  	res = res || p == 1 ;
    ...
    39  	GrB_OK (GrB_Matrix_get_INT32 (DP, &p, GxB_WILL_WAIT)) ;
    ...
    43  	GrB_OK (GrB_Matrix_get_INT32 (DM, &p, GxB_WILL_WAIT)) ;
```

`GxB_WILL_WAIT` is SuiteSparse's own zombies-and-pending-tuples
probe from
[reading-davis-toms19.md](reading-davis-toms19.md) Step 4. It is
used as an **assertion**, at `delta_wait.c:108-110`, to check that
after the three `GrB_wait(…, GrB_MATERIALIZE)` calls at `:103-105`
nothing is left deferred at either level. Two deferral systems
stacked, and this is the line that proves they are both drained.

One more ordering detail: `Delta_Matrix_wait` recurses into the
transposed twin **before** doing its own work (`:122-124`), the
same first-the-twin discipline as the mutation paths.

Now the compaction trade, made arithmetic:

```
 inputs: M holds N = 100e6 edges; threshold T = 10,000 (config.h:19)

 a fold touches O(N) entries (the GrB_transpose at :29 rewrites m)
 amortized fold cost per mutation = N / T = 100e6 / 10,000 = 10,000
                                             entries rewritten per edge

 compare eager (Step 1): 50e6 entries moved per edge
 improvement                                = 5,000×

 halve the threshold to T = 5,000:
   amortized fold cost doubles              = 20,000 entries/edge
   but DP/DM stay half as big, so Step 5's mxm overhead halves
   (0.43% → 0.21%) and a DP-miss read path shortens

 raise it to T = 100,000:
   amortized fold cost                      = 1,000 entries/edge
   but mxm overhead rises to ~4.3% and every read carries a
   10× bigger DP/DM to probe
```

That is compaction triggering by size, topic 3 again: small
thresholds mean low read amplification but frequent O(nnz(M))
folds; large thresholds mean cheap writes but every read and
multiply pays the three-way tax longer.

Why it matters: one configuration constant sets the position on
that curve, and now you can compute both ends of it.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| `delta_matrix.h:17-24` | 2 | the six accessor macros plus `MAINTAIN_TRANSPOSE` |
| `delta_matrix.h:26-106` | 3 | the state table: 4 legal states, 4 impossible ones — the spec |
| `delta_matrix.h:108-115` | 2 | the struct; `:113` is the recursive transposed twin |
| `delta_new.c:21-44` | 2 | sparsity pins: M sparse+hyper, DP/DM always-hyper, no hyper_hash |
| `delta_new.c:64-65` | 2 | `GrB_BOOL` or `GrB_UINT64` only |
| `delta_isStored.c:25-40` | 3 | the read path — **DP, then DM, then M** |
| `delta_set_element_bool.c:27-40` | 3 | insert: twin first, probe M, then DM-remove or DP-set |
| `delta_remove_element.c:28-44` | 3 | delete: twin first, probe M, then DM-set or DP-remove |
| `delta_remove_element.c:50-81` | 3 | the bulk form, via `eWiseMult` + `assign` |
| `delta_mxm.c:40-50` | 5 | the contract: **only B may carry deltas** |
| `delta_mxm.c:74`, `:86` | 5 | `mask = A·DM` (ANY_PAIR bool), `accum = A·DP` |
| `delta_mxm.c:97`, `:104`, `:107` | 5 | `GrB_DESC_RSC`, the masked product, the **unmasked** add |
| `delta_wait.c:13-33` | 6 | sync_deletions: `GrB_transpose(m, dm, NULL, m, GrB_DESC_RSCT0)` |
| `delta_wait.c:36-56` | 6 | sync_additions: assign DP into M, clear DP |
| `delta_wait.c:89`, `:97` | 6 | **the flush thresholds live here** |
| `delta_wait.c:103-110` | 6 | `GrB_wait(…, GrB_MATERIALIZE)` ×3, then the willWait assertion |
| `delta_wait.c:122-128` | 6 | twin first; read `Config_DELTA_MAX_PENDING_CHANGES` |
| `delta_will_wait.c:34-44` | 6 | asks SuiteSparse `GxB_WILL_WAIT` — **not** the delta threshold |
| `src/configuration/config.h:19` | 6 | `DELTA_MAX_PENDING_CHANGES_DEFAULT 10000` |

Navigation advice: start with the state table in
`delta_matrix.h:26-106` (it IS the design doc) and check it against
`delta_set_element_bool.c` and `delta_remove_element.c` — that is
question 1, and it takes ten minutes. Then `delta_wait.c` top to
bottom (218 lines), then `delta_mxm.c` (121 lines), then
`delta_isStored.c`. Read each against topics 3 (LSM), 6 (buffer
management) and this topic's zombies/pending-tuples machinery,
asking at each step "why not just let SuiteSparse's own deltas do
this?"

### What transfers to M20

M20 rebuilds this over OUR kernels: the trio plus the transposed
twin, the read algebra in get/extract, the mxm fold, and
threshold-driven wait. The reference is the spec; the interesting
freedom is choosing DP/DM's representation (hash of pairs? small
COO? bitmap?) now that we own it. Three numbers to design against:
the write path is 4 calls (Step 3), the flush threshold is 10,000
entries (Step 6), and the mxm overhead at that threshold is 0.43%
(Step 5).

## Questions for notes.md

1. Verify the invariants against `delta_set_element_bool.c` and
   `delta_remove_element.c`: enumerate the 4 cases (entry in M, in
   DP, in DM, absent) × (set, remove), and match each to one of the
   eight rows in the header table at `delta_matrix.h:26-106`. Which
   transitions does the table show, and are any reachable
   transitions missing from it?
2. The transposed twin doubles write work on every mutation. Cost
   it: Step 3 counts 4 GrB calls per logical edge — say which two
   belong to the twin, and work out what would break if the
   transpose were rebuilt lazily at wait instead (pull traversals
   see a stale Mᵀ between waits — for how long, given the 10,000
   threshold?).
3. `delta_mxm`'s mask over-masks. Step 5 gives a counterexample
   readable off `:74`, `:97` and `:104`; reproduce it, then find
   how correctness is restored — recompute the masked region
   against (M ∪ DP) \ DM? restrict when `delta_mxm` is called at
   all? Check the callers in `graph/graph.c`. This chapter did
   **not** verify the answer.
4. The sync thresholds: `delta_wait.c:89` and `:97` both compare
   against `DELTA_MAX_PENDING_CHANGES_DEFAULT` = 10,000
   (`config.h:19`). Map that onto LSM L0 file-count triggers
   (topic 3) — write-visible latency versus read amplification —
   and redo Step 6's amortized-cost arithmetic for the threshold
   your own M20 workload would want.
5. For M20: pick DP/DM's representation in Rust. COO
   `Vec<(u32,u32)>` + sort at wait (LSM-flavoured) versus HashMap
   (point-read-flavoured) — which do the LDBC interactive
   update+read mixes prefer? Predict from Step 3's call counts
   (4 writes, 1-3 reads per operation) first, then bench both under
   `gb_bench`'s update workload.

## Done when

Answer each before unfolding it.

- [ ] You can state the trio and write the read identity `(M ∪ DP) ∖ DM` from memory, and name all four invariants.

  <details><summary>Answer</summary>

  M (`matrix`), DP (`delta_plus`), DM (`delta_minus`), plus the
  recursive `transposed` twin carrying its own three
  (`delta_matrix.h:108-115`). Logical A ≡ (M ∪ DP) \ DM.

  The four invariants are the four **impossible** states in the
  header table: `DP ∩ M = ∅` (`:87-95`, "adding to an already
  existing entry"), `DM ⊆ M` (`:77-84`, "marked none existing entry
  for deletion"), `DP ∩ DM = ∅` (`:98-105`, "deletion of pending
  entry should have cleared it"), and `M ∩ DP ∩ DM = ∅` (`:67-74`,
  "existing entry deleted and then added back").

  Careful with names: the header's diagrams call the settled matrix
  `A`, but the struct field is `matrix` and the macro is
  `DELTA_MATRIX_M`.

  </details>

- [ ] You can give the read order and explain why it is legal.

  <details><summary>Answer</summary>

  **DP → DM → M** (`delta_isStored.c:26`, `:32`, `:39`) — not DM
  first, which is what the LSM analogy would suggest.

  It is legal because of the `DP ∩ DM = ∅` invariant
  (`delta_matrix.h:98-105`): a tombstone can never shadow a pending
  addition, so the order cannot change the answer. Probing the
  newest, smallest layer first is then simply the cheapest ordering,
  and the early return at `:28` makes a DP hit cost one probe
  instead of three. An LSM must check tombstones first precisely
  because its layers do overlap.

  </details>

- [ ] You can cost one mutation and one read in GrB calls.

  <details><summary>Answer</summary>

  Write: recurse into the twin first
  (`delta_set_element_bool.c:27-29`), then one
  `GxB_Matrix_isStoredElement` on M (`:31`), then one mutation —
  `GrB_Matrix_removeElement(dm, …)` if the entry is in M (`:36`),
  else `GrB_Matrix_setElement_BOOL(dp, …)` (`:39`). Two calls per
  orientation, **four in total**. `delta_remove_element.c:28-44` is
  the mirror image.

  Read: **1 to 3** `isStoredElement` probes — one if the entry is
  in DP (`delta_isStored.c:28` returns early), three if it is in M.

  Against Step 1's alternative: one eager splice on a 100M-edge
  matrix moves ~200 MB, about 6.7 ms at 30 GB/s.

  </details>

- [ ] You can say why each of the three matrices is pinned the way it is.

  <details><summary>Answer</summary>

  From `delta_new.c:21-44`. M allows `GxB_SPARSE | GxB_HYPERSPARSE`
  (`:25-26`), which lands in `GB_conform.c:175-184` — the bitmap
  test never runs, and per the internals chapter a graph matrix is
  13,700× below the 0.40 threshold anyway, so nothing is lost.

  DP and DM are pinned `GxB_HYPERSPARSE` and forced with
  `GxB_ALWAYS_HYPER` (`:32`, `:35`, `:41`, `:44`), landing in
  `GB_conform.c:157-160` — `GB_convert_any_to_hyper`
  unconditionally. Ten thousand entries over a 10M id space must be
  hypersparse; that is `notes.md:41`'s 50×.

  The hyper_hash is disabled (`:33`, `:42`), so row lookup in `h[]`
  is a binary search — 14 comparisons at k = 10,000, cheaper than
  building and re-clearing SuiteSparse's `A->Y`
  (`GB_Matrix_content.h:241-274`) at every flush.

  DM is always `GrB_BOOL` (`:40`) whatever the matrix type: a
  tombstone carries no value, so DM is iso by construction.

  </details>

- [ ] You can answer the load-bearing question: why FalkorDB needs its own deltas rather than SuiteSparse's pending tuples.

  <details><summary>Answer</summary>

  Three candidates, and each is grounded: (1) flush control — any
  GrB read can force a wait (`GB_Matrix_content.h:386-389`),
  whereas FalkorDB's thresholds are its own
  (`delta_wait.c:89`, `:97`); (2) the transposed twin —
  `delta_matrix.h:113` plus the recursion at every mutation, so
  `<-[]-` traversals never need a transpose; (3) a bounded fold —
  10,000 pending changes (`config.h:19`), where library pending
  tuples have no bound.

  Reason 2 is the only one SuiteSparse cannot supply at *any*
  setting, because it has no concept of a matrix pair kept in
  lockstep. Reasons 1 and 3 are about control of a mechanism that
  exists; reason 2 is about a mechanism that does not.

  The transferable lesson: a lower layer's deferred-work mechanism
  is reusable only if you control when it fires and what invariants
  it maintains.

  </details>

- [ ] You can explain `delta_mxm` as algebra instead of a flush, and construct the case where it over-masks.

  <details><summary>Answer</summary>

  Preconditions first: **only B may carry deltas**, asserted at
  `delta_mxm.c:47` and `:50`. Then four calls — `mask = A·DM` over
  `GxB_ANY_PAIR_BOOL` (`:74`), `accum = A·DP` over the real
  semiring (`:86`), the main product `C<!mask> = A·M` with
  `GrB_DESC_RSC` (`:97`, `:104`), and an **unmasked**
  `GrB_eWiseAdd` of `accum` (`:107`).

  Two departures from the comment at `:44`: the mask is `A·DM`, not
  DM; and additions are added after the mask rather than under it.

  Over-masking: let A have two live edges out of row 0, to 0 and 1;
  let B's settled M have M(0,5) and M(1,5); delete (0,5), so
  DM(0,5) is set. Then `mask = A·DM` has an entry at (0,5), and
  `:104` suppresses C(0,5) entirely — even though A(0,1)·B(1,5) is
  a live path that should produce it, and `accum` is empty so
  nothing restores it. The mask is structural: one dead path kills
  the whole cell.

  How the callers compensate is question 3, and this chapter did
  not verify it — it is not fixed inside `delta_mxm.c`.

  Cost of the algebra: at the 10,000 threshold and a mean degree of
  27.8, the two extra multiplies are about 5.6e5 flops against a
  main product of ~1.28e8 — **0.43%**.

  </details>

- [ ] You can describe the two-sided compaction that `wait` performs, and say exactly what triggers it.

  <details><summary>Answer</summary>

  Deletions first: `GrB_transpose(m, dm, NULL, m, GrB_DESC_RSCT0)`
  at `delta_wait.c:29`, then `GrB_Matrix_clear(dm)` at `:33`. The
  descriptor is `REPLACE + STRUCTURE + COMP + TRAN(input 0)`, so
  the two transposes cancel and the net effect is "copy m into
  itself, keeping only what dm does not mark" — the whole tombstone
  sweep in one call. Then additions:
  `GrB_Matrix_assign(m, dp, NULL, dp, GrB_ALL, 0, GrB_ALL, 0,
  GrB_DESC_S)` at `:51-52`, then clear at `:56`.

  The trigger is **`delta_wait.c:89` and `:97`** — `dm_nvals >=
  delta_max_pending_changes` and `dp_nvals >= …`, two independent
  tests against `DELTA_MAX_PENDING_CHANGES_DEFAULT` = 10,000
  (`config.h:19`), with `force_sync` bypassing both at `:71-73`.

  It is **not** `delta_will_wait.c`. That file asks SuiteSparse
  `GxB_WILL_WAIT` on all three matrices (`:35`, `:39`, `:43`) —
  whether the *library* has zombies or pending tuples — and is used
  as an assertion at `delta_wait.c:108-110` that both deferral
  systems are drained after the three `GrB_wait` calls at
  `:103-105`.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including your Rust representation choice for DP/DM.

  <details><summary>Answer</summary>

  Design against three measured numbers rather than taste: the
  write path is 4 GrB calls (Step 3), the fold threshold is 10,000
  entries (`config.h:19`), and at that threshold the mxm algebra
  costs 0.43% (Step 5).

  The threshold curve is the real decision. Amortized fold cost per
  mutation is N/T — at N = 100e6 and T = 10,000 that is 10,000
  entries rewritten per edge, against ~50e6 for an eager splice, a
  5,000× improvement. Halving T doubles the fold cost but halves
  the mxm overhead and shortens the read path; raising it to
  100,000 cuts the fold to 1,000 entries per edge but pushes mxm
  overhead to ~4.3%. Pick a point on that curve and say why.

  </details>

## References

**Code**

- [FalkorDB](https://github.com/FalkorDB/FalkorDB) at `ccb449a9a` —
  `src/graph/delta_matrix/` (23 files). Start with the state table
  in `delta_matrix.h:26-106` (it IS the design doc), then
  `delta_wait.c` (218 lines), `delta_mxm.c` (121 lines),
  `delta_isStored.c`, `delta_set_element_bool.c`,
  `delta_remove_element.c`, `delta_will_wait.c`. The flush constant
  is `src/configuration/config.h:19`. Full anchor table above.
- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  at `1fd5475` — the layer underneath.
  `Source/convert/GB_conform.c:157-160` and `:175-184` are the two
  cases `delta_new.c`'s pins select;
  `Source/builtin/include/GB_Matrix_content.h:241-274` is the
  hyper_hash it disables, `:361` and `:367-391` are the pending
  tuples and zombies it declines to rely on, `:386-389` is the
  implicit-wait behaviour that motivates the whole layer.
  `Include/GraphBLAS.h:666` and `:668` are `GrB_DESC_RSC` and
  `GrB_DESC_RSCT0`. Walked in
  [reading-suitesparse-internals.md](reading-suitesparse-internals.md).

**Papers**

- Davis, T. A. — "Algorithm 1000: SuiteSparse:GraphBLAS", ACM TOMS
  45(4), 2019. §3.1.8 is the O(e log e) bound this layer declines
  to depend on; §4.1 defines zombies and pending tuples. Read in
  [reading-davis-toms19.md](reading-davis-toms19.md).

**Measured, in this repo**

- `topics/20-graphblas/notes.md:41-42` — 80.4 MB → 1.59 MB and
  11,312 µs → 66 µs. The reason DP and DM are pinned hypersparse.
- `topics/24-graph-algorithms/notes.md:5-7` — RMAT scale 16, mean
  degree 27.8. Step 5's overhead arithmetic runs on it.
