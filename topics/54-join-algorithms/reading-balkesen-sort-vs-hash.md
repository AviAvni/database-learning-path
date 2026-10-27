# Radix joins vs sort-merge joins: what "hardware-conscious" buys

Every hash join in this repo's earlier topics built one table and probed it.
This paper is the reason a second design exists: before the hash table is
built, *scatter* both inputs into partitions small enough for the cache, so
that no probe ever has to leave it. It is also the paper that answers the
other perennial question — would sorting both inputs and merging them beat
hashing? — with numbers from a 64-thread machine.

**"Multi-Core, Main-Memory Joins: Sort vs. Hash Revisited"** — Cagri
Balkesen, Gustavo Alonso (ETH Zurich), Jens Teubner (TU Dortmund), M. Tamer
Özsu (Waterloo). PVLDB 7(1):85–96, 2013 (VLDB 2014)
([pdf](https://www.vldb.org/pvldb/vol7/p85-balkesen.pdf)). Section and figure
numbers below are from that PDF, which I read in full. The companion ICDE
2013 paper (*Main-Memory Hash Joins on Multi-Core CPUs: Tuning to the
Underlying Hardware*) is the origin of the radix join and no-partitioning
code the VLDB paper reuses (§2.2); its PDF could not be fetched, so nothing
below is quoted from it. The third algorithm family, MPSM (Albutiu et al.,
VLDB 2012), is evaluated *inside* this paper (§6.3, §12.5) and is described
here only as far as this paper describes it.

## The problem in one sentence

A hash join's probe is a random read into a table, so once the table is
larger than the cache every probe is a cache miss; partitioning removes the
misses at the price of an extra pass over the data, and the paper measures
whether that price is worth paying — against both a hash join that does not
partition and a sort-merge join that never hashes.

## The concepts, step by step

### Step 1 — the join being measured, and the throughput metric

> **In:** nothing. **Out:** the workload, the tuple shape and the metric every
> later step quotes.

An **equi-join** of two relations R and S pairs every tuple of R with every
tuple of S that has the same key. In a **hash join** the smaller relation, the
**build side** (here R), is loaded into a hash table, and each tuple of the
larger **probe side** (here S) looks up its key. The paper's tuples are
8 bytes — a 4-byte key and a 4-byte payload (§7.1, Table 2) — and S has a
**foreign key** into R: every S tuple finds exactly one partner in R, so the
join emits exactly |S| tuples.

Two workloads (Table 2). **Workload A** (adapted from Albutiu et al.): R is
1,600 · 10⁶ tuples = 11.92 GiB and S is m times that, m = 1…8. **Workload B**
(from Kim et al. and the ICDE paper): |R| = |S| = 128 · 10⁶ tuples, 977 MiB
each. The metric is **throughput in output tuples per second**, |S| divided
by the execution time (the caption of Figure 13). Worked: the text of §12.4
says radix join on workload B achieves about 650 million output tuples per
second, so the whole join of 128 · 10⁶ probe tuples took
128 · 10⁶ / 650 · 10⁶ = 0.197 s. That is the arithmetic that turns a
throughput into a latency, and it is how this topic's lane 1 prints "ns per
probe".

### Step 2 — why a table that does not fit in cache hurts

> **In:** Step 1's build and probe sides. **Out:** the four hardware terms
> (cache, cache line, page, TLB) and the one-sentence cost model for a probe.

A **cache** is a small fast memory between the core and DRAM, filled one
**cache line** at a time (64 bytes on the paper's Sandy Bridge machine, §7.2;
128 bytes on the M3 this topic runs on). A **page** is the unit the operating
system maps virtual to physical addresses in (4 KiB, or 2 MiB for "huge"
pages on the paper's machine). The **TLB** (translation look-aside buffer) is
a cache of those mappings: touching a page whose mapping is not in the TLB
costs a **TLB miss**, a lookup in the page tables, before the data access
even starts. The paper's machine had 64 first-level TLB entries for 4 KiB
pages and 32 for 2 MiB pages, plus 512 second-level entries (§7.2).

A probe into a shared table of random addresses therefore costs, once the
table exceeds the last-level cache, at least one DRAM access and often a TLB
miss on top; §5 says it directly: "access to the hash table results in a
cache miss". **Partitioning** is the fix §5 introduces: choose a partitioning
function so that R's partition *p* can only match S's partition *p*, make
each R partition small enough that its hash table is cache-resident, and the
probes of the join phase hit cache.

### Step 3 — radix partitioning: histogram, prefix sum, scatter

> **In:** a list of tuples and a hash of each key (Step 1's S, for example).
> **Out:** the same tuples grouped by partition, plus the start offset of
> each group.

**Radix partitioning** assigns a tuple to a partition by `b` bits of its
hashed key, so there are 2^b partitions and the number of partitions is the
**fan-out**. §5.1 gives the naive loop:

```
foreach input tuple t do
    k <- hash(t)
    p[k][pos[k]] = t        // copy t to target partition k
    pos[k]++
```

An implementation that must write into one contiguous output array first
needs to know where each partition starts. That takes three passes: a
**histogram** (count tuples per partition), a **prefix sum** (running total of
the counts, giving each partition's start offset), and the **scatter** (the
loop above, with `pos[k]` initialised to the start offset). Work it on eight
tuples with b = 2 whose hashed partition numbers are, in input order,
`2, 0, 3, 0, 1, 2, 0, 3` (an illustration, not from the paper):

```
histogram:   p0 = 3   p1 = 1   p2 = 2   p3 = 2
prefix sum:  starts   p0 = 0   p1 = 3   p2 = 4   p3 = 6    (end = 8)
scatter:     t0(p2) -> slot 4   t1(p0) -> 0   t2(p3) -> 6   t3(p0) -> 1
             t4(p1) -> 3        t5(p2) -> 5   t6(p0) -> 2   t7(p3) -> 7
output:      [t1 t3 t6 | t4 | t0 t5 | t2 t7]
```

Each partition is contiguous, and inside a partition the tuples keep their
input order. Lane 2's `partition` is exactly this, and its test checks the
grouping and the stability.

### Step 4 — the fan-out limit: the TLB

> **In:** Step 3's scatter loop and Step 2's TLB. **Out:** why one pass cannot
> use an arbitrary number of partitions, and what the paper does about it.

The scatter writes each tuple to the *current end of its partition*. With 2^b
partitions that is 2^b write streams at once, each on a different page. If
2^b exceeds the number of TLB entries, every write misses the TLB. §5.1: "The
number of TLB entries is thus treated as an upper bound to the partitioning
fan-out." §10.1 measures it: with 2 MiB pages the TLB holds 32 entries, and
"partitioning throughput significantly decreases after a fan-out of 32 due to
TLB misses." The remedy older than this paper is **multi-pass**
partitioning: instead of one pass with a fan-out of 2^12, do two passes with
a fan-out of 2^6 each — the second pass re-partitions every first-pass
partition — so no pass exceeds the TLB. The price is reading and writing the
data twice. Lane 2's `RadixConfig { bits_pass1, bits_pass2 }` is this choice.

### Step 5 — software write-combine buffers

> **In:** Step 4's problem (too many write streams). **Out:** the buffer
> design, the arithmetic for how many partitions fit, and what the paper
> measured.

§5.2's idea: give each partition a small buffer, one cache line, in a
contiguous block that lives in L1; write the tuple into its partition's
buffer; when the buffer fills, copy the whole line to the partition's
destination:

```
foreach input tuple t do
    k <- hash(t)
    buf[k][pos[k] mod N] = t
    pos[k]++
    if pos[k] mod N = 0 then copy buf[k] to p[k]
```

`N` is chosen so a buffer is one cache line. On the paper's machine that is
64 B / 8 B = 8 tuples. Now almost every write goes to the L1-resident buffer
block, which fits in a *handful of pages*, so the TLB sees one hot page plus
one new destination page every N tuples. The full-line copy also lets the
hardware use write combining and non-temporal stores, "thus avoiding to read
the cache line before writing it back" (§5.2). Capacity arithmetic from
§12.2: a 32 KiB L1 holds 32 KiB / 64 B = 512 buffers, so 9 bits is "very
efficient"; a 256 KiB L2 holds 256 KiB / 64 B = 4,096 buffers, so 12 bits "can
be tolerated"; "once the partitioning requirement goes above 16,384", two
passes of 9 bits with L1-resident buffers are best.

On the machine this topic measures (Apple M3 Pro, `sysctl`: performance-core
L1d 128 KiB, cache line 128 B, performance-cluster L2 16 MiB), the same
arithmetic gives 128 KiB / 128 B = 1,024 buffers (10 bits) in L1 and
16 MiB / 128 B = 131,072 (17 bits) in L2. Whether that is enough to make
partitioning pay is lane 2's answer, not arithmetic.

### Step 6 — the sort-merge alternative, and where it pays its cost

> **In:** Steps 1 and 3. **Out:** the three phases of the paper's `m-way`
> sort-merge join and the two numbers that explain its behaviour.

A **sort-merge join** sorts R and S by key and then walks both sorted
sequences with two cursors: advance the cursor with the smaller key, emit on
equality. The walk is linear and sequential, so all the cost is in the sort.
The paper's `m-way` (Figure 4, §6.1) does five phases: (1) range-partition
each relation by key so that threads need no synchronisation (the same
scatter as Step 3, with the fan-out about the thread count, 64–128), then
sort each partition with an AVX bitonic-network sort; (2) merge the sorted
runs across NUMA regions with a **multi-way merge**, which reads many runs at
once and writes one — "multi-way merging" is to sorting what the radix pass is
to hashing; (3–5) the same for S and then a linear merge join. §3 builds the
SIMD sort from **sorting networks** (fixed sequences of min/max compare-
exchange steps without branches) and **bitonic merge networks**.

The two numbers that explain it, both from the opening of §10 (Figure 8, "Trade-off between partitioning and merging"): partitioning alone runs at "up to
4 billion tuples per second by effectively utilizing all the available
bandwidth in our machine (4 billion × 8 bytes × 4 ≈ 128 GB/s; i.e., 3 reads/1
write for radix partitioning)", whereas merging "drops significantly from 1.5
to 0.5 billion tuples per second" as the fan-in grows (at a table size of
512 M tuples "multi-way merge runs with a fan-in of 512"). Check the first by
arithmetic: 4 · 10⁹ tuples/s × 8 B × 4 touches = 128 · 10⁹ B/s. The
consequence is the paper's §1 point about *duality*: sort-merge also needs
multiple passes once the input is big enough, and its merge fan-in does what
the partitioning fan-out does to the radix join.

### Step 7 — the verdict, and the premises it rests on

> **In:** Steps 1–6. **Out:** the paper's conclusions, each with the condition
> under which it holds.

- **Hash beats sort at the sizes databases usually see.** §12.1: "When input
  table sizes are in the hundred millions, radix hash join is more than 2
  times faster than m-way sort-merge join." §12.4, workload B at full
  threads: radix about 650 million output tuples/s, m-way about 315 million.
- **Sort-merge catches up only with very large inputs.** §12.2: "sort-merge
  join approaches the performance of radix only when the input tables become
  significantly large", because radix's partitioning needs more passes (its
  best configuration is "1 pass/12 bits up to 1 billion tuples" and then
  "2 pass/18 bits"). The sorted output is a further argument for sorting
  there.
- **MPSM was not the exception.** §12.5: the paper's own MPSM and no-
  partitioning join "achieve similar performance"; the optimised
  no-partitioning join is only 10–15% faster while lacking MPSM's partially
  sorted output; and "hash joins are still faster than sort-merge join
  counterparts".
- **Skew did not change the ranking** — but only with each side's skew
  handling turned on: radix with "fine-granular task decomposition", m-way
  with heavy-hitter handling (§12.3, Figure 16).
- **The premises**: a 4-socket Sandy Bridge with 64 hardware threads, 256 KiB
  L2, 20 MiB L3, 8-byte tuples, build keys unique, **every probe a hit**
  (§7.1–7.2). Bandle et al. (reading guide 3) show how much of the radix
  join's advantage lives in those premises, and the lane 2 measurements show
  what survives on a different memory system.

## How to read the paper (with the concepts in hand)

1. **§1 and Table 2** — the claims (radix superior; the "when sort wins"
   condition) and the two workloads of Step 1.
2. **§5, §6.4–6.5** — Step 3's loop, Step 4's TLB bound, Step 5's buffers, and
   the one-paragraph description of the radix and no-partitioning joins.
3. **§3–4** — the SIMD sort and merge: skim for the two ideas (min/max
   networks; multi-way merge) and the §4.1 argument that merge fan-in must
   fit the cache.
4. **§10, Figures 6–8** — the 4 billion tuples/s vs 0.5 billion tuples/s
   pair of Step 6, and §10.1's TLB measurement of Step 4.
5. **§12, Figures 14–18** — the verdict; for each figure write down which
   parameter varies and which is fixed.

## Where each step lives in this topic

| step | where | what to look at |
|---|---|---|
| 1 | `experiments/src/gen.rs`, `npj.rs` | the same 8-byte tuples, unique build keys, foreign-key probes; lane 1 is the no-partitioning join of §6.5 |
| 2 | lane 0 of `join_bench` | the dependent-load latency curve and the read bandwidth: this machine's cache sizes, measured |
| 3, 4 | `experiments/src/radix.rs` | `partition` and `RadixConfig` — lane 2's stubs and their tests |
| 5 | notes.md, "Reference numbers" | the single-pass vs two-pass and write-combine columns of the reference grid |
| 6 | `experiments/src/sortmerge.rs` | lane 3: sort and merge timed separately |
| 7 | README, "Where the crossover is" | the crossover between lane 1, lane 2 and lane 3 on the M3 |

## Questions (answer in notes.md)

1. Step 5 says a 32 KiB L1 holds 512 buffers of 64 B. §12.2 nevertheless
   says 12 bits "can be tolerated" using the L2. What does each additional
   bit cost when the buffers no longer fit the L1, and why does the paper's
   Figure 11-style curve degrade "gracefully" rather than falling off a cliff?
2. The throughput metric is output tuples per second. Why does that make the
   comparison sensitive to join selectivity (Schuh et al. open on this
   point), and what would 650 million output tuples/s mean at a 10% hit rate?
3. §12.2 sees radix go from 1 pass to 2 passes at about a billion tuples.
   Using Step 4, which resource ran out — TLB entries, L1 buffer space, or
   cache capacity for the partition's hash table?
4. Your lane 1 numbers show how many of the paper's premises (8-byte tuples,
   unique keys, 100% hits, huge pages, many threads) hold on the M3. Which of
   them could you flip in `gen.rs` to test the claim directly?

## Done when

Answer each before unfolding it.

- [ ] You can run radix partitioning on a short list by hand.

  <details><summary>Answer</summary>

  Count tuples per partition (histogram), turn the counts into start offsets
  with a running sum (prefix sum), then place each tuple at its partition's
  next free slot (scatter). For partition numbers 2, 0, 3, 0, 1, 2, 0, 3 the
  counts are 3, 1, 2, 2, the starts 0, 3, 4, 6, and the output order
  t1 t3 t6 | t4 | t0 t5 | t2 t7, stable inside each partition (Step 3, §5.1).

  </details>

- [ ] You can say why the fan-out of one pass is bounded and what the paper does about it.

  <details><summary>Answer</summary>

  The scatter keeps one write position per partition, each on a different
  page; when 2^b exceeds the TLB's entries every write misses the TLB
  (§5.1). §10.1 measures the drop after a fan-out of 32 with 2 MiB pages.
  Two remedies: multi-pass partitioning (two passes of fewer bits at the
  price of moving the data twice) and software write-combine buffers, which
  stage writes in an L1-resident block so the TLB sees one hot page and a
  new destination page only every N tuples (§5.2).

  </details>

- [ ] You can compute how many write-combine buffers a cache holds.

  <details><summary>Answer</summary>

  Cache size divided by buffer size (one line). Paper machine, 64 B lines:
  32 KiB / 64 B = 512 (9 bits), 256 KiB / 64 B = 4,096 (12 bits), the two
  limits quoted in §12.2. M3 performance core, 128 B lines: 128 KiB / 128 B =
  1,024 (10 bits) in L1; 16 MiB / 128 B = 131,072 (17 bits) in the cluster's
  L2.

  </details>

- [ ] You can explain why sort-merge also becomes multi-pass.

  <details><summary>Answer</summary>

  The multi-way merge reads one input per run, so its fan-in is the number of
  runs; at 512 M tuples the fan-in is 512, the merge no longer fits in the
  cache and its throughput falls from 1.5 to 0.5 billion tuples/s, while
  partitioning stays at about 4 billion tuples/s (§10, Figure 8). The
  paper's Partition-then-Sort variant range-partitions first with
  write-combine buffers and keeps stable performance as the input grows,
  where Sort-then-Merge degrades beyond 256 M tuples. "Sort also needs
  several passes" is the duality argument of §1.

  </details>

- [ ] You can state the paper's verdict and its three conditions.

  <details><summary>Answer</summary>

  Radix-partitioned hash join is more than 2× faster than the best sort-merge
  join at hundreds of millions of tuples (§12.1) and about 650 vs 315 million
  output tuples/s on workload B (§12.4); sort-merge approaches it only at
  billions of tuples (§12.2). Conditions: a many-core NUMA machine with
  256 KiB L2 and 64 B lines, 8-byte tuples with unique build keys, and every
  probe a hit (§7). Whether the ranking holds without those conditions is
  the question of the next two guides.

  </details>

## References

- Cagri Balkesen, Gustavo Alonso, Jens Teubner, M. Tamer Özsu, **"Multi-Core,
  Main-Memory Joins: Sort vs. Hash Revisited"**, PVLDB 7(1):85–96, 2013 —
  §3–4 (SIMD sort, merge), §5 (partitioning, buffers), §6 (algorithms),
  §7 (workloads, hardware), §10 (partitioning and merging phases), §12
  (verdict).
- Martina-Cezara Albutiu, Alfons Kemper, Thomas Neumann, **"Massively
  Parallel Sort-Merge Joins in Main Memory Multi-Core Database Systems"**,
  PVLDB 5(10):1064–1075, 2012 — MPSM, evaluated in this paper but not read
  directly.
- Next: [reading-schuh-thirteen-joins.md](reading-schuh-thirteen-joins.md).
  The topic README ([README.md](README.md)) has the measured problem.
