# Topic 48 notes — memory tiering

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin tier_bench`. 36 GB unified memory, 16 KiB base
pages, 128-byte cache lines (`sysctl hw.pagesize hw.cachelinesize`). Other
agents' benchmarks were running throughout: every run below carries its
1-minute load average, and none ran below 7. The id streams, counts, model
rows and checksums are seeded and reproduce exactly; every timing is one run
on a busy machine and should be read as a shape and a range, not a digit.

### Lane 1a — dependent-load latency ladder (load 7.21)

| working set | ns/load |
|---|---|
| 16 KiB | 1.18 |
| 64 KiB | 1.16 |
| 256 KiB | 4.34 |
| 1 MiB | 6.39 |
| 4 MiB | 8.85 |
| 8 MiB | 14.95 |
| 16 MiB | 44.02 |
| 32 MiB | 90.03 |
| 64 MiB | 101.09 |
| 256 MiB | 115.15 |
| 512 MiB | 116.29 |

Topic 0 measured 1.02 / 5.3–5.8 / 104–113 ns for L1 / L2 / DRAM on the same
machine with criterion (topic 0 notes, Experiment 1); this lane agrees within
noise at both ends. The middle is where load shows: across four runs the 8 MiB
rung read 9.76, 31.91, 58.90 and 14.95 ns, and the 16 MiB rung 17.28, 105.39,
94.95 and 44.02 ns. The L2 is shared by a cluster's cores, so its effective
size is whatever other processes leave — the fast tier's capacity is not a
constant.

### Lane 1b — hot/cold placement (load 7.21; 5 alternating reps of 4 M accesses)

Loop floor (every object in one 16 KiB page): 7.96 ns. All cold (uniform
over 1 GiB): 129.28 ns.

| hot set | layout | ns/access | hot lines (128 B) | hot pages (16 KiB) | pages for 90% | hot-page util | scattered/packed |
|---|---|---|---|---|---|---|---|
| 0.1% (1.0 MiB) | packed | 26.98 | 8,389 | 66 | 66 | 99.3% | |
| | scattered | 37.03 | 16,777 | 8,218 | 7,964 | 3.2% | 1.37× |
| 0.5% (5.1 MiB) | packed | 37.19 | 41,943 | 328 | 328 | 99.9% | |
| | scattered | 86.12 | 83,886 | 35,218 | 31,670 | 3.3% | 2.35× |
| 1.0% (10.2 MiB) | packed | 60.79 | 83,886 | 656 | 655 | 100.0% | |
| | scattered | 110.73 | 167,772 | 65,536 | 55,806 | 3.4% | 1.76× |
| 5.0% (51.2 MiB) | packed | 114.96 | 419,431 | 3,277 | 3,257 | 98.7% | |
| | scattered | 126.76 | 838,861 | 65,536 | 57,116 | 7.3% | 1.07× |

At 4 KiB pages (the same window):

| hot set | layout | pages touched | pages for 90% | MiB for 90% |
|---|---|---|---|---|
| 0.1% | packed / scattered | 205,544 / 208,660 | 263 / 16,634 | 1.0 / 65.0 |
| 0.5% | packed / scattered | 205,587 / 223,532 | 1,311 / 79,783 | 5.1 / 311.7 |
| 1.0% | packed / scattered | 205,939 / 241,398 | 2,619 / 149,206 | 10.2 / 582.8 |
| 5.0% | packed / scattered | 207,855 / 262,142 | 13,018 / 215,327 | 50.9 / 841.1 |

- **"Pages touched" is useless as a hotness signal.** Within one 4 M-access
  window the 10% of uniform cold accesses touch ~205,000 of the 262,144 4 KiB
  pages under *either* layout. An OS that marks a page active on any touch
  (TPP's premise; OBASE §2.1) sees nearly the whole GiB as active. Only
  *how many* accesses each page gets separates the layouts.
- **The counts are exact and machine-independent.** Checksums (XOR of every
  payload loaded) agree between layouts on every row — the two layouts load
  the same objects in the same order; only the addresses differ.
- **The timing gap peaks where the packed hot set fits the cache and the
  scattered one does not** (0.5%: 5.1 MiB of lines vs 10.2 MiB over 35,218
  pages), and closes when neither fits (5%: 1.07×). That is the tier boundary,
  and it moved with load (next table).

Run-to-run spread of scattered/packed (five runs; runs 1–3 timed each layout's
three reps back to back, runs 4–5 alternate five reps; run 5 is the full
`./verify.sh` pass of 2026-10-26 after every parallel build had finished; the
machine was still not idle — load average 5.97 just after it):

| hot set | run 1 (load 10.95) | run 2 (7.97) | run 3 (8.19) | run 4 (7.21) | run 5 (~6) |
|---|---|---|---|---|---|
| 0.1% | 1.41× | 1.87× | 1.63× | 1.37× | 1.38× |
| 0.5% | 2.82× | 1.87× | 2.10× | 2.35× | 2.33× |
| 1.0% | 2.19× | 1.37× | 1.68× | 1.76× | 1.57× |
| 5.0% | 2.01× | 1.06× | 0.93× | 1.07× | 1.07× |

Run 1's 5% scattered row read 223.6 ns against a DRAM rung of 111.3 ns — more
than a full miss per access, impossible for a hot set — and the sanity check
flagged it. That run's machine was not the machine the row describes; it is
recorded as the reason the all-cold row and its check exist, not as data.
Run 1 is therefore excluded from the quoted range as well. Runs 2–5 give
1.4–2.4× for 0.1–1% hot sets and ~1× at 5%; that is the figure to quote.

### Lane 1c — two-tier emulation (slow tier MODEL; load 13–25, 5 alternating reps of 4 M accesses)

Fast tier: the 1,024 most-accessed 16 KiB pages of an earlier window
(learned per layout, ties broken by page number). Every other access pays a
calibrated dependent spin: 165.7 ns = SIDLE Figure 1(c)'s 276.9 − 111.2.
**The 165.7 ns is a constant taken from a paper; nothing here touched CXL.**
Counted: slow share (exact for the seed). Measured: control (same loop, same
bitmap lookup, 0 spin iterations), tiered, and the spin's calibration
(1.069 ns/iteration, 166 iterations = 163.8 ns per call with call overhead
included, in the run below).

| hot set | layout | slow share | control ns | tiered ns | control + share × 165.7 | tiered/control |
|---|---|---|---|---|---|---|
| 0.1% | packed | 9.8% | 30.61 | 47.79 | 46.92 | 1.56× |
| | scattered | 83.4% | 51.39 | 219.05 | 189.50 | 4.26× |
| 0.5% | packed | 9.9% | 41.20 | 59.61 | 57.53 | 1.45× |
| | scattered | 96.6% | 94.45 | 275.26 | 254.43 | 2.91× |
| 1.0% | packed | 9.8% | 78.06 | 84.70 | 94.37 | 1.09× |
| | scattered | 97.4% | 111.44 | 274.28 | 272.84 | 2.46× |
| 5.0% | packed | 71.7% | 105.69 | 217.38 | 224.53 | 2.06× |
| | scattered | 98.2% | 128.55 | 293.20 | 291.32 | 2.28× |

scattered/packed with the slow tier, two runs (the first at load 35–42, spin
calibrated to 166.9 ns per call; the second above):

| hot set | run A | run B |
|---|---|---|
| 0.1% | 4.42× | 4.58× |
| 0.5% | 5.74× | 4.62× |
| 1.0% | 4.07× | 3.24× |
| 5.0% | 1.26× | 1.35× |

- **The slow share is the finding, and it is exact:** 9.8% packed against
  97.4% scattered at a 1% hot set. Time follows it; the ratio is not a
  property of CXL.
- **Run B's 1% packed control read 78.06 ns**; runs A and a third run read
  42.75 and 37.89 ns for the same row. That is the machine, not the layout
  (load average 13–25), and it is why the 1% row's ratio (3.24×) is the low end
  of the range. The quotable range is 3.2–5.7× for 0.1–1% hot sets and 1.3×
  at 5%; the integrator's quiet-machine rerun replaces it.
- **The spin runs over its calibration inside the loop.** Tiered exceeds
  control + share × 165.7 by up to 16% (run A: up to 18%; the disturbed 1%
  packed row is 10% below it), because the in-loop
  call, the opaque-zero dependency and the slow/fast branch cost a few ns the
  isolated calibration does not see. The bench's sanity line allows 25% and
  prints the observed maximum.
- **5% is the boundary, not an exception:** the packed hot set (3,257 pages)
  exceeds the 1,024-page fast tier, so 71.7% of its accesses go slow and the
  advantage of packing shrinks to 1.3×.
- **What it cannot show:** bandwidth limits, controller queueing and
  prefetch behaviour of a real CXL link (README, "What this machine cannot
  measure").

## Predictions (fill BEFORE running the lanes or implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 1a: ns/load at 16 KiB, 4 MiB, 512 MiB | | |
| lane 1b: packed vs scattered ns/access at a 1% hot set | | |
| lane 1b: at which hot fraction is scattered/packed largest, and why? | | |
| lane 1b: 4 KiB pages needed for 90% of accesses, 1% hot, scattered | | |
| lane 1c: fraction of accesses a 16 MiB fast tier serves, 1% hot set, packed vs scattered (counted) | | |
| lane 1c: scattered/packed time at 1% hot once every slow access costs +165.7 ns | | |
| lane 2: pages for 90% for your learned layout on the held-out window | | |
| lane 2: the same after half the hot set drifts | | |
| lane 3: TPP fast-hit rate, packed and scattered, 20% fast tier | | |
| lane 3: does TPP beat first touch in MODEL ns/access on the packed layout? | | |
| hash-join model: x* and g(x*) for NPHJ from Table 1's k and m | | |

<details><summary>The author's predictions, written before the first run (unfold after filling yours)</summary>

- Lane 1b, 1% hot set. Reasoning: 90% of accesses to a 10.2 MiB hot set that
  fits the 16 MiB L2 at ~6 ns, 10% uniform cold at ~110 ns: packed ≈ 0.9 × 6 +
  0.1 × 110 ≈ **16 ns**; scattered drags 20.5 MiB of lines, so mostly DRAM:
  ≈ 0.9 × 70 + 11 ≈ **74 ns**. Measured 60.8 and 110.7 ns. Wrong by ~4× on
  packed: it ignored the 8 ns loop floor, the cold accesses evicting hot lines
  from the L2, and other processes sharing the L2. The ratio (predicted 4.6×,
  measured 1.4–2.2× across runs) was the wrong thing to predict confidently.
- Lane 1b, 0.1%: "the lines fit the cache either way, so the only difference
  is the TLB" — 8,218 pages touched by the scattered hot set vs 66. Measured
  1.37–1.87×: plausible for TLB walks, not separated from cache effects here
  (no TLB counters without root on macOS).
- Lane 3 scattered: hit rate ≈ 20% / 47% of hot accesses (47% of 4 KiB pages
  hold a hot object) ≈ 38% + cold share. The oracle-static row came out 38.1%.

</details>

## Reference numbers for the stubs

From reference implementations of `reorg::learn_layout` and `tier::tpp`, run
against the same seeded streams before the functions were stubbed. These are
deterministic: they reproduce to the digit on any machine. The eight stub tests
were green against these references.

### Lane 2 — learned layout (1% hot set, 16 KiB pages)

| layout | pages for 90% | hot-page util | pages for 90% after drift |
|---|---|---|---|
| scattered (start) | 55,800 | 3.4% | 47,467 |
| packed (oracle) | 655 | 100.0% | 47,561 |
| learned (frequency packing) | 655 | 100.0% | 46,506 |

The learning window is accesses 1–4,000,000 and the scoring window the next
4,000,000 (no warm-up, unlike lane 1b — hence 55,800 here vs 55,806 there).
Drift: ids below hot/2 keep their identity, all others are renamed by
`keep + (id − keep) × 2,654,435,761 mod (n − keep)`, so half the hot set
becomes scattered objects. Every static layout ends near 47,000 pages.

### Lane 3 — two-tier MODEL (1% hot set, 262,144 4 KiB pages, 52,428 fast)

Priced with SIDLE Figure 1(c): fast 111.2 ns, slow 276.9 ns, 189 ns per page
moved (4,096 B at 21,724.3 MB/s — a lower bound). All-fast 111.2, all-slow
276.9 ns/access.

| layout | policy | fast hits | promotions | demotions | MODEL ns/access |
|---|---|---|---|---|---|
| packed | first touch | 93.1% | 0 | 0 | 122.6 |
| packed | promote on touch | 93.1% | 121,648 | 121,648 | 134.1 |
| packed | oracle static | 94.5% | 0 | 0 | 120.4 |
| packed | **tpp (reference)** | 96.9% | 3,560 | 158,119 | 124.0 |
| scattered | first touch | 30.5% | 0 | 0 | 226.4 |
| scattered | promote on touch | 31.0% | 2,569,245 | 2,569,245 | 467.7 |
| scattered | oracle static | 38.1% | 0 | 0 | 213.8 |
| scattered | **tpp (reference)** | 34.4% | 681,004 | 871,022 | 293.0 |

- **TPP raises hits and loses in the model.** On the packed layout it reaches
  the best hit rate of any policy (96.9%, above the static oracle's 94.5%,
  because it adapts within the window), but the 158,119 demotions that keep 2%
  headroom cost more than the hits save: 124.0 vs first touch's 122.6 ns. On
  the scattered layout its 1.55 M migrations make it 30% worse than not
  migrating at all (293.0 vs 226.4). With migration priced at a lower bound,
  the real verdict can only be worse. This is a model's negative result: it
  says migrations must be rarer than this window allows, not that TPP is wrong
  on its own workloads — TPP's Table 1 shows it within 1% of all-local on five
  of six production rows.
- **Hysteresis works as intended:** 681,004 promotions against
  promote-on-touch's 2,569,245 (3.8× fewer) on the scattered layout.
- **Layout dominates policy:** the worst packed row (134.1) beats the best
  scattered row (213.8).

### Hash Joins Meet CXL, Table 1 recomputed (lane 3, provided)

| algorithm | k | m | x* (Eq. 5) | g(x*) (Eq. 6) | Table 1 x* | Table 1 g |
|---|---|---|---|---|---|---|
| PHJ | 2.3534 | 1.0977 | 0.5532 | 1.1665 | 0.5528 | 1.1703 |
| NPHJ | 3.7879 | 0.3641 | 0.4789 | 1.0225 | 0.5104 | 1.0214 |

The NPHJ row does not follow from its own k and m; g(0.5104) = 1.0225 too, so
neither printed number is the model evaluated at the printed inputs. The
released scripts run NPHJ's probe at 0.51 and PHJ at 0.45
(`test_hc_join.sh:44-45`).

## Paper numbers worth keeping

- **Hash Joins Meet CXL (Huang, Lu & Tan, CIDR'26).** DRAM 30.01 GB/s, CXL
  20.95 GB/s, 3 : 2 interleave 47.50 GB/s (§4.1); T(x) = 200/x − 1 (Eq. 1);
  partial move beats in-CXL by 22.20% (PHJ) and 3.59% (NPHJ) runtime (§4.2);
  partition-phase optimum 0.5528 model vs 0.4972 measured, gain 17.03% vs
  24.20% (§4.3); λ = mk = 2.5353 (Figure 6). Scripts' interleave ratio 10 : 7.
- **TPP (Maruf et al., ASPLOS'23).** Main memory 80–140 ns, CXL 170–250 ns
  (Figure 2); 55–80% of memory idle within two minutes (§2); demotion
  headroom 2% (§5.2); 256 MB sampled per NUMA-balancing scan (§5.3); Table 1
  (normalised throughput %, default / TPP / NUMA balancing / AutoTiering): Web1
  2:1 83.5 / 99.5 / 82.8 / 87.0; Cache2 1:4 82.0 / 95.0 / 78.0 / fails. Measured
  mostly on two-socket servers emulating a CPU-less node (§6).
- **SIDLE (Zhao et al., VLDB'26).** Fast 111.2 ns / 33,095.3 MB/s; CXL 276.9
  ns / 21,724.3 MB/s (Figure 1(c)); 25% fast memory under interleaving → ~70%
  throughput drop (Figure 2); hottest node 82% / 85% of a hot page's accesses
  (Figure 5); up to 71% throughput, 81% lower P99 (abstract); 42.6–133.5% over
  page-level schemes (§6.2). Genuine CXL 1.1 hardware.
- **OBASE (Banakar et al., OSDI'26).** Bravo: 3.2% of bytes, 91.8% of pages
  accessed (§1); 70–90% of bytes in active pages cold (Finding 1); 2–4× page
  utilization, 65–72% RSS reduction (§5.2); hot set 16.3 GiB @ 21% → 6.33 GiB @
  57%; TPP 1.65× → 1.85× at 1:4, 1.25× → 1.45× at 1:16 (§5.3.2); 1.5–5%
  overhead (§5.4). Slow tier is Optane PMem, not CXL.
- **Weisgut et al. (VLDB'25).** FPGA CXL prototypes: random 8 B reads average
  520 ns; sequential 65 ns vs 45 ns local (§4.4); >80% of TPC-H data in CXL at
  85% of local performance with access-frequency column placement (abstract).
- **MAC (Lee et al., OSDI'26).** CXL DRAM ≈ 2.4× DDR latency (§1);
  memory-management metadata up to 24% of DDR at 1 : 4, ~40% at 1 : 8
  (abstract); +2.4× metadata latency → ~2.7× p99.99, 6.5× more direct
  reclaims (§1); up to 98% lower tail latency (abstract).
- **SHARD (Zha et al., VLDB'26).** RDMA DM: 100–400 Gbps, 2–3 µs latency (§1);
  6.7× / 3.6× / 1.8× over RACE / SepHash / Outback on YCSB (abstract).

## Cross-topic threads

- Topic 0's ladder is lane 1a; its lying-benchmark story is why both loops
  carry their position.
- Topic 6's buffer pool is the same placement problem one tier further down;
  TPP's active-list hysteresis is CLOCK's second chance.
- Topic 20's matrices: a vertex permutation is a layout; lane 2 learns one.

## Open questions

- How much of lane 1b's 0.1% gap is TLB and how much cache? Needs PMU
  counters (`kperf` requires root on macOS; `perf stat -e dTLB-load-misses` on
  Linux).
- Do TPP's migrations pay on a workload with a *stable* hot set larger than
  one window? Lane 3's window is 4 M accesses; the paper's are minutes.
- What is the Table 1 NPHJ row computed from? Ask the authors, or derive k and
  m from Figure 4's bars.

## Implementation log

- [ ] `reorg::learn_layout` — four tests green; lane 2 matches the reference
- [ ] `tier::tpp` — four tests green; lane 3 matches the reference rows
- [ ] prediction table reconciled
- [ ] stretch: OBASE's CIW classifier over a drifting trace (exercise 2)
- [ ] stretch: time the learned layout (exercise 3)
