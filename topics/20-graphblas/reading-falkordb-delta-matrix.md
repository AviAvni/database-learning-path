# Delta matrices: an LSM memtable over GraphBLAS

FalkorDB's answer to "GrB matrices are fast to read, slow to mutate
one edge at a time" — your own code, with this curriculum's eyes.
The delta matrix is topic 3's LSM memtable+tombstone pattern rebuilt
over GrB matrices. This chapter builds the design step by step —
the mutation problem, the three-matrix trio, its invariants, the
load-bearing "why not the library's own deltas" question, the
masked-multiply fold, and the compaction — then hands you the
anchors into `src/graph/delta_matrix/` to verify each piece.

## The problem in one sentence

Deleting or inserting one edge in a packed sparse matrix means
splicing contiguous arrays — O(nnz) work, potentially hundreds of
MBs moved, for a one-edge change — and a graph database takes
single-edge writes *continuously* while readers expect
multiply-speed reads.

## The concepts, step by step

### Step 1 — the mutation problem: packed arrays hate point writes

A settled `GrB_Matrix` in sparse/hypersparse form is CSR-like:
contiguous row-pointer and column-index arrays, packed with no
slack. That's exactly what makes reads and multiplies fast — and
exactly what makes one-edge mutation expensive: inserting edge
(i,j) means shifting every index after it, deleting means the
same splice in reverse. For a 100M-edge relation matrix, one edge
insert done eagerly is an O(100M) memmove. The generic fix, seen
in topic 3 (LSM) and in SuiteSparse's own zombies/pending tuples:
don't restructure — *record the change somewhere cheap and merge
later*.

### Step 2 — the trio: settled matrix plus two delta matrices

FalkorDB's delta matrix keeps three GrB matrices (plus the same
three transposed):

```
 Delta_Matrix = M (settled GrB_Matrix, hypersparse CSR)
              + delta-plus  DP (pending additions)
              + delta-minus DM (pending deletions)
              + the same trio TRANSPOSED        (delta_matrix.h:110-113)
```

M is big and packed; DP and DM are tiny (bounded by the write
batch since the last sync), so mutating them is cheap. The logical
matrix the rest of the engine sees is defined algebraically:
**A ≡ (M ∪ DP) \ DM** — everything settled or pending-added,
minus everything pending-deleted. Same read algebra as an LSM
point-read (memtable ∪ sstables minus tombstones — DM's entries
are exactly **tombstones**, deletion markers that suppress a
still-physically-present entry).

### Step 3 — the invariants, and the read/write paths

The header comment (delta_matrix.h:34-108) walks every operation
through a worked example — it IS the design doc. Distilled:

```
 logical A  ≡  (M ∪ DP) \ DM
 invariants: DP ∩ M = ∅   (additions are NEW entries)
             DM ⊆ M       (you can only pending-delete settled entries)
             delete of a DP entry clears DP directly (:99) — never
             passes through DM
 the transposed twin maintains the same trio, updated in lockstep
```

The invariants keep every entry in exactly one state, so reads
never need conflict resolution. The read and write paths,
distilled:

```rust
// logical A ≡ (M ∪ DP) \ DM — an LSM point-read over matrices
fn contains(&self, i: u64, j: u64) -> bool {
    if self.dm.contains(i, j) { return false; }   // tombstone wins
    self.dp.contains(i, j) || self.m.contains(i, j)
}

fn set(&mut self, i: u64, j: u64) {
    if self.m.contains(i, j) {
        self.dm.remove(i, j);        // resurrect a pending-deleted entry
    } else {
        self.dp.set(i, j);           // NEW entry → DP (keeps DP ∩ M = ∅)
    }
    self.transposed.set(j, i);       // the twin trio, in lockstep
}
```

What it costs: every read is a 3-way check (read amplification),
every write touches two trios (the twin doubles write work —
question 2). What it buys: O(1)-ish writes against a matrix that
stays multiply-ready.

### Step 4 — why not SuiteSparse's own pending tuples? (the load-bearing question)

SuiteSparse already defers mutations (zombies + pending tuples,
[reading-davis-toms19.md](reading-davis-toms19.md) step 4). The
delta layer exists because:

1. **flush control**: ANY GrB read op can force internal wait;
   FalkorDB needs reads that DON'T flush (readers under a write
   lock, MVCC-ish semantics) — DP/DM are ordinary matrices the
   library never touches implicitly.
2. **the transposed twin**: SuiteSparse maintains ONE matrix;
   FalkorDB needs M and Mᵀ synced under the same deltas
   (delta_matrix.h:20-22) — pull traversals are always available
   (`<-[]-` patterns).
3. **bounded sync cost**: wait folds a SMALL DP/DM (bounded by
   write-batch size) — library pending tuples can degrade into a
   full rebuild inside an unrelated query.

The general lesson: a lower layer's deferred-work mechanism is
only reusable if you control *when* it fires and *what
invariants* it maintains — otherwise you rebuild it one level up,
which is exactly what happened here.

### Step 5 — delta_mxm: algebra instead of a flush

The expensive operation on a delta matrix is a multiply — must the
deltas be folded into M first? delta_mxm.c:44-86 says no: fold the
pending state into the *algebra* of one multiply. To compute
C = A*B where B carries pending state:

```
 accum = A * DP            (:86 — the additions' contribution)
 mask  = A * DM  (ANY_PAIR bool, :74 — rows poisoned by deletions)
 C     = (A * M) + accum, masked by !mask     — "(A*(M+DP))<!A*DM>"
```

Two extra *small* multiplies (DP and DM are tiny) instead of one
big compaction — the LSM read-amplification-vs-compaction trade,
chosen per multiply. Note the mask is *coarse*: A*DM marks any
output touched by a deleted edge, potentially over-masking; check
how the caller compensates (question 3).

### Step 6 — wait: the two-sided compaction

`Delta_Matrix_wait` is the compaction that folds the deltas into M
and resets the trio. Deletions first: `GrB_transpose(m, dm, NULL,
m, GrB_DESC_RSCT0)` — a transpose of m into itself, masked by the
COMPLEMENT of dm, with T0 transposing the transpose away: one
library call that copies M minus its tombstoned entries. Then
additions (assign/eWiseAdd DP into M), then clear both deltas
(delta_wait.c:13-46).

The policy decision — sync now vs stay lazy — consults nvals
thresholds (delta_will_wait.c: "would GrB_wait do work?"). That's
compaction triggering by size, topic 3 again: small thresholds =
low read amplification but frequent O(nnz(M)) folds; large =
cheap writes but every read/multiply pays the 3-way tax longer.

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| delta_matrix.h:110-116 | 2 | the struct: M + delta_plus + delta_minus + transposed twin |
| delta_matrix.h:17-22 | 2 | accessor macros incl. the T* transposed trio |
| delta_matrix.h:34-108 | 3 | the state-transition comment table (A/DP/DM invariants per op) — the spec |
| delta_get_set.c / delta_isStored.c | 3 | the 3-way read path (check DM, DP, M) |
| delta_mxm.c:44-99 | 5 | `(A*(M+DP))<!A*DM>` — multiply WITHOUT forcing a sync |
| delta_wait.c:13-33 | 6 | sync_deletions: `GrB_transpose(m, dm, NULL, m, GrB_DESC_RSCT0)` — transpose-as-masked-copy |
| delta_wait.c:36-46+ | 6 | sync_additions: fold DP into M, clear DP |
| delta_will_wait.c | 6 | "would GrB_wait do work?" — the flush-decision probe |

Navigation advice: start with the state-transition comment table
in `delta_matrix.h:34-108` (it IS the design doc), then
`delta_wait.c`, `delta_mxm.c`, `delta_get_set.c`,
`delta_will_wait.c` — read each against topics 3 (LSM), 6 (buffer
mgmt), and this topic's zombies/pending-tuples machinery, asking
at each step "why not just let SuiteSparse's own deltas do this?"

### What transfers to M20

M20 rebuilds this over OUR kernels: the trio + transposed twin,
the read algebra in get/extract, the mxm fold, threshold-driven
wait. The reference is the spec; the interesting freedom is
choosing DP/DM's format (hash-of-pairs? small COO? bitmap?) now
that we own the representation — measure against `GrB_Matrix` DP
via the LDBC update workloads.

## Questions for notes.md

1. Verify the invariants against delta_set_element_bool.c and
   delta_remove_element.c: enumerate the 4 cases (entry in M, in
   DP, in DM, absent) × (set, remove) — which transitions does the
   header table at :34-108 show, and are any missing?
2. The transposed twin doubles write work on every mutation. Cost
   it: per set_element, how many GrB calls hit each trio — and
   what would break if the transpose were rebuilt lazily at wait
   instead (pull traversals see stale AT between waits)?
3. delta_mxm's mask A*DM over-masks (kills a full output entry if
   ANY contributing edge is deleted — but other, live edges might
   also produce it). Find how correctness is restored (recompute
   masked region against (M+DP)\DM? restrict when delta_mxm is
   used at all — check callers in graph/graph.c) — and write the
   counterexample matrix that exposes it.
4. delta_will_wait / the sync thresholds: what nvals bounds
   trigger a flush, and how do they map to LSM L0 file-count
   triggers (write-visible latency vs read amplification)?
5. For M20: pick DP/DM's representation in Rust. COO
   `Vec<(u32,u32)>` + sort at wait (LSM-flavored) vs HashMap
   (point-read-flavored) — which do the LDBC interactive
   update+read mixes prefer? Predict, then bench both under
   gb_bench's update workload.

## References

**Code**
- [FalkorDB](https://github.com/FalkorDB/FalkorDB)
  `src/graph/delta_matrix/` — start with the state-transition
  comment table in `delta_matrix.h:34-108` (it IS the design doc),
  then `delta_wait.c`, `delta_mxm.c`, `delta_get_set.c`,
  `delta_will_wait.c`
