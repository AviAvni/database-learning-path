# Reading guide — FalkorDB's delta matrices (`~/repos/FalkorDB/src/graph/delta_matrix/`)

Your own code, with this curriculum's eyes. The delta matrix is
topic 3's LSM memtable+tombstone pattern rebuilt over GrB matrices
— read it against topics 3 (LSM), 6 (buffer mgmt), and this
topic's zombies/pending-tuples machinery, and ask at each step
"why not just let SuiteSparse's own deltas do this?"

## Anchor map

| anchor | what it is |
|---|---|
| delta_matrix.h:34-108 | the state-transition comment table (A/DP/DM invariants per op) — the spec |
| delta_matrix.h:110-116 | the struct: M + delta_plus + delta_minus + transposed twin |
| delta_matrix.h:17-22 | accessor macros incl. the T* transposed trio |
| delta_wait.c:13-33 | sync_deletions: `GrB_transpose(m, dm, NULL, m, GrB_DESC_RSCT0)` — transpose-as-masked-copy |
| delta_wait.c:36-46+ | sync_additions: fold DP into M, clear DP |
| delta_mxm.c:44-99 | `(A*(M+DP))<!A*DM>` — multiply WITHOUT forcing a sync |
| delta_get_set.c / delta_isStored.c | the 3-way read path (check DM, DP, M) |
| delta_will_wait.c | "would GrB_wait do work?" — the flush-decision probe |

## 1. The core invariant (the header comment IS the design doc)

delta_matrix.h:34-108 walks every operation through a worked
example. Distilled:

```
 logical A  ≡  (M ∪ DP) \ DM
 invariants: DP ∩ M = ∅   (additions are NEW entries)
             DM ⊆ M       (you can only pending-delete settled entries)
             delete of a DP entry clears DP directly (:99) — never
             passes through DM
 the transposed twin maintains the same trio, updated in lockstep
```

Same read algebra as an LSM point-read (memtable ∪ sstables minus
tombstones), and `wait` is minor compaction.

## 2. Why not SuiteSparse's own pending tuples? (the load-bearing question)

SuiteSparse already defers mutations (zombies + pending tuples,
reading-davis-toms19.md §1). The delta layer exists because:

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

## 3. delta_mxm — algebra instead of a flush

delta_mxm.c:44-86: to compute C = A*B where B has pending state,

```
 accum = A * DP            (:86 — the additions' contribution)
 mask  = A * DM  (ANY_PAIR bool, :74 — rows poisoned by deletions)
 C     = (A * M) + accum, masked by !mask     — "(A*(M+DP))<!A*DM>"
```

Two extra small multiplies instead of one big compaction — the
LSM read-amplification-vs-compaction trade, chosen per multiply.
Note the mask is *coarse*: A*DM marks any output touched by a
deleted edge, potentially over-masking; check how the caller
compensates (question 3).

## 4. wait — the two-sided compaction

delta_wait.c: deletions first (`GrB_transpose(m, dm, NULL, m,
GrB_DESC_RSCT0)` — a transpose of m into itself, masked by the
COMPLEMENT of dm, T0 transposing the transpose away: a masked
copy that drops deleted entries in one library call), then
additions (assign/eWiseAdd DP into M), then clear both. The
`Delta_Matrix_wait` policy decision — sync now vs stay lazy —
consults nvals thresholds (delta_will_wait.c): compaction
triggering by size, topic 3 again.

## 5. What transfers to M20

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
5. For M20: pick DP/DM's representation in Rust. COO Vec<(u32,u32)>
   + sort at wait (LSM-flavored) vs HashMap (point-read-flavored)
   — which do the LDBC interactive update+read mixes prefer?
   Predict, then bench both under gb_bench's update workload.
