# Topic 54 notes — main-memory join algorithms

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin join_bench`. Seeded generator (`gen.rs`, seed
`0x544A4F494E54`): every count, match total and checksum reproduces exactly on
any machine; timings are **thread CPU time**, best of 3, from one run (`timing.rs`
explains why: other agents were compiling and benchmarking on the same machine,
and wall time on a single-threaded lane would have measured the scheduler).
Lane 1c's threaded probe is wall time. The run below started at load average
4.71; `dependent-add period` prints the clock period of the core the thread
landed on (0.255 ns here) so a run that was moved to an efficiency core is
recognisable: an earlier run under load average 17 printed 28.7 ns at 8 MiB and
77.8 ns at 16 MiB (the efficiency cluster's 4 MiB L2) and was discarded.

```
=== join_bench — topic 54 ===
load average (1/5/15 min) at start: 4.71 4.71 6.79

-- lane 0: the machine, measured --
   dependent-add period: 0.255 ns (a larger figure than a quiet run means a slower core)
   dependent-load latency (random cycle, one node per 128-byte line):
    working set    ns/load
         32 KiB       0.99
         64 KiB       0.99
        128 KiB       1.07
        512 KiB       5.25
          2 MiB       6.04
          8 MiB       8.31
         16 MiB      45.26
         32 MiB      91.77
         64 MiB     102.84
        256 MiB     109.01
       1024 MiB     112.71

   dependent-load latency by number of 16 KiB pages touched (one 128 B node per page):
      pages      reach   ns/load
         32    512 KiB      1.04
        128      2 MiB      1.06
        256      4 MiB      2.65
        512      8 MiB      3.08
       1024     16 MiB      4.22
       2048     32 MiB      6.56
       4096     64 MiB     15.16
      16384    256 MiB     16.04
      65536   1024 MiB     23.87

   sequential read bandwidth over 1 GiB:
    1 thread(s)     70.0 GB/s
    5 thread(s)    122.4 GB/s
   11 thread(s)    117.2 GB/s

-- lane 1a: the probe loop with no cache misses — |S| = 16777216, uniform --
         |R|  table KiB    scalar ns    bucket ns   batched ns
         2^4        0.2         8.59         6.02         6.46
         2^8        4.0        11.77         7.52         8.11
        2^12       64.0        11.71         7.29         8.13
        2^14      256.0        13.74         8.61         8.43
   (scalar: one slot per step, a branch per slot; bucket: four slots per step, a branch only on a full bucket)

-- lane 1: no-partitioning hash join, uniform FK probes — |S| = 33554432 (256 MiB), 8-byte tuples --
         |R|     R MiB  table MiB  build ms  scalar ms  probe ms  batch ms  ns/probe     Mtup/s
        2^14      0.12       0.25      0.08     464.16    290.79    284.87      8.49      117.8
        2^16      0.50       1.00      0.45     504.93    314.27    294.10      8.76      114.1
        2^18      2.00       4.00      2.09     535.40    338.17    301.39      8.98      111.4
        2^20      8.00      16.00      9.35     623.78    374.67    356.80     10.63       94.5
        2^22     32.00      64.00     56.11    1693.13    927.68    663.80     19.78       52.4
        2^24    128.00     256.00    241.51    2307.70   1112.20    719.42     21.44       52.4
   (times are this thread's CPU time; ns/probe and Mtup/s = (|R|+|S|) / (build + batched probe) use the batched probe; 'scalar' is the one-slot-per-step loop, 'probe' the bucket loop)
   sanity: every probe hits, matches == |S| and scalar == bucket == batched checksum on every row; S is read at <= 1.5 x the measured read bandwidth

-- lane 1d: probe order — S random vs S sorted by home bucket (sort not timed), |S| = 16777216 --
         |R|  table MiB      random ns      sorted ns     ratio
        2^14       0.25           8.70           4.38     1.99x
        2^20      16.00          12.17           5.00     2.43x
        2^22      64.00          27.66           5.10     5.42x
        2^24     256.00          32.66           5.80     5.63x

-- lane 1b: skew — the same join with Zipf(theta) probe keys, |S| = 33554432 --
         |R|  theta  probe ms  ns/probe vs uniform
        2^20   0.00    368.87     10.99     1.00x
        2^20   0.75    371.74     11.08     1.01x
        2^20   0.99    371.94     11.08     1.01x
        2^24   0.00   1118.17     33.32     1.00x
        2^24   0.75   1022.52     30.47     0.91x
        2^24   0.99    760.09     22.65     0.68x

-- lane 1c: threads on the probe — |R| = 2^24 (256 MiB table), |S| = 33554432 --
    threads  probe ms   Mprobe/s   speedup
          1    703.34       47.7     1.00x
          2    379.61       88.4     1.85x
          5    193.54      173.4     3.63x
         11    127.26      263.7     5.53x
```

Run-to-run spread, same probe loop, same machine, same day (lane 1, 2²⁰ build,
batched probe, ms): 358.9 (load average 6.4) and 356.8 (4.7). The 16 MiB point of
lane 0 measured 19.1, 19.8, 21.0 and 45.3 ns in four runs: it sits on the edge
of the cluster's L2, so quote the 8 MiB and 32 MiB points. Quote ratios and
regimes, not single milliseconds.

- Matches = |S| = 33,554,432 on every lane-1 row; the checksum of the lane-2
  oracle (2²⁰ build, 2²² probe) is `a1f3afb6193b238f`.
- **Hand check of the bandwidth floor.** S alone is 256 MiB; at 70.0 GB/s a
  single sequential pass is 3.8 ms, so the 285–719 ms batch probe times are
  75–190× above streaming S: every row is latency- or compute-bound, none
  bandwidth-bound.
- **Three regimes** (ns/probe, batched): 8.5–9.0 up to a 4 MiB table; 10.6 at
  16 MiB; 19.8–21.4 at 64–256 MiB. The lane-0 pages ladder explains the second
  step: 128 → 256 pages (2 → 4 MiB reach) is 1.06 → 2.65 ns, 2,048 → 4,096
  pages (32 → 64 MiB) is 6.56 → 15.16 ns.
- **Negative result of the provided lane:** the shared table's cost below the
  cache cliff is not memory; lane 1a measures 6–8 ns per probe for a 16-tuple
  table (all L1), and lane 1d shows the same probes sorted by table address at
  4.4 ns for a 256 KiB table. The loop's own cost, sensitive to probe order, is
  most of the shared join's price for builds up to 16 MiB.

## Reference numbers for the exercise lanes

Read these after filling the worksheet. They come from a reference solution
(single-pass and two-pass histogram-and-scatter partitioning with and without
software write-combine buffers of 16 tuples = one 128-byte line, per-partition
tables built with lane 1's `insert` and probed with `probe_one`, one scratch
table reused across partitions; LSD radix sort of 3 × 11 bits and a merge
that emits equal-key cross products) measured 2026-10-26 on the same Apple M3 Pro,
|S| = 2²⁵ uniform foreign keys, one thread, thread CPU time, best of 2 (radix grid)
or 3. The solution is not committed — it is the exercise. Its tests are the
stub tests in `radix.rs` and `sortmerge.rs`, which it passes.

```
dependent-add period: 0.272 ns
== crossover: NPJ (batched probe) vs best radix config vs sort-merge, |S| = 2^25, uniform ==
   |R|   tbl MiB |    NPJ ms |  radix ms       best cfg   part ms   join ms |    SM std    SM lsd  merge ms
  2^14      0.25 |     288.8 |     221.3           12+0      60.3     161.0 |     251.6     250.2      18.5
  2^16      1.00 |     303.7 |     248.9           12+0      61.6     187.3 |     284.4     248.4      20.3
  2^18      4.00 |     307.3 |     264.8            9+0      54.5     210.3 |     349.0     268.8      21.7
  2^20     16.00 |     431.7 |     277.5           10+0      57.6     219.8 |     411.8     258.6      26.6
   grid for |R| = 2^20: (cfg, swwcb) -> total ms [partition + join]
         4+0   plain     355.8 [   65.3 +   290.5]
         6+0   plain     329.4 [   59.0 +   270.4]
         8+0   plain     285.6 [   57.3 +   228.3]
         9+0   plain     278.8 [   57.8 +   221.0]
        10+0   plain     277.5 [   57.6 +   219.8]
        12+0   plain     282.6 [   65.3 +   217.4]
        14+0   plain     308.0 [   99.6 +   208.4]
         5+5   plain     339.3 [  124.7 +   214.7]
         6+6   plain     330.4 [  118.1 +   212.3]
         7+7   plain     323.2 [  116.4 +   206.8]
         4+0   swwcb     374.3 [   84.5 +   289.8]
         6+0   swwcb     346.7 [   80.3 +   266.4]
         8+0   swwcb     306.4 [   80.5 +   225.9]
         9+0   swwcb     297.9 [   81.9 +   216.0]
        10+0   swwcb     299.8 [   85.2 +   214.6]
        12+0   swwcb     313.4 [   96.5 +   216.9]
        14+0   swwcb     331.5 [  123.1 +   208.4]
         5+5   swwcb     384.4 [  165.0 +   219.4]
         6+6   swwcb     373.8 [  158.8 +   215.0]
         7+7   swwcb     369.3 [  163.3 +   206.0]
  2^22     64.00 |     719.1 |     311.2           10+0      63.1     248.1 |     563.2     358.3      53.4
   grid for |R| = 2^22: (cfg, swwcb) -> total ms [partition + join]
         4+0   plain     404.6 [   71.9 +   332.6]
         6+0   plain     374.6 [   64.2 +   310.4]
         8+0   plain     354.0 [   61.8 +   292.3]
         9+0   plain     333.2 [   61.4 +   271.9]
        10+0   plain     311.2 [   63.1 +   248.1]
        12+0   plain     312.5 [   71.4 +   241.1]
        14+0   plain     350.2 [  110.6 +   239.6]
         5+5   plain     398.3 [  142.2 +   256.1]
         6+6   plain     374.4 [  131.4 +   243.0]
         7+7   plain     371.8 [  134.6 +   237.2]
         4+0   swwcb     442.4 [   95.8 +   346.6]
         6+0   swwcb     394.3 [   85.0 +   309.3]
         8+0   swwcb     377.4 [   87.3 +   290.1]
         9+0   swwcb     355.4 [   89.1 +   266.3]
        10+0   swwcb     335.8 [   92.3 +   243.6]
        12+0   swwcb     346.2 [  103.8 +   242.4]
        14+0   swwcb     382.4 [  145.0 +   237.4]
         5+5   swwcb     432.2 [  184.6 +   247.6]
         6+6   swwcb     420.4 [  180.5 +   239.9]
         7+7   swwcb     420.7 [  181.9 +   238.8]
  2^24    256.00 |     973.5 |     424.9           12+0      97.1     327.8 |     833.3     544.1     147.8
   grid for |R| = 2^24: (cfg, swwcb) -> total ms [partition + join]
         4+0   plain     933.1 [  102.0 +   831.2]
         6+0   plain     524.7 [   90.2 +   434.5]
         8+0   plain     493.4 [   93.3 +   400.1]
         9+0   plain     470.9 [   82.6 +   388.3]
        10+0   plain     467.7 [   84.7 +   382.9]
        12+0   plain     424.9 [   97.1 +   327.8]
        14+0   plain     464.3 [  142.0 +   322.3]
         5+5   plain     575.3 [  187.8 +   387.5]
         6+6   plain     510.3 [  181.2 +   329.1]
         7+7   plain     504.6 [  169.9 +   334.7]
         4+0   swwcb     956.9 [  125.5 +   831.5]
         6+0   swwcb     557.1 [  114.1 +   443.0]
         8+0   swwcb     520.4 [  118.6 +   401.9]
         9+0   swwcb     512.8 [  120.7 +   392.1]
        10+0   swwcb     508.7 [  124.6 +   384.2]
        12+0   swwcb     471.0 [  137.3 +   333.7]
        14+0   swwcb     489.6 [  166.6 +   323.0]
         5+5   swwcb     629.2 [  244.0 +   385.2]
         6+6   swwcb     575.6 [  234.5 +   341.2]
         7+7   swwcb     562.4 [  235.5 +   326.9]

== skew and selectivity at |R| = 2^24 ==
uniform, 100% hit    NPJ    966.6 ms   best radix    439.7 ms (12+0)  matches 33554432
zipf 0.99            NPJ    774.8 ms   best radix    402.1 ms (12+0)  matches 33554432
uniform, 10% hit     NPJ    863.9 ms   best radix    405.6 ms (12+0)  matches 3357368
```

Reading it:

- **Crossover.** There is none to find in 2¹⁴…2²⁴: best radix beats the shared
  table by 1.31, 1.22, 1.16, 1.56, 2.31, 2.29× (NPJ ÷ radix). Sort-merge (LSD)
  beats the shared table everywhere and is within 13% of the radix join up to
  2²⁰ (it is faster at 2¹⁶ by 0.2% and at 2²⁰ by 7%), 15% and 28% slower at 2²²
  and 2²⁴.
- **Best bits.** 12, 12, 9, 10, 10, 12 for 2¹⁴…2²⁴, all single pass, plain. Two-pass
  (7+7 at best) lost by 16–20% at the three sizes whose grids are printed (2²⁰:
  323.2 vs 277.5; 2²²: 371.8 vs 311.2; 2²⁴: 504.6 vs 424.9). Write-combine buffers lost by 7–11% (2²⁰: 297.9 vs 278.8 at
  9 bits; 2²⁴: 471.0 vs 424.9 at 12 bits).
- **Equation (1) (Schuh et al.), worked for the M3.** `n_p = log2(|R|·st/(l·cache))`,
  st = 8 B, l = 0.5. With the 16 MiB L2: 2²⁴ → 4 bits (measured 933.1 ms, the
  worst plain single-pass point), 2²⁰ → 0 bits. With the 128 KiB L1d: 2²⁴ → 11
  bits (measured optimum 12), 2²⁰ → 7 bits (measured flat 8–12). The L1d is the
  working cache on this machine.
- **The partition pass** takes 54.5–63.1 ms for 2¹⁴…2²² (97.1 ms at 2²⁴, 12
  bits): 1.6–1.8 ns per tuple, 7–8× the 7.9 ms the 70 GB/s read bandwidth and
  a write of the same size would take.
- **Selectivity and skew at |R| = 2²⁴** (same bench, last block): at 10% hits
  the shared table costs 863.9 ms against 966.6 at 100% (the miss is paid
  per probe; fewer matches only save the checksum), the radix join 405.6 vs
  439.7. Zipf 0.99: 774.8 vs 402.1 — the shared table benefits more from skew
  (966.6 → 774.8, 0.80×) than the radix join (439.7 → 402.1, 0.91×), which is
  the direction Bandle et al. §5.4.5 and Schuh et al. Figure 15 report; at θ
  < 1 it does not flip the ranking.
- **Tiny build sides** (a separate run of the same solution, |S| = 2²⁵):

```
|R|     NPJ ms   radix ms    best bits  SM lsd ms
   2^4      213.8      186.1           12      267.6
   2^6      258.3      182.9           10      286.2
   2^8      267.6      183.9           12      297.8
  2^10      267.3      189.1           12      311.0
  2^12      260.6      201.0           12      290.6
```

  A 16-tuple build side still goes faster through 4,096 radix partitions
  (186.1 ms) than through the shared table (213.8 ms) — the evidence that the
  gain below the cache cliff is probe order, not cache size.

## Predictions (fill BEFORE implementing the stubs)

<details>
<summary>Worksheet — leave the right-hand cells empty until you have written your prediction down</summary>

| question | your prediction |
|---|---|
| lane 1: ns per probe at \|R\| = 2¹⁴ (table in L2) vs 2²⁴ (table 16× the cluster L2) | |
| lane 1: at which build size does ns/probe first exceed 2× its 2¹⁴ value? | |
| lane 1b: probe time at \|R\| = 2²⁴, Zipf 0.99 vs uniform | |
| lane 1c: probe speedup with 11 threads on the 256 MiB table | |
| lane 2: best total bits at \|R\| = 2²⁰ and at 2²⁴ (use Schuh Eq. 1 with the M3's 16 MiB L2) | |
| lane 2: one pass or two at \|R\| = 2²⁴? | |
| lane 2: does a write-combine buffer help on this machine? | |
| lane 2: partition phase, ns per tuple, as a multiple of the read-bandwidth floor from lane 0 | |
| lane 2: smallest \|R\| at which the best radix join beats lane 1 | |
| lane 2: same question at a 10% hit rate | |
| lane 3: sort time as a fraction of sort-merge total | |
| lane 3: smallest \|R\| at which sort-merge beats lane 1 | |

</details>

## Paper numbers worth keeping (each checked against the PDF)

Balkesen, Alonso, Teubner, Özsu, "Multi-Core, Main-Memory Joins: Sort vs. Hash
Revisited", PVLDB 7(1), 2013:

| number | where |
|---|---|
| workload A: 1,600 · 10⁶ build tuples = 11.92 GiB, probe m × that; workload B: 128 · 10⁶ each = 977 MiB; 4-byte key + 4-byte payload | §7.1, Table 2 |
| hardware: 4-socket Sandy Bridge, 32 KiB L1, 256 KiB L2, 20 MiB L3, 64 B lines, TLB 64 / 32 entries (4 KiB / 2 MiB pages), 512 second-level | §7.2 |
| radix partitioning throughput drops after a fan-out of 32 (the 2 MiB-page TLB) | §10.1 |
| partitioning up to 4 billion tuples/s ("4 billion × 8 bytes × 4 ≈ 128 GB/s"); merging falls from 1.5 to 0.5 billion tuples/s, fan-in 512 at 512 M tuples | §10 (Figure 8) |
| radix join more than 2× faster than m-way sort-merge at hundreds of millions of tuples | §12.1 |
| radix ≈ 650 M output tuples/s vs m-way ≈ 315 M (workload B, full threads) | §12.4, Figure 17(a) |
| radix 1 pass / 12 bits up to 1 billion tuples, then 2 pass / 18 bits; 512 × 64 B = 32 KiB (9 bits), 4,096 × 64 B = 256 KiB (12 bits) | §12.2 |
| optimised no-partitioning join only 10–15% faster than MPSM | §12.5 |

Schuh, Chen, Dittrich, "An Experimental Comparison of Thirteen Relational
Equi-Joins in Main Memory", SIGMOD 2016:

| number | where |
|---|---|
| NOP beat PRB in 2011 (Blanas), PRB beat NOP in 2013 (Balkesen), NOP beat PRB in 2013 (Lang) | §1 |
| 256 TLB entries for 4 KiB pages, 32 for 2 MiB; Xeon E7-4870 v2, 30 MB L3, 256 KiB L2 | §7.1 |
| every algorithm except PRB speeds up with huge pages; PRB partitions 7 bits = 128 per pass | §7.2, Figure 8 |
| partition time per tuple flat up to 2¹⁵ partitions, worse from 2¹⁶ (32 threads) | §7.3 |
| Equation (1) for the number of radix bits | §7.3 |
| wrong bits cost up to 2.5×; array joins up to 44% on dense keys; chunking and NUMA scheduling up to 26% and 20% | §9, lessons 6–8 |
| partition-based beats no-partitioning except 32 GB inputs and Zipf factor > 0.9 | §9, lesson 3 |
| join runtime only 10–15% of a TPC-H query | §9, lesson 9 |
| CPRL/CPRA up to 3.4 billion input tuples/s | §10 |

Bandle, Giceva, Neumann, "To Partition, or Not to Partition, That is the Join
Question in a Real System", SIGMOD 2021:

| number | where |
|---|---|
| radix join improves 1 of 59 TPC-H joins; BRJ beats BHJ for one join at SF 100, by 30% | abstract, §6 |
| Skylake-X i9-9900X: 10 cores, 1 MiB L2, 19 MiB LLC, 79.4 GiB/s | §5.1, Table 2 |
| BRJ up to 50% faster than RJ at low selectivity; RJ ahead above 50% hits; RJ 10–40% faster than BHJ at low selectivity, other factors near-optimal | §5.4.1 |
| RJ slows 7× while BHJ is flat over 5× larger tuples | §5.4.2 |
| late materialisation: RJ 153 → 341 M tuples/s (+122%), BRJ 487 → 656 (+35%), BHJ 452 vs 453 | §5.4.3, Table 3 |
| BHJ ahead of RJ above Zipf z = 1, 5× at z = 2 | §5.4.5 |
| Table 4: payload ≤ 32 B / 16 B, depth < 8 / < 2, Zipf ≤ 1 / 0.5, size difference < ×50 / ×10 | §6 |

## Guide-question checklist

- [ ] reading-balkesen-sort-vs-hash.md Q1–Q4
- [ ] reading-schuh-thirteen-joins.md Q1–Q4
- [ ] reading-bandle-to-partition.md Q1–Q4
- [ ] reading-duckdb-join-hashtable.md Q1–Q4
- [ ] reading-datafusion-hash-join.md Q1–Q4

## Cross-topic threads

- Topic 11 ↔ 54: DuckDB's radix-partitioned build side exists for parallel
  finalize and spilling; the in-memory probe is one global table.
- Topic 17 ↔ 54: SIMD histogram-and-scatter is lane 2's partition pass; Exercise 3
  here (a lower probe floor) is the other half of the SIMD story.
- Topic 47 ↔ 54: a Bloom or bitmap pre-filter is Bandle et al.'s BRJ; the join
  is the build side those filters are made from.
- Topic 48 ↔ 54: lane 0's latency and bandwidth ladder is the fast-tier input
  the CXL paper's partitioned/non-partitioned model assumes.

## Open questions

- How much of lane 2's gain below the cache cliff survives a probe with a lower
  floor (Exercise 3)? Lane 1d says the probe order accounts for 2× at a 256 KiB
  table; the unmeasured part is how low a branch-free probe goes.
- At what tuple width does the shared table win on the M3 (Exercise 4)? Bandle
  et al.'s Table 4 says 16–32 B on x86.
- Is the 16 MiB L2 really the last-level cache for a join, or does the system
  cache catch part of a 32–64 MiB table? Lane 0's 16 MiB point moves between
  runs (19–45 ns); a finer ladder between 12 and 40 MiB would say.
- Would non-temporal stores in the scatter move the best configuration from
  plain to write-combine buffers, as in the paper?

## Implementation log

- Lanes 0, 1, 1a–1d provided; lanes 2 and 3 stubbed with 12 failing tests (the
  specification). The reference solution passes all 21 tests.
