# Durner, Leis & Neumann: the allocator is a query-engine parameter

Every other chapter in this topic reads an allocator. This one reads the
only paper that measured allocators *inside a database* — and its main
result is uncomfortable: the same query engine, the same queries, the same
data, a different `malloc`, and the answer to "how fast is it, and how much
memory does it hold?" changes by more than any planner trick in this
curriculum.

**"On the Impact of Memory Allocation on High-Performance Query
Processing"** — Dominik Durner, Viktor Leis, Thomas Neumann. DaMoN 2019
(arXiv:1905.01135, [pdf](https://arxiv.org/pdf/1905.01135)). Five pages;
section and table numbers below are the paper's. The figures in this paper
are bar and line charts, so the numbers this chapter uses are the ones the
paper prints in text and in Tables 1 and 2; where a figure would be needed,
the chapter says so rather than reading values off a plot.

## The problem in one sentence

A parallel analytical engine allocates large numbers of short-lived blocks of
very different sizes from every core, so the system `malloc` is on its
critical path — and nobody had measured how much the choice of allocator
matters there.

## The concepts, step by step

### Step 1 — what the engine allocates

> **In:** the paper's engine and workload (§4).
> **Out:** the allocation profile every later number is conditioned on —
> which operators allocate, and how much of it reaches the allocator at all.

The authors' engine (their own main-memory system) runs group-bys and
joins with **pre-aggregation hash tables** — each thread builds a small local
hash table, then partitions are merged. Its tuple materialisation uses a
**transaction-local chunk allocator** for everything under 32 KB (§4): the
engine itself grabs medium-sized blocks and carves them up, so only
*medium and large* requests ever reach `malloc`. This matters for how to
read the results: the paper measures allocators under an engine that has
already removed the smallest allocations from their reach. A graph engine
that calls `malloc` per node or edge (as this topic's lane 1 does) is a
harsher client.

The profile is read off the paper's Figure 2 and one table. By *count*, the
most frequent allocations are 32 KB–512 KB (the chunk blocks); by *size*,
large hash-table bucket arrays dominate. Share of allocations by operator
(§4, TPC-DS scale factor 100, serial execution):

| operator | by size | by count |
|---|---|---|
| group by | 61.2% | 77.9% |
| join | 25.7% | 11.7% |
| set operations | 4.3% | 8.5% |
| temporary | 8.4% | 1.8% |
| other | 0.4% | 0.1% |

Both columns sum to 100.0%. Two operators — group by and join — account for
86.9% of the bytes and 89.6% of the allocations (61.2 + 25.7 and 77.9 + 11.7).

### Step 2 — the four goals

> **In:** Step 1's profile.
> **Out:** the four vocabulary words the paper judges allocators by, each
> given a measurable definition.

§1 lists the goals; the evaluation measures each.

- **Scalability** — overhead of `malloc` and `free` as threads are added.
  Measured by latency under load on a 60-core machine (Figures 5 and 7).
- **Performance** — the cost of one `malloc`/`free` pair on one thread.
- **Memory fairness** — "give freed memory back to the OS", so other
  processes on the machine can use it. Measured by how far the process's
  resident memory falls after a query ends (Figures 3 and 6, Table 2).
- **Memory efficiency** — avoid **fragmentation**: memory the allocator
  holds that the program is not using. Measured as resident bytes against the
  bytes the engine *requested* (Table 2).

Fairness and efficiency are different. An allocator can hold 10 GB it will
never reuse (unfair) or hold 2 GB scattered across half-empty pages it cannot
release (inefficient).

### Step 3 — the five contenders, in the paper's words

> **In:** Step 2's goals.
> **Out:** for each allocator, the one design fact that explains a later
> result.

(§3.) All are Ubuntu 18.10 packages except glibc 2.23, "still used in many
current distributions".

- **glibc malloc 2.23** — derived from ptmalloc2/dlmalloc. A pool of
  **arenas** (independent heaps), limited to eight times the number of cores;
  "for every allocation, an arena-wide mutex needs to be acquired"; memory
  returns to the OS only "if the top chunk (adjacent unmapped memory) is large
  enough".
- **glibc malloc 2.28** — the same plus a **thread-local cache (tcache)**:
  the paper's description is that a free slot in the tcache stores a freed
  chunk "instead of immediately freeing it", skipping the arena lock.
- **jemalloc 5.1** — independent arenas of 4 MB chunks; "low address reusage"
  (first-fit among large free regions, similar to best-fit in theory); and —
  most important for what follows — **decay-based purging with a wall clock**
  (since v4.1): unused pages are returned to the OS gradually, by time, via
  `MADV_FREE`.
- **TBBmalloc** — Intel's allocator, thread-local heaps of memory blocks, with
  huge objects (≥ 4 MB) going straight to the OS.
- **TCMalloc 2.5** — thread caches for small objects (≤ 256 KB), a central
  heap of 8 KB pages for the rest, and *aggressive decommit*: freed spans are
  released immediately with `MADV_FREE`.

`MADV_FREE` is a Linux system call telling the kernel that a range of pages
is no longer needed. The kernel *may* reclaim them but need not; the process
still sees them as part of its resident set until it does. That detail makes
Step 5's puzzle.

### Step 4 — the headline: one allocator, 2.7×

> **In:** Steps 1–3 and the paper's Figure 1 setting (4-socket Xeon E7-4870,
> 60 cores, 1 TB; TPC-DS SF 100; queries executed in sequence).
> **Out:** the size of the effect, as the paper states it.

The abstract's claim: "The right allocator can increase the performance of
TPC-DS (SF 100) by 2.7x on a 4-socket Intel Xeon server." In the realistic
setting of §5.1 — queries arriving with exponential gaps at λ = 1.25 queries
per second, at most ten in flight — overall *runtime* was similar across
allocators, but the per-query picture was not: "only jemalloc has minor wait
latencies", TBBmalloc and jemalloc are "bound by the actual execution of the
query", while both glibc versions and TCMalloc are "dominated by the wait
latencies" — queries sit in the scheduler's queue because the engine cannot
finish the ten in flight fast enough (Figure 4). §5.3 adds scale: TCMalloc is
"worse by more than 10x in comparison to any other allocator" on the 4-socket
machine while finishing level with the rest on the 10-core single-socket
one. **The allocator's ranking depends on the core count.**

### Step 5 — reading Table 2: resident memory is not the engine's memory

> **In:** Table 2 (4-socket Xeon, λ = 1.25 q/s, SF 100): per allocator, the
> bytes the engine *requested* and the bytes the process had *measured*
> resident, at peak and on average.
> **Out:** one ratio per allocator, and a method — compare resident memory
> against a request counter, because resident memory alone mis-ranks the
> allocators.

The engine tracks its own requested bytes "with almost no overhead", so the
paper can print both sides (Table 2):

| allocator | peak requested | peak measured | avg requested | avg measured |
|---|---|---|---|---|
| TCMalloc | 55.7 GB | 58.1 GB | 17.8 GB | 53.7 GB |
| malloc 2.23 | 61.4 GB | 61.0 GB | 26.2 GB | 41.3 GB |
| malloc 2.28 | 61.5 GB | 62.6 GB | 20.2 GB | 42.5 GB |
| TBBmalloc | 55.7 GB | 55.7 GB | 15.9 GB | 27.9 GB |
| jemalloc | 58.6 GB | 59.4 GB | 11.1 GB | 24.7 GB |

Work the **average inflation** — average measured ÷ average requested — on
three rows. jemalloc: 24.7 ÷ 11.1 = **2.23×**. glibc 2.28: 42.5 ÷ 20.2 =
**2.10×**. TBBmalloc: 27.9 ÷ 15.9 = **1.76×**. Two readings follow. First,
*peak* memory is nearly allocator-independent — everyone must hold the
working set at the moment of maximum demand (55.7–61.5 GB requested). The
allocators differ in what they do *between* peaks. Second, the ratios are
not the ranking you might hope: jemalloc's inflation (2.23×) is worse than
glibc 2.28's (2.10×) even though its *absolute* average (24.7 GB) is far lower
than glibc's (42.5 GB). The paper does not explain the gap; its table shows
that jemalloc's average *requested* footprint was also much lower (11.1 vs
20.2 GB), and the paper notes that "the DBMS requested average memory is also
higher for the allocators with increased memory usage" (§5.4) — slower
queries stay in flight longer. Ratios and absolutes answer different
questions.

Now the puzzle: TCMalloc's measured average (53.7 GB) is the highest of the
five against a middling request (17.8 GB), a 17.8-to-53.7 ratio of 3.02×. The paper's
explanation is Step 3's `MADV_FREE`: TCMalloc releases memory "accurately
with MADV_FREE", the kernel keeps those pages counted as resident until it
needs them, and a resident-set reading therefore overstates TCMalloc's real
use. On a single-socket machine where tracking `MADV_FREE` calls is cheap
(Figure 8), subtracting the freed pages makes TCMalloc's measured curve
"follow the DBMS required curve almost perfectly", and jemalloc, the other
`MADV_FREE` user, drops by 15% (§5.4).

This is directly relevant to this repo's own measurement. macOS has the same
effect under a different name — libmalloc uses `MADV_FREE_REUSABLE` — and
the bench's `rss.rs` records both `ri_resident_size` and `ri_phys_footprint`
for that reason.

### Step 6 — the NUMA table: more page faults, fewer remote accesses

> **In:** Table 1 (4-socket Xeon, λ = 1.25 q/s, SF 100), normalised to glibc
> malloc 2.28 = 100%.
> **Out:** the one trade-off that "fast" allocators make visible.

| allocator | local DRAM | remote DRAM | total DRAM | page faults |
|---|---|---|---|---|
| malloc 2.28 | 100% (63 B accesses) | 100% (172 B) | 100% (236 B) | 100% (41 M) |
| jemalloc | 120% | 97% | 103% | **400%** |
| TBBmalloc | 121% | 97% | 103% | **516%** |
| TCMalloc | 106% | 105% | 104% | 153% |
| malloc 2.23 | 103% | 100% | 101% | 139% |

("B" is the paper's unit: billions of accesses.) The two fastest allocators
touch **more** local memory and **less** remote memory, at the price of 4–5×
the minor page faults — because they return memory to the OS and re-acquire
it, so pages get zeroed and re-faulted (§5.1). The paper's conclusion is that
minor faults "are not crucially critical" while remote accesses are "the major
performance indicator". For a database that is not NUMA-aware this table is
a warning against reading `perf stat` page-fault counts as a defect.

## How to read the paper (with the concepts in hand)

1. **§4 first** — the profile of Step 1, and the chunk allocator that hides
   the small allocations.
2. **§3** — one paragraph per allocator; match each design fact to a Step 5
   or Step 6 result.
3. **Table 2 and Figure 3** — compute the inflation ratios yourself before
   reading the text.
4. **Table 1** — then the sentence about minor faults.
5. **§5.3 and Figure 7** — the scalability claim; note the three machines.
6. **§6** — the one-line summary table and the decision (jemalloc).

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | `experiments/src/trace.rs` | the graph-engine trace is the *opposite* regime: small objects straight to `malloc` |
| 3 | `alloc_bench --features mimalloc` / `jemalloc` | two of the contenders' descendants, on this machine |
| 5 | `experiments/src/rss.rs` | resident vs footprint on macOS |
| 6 | lane 1d | cross-thread frees and the allocators' resident cost |

## Questions (answer in notes.md)

1. The engine's chunk allocator keeps allocations under 32 KB away from
   `malloc`. Which of Table 2's rows would you expect to move most if it were
   removed, and why?
2. jemalloc's average inflation (2.23×) exceeds glibc 2.28's (2.10×), yet
   the paper calls jemalloc "mem. efficient" (+) and glibc 2.28 "∼" in the §6
   table. Which of the two ratios would a capacity planner use, and which a
   scheduler?
3. Table 1's 4× page faults come with 20% more local and 3% fewer remote
   accesses. On a machine with one NUMA node (as this one), would you expect
   the page-fault cost or the locality gain to survive? What would you
   measure?
4. The paper never runs a graph workload. Using lane 1b's output, say which of
   its five conclusions you would *not* carry over to a loader that
   allocates one `malloc` per edge.

## Done when

Answer each before unfolding it.

- [ ] You can state the four goals and why fairness and efficiency are
      different.

  <details><summary>Answer</summary>

  Scalability (cost under many threads), performance (single-thread
  cost), memory fairness (returning freed memory to the OS), memory
  efficiency (avoiding fragmentation) — §1. An allocator can be efficient but
  unfair: it packs live data tightly yet keeps the empty pages mapped. It can
  be fair but inefficient: it unmaps promptly but only the pages that happen
  to be completely free, leaving scattered half-empty ones.

  </details>

- [ ] You can compute Table 2's inflation for any row and say why peak
      memory barely differs.

  <details><summary>Answer</summary>

  Average measured ÷ average requested: jemalloc 24.7 ÷ 11.1 = 2.23,
  glibc 2.28 42.5 ÷ 20.2 = 2.10, TBBmalloc 27.9 ÷ 15.9 = 1.76. Peaks are within
  55.7–61.5 GB requested because every allocator must hold the working set when
  demand peaks; they differ in what they keep afterwards.

  </details>

- [ ] You can explain why TCMalloc's resident memory overstates its use.

  <details><summary>Answer</summary>

  It releases memory with `MADV_FREE`, which lets the kernel reclaim pages
  lazily; until it does, the pages still count as resident. The paper's
  single-socket measurement with `MADV_FREE` pages subtracted (Figure 8) makes
  its curve follow the engine's requested curve closely, and jemalloc's drops
  15% (§5.4).

  </details>

- [ ] You can explain Table 1's page-fault column without calling it a
      regression.

  <details><summary>Answer</summary>

  jemalloc and TBBmalloc return memory to the OS and take it back, so pages
  are re-zeroed and re-faulted: 400% and 516% of glibc 2.28's 41 M minor
  faults. But they also show 20–21% more local and 3% fewer remote DRAM
  accesses, and remote accesses are the paper's "major performance
  indicator" on a 4-socket machine (§5.1).

  </details>

## References

- Dominik Durner, Viktor Leis, Thomas Neumann, **"On the Impact of Memory
  Allocation on High-Performance Query Processing"**, DaMoN 2019,
  arXiv:1905.01135 — abstract (2.7×), §3 (allocators), §4 (engine, operator
  table), §5.1 (Table 1, Figures 3–4), §5.3 (scalability), §5.4 (Table 2,
  Figure 8), §6.
- Next: [reading-hoard.md](reading-hoard.md). Cross-topic:
  topic 6's [reading-redis-zmalloc.md](../06-buffer-pool/reading-redis-zmalloc.md)
  (what an accounting wrapper adds), topic 48
  ([README](../48-memory-tiering/README.md)) (page-granular placement
  policies inherit the allocator's page layout), and `topics/54-join-algorithms/`
  (the hash tables whose bucket arrays dominate Step 1's bytes).
