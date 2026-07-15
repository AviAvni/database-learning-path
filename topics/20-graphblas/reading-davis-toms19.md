# SuiteSparse:GraphBLAS: a sparse-matrix executor in disguise

Davis's TOMS '19 system paper (plus the '23 v2 update) describes the
library under FalkorDB. Read it as an executor-design paper, not a
math paper: it's about lazy evaluation, format polymorphism, and
kernel dispatch — the same problems as topics 8-11, in matrix
clothing. Before you open it, this chapter builds the six concepts
the paper assumes, one at a time — then hands you a reading route
and the numbers worth retaining.

## The problem in one sentence

One `GrB_mxm` call must behave well whether the operand is a
10M×10M matrix with 100K entries or with 1B entries — a density
range of four orders of magnitude — and must absorb millions of
single-entry mutations without restructuring a packed array each
time; this paper is the design that does both behind one opaque
handle.

## The concepts, step by step

### Step 1 — a graph is a sparse matrix; store only what exists

A **sparse matrix** is one where almost every cell is zero/absent,
so you store only the present entries — each one an
`(row, col, value)` fact. A graph maps onto this directly: the
**adjacency matrix** A has A(i,j) present iff there's an edge
i→j, so "the graph" and "the matrix" are the same object. The count
of present entries is **nnz** ("number of nonzeros") — the number
that drives every cost in this topic.

Concrete: 10M nodes stored densely as booleans is 10M × 10M =
100 *trillion* cells (~12.5 TB at a bit each). The same graph with
100M edges stored sparsely is ~100M entries — roughly 1 GB with
32-bit indices. Everything in this paper is machinery for
exploiting the zeros you didn't store.

### Step 2 — the format ladder: one matrix, four representations

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
 (10M×10M with   the graph default    ~4-8% of   present
  100K edges)                         n×m
```

**Hypersparse** matters most to FalkorDB: node IDs are a shared
namespace across all relation types, so most rows of any one
relation matrix are empty — and plain CSR's `rowptr` alone for 10M
nodes is 80 MB *per relation type*, before storing a single edge.
Hypersparse stores only the list of non-empty rows and their
pointers, so an almost-empty 10M×10M matrix costs KBs, not tens of
MBs. The switches between rungs are decided by two per-matrix
knobs (`hyper_switch`, `bitmap_switch`) applied after every
operation — the internals chapter reads that code.

### Step 3 — semiring, mask, accum: the GraphBLAS ops are executor concepts

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

Why it matters: the paper's §3 describes these as an API; you
should read them as an *operator algebra* — the same shape as a
relational executor's, which is question 1 below.

### Step 4 — lazy mutation: zombies and pending tuples

CSR's packed arrays make single-entry mutation expensive: deleting
one edge means splicing `colidx` (O(nnz) memmove), inserting one
means the same. SuiteSparse's answer is to *not do it yet*:

- a **zombie** is a deleted-but-still-present entry — deletion just
  flags it in place;
- a **pending tuple** is an inserted-but-unsorted entry — insertion
  appends to a side list, never touching the CSR.

The whole mechanism, distilled:

```rust
fn set_element(a: &mut Matrix, i: u64, j: u64, v: f64) {
    a.pending.push((i, j, v));       // O(1): append, don't restructure CSR
}

fn delete_element(a: &mut Matrix, i: u64, j: u64) {
    if let Some(e) = a.find_mut(i, j) {
        e.mark_zombie();             // flag in place — no O(nnz) splice
    }
}

fn wait(a: &mut Matrix) {            // the GrB_wait boundary
    a.prune_zombies();               // one sweep drops ALL zombies
    a.pending.sort_unstable();       // n inserts → one sort + one merge,
    a.merge_pending_into_csr();      //   not n binary-searched splices
    conform(a);                      // then maybe switch format
}
```

The cost shape: n single inserts done eagerly = n O(nnz) splices;
done lazily = n O(1) appends + one sort + one merge. This is the
LSM memtable move (topic 3) inside a matrix library — and it's the
library's OWN delta mechanism, which makes FalkorDB's delta
matrices (this topic §5) look redundant until you ask who controls
the flush. Question 2.

### Step 5 — non-blocking mode: the object model assembled

The GraphBLAS spec allows every operation to return before doing
work; the deferred state is reconciled at **`GrB_wait`**
boundaries — or forced implicitly by any operation that needs to
*read* the matrix. Assembling steps 2-4, the opaque handle looks
like:

```
 GrB_Matrix = opaque header
   ├─ format: hypersparse | sparse | bitmap | full   (×2: by row/col)
   ├─ pending tuples + zombies       (lazy mutation!)
   ├─ hyper_switch / bitmap_switch   (per-matrix knobs)
   └─ iso flag (all values equal — store ONE value)   ← step 6
```

SuiteSparse uses non-blocking mode for *mutation batching*
(pending tuples get sorted+merged once), not full lazy fusion (the
v2 paper discusses the JIT changing this calculus). Compare topic
27's incremental view maintenance: same "amortize small updates"
shape. The cost to remember: the flush point is chosen by the
*library* (any read can trigger it), not by the application — the
single fact that motivates FalkorDB's own delta layer.

### Step 6 — the v2 update (TOMS '23): JIT, small indices, iso values

Three changes since 2019, each a concrete constant-factor win:

- the **CPU JIT** (topic 19's jitifyer) — user-defined
  types/semirings now compile to specialized kernels at runtime
  and run at factory speed, instead of through a
  function-pointer-per-element fallback;
- **32/64-bit integer indices chosen per matrix** (v10) — halves
  index memory for graphs under 4B edges, i.e. all of ours;
- **iso-valued matrices** — an **iso** matrix is one whose entries
  all hold the same value, so it stores the pattern plus ONE
  scalar and ZERO bytes of per-entry values. An unweighted graph
  (A(i,j)=true for all edges) is exactly this.

Iso + the (ANY,PAIR) semiring is why BFS over an unweighted
FalkorDB relation matrix moves no value data at all — pattern in,
pattern out. Question 4 traces that path.

## How to read the paper (with the concepts in hand)

- **TOMS '19, §3 (object model)** — read closely. It's steps 2, 4,
  and 5 in the authors' words; the code counterpart is one header,
  `Source/matrix/GB_matrix.h`. Keep asking "what executor concept
  is this?" (step 3's mapping — question 1).
- **TOMS '19, non-blocking mode discussion** — read closely
  against step 5; note every place an implicit wait can fire.
- **TOMS '23 (the v2 update)** — read for the three items in
  step 6; the JIT sections connect directly to topic 19.

Numbers to retain while you read:

- format switch defaults: bitmap when nnz > ~4-8% (op-dependent),
  hyper when non-empty vectors < hyper_switch × nrows (~1/16)
- saxpy3 hash→Gustavson threshold: hash table > m/16 ⇒ Gustavson
  (the internals chapter reads this code)
- mxm engines: dot3 work ∝ nnz(M); saxpy3 work ∝ flops — the mask
  changes the complexity CLASS, not a constant

## Questions for notes.md

1. Map GrB objects to executor concepts: semiring ↔ ?, mask ↔ ?,
   accum ↔ ?, descriptor ↔ ? (operator, semi-join filter, UPDATE
   expression, query hints — defend each).
2. Zombies+pending vs FalkorDB's DP/DM: why does FalkorDB need its
   OWN deltas when the library already has them (control over WHEN
   wait happens; transposed pair kept in lockstep; readers must see
   pre-wait state — which reason dominates)?
3. The iso optimization: which FalkorDB matrices are iso (adjacency
   bool — yes; relation with edge IDs as values — no). What does
   losing iso cost on mxm bandwidth (values move again — 8×?)?
4. Trace one BFS step through the v2 machinery: iso bool matrix,
   ANY_PAIR semiring, sparse frontier — which engine runs
   (saxpy3/SpMSpV), and what does the JIT specialize away?
5. 32-bit indices (v10): for a 10M-node 100M-edge graph, compute
   the CSR memory in v9 (64-bit) vs v10 — and where the same 2×
   shows up in our Rust CSR if we switch usize→u32.

## References

**Papers**
- Davis — "Algorithm 1000: SuiteSparse:GraphBLAS: Graph Algorithms
  in the Language of Sparse Linear Algebra" (ACM TOMS 2019) — the
  system paper; read §3 (object model) and the non-blocking-mode
  discussion closely
- Davis — "Algorithm 1037: SuiteSparse:GraphBLAS: Parallel Graph
  Algorithms in the Language of Sparse Linear Algebra" (ACM TOMS
  2023) — the v2 update: JIT, 32/64-bit indices, iso matrices

**Code**
- [SuiteSparse:GraphBLAS](https://github.com/DrTimothyAldenDavis/GraphBLAS)
  `Source/matrix/GB_matrix.h` — the object model in one header;
  the internals walk is
  [reading-suitesparse-internals.md](reading-suitesparse-internals.md)
