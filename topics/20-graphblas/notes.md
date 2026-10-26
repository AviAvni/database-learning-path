# Topic 20 notes — sparse linear algebra & GraphBLAS internals

## Baseline (provided kernels, Apple M3 Pro, measured 2026-07-10)

gb_bench, RMAT edge_factor 8, best-of-N. The generators are seeded, so n, nnz, flops, nnz(C) and the index bytes reproduce exactly; the timings are this run's (`./verify.sh 20` gives your machine's).

### SpMV (PLUS,TIMES) — the bandwidth story

| scale | n | nnz | µs | GB/s |
|---|---|---|---|---|
| 14 | 16K | 120K | 146 | 19.1 |
| 16 | 65K | 495K | 617 | 18.6 |
| 18 | 262K | 2.0M | 2547 | 18.3 |
| 20 | 1.05M | 8.2M | 11958 | 15.8 |

~16-19 GB/s single-thread — below the ~30 GB/s streaming baseline
(topic 0/13) because the x-gathers are random: RMAT colidx sprays
across the vector. Slight decay with scale = x outgrowing L2/LLC.

### SpGEMM C=A*A — hash reference (SPA stub pending)

| scale | nnz(A) | flops | nnz(C) | hash ms | Mflop/s |
|---|---|---|---|---|---|
| 10 | 6.7K | 298K | 142K | 3.9 | 76 |
| 12 | 28.7K | 2.27M | 1.14M | 33.0 | 69 |
| 14 | 120K | 17.1M | 8.9M | 279.4 | 61 |

~60-75 Mflops/s — HashMap entry ≈ 15 ns/flop: hashing + probe +
per-row alloc/sort dominate. Note compression ratio flops/nnz(C)
≈ 2: RMAT A² produces mostly-distinct pairs, so the accumulator
rarely accumulates (hash's worst case, SPA's too).

### BFS scalar oracle

rmat18 3308 µs (2M edges ⇒ ~1.6 ns/edge), uniform-256K 6446 µs,
path-100K 2041 µs (~20 ns/hop — pure dependent-load latency, no
parallelism available: topic 13's pointer chase).

### Hypersparse — the headline

10M-node id space, 100K edges: index bytes **80.4 MB CSR vs 1.59 MB
hyper (50×)**; full sweep **11312 µs vs 66 µs (171×)** — iterating
the id space vs iterating what exists. This is why every FalkorDB
relation matrix is hypersparse.

### GraphBLAS on the same matrices — `gb_container` (measured 2026-10-26, Apple M3 Pro)

`cargo run --release --bin gb_container` links a SuiteSparse:GraphBLAS v10
install (`GRAPHBLAS_DIR`, a standard prefix, or FalkorDB's
`~/.cache/falkordb/native-deps/graphblas/<hash>/` cache; the run below used a
10.5.0 build from that cache — the Container source read in
[reading-gxb-container.md](reading-gxb-container.md) is the pinned 10.5.2) and
prints `[skip — libgraphblas not found]` and exits 0 when there is none. The
matrices are the RMAT scale-14/16/18/20, edge-factor-8, seed-42 ones the SpMV
sweep above uses, so nnz is identical (120,128 / 494,634 / 2,018,190 /
8,175,704). Load average was 24–26 (other agents' benchmarks); the
Container rows and the order of the others are stable across three runs, the
single-thread mxv digits are not (the scale-14 JIT-on column read 63 µs in one
run and 150 µs in another).

**Hand-off (lane A; the full table and its reading are in the guide's Step 7).**
`GxB_load_Matrix_from_Container` of the Rust `Csr` (readonly, rowptr `u64`,
colidx `u32`) takes **0.12–0.17 µs at every size**; `GrB_Matrix_build` of the
same entries takes 349 → 6,822 µs in CSR order and 9,384 → 146,676 µs shuffled;
`extractTuples` 118 → 4,039 µs; a `memcpy` of the CSR 21 → 2,355 µs.

**The kernel against the library — lane B.** Rust `spmv::spmv` (single thread)
against `GrB_mxv` with the `GrB_PLUS_TIMES_SEMIRING_FP64` semiring on the
*Rust* CSR loaded through the Container, y = A·x with x = `i % 7`. GB/s uses
`spmv::spmv_bytes` for every column. `sum(y)` of the library's result equals
the Rust kernel's to 1e-9 relative on every row (the lane asserts it).

| scale | Rust µs (GB/s) | `GrB_mxv` 1 thread, JIT off | 1 thread, JIT on | 11 threads, JIT on |
|---|---|---|---|---|
| 14 | 116 (24.2) | 502 (5.6) | 150 (18.6) | 65 (42.8) |
| 16 | 568 (20.2) | 2,132 (5.4) | 452 (25.3) | 191 (60.1) |
| 18 | 2,796 (16.7) | 8,712 (5.4) | 2,647 (17.6) | 759 (61.4) |
| 20 | 12,608 (15.0) | 44,868 (4.2) | 11,905 (15.9) | 2,864 (65.9) |

- **With the JIT off the library loses 3–4×** (3.1–4.3× slower than the Rust
  loop). `GxB_BURBLE` says why: every call printed `GrB_mxv C=A'*B,
  dot_product (dot2) ... (generic C=A'*B, C bitmap)` — the *generic* kernel,
  which calls the semiring's multiply and add through function pointers per
  entry. This install has no pre-compiled factory kernel for FP64 PLUS_TIMES on
  this path, so the JIT-off number is "the library when it cannot compile".
  Whether FalkorDB's own build behaves the same was not checked.
- **With the JIT on it is at parity on one thread**: from 1.3× slower
  (scale 14, a noisy cell) to 1.26× faster (scale 16) and 0.94–0.95× the Rust
  time at scales 18 and 20. The kernel was compiled during the warm-up call
  and is not in the timing. The Rust loop is not slow code — it reaches 15–24
  GB/s, near the ~16–19 GB/s the baseline above recorded — so the library's
  compiled dot kernel is about as fast as a plain gather loop, no faster:
  SpMV with a dense x is bandwidth- and gather-bound, and neither side has a
  trick for the gather.
- **Parallelism is where the library pulls ahead**: 11 OpenMP threads run
  1.8×, 3.0×, 3.7× and 4.4× faster than the single-thread Rust kernel at
  scales 14–20 (65.9 GB/s of the byte count at scale 20). That compares one
  thread against eleven; the fair rayon-vs-OpenMP comparison is the topic's
  M20 exercise ([reading-openmp-vs-rayon.md](reading-openmp-vs-rayon.md)).
- A matrix GraphBLAS built itself (`GrB_Matrix_build`) multiplies at the same
  speed as the Container-loaded Rust one (JIT off: 490 / 2,135 / 8,750 /
  50,361 µs against 502 / 2,132 / 8,712 / 44,868), so a shallow matrix is not
  handicapped in this operation.
- The output y has one entry per non-empty row of A (9,256 of 16,384 rows at
  scale 14): GraphBLAS stores no entry where a row has no product term, and the
  lane checks that count.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| SPA vs hash at scale 14 (SPA array = 16K×12B, fits L2) — speedup ×? | | |
| SPA at scale 20 (1M×12B = 12 MB SPA, out of L2) — still wins? | | |
| push-BFS edge checks on rmat18 vs scalar's nnz — ratio | | |
| diropt on rmat18: which levels flip to pull, checks saved ×? | | |
| diropt on uniform graph — does pull trigger at all? | | |
| pull-only on path-100K — catastrophic by ×? (n probes per level) | | |

## Implementation log

- [ ] spgemm_spa (symbolic+numeric, stamp-marked SPA) — test green
- [ ] bfs_push / bfs_pull / bfs_diropt — tests green incl. path-stays-push
- [ ] per-level trace analyzed vs LAGraph α/β thresholds
- [ ] stretch: masked SpGEMM (triangle count `C<L>=L*L`) — dot vs saxpy
- [ ] prediction table reconciled

Surprises / dead ends:

## Questions from the reading guides

### Davis TOMS '19/'23 (reading-davis-toms19.md)

1. GrB objects ↔ executor concepts mapping:
2. Why FalkorDB needs own deltas over zombies/pending:
3. Which FalkorDB matrices are iso; cost of losing iso:
4. BFS step through v2 machinery (engine + JIT specialization):
5. 32-bit index memory math v9 vs v10:

### SuiteSparse internals (reading-suitesparse-internals.md)

1. FalkorDB's GxB_set sparsity pins:
2. Fine-task atomics vs coarse — topic 11 analogue:
3. Hash-task underestimate handling vs SwissTable resize:
4. dot3 vs saxpy3 cost estimate for triangle counting:
5. SPA-vs-hash crossover scale measured:

### Gustavson + survey (reading-gustavson-spgemm.md)

1. Why flops = Σ nnz(B(k,:)) is the lower bound; ANY short-circuit:
2. SPA-vs-hash crossover from cache numbers:
3. When guess-and-grow beats symbolic+numeric:
4. Outer-product = LSM runs + merge:
5. Masked saxpy vs masked dot on `C<L>=L*L`:

### Beamer SC '12 (reading-beamer-sc12.md)

1. Measured wasted-check fraction at peak level:
2. Why early exit needs ANY/OR; which semirings break:
3. Road vs RMAT pull prediction + trace check:
4. Which FalkorDB query shapes need AT besides pull-BFS:
5. Why LAGraph β2=512 vs paper's 24:

### LAGraph (reading-lagraph.md)

1. Switch-heuristic inputs and their maintenance cost:
2. Frontier format at peak level (conform reasoning):
3. dot3 + tril mask visits each wedge once — spell out:
4. PageRankGAP prescaling savings:
5. M20 semiring per BFS variant (parent vs level):

### FalkorDB delta matrices (reading-falkordb-delta-matrix.md)

1. 4×2 case table verified against set/remove code:
2. Transposed-twin write cost; lazy-rebuild breakage:
3. delta_mxm over-masking counterexample + caller compensation:
4. Sync thresholds ↔ LSM L0 triggers:
5. DP/DM representation for M20 (COO+sort vs HashMap) — bench:

## Cross-topic threads

- SpMV's 16-19 GB/s vs sum's 30+ GB/s (topic 0): the gather tax —
  same lesson as hash probing (topic 8) and pointer chasing (13).
- saxpy3 flopcount pre-pass = cudf size/retrieve (18) = Gustavson
  symbolic phase (1978): sparse output size, the eternal villain.
- Delta matrices = LSM (3): memtable=DP, tombstones=DM, wait=minor
  compaction, delta_mxm=read-path merge.
- Direction switch = three-level dispatch: algorithm (push/pull),
  engine (saxpy/dot), format (sparse/bitmap vector) — SuiteSparse
  makes the same call at each layer.
- ANY_SECONDI's benign nondeterminism = Gunrock's lost-CAS (18) =
  the ANY monoid making races algebra.

## M20 log (capstone)

- [ ] kernel core: CSR+hypersparse, SpMV/SpMSpV, masked dot-SpGEMM
      subset, semirings (ANY,PAIR)/(PLUS,TIMES)/(MIN,PLUS)
- [ ] delta trio + transposed twin + wait + delta_mxm fold over it
- [ ] LDBC bench vs reference graphblas layer; BFS switch parity

## Done when

- All 4 stub tests green; SPA-vs-hash + BFS traces + wasted-check
  numbers in tables; prediction table reconciled; guide questions
  answered; M20 kernel-core design sketched from the measurements.
