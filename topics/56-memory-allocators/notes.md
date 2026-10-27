# Topic 56 notes — memory allocators for database workloads

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin alloc_bench`, and the same with `--features
mimalloc`, `--features jemalloc`, and `JEMALLOC_SYS_WITH_LG_QUANTUM=3` with
`--features jemalloc`. The trace is seeded (`trace.rs`, seed 56): the object
count (1,797,141), the live bytes (80,949,568 B = 77.2 MiB) and the edge count
(997,374) reproduce exactly on any machine. Timings are the best of three fresh
child processes; memory columns are the median of the same three. All five
timed runs were taken back to back under the machine-wide benchmark lock with
the load average at start recorded:

| run | load average (1/5/15) at start |
|---|---|
| system allocator, stub build (README) | 4.72 4.89 7.68 |
| system allocator, reference lanes | 4.72 4.89 7.68 |
| mimalloc | 4.74 4.89 7.67 |
| jemalloc, default quantum | 4.74 4.89 7.67 |
| jemalloc, `--with-lg-quantum=3` | 4.76 4.90 7.65 |

### Lane 1a — bulk load (resident MiB after loading 77.2 MiB live)

| `global` allocator | ns/alloc | resident | res/live | arena `bump` row (ns, res/live) |
|---|---|---|---|---|
| libmalloc | 14.0 | 86.3 | 1.12 | 4.8, 1.00 |
| mimalloc 2.x | 8.2 | 86.8 | 1.12 | 4.2, 1.00 |
| jemalloc 5.3 (quantum 16) | 10.1 | 87.0 | 1.13 | 4.9, 1.02 |
| jemalloc 5.3 (quantum 8, as Redis builds it) | 9.6 | 78.2 | **1.01** | 4.8, 1.02 |

- Edge records are 40 B; 997,374 × 8 B = **7.6 MiB** of padding under a 16-byte
  quantum. 87.0 − 78.2 = 8.8 MiB saved by `--with-lg-quantum=3` (10.1%).
  (The arena's 1.02 under the jemalloc builds is the chunk allocator's own
  16-byte alignment of its 1 MiB chunks, not the trace.)
- `footprint` (`ri_phys_footprint`) equalled `resident` to within 0.1 MiB in
  every row; on this machine in these runs compression did not separate them.

### Lane 1b — churn (3 rounds: free a random 50%, insert replacements, properties grown)

Live bytes after churn: 132.1 MiB in every row.

| `global` allocator | ns/op | res1 (MiB) | res1/live1 | range over 3 processes |
|---|---|---|---|---|
| libmalloc (stub-build binary) | 29.6 | 216.2 | **1.64** | 1.64–1.64 |
| libmalloc (reference-lanes binary) | 30.1 | 150.0 | **1.14** | 1.13–1.64 |
| mimalloc 2.x | 19.2 | 152.1 | 1.15 | 1.15–1.15 |
| jemalloc 5.3 (quantum 16) | 23.7 | 152.0 | 1.15 | 1.15–1.15 |
| jemalloc 5.3 (quantum 8) | 23.4 | 143.2 | 1.08 | 1.08–1.08 |
| `bump` (never reclaims) | 15.6 | 288.6 | 2.18 | — |

**The libmalloc row is bimodal, and unexplained.** Twenty fresh processes of
the stub-build binary: res1/live1 = 1.64 in 20 of 20. Twenty of the
reference-lanes binary (same code path for `global`; its lane-2 and lane-3
code differs): 1.13 in 5, 1.14 in 10, 1.64 in 5. A further 12 runs of each
binary with and without `MallocNanoZone=0`: the stub-build binary 1.64 in 12 of
12, the reference binary 1.14 in 6 and 1.64 in 6, so the nano zone is not the
cause. `footprint1` equals `res1`, so it is not reusable pages still counted
resident. The two numbers differ by 66 MiB of the same 132.1 MiB live set.
Exercise 3 asks for the cause. Until then: the system allocator after this
churn holds between 1.13× and 1.64× of the live bytes, and one run cannot say
which.

### Lane 1c — query arena (400 queries × 20,000 intermediates of 32–128 B)

| `global` allocator | ns per object (allocate + free) | footprint | peak |
|---|---|---|---|
| libmalloc | 26.4 | 1.27 MiB | 1.33 MiB |
| mimalloc 2.x | 9.7 | 1.28 MiB | 1.33 MiB |
| jemalloc 5.3 (quantum 16) | 16.1 | 1.23 MiB | 1.30 MiB |
| jemalloc 5.3 (quantum 8) | 16.0 | 1.25 MiB | 1.31 MiB |
| `bump` + reset per query | 1.7 (1.6–1.8 across builds) | 1.17–1.23 MiB | 1.23–1.28 MiB |

Arena vs libmalloc: 26.4 ÷ 1.7 = **15.5×**; vs mimalloc 9.7 ÷ 1.7 = 5.7×.

### Lane 1d — cross-thread frees (4 producers × 1,000,000 objects, batches of 1,024)

| `global` allocator | free on own thread (ns/obj, Mobj/s) | free on other thread (ns/obj, Mobj/s) | ratio | peak (other) |
|---|---|---|---|---|
| libmalloc | 33.1, 115.6 | 46.5, 75.0 | 1.40 | 9.1 MiB (0.9 same-thread) |
| mimalloc 2.x | 9.2, 416.1 | 21.1, 188.3 | 2.29 | 4.3 MiB |
| jemalloc 5.3 (quantum 16) | 13.6, 289.2 | 15.4, 250.0 | 1.13 | 6.6 MiB |
| jemalloc 5.3 (quantum 8) | 13.6, 294.2 | 14.8, 265.3 | 1.09 | 6.6 MiB |

The libmalloc peak on the other-thread pattern is itself noisy (6.1 MiB in the
reference-lanes run, 9.1 MiB in the stub-build run; footprint 6.0 vs 8.9).
Quote the ns columns, not the peak, as the ranking.

### Repeat runs — how much a cell moves

The mimalloc and jemalloc builds were run a second time (load average 4.38 and
4.67 at the start). Every timing cell moved by 0–30%; no ranking changed.

| `global` allocator | 1a ns | 1b ns | 1c ns | 1d own | 1d other |
|---|---|---|---|---|---|
| mimalloc, first / second run | 8.2 / 8.5 | 19.2 / 20.8 | 9.7 / 9.9 | 9.2 / 12.6 | 21.1 / 21.9 |
| jemalloc (quantum 16), first / second | 10.1 / 10.7 | 23.7 / 24.4 | 16.1 / 16.4 | 13.6 / 17.7 | 15.4 / 16.8 |

mimalloc is faster in 1a, 1b, 1c and on the same-thread free in both runs;
jemalloc is faster on the other-thread free in both runs. Memory columns
(res/live, res1/live1) repeated to within 0.01 in both runs. Quote the
rankings, not single cells; the integration pass re-runs everything on a quiet
machine.

### What the baseline says

- **Bulk load: the allocator is nearly irrelevant to time** (14.0 vs 8.2–10.1
  ns × 1.8 M allocations = 7–11 ms of difference) **and decisive for padding**:
  1.12–1.13× for every allocator at quantum 16, 1.01× at quantum 8. The 40-byte
  record is what matters.
- **Is the macOS system allocator competitive? No, on the three patterns that
  are not a bulk load.** Query temporaries 1.6–2.7× slower than jemalloc and
  mimalloc, cross-thread frees 2.2–3.0× slower, post-churn residency 1.13–1.64×
  against 1.15×. On a pure append-only load it ties.
- **A bump arena is right for query temporaries and wrong for anything with
  deletes**: 15.5× faster on 1c, and 2.18× the live bytes on 1b.
- **Caveat on everything above:** the trace is this repo's stand-in for a graph
  engine's allocations, not FalkorDB's real structs, and the machine has no NUMA
  (Durner et al.'s table 1 effects cannot appear).

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 2: ns per allocation on the bulk load vs libmalloc's 14.0 | | |
| lane 2: res/live after bulk load (class rounding: which sizes are padded?) | | |
| lane 2: res1/live1 after churn vs the system allocator's 1.13–1.64 | | |
| lane 2: ns per object on the query pattern vs 26.4 (libmalloc) and 1.7 (arena) | | |
| lane 2: pages touched by 1,000 allocations after scattered frees (test: ≤ 3) | | |
| lane 3: ns per object on the producer/consumer pattern vs libmalloc's 46.5 | | |
| lane 3: pages owned at the end (test cap 40; no reuse would be ~440) | | |
| exercise 2: resident after load for a 64-byte node under quantum 8 and 16 | | |
| exercise 4: counter drift after freeing everything, on lane 1's trace | | |
| exercise 5: fraction of same-class page pairs that are meshable after lane 1b | | |

## Reference numbers for the exercise lanes

Read these after filling the worksheet. What a reference solution printed
(written in a scratch copy of this crate; not committed; measured 2026-10-26,
Apple M3 Pro; the pages the two heaps manage come from the global allocator
under test, so the lane-2 rows move slightly with it):

**Lane 2 — `PageHeap`** (single-threaded, 64 KiB pages, 24 classes, per-page free
list, a `full` list, empty pages released but one per class kept; no bump
pointer, no `local_free`):

| build (`global` allocator) | 1a ns/alloc | 1a res/live | 1b ns/op | 1b res1/live1 | 1c ns/obj |
|---|---|---|---|---|---|
| libmalloc | 8.5 | 1.01 | 21.0 | **1.07** | 7.2 |
| mimalloc 2.x | 8.8 | 1.01 | 20.8 | 1.11 | 7.1 |
| jemalloc (quantum 16) | 10.0 | 1.01 | 21.2 | 1.05 | 7.3 |
| jemalloc (quantum 8) | 10.0 | 1.01 | 21.1 | 1.05 | 7.2 |

- 1.01 on the bulk load: the class table has a 40-byte class, so the edge is
  not padded; the 1% is page headers.
- After churn 1.05–1.11× against the system allocator's 1.13–1.64×, at 21 ns
  per operation vs libmalloc's 30. It is a toy — fixed size classes up to 1,024
  B, single thread, no thread cache, no stats, no security — so this is a
  statement about *page-sharded free lists on this trace*, not a claim that it
  beats jemalloc (jemalloc's 23.7 is within 13%).
- The six stub tests all pass; the locality test's "≤ 3 pages" holds with
  margin (the reference touches 2).

**Lane 3 — `SharedHeap`** (per-thread handle; per-page atomic `thread_free`
stack with a CAS push; the owner scans its full pages for remote frees when its
available pages run out; no page ever returned):

| run | ns/obj | Mobj/s | peak | pages owned |
|---|---|---|---|---|
| libmalloc build | 15.5 | 227.3 | 3.6 MiB | 42 |
| mimalloc build | 15.8 | 251.6 | 4.5 MiB | 43 |
| jemalloc build | 16.5 | 212.5 | 7.2 MiB | 43 |
| jemalloc, quantum 8 | 15.6 | 255.7 | 7.0 MiB | 42 |

Against lane 1d's other-thread rows: libmalloc 46.5 → **15.5 ns (3.0×)**;
mimalloc 21.1 → 15.8 (1.3×); jemalloc 15.4 → 16.5 (about even). Pages owned is
42–43 (40 in an earlier run; not seeded: it depends on scheduling) against ~440
if remote frees were never reused. The four tests pass; `pages ≤ 40` holds in
the test's smaller configuration, and the scan of the full list is the
reference's "boring half" (without it the producer/consumer test fails).

## Paper and source numbers worth keeping

- mimalloc (MSR-TR-2019-18): sharded free list alone gave "over 25%" in Lean's
  1 GiB-heap benchmarks (§2.1); bump pointer "consistently about 2% worse"
  (§2.2); full list fixed a 30% slowdown on SpecMark `gcc` with "over 18000"
  full pages (§3.4); without the one-delayed-free optimisation `xmalloc-test`
  is 30% slower (§3.4); redis 5.0.3 benchmark: 14% faster than jemalloc (§4);
  abstract: "7% and 14%, respectively, on redis" against tcmalloc and jemalloc.
- Durner, Leis, Neumann (DaMoN'19): abstract's 2.7× on TPC-DS SF 100, 4-socket
  Xeon; Table 2 average measured ÷ requested: jemalloc 2.23, glibc 2.28 2.10,
  TBBmalloc 1.76, TCMalloc 3.02 (`MADV_FREE`-inflated), glibc 2.23 1.58; Table 1:
  page faults 400% / 516% for jemalloc / TBBmalloc against glibc 2.28, remote
  DRAM 97%.
- Hoard (ASPLOS 2000): blowup O(1) (§4.1); S = 8 KiB, f = 1/4, K = 4, b = 1.2
  (§5); abstract: up to 60× over Solaris on 14 processors (2000-era, Solaris).
- Mesh (PLDI 2019): Firefox 632 → 530 MB (§6.2.1); Redis 39% same as
  `activedefrag`, 0.23 s vs 1.49 s (§6.2.2); Robson 13× for 16 B – 128 KiB
  (§1); `(1/b)^(n−1)` = 10⁻¹⁵² for b = 256, n = 64 (§2.2).
- jemalloc source: four classes per doubling (`sc.h:173`); quantum 16 on
  x86-64 and arm64 (`quantum.h:23,29`); dirty decay 10 s, muzzy 0
  (`arena.h:31-32`); 200 smootherstep epochs (`smoothstep.h:27`).
- Redis: `--with-lg-quantum=3` (`deps/Makefile:162`); jemalloc on Linux, libc
  elsewhere (`src/Makefile:81-85`).

## Cross-topic threads

- Topic 6: the zmalloc ledger counts the *usable* size; FalkorDB's per-query
  counter counts the *requested* size on allocation and the *usable* size on
  free (`rmalloc.c:56`, `:83`) — the same ledger, with a bias.
- Topic 9: a heap's false-sharing hazard is the allocator's passive false
  sharing (Hoard §2.1).
- Topic 48: a churned heap interleaves hot and cold objects within pages — the
  precondition of every page-granular tiering policy's weakness.

## Open questions

- Why is libmalloc's post-churn residency bimodal across processes, and why
  does it depend on the binary (exercise 3)?
- What does FalkorDB's real entity layout look like under quantum 8 vs 16
  (exercise 2)? The trace here is a stand-in.
- Does mimalloc 3.x (the v3 line anchored in the guides) change lane 1d? The
  crate's default is 2.x.
- Linux: every row above is macOS. Durner et al.'s NUMA and `MADV_FREE`
  effects need a Linux box (and a NUMA one) to reproduce.

## Guide-question checklist

- [ ] reading-mimalloc.md Q1–Q4
- [ ] reading-jemalloc.md Q1–Q4
- [ ] reading-durner-allocation-impact.md Q1–Q4
- [ ] reading-hoard.md Q1–Q4
- [ ] reading-mesh.md Q1–Q4
- [ ] reading-falkordb-redis-allocator.md Q1–Q4
