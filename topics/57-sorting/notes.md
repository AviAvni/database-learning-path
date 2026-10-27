# Topic 57 notes — sorting at scale

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin sort_bench`. Seeded generators (`gen.rs`,
`SEED = 57`): every count and checksum below reproduces exactly on any machine
(identical in four runs). Timings are **thread CPU time** (`clock_gettime
(CLOCK_THREAD_CPUTIME_ID)`), best of three, each run on a fresh copy of the
input. Other agents were benchmarking on the same machine, so `sysctl -n
vm.loadavg` is recorded with each run.

| run | load average (1/5/15) at start | 1a u64 comparator / bare / stable (ms) | 1b b1 / b2 / b3 (ms) | 1c collating comparator (ms) |
|---|---|---|---|---|
| quoted in README | 4.47 5.09 8.34 | 152.3 / 112.4 / 211.9 | 545.1 / 111.3 / 530.0 | 744.8 |
| second run | 4.83 5.09 8.14 | 150.2 / 110.9 / 208.9 | 536.0 / 111.1 / 511.0 | 757.5 |

The two runs agree within 4%. Ordering is stable (stable > unstable > bare;
b1 ≈ b3 ≫ b2). Quote ratios and counts, not milliseconds.

### Lane 1a — `u64`, 10,000,000 rows

| sort | ms | ns / (n·log₂ n) |
|---|---|---|
| `sort_unstable` on bare `u64` | 112.4 | 0.48 |
| `sort_unstable_by` over `Row64` (16 B) | 152.3 | 0.65 |
| `sort_by` (driftsort) over `Row64` | 211.9 | 0.91 |

Checksum `0812712ec852b87e`. n sweep of the comparator row: 1,000 → 0.62,
10,000 → 0.56, 100,000 → 0.64, 1,000,000 → 0.72 ns/(n·log₂ n). About 15 ns per
row at ten million. The bare-`u64` row takes the sorting-network base case, the
`Row64` row the general one (`library/core/src/slice/sort/shared/smallsort.rs:142-170`
in rustc 1.99.0), so the 1.35× between them is payload *and* base case.

### Lane 1b — `(int, string)`, 5,000,000 rows

| shape | ms | ns / (n·log₂ n) | rows with an `a` already seen | checksum |
|---|---|---|---|---|
| b1: `a` in 1,000 values, random strings | 545.1 | 4.90 | 4,999,000 | `534fdd4bcaf33e4b` |
| b2: `a` over all of `i32`, random strings | 111.3 | 1.00 | 2,904 | `1cd6d972296e3024` |
| b3: `a` in 1,000 values, 20-byte shared prefix | 530.0 | 4.76 | 4,999,000 | `c23c7c8734b16de6` |

("rows with an `a` already seen" is the bench's `ties on a`, `n − distinct a`.)
**Hand check:** a sort of 5,000,000 rows makes about log₂ 5,000,000 = 22.3
levels of partitions. With 1,000 values of `a`, the first ~10 levels
(log₂ 1,000 ≈ 10) separate the values of `a`; in the remaining ~12 levels every
comparison is between rows with the same `a`, ties on the integer, and chases
the string pointers. So more than half the comparisons reach the arena, against
almost none for the all-distinct case (2,904 ties); 4.9× is the price of those.
The shared 20-byte prefix of b3 does not add to it (4.76 vs 4.90 ns).

### Lane 1c — collation inside the comparator, 2,000,000 strings

744.8 ms, 17.79 ns/(n·log₂ n): 18× the all-distinct `(int, string)` sort per
`n log₂ n`. 13.6 bytes per string on average, 1,978,777 distinct strings (equal
under the collation) of 2,000,000 — duplicates arise only when two rows
re-spell the same base word identically. Under the toy collation *distinct
strings never tie* (every character maps to a distinct `(p, s, t)`), so the
class count equals the distinct-string count; at primary strength only it falls
to 490,966 (see the reference numbers below).

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 1: ms for `sort_unstable_by` over 10M `Row64` on your machine | | |
| lane 1b: b1 ÷ b2 — how much slower is the sort when `a` ties? | | |
| lane 1b: b3 ÷ b1 — what does a 20-byte shared prefix cost the *comparator*? | | |
| lane 1c: collating comparator ÷ b2, per `n log₂ n` | | |
| lane 2: LSD `sort_u64` speed-up over `sort_unstable_by` at 10M | | |
| lane 2: the `n` below which LSD is slower than the comparator | | |
| lane 2: speed-up on already-sorted `u64` input | | |
| lane 2: speed-up on `u64` input with 256 distinct values | | |
| lane 2: `sort_int_str` on b1 / b2 / b3 | | |
| lane 2: shared-prefix length at which `sort_int_str` stops winning | | |
| lane 3: speed-up of sort keys over the collating comparator at 2M | | |
| lane 3: comparisons per string at break-even | | |
| lane 3: bytes of sort key per string | | |

## Reference numbers for the exercise lanes

From reference implementations of `normalize.rs`, `radix.rs` and the two lane-3
functions written to the stub contracts, run against lane 1's data, all 22
tests green, then removed (not committed). Same machine, same CPU-time method,
best of three, ms per sort. Counts are exact; timings are one run at the load
shown (two runs differ by under 5% except where noted).

### Lane 2 — `u64`, uniform, `n` sweep (load 4.25 at start)

| n | comparator | LSD | MSD | LSD speed-up | MSD speed-up |
|---|---|---|---|---|---|
| 100 | < 0.001 | 0.002 | < 0.001 | 0.19× | 0.84× |
| 300 | 0.001 | 0.003 | 0.001 | 0.41× | 1.24× |
| 1,000 | 0.006 | 0.008 | 0.003 | 0.76× | 1.70× |
| 3,000 | 0.019 | 0.020 | 0.012 | 0.94× | 1.63× |
| 10,000 | 0.071 | 0.067 | 0.103 | 1.06× | 0.68× |
| 100,000 | 0.958 | 0.661 | 0.651 | 1.45× | 1.47× |
| 1,000,000 | 13.315 | 7.468 | 10.170 | 1.78× | 1.31× |
| 10,000,000 | 150.404 | 73.152 | 92.935 | 2.06× | 1.62× |
| 50,000,000 | 832.792 | 375.056 | 467.736 | 2.22× | 1.78× |

LSD: 8 byte passes with every constant byte position skipped, built from one
histogram pass. MSD: 256-way out-of-place scatter with copy-back, insertion
sort at ≤ 24 rows, "all in one bucket" skipped. The MSD dip at 10,000 rows
(0.68×) is the bucket size, 10,000 / 256 ≈ 39 rows, just above the 24-row
cutoff: every bucket pays another 256-bucket pass for a handful of rows. The
sub-microsecond rows are at the timer's resolution; read the ratios, not the
digits.

### Lane 2 — `u64`, shapes at n = 10,000,000 (load 4.44)

| shape | comparator | LSD | MSD | LSD speed-up | MSD speed-up |
|---|---|---|---|---|---|
| keys < 2²⁰ (3 live bytes) | 143.692 | 44.925 | 138.068 | 3.20× | 1.04× |
| 256 distinct keys | 50.429 | 35.503 | 93.017 | 1.42× | 0.54× |
| already sorted | 2.889 | 76.822 | 129.571 | **0.04×** | **0.02×** |
| reverse sorted | 5.922 | 77.147 | 145.710 | 0.08× | 0.04× |
| sorted + 1% random swaps | 142.927 | 77.156 | 134.823 | 1.85× | 1.06× |
| only top 3 bytes vary | 150.385 | 44.491 | 75.173 | 3.38× | 2.00× |
| shared top 5 bytes | 150.796 | 44.641 | 143.817 | 3.38× | 1.05× |

The comparison sort's two fast paths show: sorted and reverse-sorted input cost
2.9 and 5.9 ms (the existing-run check plus one reverse), and 256 distinct values
cost 50.4 ms — a third of the random case — from the equal-to-ancestor partition
(reading-rust-sort.md Step 3).

### Lane 2 — `(int, string)`, 5,000,000 rows (load 4.57)

| shape | comparator | radix | speed-up |
|---|---|---|---|
| b1: `a` in 1,000 values, random strings | 532.713 | 177.690 | 3.00× |
| b2: `a` over all of `i32` | 111.926 | 104.022 | 1.08× |
| b3: 1,000 `a`-values, 20-byte shared prefix | 513.311 | 721.304 | **0.71×** |

b1 `n` sweep: 100 → 0.19×, 1,000 → 0.52×, 10,000 → 0.99×, 100,000 → 1.95×,
1,000,000 → 2.16×. Shared-prefix length sweep (2,000,000 rows, `a` in 1,000
values, strings = prefix + 8–16 random letters):

| shared bytes | comparator | radix | speed-up |
|---|---|---|---|
| 0 | 135.0 | 70.1 | 1.93× |
| 4 | 191.5 | 81.3 | 2.36× |
| 8 | 177.4 | 99.0 | 1.79× |
| 12 | 168.9 | 244.5 | **0.69×** |
| 16 | 185.1 | 259.6 | 0.71× |
| 24 | 195.8 | 270.1 | 0.72× |
| 40 | 213.2 | 290.6 | 0.73× |

With `a` constant (a pure string sort): 0 shared bytes 2.13×, 12 → 0.52×,
24 → 0.54×. The reference radix sort is an MSD over the 16-byte key
(`a` + 12 string bytes) with a 20-byte `(key, row index)` record, a tie-break
that sorts runs of equal keys by the full strings, and a final gather of the rows.

- **Negative result — Graefe's fourth objection.** The paper relays "radix sort
  has worse cache efficiency than comparison-based sorts" (§VI-C). At 50,000,000
  rows (800 MB of rows plus 800 MB of scatter buffer) the LSD win is 2.22×,
  larger than at 1,000,000 (1.78×). It does not reproduce on this machine at
  this scale; the objections that do reproduce are (1) unproductive passes on
  shared prefixes, and (3) presorted input.
- **The paper's LSD/MSD rule does not transfer to 8-byte keys here.** §VI-C
  picks LSD for keys ≤ 4 bytes and MSD above; with the skip optimisation LSD
  beats this MSD at every `n` above 10,000 (except a tie at 100,000). Caveat:
  this MSD copies back after every scatter; an in-place American flag sort
  (DuckDB's `ska_sort`) has different costs.

### Lane 3 — collation sort keys (load 4.55 / 4.64, two runs)

Per input size: `cmp` = collating comparator over indices; `build` = all sort
keys into one buffer; `sort` = comparing the keys with `memcmp`; `keys` =
`build + sort`; `cmps/str` = 2 × (comparator calls counted inside the sort) ÷ n;
`g` = build ns per string; `c` = comparator ns per call; `b` = key-comparison ns
per call; `m* = 2g / (c − b)`.

| n | cmp ms | build ms | sort ms | keys ms | speed-up | key B / string | cmps / string | g ns | c ns | b ns | m* |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 16 | 0.0008 | 0.0005 | 0.0002 | 0.0007 | 1.04× | 71.5 | 8.6 | 34.2 | 11.2 | 2.9 | 8.2 |
| 64 | 0.0050 | 0.0026 | 0.0013 | 0.0040 | 1.26× | 93.6 | 12.0 | 41.4 | 13.0 | 3.5 | 8.6 |
| 256 | 0.0221 | 0.0101 | 0.0065 | 0.0166 | 1.33× | 82.8 | 16.6 | 39.3 | 10.4 | 3.1 | 10.8 |
| 1,000 | 0.1032 | 0.0370 | 0.0306 | 0.0676 | 1.53× | 81.9 | 20.9 | 37.0 | 9.9 | 2.9 | 10.6 |
| 10,000 | 1.8847 | 0.4119 | 0.5742 | 0.9861 | 1.91× | 82.0 | 27.7 | 41.2 | 13.6 | 4.1 | 8.7 |
| 100,000 | 25.2603 | 4.7954 | 10.8390 | 15.6344 | 1.62× | 81.9 | 34.6 | 48.0 | 14.6 | 6.3 | 11.5 |
| 1,000,000 | 318.8247 | 49.3891 | 171.6513 | 221.0404 | 1.44× | 82.0 | 41.2 | 49.4 | 15.5 | 8.3 | 13.8 |
| 2,000,000 | 784.9752 | 94.3910 | 351.4561 | 445.8471 | 1.76× | 82.0 | 43.2 | 47.2 | 18.2 | 8.1 | 9.4 |

A second run (load 4.64) gives speed-ups 1.00 / 1.27 / 1.31 / 1.41 / 1.95 /
1.94 / 1.70 / 1.72 at the same eight sizes — differences up to 0.26 at 1M, so
read the speed-up column as ±15%. Observations:

- **The standard's break-even reproduces.** UTS #10 §1.7: keys win "whenever
  there will be more than about 10 comparisons per string". Measured: 1.00–1.04×
  at 8.6 comparisons per string, 1.26× at 12.0; `m*` computed from the measured
  `g, c, b` is 8.2–13.8. The comparisons per string are counted, and match the
  `2·log₂ n` the model predicts (43.2 counted vs 41.8 at 2,000,000).
- **`g / c` is 3–4 here, not the 5–10 §1.7 quotes** (g ≈ 34–49 ns per string,
  c ≈ 9–18 ns per call): the toy comparator mostly stops at the first
  character, and the toy key is built in one pass with three table lookups per
  character. The break-even still lands near 10 because `b` is small.
- **Memory:** 82 bytes of key per 13.6-byte string on average (4 bytes of
  separators + 6 bytes per character), 6× the string data; 164 MB for
  2,000,000 strings. UTS #10 §9.1's reductions (drop the separators, store L2/L3
  in 8 bits) are exercise 8.
- **Collation classes at n = 2,000,000:** 1,978,777 distinct full keys, and
  **490,966** distinct primary-level keys (case and accents ignored) against a
  prediction of `500,000 · (1 − e⁻⁴) = 490,842` (the 500,000 base words, each
  picked at least once with probability 1 − e⁻⁴; 0.03% off).

### IPS⁴o, measured once (C++, sequential)

`ips4o::sort` (the `ips4o/ips4o` repository at `08a5b92`, built with
`-std=c++17 -O3`, a stub header standing in for the TBB include the sequential
path does not use) against `std::sort` (Apple clang 21 / libc++), 16-byte rows
`{u64 key, u64 payload}`, uniform random keys from `mt19937_64`, thread CPU time,
best of three, load 4.78 at start:

| n | `std::sort` | `ips4o::sort` | ratio |
|---|---|---|---|
| 1,000,000 | 48.7 ms | 23.5 ms | 2.07× |
| 10,000,000 | 578.9 ms | 247.0 ms | 2.34× |

An earlier run at load 16.7 gave 2.22× and 2.34×. This is a different language,
compiler and random generator from the Rust crate: the ratio between the two C++
sorts is the finding, not a comparison with the Rust milliseconds. The paper
reports 1.86× or more over `std::sort` at n = 2³² on its Intel2S machine (§5).
Not measured: the parallel version, other distributions.

## Paper numbers worth keeping

Each read in the paper's PDF; section or figure in parentheses.

- Kuiper & Mühleisen: DuckDB sorts 100 million random 32-bit integers in 1.55 s
  on a 16-core/32-thread `m5d.8xlarge` (§VII-B, Fig. 10); single-threaded,
  100 M integers 10.41 s and floats 9.14 s (§VII-B). MonetDB, single-threaded
  quicksort: 29.18 s and 36.39 s for the same. Fig. 5 is the worked key
  example. Radix: LSD for ≤ 4-byte keys, MSD above with insertion sort ≤ 24
  tuples; strings → pdqsort (§VI-C). Static `memcmp` is on average 25% faster
  than dynamic below 16 bytes and static `memcpy` 55% faster on Graviton2
  (§VI-B).
- UTS #10 (v18.0.0, rev. 55): sort key = non-zero weights per level with `0000`
  separators (§7.3, Fig. 2: 13 weights for 4 collation elements); direct
  comparison "about 5 to 10 times faster" than generating a key and comparing,
  keys win "whenever there will be more than about 10 comparisons per string"
  (§1.7); L2/L3 in 8 bits and separator elimination shorten keys (§9.1).
- IPS⁴o (arXiv 1705.02257v2): sequential 1.14× / 1.22× / 1.57× faster than
  BlockQuicksort at n = 2³² on Intel2S / Intel4S / AMD1S; slower than it for
  n ≤ 2¹⁵ (§5); 48n bytes of I/O volume vs more than 86n for s³-sort at k = 256,
  8-byte elements, one level (§4.5, Appendix B); defaults k = 256, α = 0.2·log n,
  n₀ = 16, b ≈ 2 KiB (§4.7).
- Not checked: Graefe's survey itself (paywalled; the four radix objections are
  quoted as the paper relays them); the ipnsort/driftsort design documents
  (only the library source was read); the CLDR tailorings and ICU.

## Cross-topic threads

- **Topic 11:** the sort is the third blocking operator that re-packs vectors
  into rows; reading-duckdb-sort.md's Step 1 is the cost model.
- **Topic 12:** sorted columns are the comparison sort's best case (2.9 ms for 10M
  sorted rows above) and the radix sort's worst.
- **Topic 17:** the sorting-network base case and the branchless partitions.
- **Topics 3 and 4:** index and SSTable keys are normalized keys.

## Open questions

- Lane 3's break-even used a toy collation whose comparator is cheap; a real
  collation (ICU's incremental comparison with fast paths, §9.7 of the UTS) has
  a different `c`. Where does it cross over?
- The pinned DuckDB radix-sorts only 8 key bytes and compares the rest. Construct
  an `ORDER BY` for which that costs more than a full-key radix, and measure it.
- IPS⁴o against ipnsort on the same Rust-side data: a Rust port or an FFI bench.
- The parallel sort: a morsel-parallel version of lane 2 with a Merge Path merge
  (the paper's §VII pipeline).
