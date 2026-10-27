# To partition or not to partition: the radix join inside a real system

The first two guides measured joins as stand-alone kernels: two arrays of
8-byte tuples, every probe a hit, the join time and nothing else. This paper
takes the best radix join of that literature, puts it into a compiling
database (Umbra), runs TPC-H through it, and reports that on 58 of TPC-H's 59
joins it does not help. Its microbenchmarks then show *which* assumption of the
kernel benchmarks carries the radix join's advantage, one assumption at a time.
For a developer deciding whether to build a partitioned join, this is the
paper to read before the code.

**"To Partition, or Not to Partition, That is the Join Question in a Real
System"** — Maximilian Bandle, Jana Giceva, Thomas Neumann (TU Munich).
SIGMOD 2021, DOI 10.1145/3448016.3452831
([pdf](https://db.in.tum.de/~bandle/papers/bandle-partitionVsNonPartition.pdf)).
I read the PDF in full; section, figure and table numbers are from it.

## The problem in one sentence

A radix join wins when the build side is much larger than the cache, the
tuples are narrow, every probe finds a partner and the join stands alone — and
real queries mostly have a small build side, wide tuples, selective joins and
several joins in a row, so the partitioned join's extra pass moves tuples that
never reach the output.

## The concepts, step by step

### Step 1 — three joins, one system

> **In:** the radix join of the previous two guides. **Out:** the three
> implementations the paper compares and what each does that the kernel
> benchmarks did not.

All three run inside Umbra, a **code-generating** engine: it compiles each query
into machine code in which a pipeline of operators is one loop over the
tuples, and a **pipeline breaker** (an operator that must see all its input
before emitting — a hash-join build is one) ends the loop. §5.1.1 names them:

- **RJ**, the radix-partitioned join: software write-combine buffers,
  non-temporal streaming stores, two-pass partitioning, thread-local output
  buffers.
- **BRJ**, the Bloom radix-partitioned join: RJ plus a **Bloom filter** — a
  compact bit array that answers "definitely not in the build side" or "maybe in
  it" — probed *before* partitioning, so probe tuples that cannot match are
  dropped before they are copied. Section 4.7.
- **BHJ**, the buffered non-partitioned join: one global chaining hash table
  and a semi-join reducer built from tagged pointers, with **relaxed operator
  fusion** (§4.3): normally a compiled pipeline passes each tuple through every
  join in registers ("operator fusion"), which hides the cache misses from the
  CPU; relaxed fusion "loosen[s] the original idea ... in favor of intermediate
  materialization", buffering small batches of tuples between operators so that
  software prefetching can overlap the table misses (§1, §4.3).

The point of BRJ is a finding of §1: "for most queries the majority of the
shuffled tuples in the partitioning phase might not even be present in the
final result", because TPC-H joins are selective (Figure 2).

### Step 2 — where the kernels' assumptions come from

> **In:** Step 1. **Out:** the paper's table of prior-work assumptions
> against TPC-H's, one row per factor the later steps vary.

Table 5 sets the two worlds side by side. Prior work: skew Zipf 0–2, payload
8–16 B, pipeline depth one join, selectivity 100%, size difference 1–25,
build side far larger than the last-level cache. TPC-H: no skew, payload about
32 B, one to five joins per query, low selectivity, mostly large size
differences, build side mostly small. "Selectivity" here means the percentage
of probe tuples that find a partner. **Late materialization** (LM) is the
idea of carrying only the key and a tuple id through the join and fetching the
payload columns afterwards, so the partitioned tuple stays narrow.

The hardware (§5.1, Table 2) is three x86 machines. The main one is a
Skylake-X Core i9-9900X: 10 cores (20 threads), 32 KiB L1d, **1 MiB L2**,
**19 MiB LLC** (last-level cache), 79.4 GiB/s DRAM bandwidth.

### Step 3 — what happens on TPC-H

> **In:** Step 1's three joins. **Out:** the headline result and the join-level
> picture that explains it.

The abstract: "the radix join brings a noticeable improvement in only one out
of all 59 joins in TPC-H." §6: BRJ "is faster than the BHJ for SF 100 only for
one join in TPC-H, and even then only by 30%"; BHJ "achieved comparable speed
and a more stable performance than the BRJ for all queries". (SF is TPC-H's
scale factor; SF 100 is about 100 GB of data.) Figure 1 plots every SF 100 join by build-side and probe-side size, with the
LLC marked and each point coloured by the relative performance of the
Bloom-filtered partitioned join against the non-partitioned one. The text's
rule for the build axis is §5.4.6's: "having a build side smaller than the LLC
means there is no need for partitioning" (§5.4.6). Examples from §5.3.1: Query 11's largest
build side is 480 KB, so its table "fits in the L2 cache, making the
partitioning phase redundant"; Query 12's build side is 87 MB, "four times the
LLC size", and still "the prefetching keeps the BHJ's performance stable".
Also §5.3.1: for the "otherwise dominated" queries (Q3, Q10, Q15–Q18) joins
are under 40% of the runtime, which limits what any join implementation can do.

### Step 4 — the microbenchmarks: one assumption at a time

> **In:** Step 2's factors. **Out:** a number for each factor's effect, all
> from §5.4.

- **Selectivity (§5.4.1, Figure 14).** BRJ is "up to 50% faster than the RJ
  for low selectivities", but "when more than 50% of the foreign keys find a
  join partner, the RJ overtakes the BRJ" because each Bloom lookup is up to
  one more cache miss. The paper adds an adaptive switch that samples the
  probe and turns the filter off. And: "the RJ is 10 to 40% faster than the BHJ
  for low selectivities, when all other parameters are near-optimal".
- **Payload size (§5.4.2, Figure 15).** "The RJ performance degrades by a
  factor of 7, while the BHJ remains constant for five times larger tuples."
  Tuples are padded to a power of two for the write-combine buffers, and not
  buffered above 64 B.
- **Selectivity and payload together (§5.4.3, Table 3).** Late
  materialization helps the radix joins (RJ 153 → 341 million tuples/s, +122%;
  BRJ 487 → 656, +35%) and leaves BHJ at 453 vs 452.
- **Pipeline depth (§5.4.4, Figure 16).** Each radix join in a chain must
  materialise and carries its output columns as payload into the next, so the
  tuple widens with depth; BHJ pipelines through.
- **Skew (§5.4.5, Figure 17).** Skew *helps* the non-partitioned joins ("better
  temporal cache locality", fewer random accesses) and hurts the radix joins
  ("heterogeneous partition sizes, which complicates the partition
  scheduling"); on workload A the BHJ outperforms RJ "once the skew is higher
  than z = 1, and is more than five times faster for z = 2". The Zipf
  exponent z is defined as in the previous guide; the paper reaches z = 2, this
  topic's generator is limited to z < 1.
- **Build size (§5.4.6).** "As long as the build side fits into the LLC, the
  global hashtable does not suffer from cache misses, rendering partitioning
  useless."
- **Size difference (§5.4.7).** The partitioning cost is paid on both sides, so
  a probe side much larger than the build side makes it dominate: Query 22,
  size ratio 1:11, BRJ 30% faster; Query 5's fourth join, ratio 1:100, BHJ
  40% faster.

### Step 5 — the summary table and the conclusion

> **In:** Step 4. **Out:** Table 4, the ranges where partitioning "works" and
> where it is "beneficial", and the paper's recommendation.

Table 4 of §6:

```
factor            workable         beneficial
selectivity       handled by the Bloom filter
payload size      <= 32 B          <= 16 B
pipeline depth    < 8 joins        < 2 joins
skew (Zipf)       <= 1             <= 0.5
build size        > LLC            >> LLC
size difference   < x50            < x10
```

The conclusion: the radix join "is very sensitive to any deviation from the
near-optimal workload characteristics"; "theoretically, we can expect up to a
300% improvement by choosing the radix join" and in reality "for some cases we
even observe a performance drop"; so "unless the radix join is beneficial for
other reasons, e.g., larger than main-memory working sets, we express
reservations that implementing the radix join in a general-purpose production
system justifies the added complexity."

### Step 6 — how this changes what lane 2 means

> **In:** Steps 3–5. **Out:** the premises that lane 2's measurements on the
> M3 do and do not touch.

Lane 2's data is the kernel benchmark: 8-byte tuples, a probe side of 2²⁵
tuples against a build side of 2¹⁴ to 2²⁴, uniform keys, 100% hits (and one
10%-hit row), one join. Against Table 4 that is "beneficial" on payload
(8 B ≤ 16 B), pipeline depth (one join) and skew (uniform), and on build size
only for the larger builds. Its size difference is |S| / |R| = 2²⁵ / 2¹⁴ =
2,048 for the smallest build and 2 for the largest, so only builds of 2²² and
up (ratio ≤ 8) are inside the "beneficial" size-difference range of under 10×.
A partitioned join that does not win on the larger builds has therefore lost
*before* any of this guide's real-system factors are added; at the smaller
builds the table predicts it should lose on size difference alone. The
experiment also has a premise the paper's hardware does not: an M3's cache
hierarchy (lane 0). The README says where the crossover lands.

## How to read the paper (with the concepts in hand)

1. **§1, Figures 1–2** — the whole argument in two pictures: join sizes against
   the LLC, and tuple widths against selectivity in TPC-H vs prior work.
2. **§3 and §4.4–4.7** — the partitioned join's mechanics (Step 1 of
   guide 1) and how it sits in a compiled pipeline; skim §4.2 (materialization).
3. **§5.1–5.2** — hardware (Table 2) and the check that their joins match the
   reference implementations of Balkesen et al.
4. **§5.3.1–5.3.2, Figure 1, Figure 18** — Step 3.
5. **§5.4 and Table 4** — Steps 4 and 5; for each factor write down the
   direction and the size of the effect.

## Where each step lives in this topic

| step | where | what to look at |
|---|---|---|
| 1 | `experiments/src/npj.rs` | `probe_batched` is lane 1's version of relaxed operator fusion's batching (batches of 32 independent loads), without explicit prefetch instructions |
| 3 | lane 0, README | the 16 MiB P-cluster L2 is the "LLC" of this machine: the latency curve steps from 21 ns to 87 ns between 16 and 32 MiB |
| 4, selectivity | `gen.rs` (`hit_pct`), notes.md reference numbers | the 10%-hit row: partitioning moves tuples that never match |
| 4, skew | lane 1b | Zipf probes make the single shared table faster, as Step 4 says |
| 5 | notes.md | the reference crossover, read against Table 4's "build size" row |

## Questions (answer in notes.md)

1. Table 4's "build size > LLC" assumes an LLC of 19 MiB on the paper's
   machine. What is the right analogue on an M3 Pro — the 16 MiB cluster L2, or
   a larger system-level cache nobody publishes — and what in lane 0's latency
   curve decides it?
2. Step 4's selectivity result is about extra cache misses from the Bloom
   probe. Construct the case on lane 1's data (which `hit_pct`, which build
   size) where a Bloom pre-filter in front of the *non-partitioned* join helps,
   and say what it costs on the 100% case.
3. DuckDB and DataFusion (the next two guides) use a single shared table for
   an in-memory join. Which rows of Table 4 explain that choice for a system
   whose joins are mostly small-build, selective, and pipelined?
4. The paper's radix join writes 32-byte tuples (§5.4.2: 8 B payload + 8 B key
   + 8 B hash + 8 B padding). Lane 2's tuples are 8 bytes. Estimate how the
   partition-phase time per tuple should change with width, and test it by
   adding a payload array to your lane-2 implementation.

## Done when

Answer each before unfolding it.

- [ ] You can state the paper's headline and the mechanism behind it.

  <details><summary>Answer</summary>

  The optimised radix join improves only one of TPC-H's 59 joins noticeably
  (abstract; by 30% at SF 100, §6). The mechanism: partitioning copies every
  probe tuple, and in TPC-H most probe tuples do not find a partner (Figure 2),
  so the partitioning moves data that is then discarded; most build sides
  are also smaller than the LLC, where no partitioning is needed at all
  (§5.4.6, Figure 1).

  </details>

- [ ] You can give the direction and size of each factor's effect.

  <details><summary>Answer</summary>

  Selectivity: Bloom pre-filter up to 50% faster than RJ at low selectivity,
  RJ ahead above 50% hits (§5.4.1). Payload: RJ degrades 7× over 5× wider
  tuples while BHJ is flat (§5.4.2). Depth: radix joins widen their tuples at
  every join (§5.4.4). Skew: helps BHJ, hurts RJ; BHJ ahead above z = 1 and
  5× at z = 2 (§5.4.5). Build size: no benefit below the LLC (§5.4.6). Size
  difference: BRJ +30% at 1:11, BHJ +40% at 1:100 (§5.4.7).

  </details>

- [ ] You can read Table 4.

  <details><summary>Answer</summary>

  "Workable" is the range where partitioning is competitive: payload ≤ 32 B,
  fewer than 8 joins in a pipeline, Zipf ≤ 1, build side above the LLC, size
  difference under 50×. "Beneficial" is the narrower range where it beats the
  non-partitioned join: payload ≤ 16 B, fewer than 2 joins, Zipf ≤ 0.5, build
  side far above the LLC, size difference under 10×. Selectivity is handled by
  the Bloom filter and has no row of its own (§6).

  </details>

- [ ] You can say why this paper's conclusion does not contradict Balkesen and Schuh.

  <details><summary>Answer</summary>

  Those studies' workloads sit inside Table 4's "beneficial" column on every
  factor — 8–16 B tuples, one join, 100% selectivity, build side much larger
  than the cache (Table 5, "prior work") — so both are right about that
  workload. Bandle et al. change one factor at a time and show how quickly the
  benefit disappears, then show TPC-H sits outside the column (Figure 2).

  </details>

- [ ] You can name the one case the paper concedes to partitioning.

  <details><summary>Answer</summary>

  Working sets larger than main memory: "unless the radix join is beneficial
  for other reasons, e.g., larger than main-memory working sets". DuckDB's
  external hash join (next guide) is a radix-partitioned design for exactly
  that reason, with a shared table in the in-memory case.

  </details>

## References

- Maximilian Bandle, Jana Giceva, Thomas Neumann, **"To Partition, or Not to
  Partition, That is the Join Question in a Real System"**, SIGMOD 2021 —
  abstract and §1 (headline, Figures 1–2), §4.7 (Bloom reducer), §5.1
  (setup, Table 2), §5.3 (TPC-H), §5.4 (microbenchmarks, Table 3), §6
  (Table 4, conclusion), Table 5.
- Previous: [reading-schuh-thirteen-joins.md](reading-schuh-thirteen-joins.md).
  Next: [reading-duckdb-join-hashtable.md](reading-duckdb-join-hashtable.md).
