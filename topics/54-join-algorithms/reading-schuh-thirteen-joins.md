# Thirteen joins in one framework: why the literature contradicted itself

Balkesen et al. (the previous guide) found radix joins superior; Blanas et
al. and Lang et al. had found the no-partitioning join superior, with
different code on different machines. Schuh, Chen and Dittrich put every
published variant into one benchmarking framework, and found that most of the
contradiction was *implementation detail* — page size, write-combine buffers,
hash-table layout, how throughput is defined — and not algorithm. What
survives is a short list of rules, one of which is a formula that picks the
number of radix bits.

**"An Experimental Comparison of Thirteen Relational Equi-Joins in Main
Memory"** — Stefan Schuh, Xiao Chen, Jens Dittrich (Saarland University).
SIGMOD 2016
([pdf](https://faculty.cc.gatech.edu/~jarulraj/courses/4420-s19/papers/19-hashjoins/schuh-sigmod2016.pdf),
a course-hosted copy of the SIGMOD'16 paper; DOI 10.1145/2882903.2882917). I
read the PDF; section, figure and lesson numbers are from it.

## The problem in one sentence

Four papers from 2011–2013 reported opposite winners for "partition first"
vs "do not partition" because they differed in code and settings, so no
published number could settle which join to build — until the algorithms,
their optimisations and the metric were pinned down in one framework.

## The concepts, step by step

### Step 1 — why published results disagreed

> **In:** nothing. **Out:** the three sources of disagreement the paper
> lists, which are the checklist for reading any join benchmark.

The paper's introduction names them. (1) **Different implementations**: one
study's no-partitioning join used linked lists with separate lock and head
arrays; another used a single cache-friendlier array; a third used linear
probing with compare-and-swap. (2) **Different optimisations**: some radix
joins used software write-combine buffers (Balkesen's §5.2, guide 1), some did
not; some were NUMA-aware, some were not. (3) **Different metrics**: "join
throughput" as *output tuples per second* is sensitive to the join's
selectivity, while "input tuples per second" (|R| + |S| divided by time) is
not. The paper's wry summary of the first-order picture: "In odd years it is
NOP? In even years it is PRB? The answer is: we do not know."

Abbreviations used from here (Table 2): **NOP** is the no-partitioning join
(lane 1 here); **PRB** is the original two-pass radix join; **PRO** the same
with write-combine buffers; **CPRL** and **CPRA** the paper's own chunked
radix joins (L = linear-probing table per partition, A = array); **MWAY** is
the sort-merge join of Balkesen et al.; **CHTJ** is the concise hash table
join. A suffix **iS** means improved scheduling.

### Step 2 — page size is an algorithm parameter

> **In:** Step 1 of the previous guide (page, TLB). **Out:** a measured case
> where the "better" optimisation makes an algorithm slower.

§7.2 reruns all thirteen joins with 4 KiB pages and with 2 MiB pages (Figure 8)
on a machine whose TLB holds 256 entries for 4 KiB pages but only 32 for
2 MiB pages (§7.1). Every algorithm except PRB speeds up with huge pages. PRB
slows down, and the paper says why: it partitions 7 bits = 128 partitions per
pass with plain scatter writes. 128 write streams fit 256 small-page TLB
entries, but not 32 huge-page ones. The write-combine buffers of PRO remove
the problem. The general lesson, §9 lesson (4): "Use huge pages", and lesson
(5): "Use software-write combine buffers … a very effective technique to
reduce the number of TLB misses. Hence, using SWWCBs allows us to use single
pass algorithm".

### Step 3 — choosing the number of radix bits

> **In:** the partition scatter of the previous guide's Step 3 and Step 1's
> cache sizes. **Out:** the paper's Equation (1), every symbol named, worked
> once.

Too few bits and a partition's table spills out of the cache; too many and the
scatter itself falls apart. §7.3 shows the partitioning time per tuple "stays
almost constant up to including 2¹⁵ partitions" and deteriorates from 2¹⁶ with
32 threads, because 2¹⁵ one-line write buffers still fit the shared
last-level cache and 2¹⁶ do not. §9 lesson (6): suboptimal bits cost "up to a
factor of 2.5" (Figure 9). Equation (1) is the paper's rule, as printed:

```
            ⎧ log2( |R|·st / (l·L2) )     if  |R|·sb·st / (L2·l)  <  LLCt
n_p(|R|) =  ⎨
            ⎩ log2( |R|·st / (l·LLCt) )   otherwise
```

Symbols: `|R|` the number of build tuples; `st` the size of a tuple in bytes;
`l` the intended **load factor** of the per-partition hash table (fraction of
slots filled); `L2` the size of the L2 cache; `sb` the size of a partition
buffer; `LLCt` the last-level cache available per thread (footnote 5: the
last-level cache is shared, so it depends on how many threads run).

First branch: `|R|·st` is the bytes of R, dividing by `l·L2` asks "how many
L2-sized pieces is R, once each piece's table is inflated by 1/l?" and the
base-2 log turns that count into bits. Worked on assumed numbers, not the
paper's: |R| = 2²⁷ tuples (128 · 2²⁰), st = 8 B, l = 0.5, L2 = 256 KiB = 2¹⁸ B:

```
|R|·st = 2^27 · 2^3 = 2^30 B (1 GiB)
l·L2   = 0.5 · 2^18 = 2^17 B
ratio  = 2^30 / 2^17 = 2^13 = 8192 partitions   ->   n_p = 13 bits
```

The condition on the first line is the check that the one-line buffers for
those 2¹³ partitions still fit the last-level cache share (the formula's
second factor, as printed, carries `sb·st`; read the PDF's page 9 for exactly
what it multiplies). When it fails, the second branch sizes partitions to the
last-level cache instead of L2 — fewer bits, bigger partitions. §7.3's
verdict: the number of bits from (1) "leads in almost all cases to the lowest
runtime" (Figure 12), and the sweet spot is "the minimal number of bits such
that the partitions still fit in the shared level cache".

### Step 4 — what the thirteen look like side by side

> **In:** Steps 1–3. **Out:** the paper's ranking and the two cases where it
> inverts.

§10: "hardware-conscious partition-based approaches typically outperform
hardware-oblivious no-partition based joins on modern multi-core NUMA
architectures." The best, CPRL and CPRA, reach "up to 3.4 billion input
tuples per second". §9 lesson (3) names the exceptions: "All Partition-based
algorithms outperform all no-partitioning algorithms in almost all except for
only two cases." The first, **input relations of 32 GB**, where the worst
partition-based join is "a little bit slower than the best no-partitioning
algorithm". The second, **skewed probe keys**: partition-based joins suffer
unbalanced thread load while no-partitioning joins have fewer cache misses
(a hot key stays in cache); "No-partitioning algorithms start outperforming
partition-based algorithms only for a Zipf factor > 0.9" (Figure 15).
**Zipf factor** θ here is the exponent of the skew: the probability of the
k-th most popular key is proportional to 1/k^θ, so θ = 0 is uniform and θ near
1 puts a large share of the probes on a handful of keys. This topic's
generator draws exactly that distribution (`gen.rs`, Gray et al.'s method).

### Step 5 — the other rules, and the boundary of the whole study

> **In:** Steps 1–4. **Out:** the remaining lessons, each with the number
> the paper attaches.

- Lesson (1): "Don't use CPR* algorithms on small inputs" — below 8 million
  tuples chunked partitioning showed no benefit and sometimes a loss (thread
  creation does not pay; a chunk shrinks below a page).
- Lesson (7): a **dense** key domain (keys 0…n−1) lets the table be a plain
  array indexed by key; "array joins … outperform other non-array variants
  under all workloads by up to 44%". Lane 1's keys are deliberately sparse,
  so that this shortcut does not apply.
- Lesson (8): NUMA-aware chunking and scheduling gain "up to 26% and 20%".
  The M3 is not NUMA; this lesson has no counterpart here.
- Lesson (9): "the join time may actually only be a 10%–15% share of the
  total runtime of the query" in a TPC-H query. This is the sentence Bandle et
  al. (next guide) build their paper on.
- The setting for all of it (§7.1): four Xeon E7-4870 v2, 30 MB L3, 256 KiB L2,
  half a terabyte of memory, 4-byte key + 4-byte payload, a **dense primary-key
  build side** and foreign-key probes. The study's own caveats are its
  boundary: narrow tuples, dense keys, joins measured in isolation.

## How to read the paper (with the concepts in hand)

1. **§1** — the contradiction and its three causes (Step 1).
2. **§3, Table 2** — what each of the thirteen names means.
3. **§7.1–7.3** — Figure 8 (page size), Figure 9 and Equation (1) (bits).
4. **Figure 15 and the appendix** — skew and semi-dense keys.
5. **§9** — all nine lessons; for each, write down which premise from
   Step 5's list it depends on.

## Where each step lives in this topic

| step | where | what to look at |
|---|---|---|
| 1 | `experiments/src/tuple.rs` | `JoinOutput` — one definition of "matches" and one order-independent checksum, so no algorithm can win by changing the question |
| 2 | notes.md, "Page size" | the M3's page is 16 KiB (`sysctl hw.pagesize`); no 4 KiB / 2 MiB experiment is available, which is itself a finding |
| 3 | lane 2 grid in notes.md | the measured best bit count per build size next to Equation (1)'s prediction for the M3's L2 |
| 4 | `gen.rs`, lane 1b | the Zipf probe generator; the skew rows of lane 1b |
| 5 | `gen.rs` (`key_of`) | sparse keys by construction; no array-join shortcut |

## Questions (answer in notes.md)

1. Equation (1) takes `L2` and `LLCt`. On the M3 Pro the performance cluster
   has one 16 MiB L2 shared by five cores and the system has no separately
   advertised L3. What would you plug in for each, and what does lane 0's
   latency curve say about which is right?
2. Lesson (2) says to over-specify experiments. List every parameter of lane
   1 (hash function, load factor, tuple width, key distribution, thread count,
   page size) and say which of the paper's three disagreement causes each
   could have caused.
3. Figure 15's crossover is Zipf θ > 0.9. Lane 1b has θ = 0, 0.75 and 0.99.
   Predict from Step 4's argument which direction no-partitioning moves as
   θ grows, and by roughly how much at |R| = 2²⁴, then check.
4. The paper's metric for the headline is input tuples per second. Convert
   lane 1's last column to the same metric and to output tuples per second;
   at what hit rate do they differ by 10×?

## Done when

Answer each before unfolding it.

- [ ] You can list why published join benchmarks disagreed.

  <details><summary>Answer</summary>

  Different implementations (hash-table layouts), different optimisations
  (write-combine buffers, NUMA awareness) and different metrics — output
  tuples per second vs input tuples per second — (§1). The paper's response
  is a common framework with all optimisations available to every algorithm
  (Table 2), and its lesson (2): over-specify the experiment.

  </details>

- [ ] You can explain why PRB got slower with huge pages and PRO did not.

  <details><summary>Answer</summary>

  PRB scatters directly to 128 partitions per pass. With 4 KiB pages the
  TLB's 256 entries cover 128 write streams; with 2 MiB pages the TLB has
  only 32 entries, so most writes miss (§7.2, Figure 8). PRO stages writes in
  write-combine buffers, so the TLB sees one hot buffer page and a new
  destination page every N tuples.

  </details>

- [ ] You can apply Equation (1) and name its symbols.

  <details><summary>Answer</summary>

  n_p = log2(|R|·st / (l·L2)) while the buffers fit the last-level cache
  share; otherwise log2(|R|·st / (l·LLCt)). |R| build tuples, st tuple bytes,
  l table load factor, L2 and LLCt cache sizes. With |R| = 2²⁷, st = 8 B,
  l = 0.5, L2 = 256 KiB: |R|·st = 2³⁰ B, l·L2 = 2¹⁷ B, ratio 2¹³, so 13 bits —
  8,192 partitions of 128 KiB each, whose tables at load factor 0.5 take
  256 KiB, exactly the L2.

  </details>

- [ ] You can state the two cases where the best no-partitioning join wins.

  <details><summary>Answer</summary>

  Relations scaled to 32 GB (the worst partition-based join is a little
  slower than the best no-partitioning one), and highly skewed probe keys —
  Zipf factor above 0.9 — where partition sizes are unbalanced but the shared
  table's hot entries stay in cache (§9 lesson 3, Figure 15).

  </details>

- [ ] You can say why the paper's own measurements understate the join's share of a query.

  <details><summary>Answer</summary>

  Lesson (9): in a TPC-H query the join was only 10–15% of the runtime;
  scanning, filtering and tuple reconstruction made up the rest. The
  microbenchmarks time the join in isolation, on narrow tuples whose payload
  needs no reconstruction, so a 2× join speedup is a small query speedup —
  and the partitioning cost grows with the tuple width that the
  microbenchmark held at 8 bytes.

  </details>

## References

- Stefan Schuh, Xiao Chen, Jens Dittrich, **"An Experimental Comparison of
  Thirteen Relational Equi-Joins in Main Memory"**, SIGMOD 2016 — §1
  (contradictions), Table 2 (algorithms), §7.1 (setup), §7.2 (page sizes,
  Figure 8), §7.3 (Equation (1), Figures 9–12), Figure 15 (skew), §9 (lessons),
  §10 (conclusions).
- Previous: [reading-balkesen-sort-vs-hash.md](reading-balkesen-sort-vs-hash.md).
  Next: [reading-bandle-to-partition.md](reading-bandle-to-partition.md).
