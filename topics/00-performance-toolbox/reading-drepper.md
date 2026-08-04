# CPU caches and TLBs: the constants aged, the structure didn't

Every latency table in topic 0 §2 is a compressed version of one 2007 paper —
Drepper's "What Every Programmer Should Know About Memory". Before you open
its 114 pages, this chapter builds the eight concepts the paper assumes, one
at a time — then hands you a section-by-section reading lens, and finally a
table mapping each concept to the counter and the experiment that identify it
in someone else's program. The DDR2 numbers are stale; the cache-organization
math, the prefetching rules, and the measurement methodology behind
`cache_ladder` are forever.

## The problem in one sentence

A modern core executes an instruction in ~0.3 ns, but fetching data from
main memory (DRAM) takes ~80–100 ns — roughly **300 instructions of waiting**
for one load. Everything in this paper is machinery to hide that gap, and
every database trick in later topics (columnar layouts, B-tree fanout,
vectorized execution) is a way of cooperating with that machinery.

## The concepts, step by step

### Step 1 — the speed gap, and why caches exist

Memory got bigger much faster than it got faster. The fix: put small,
fast memories *between* the core and DRAM, and keep recently-used data there.

```
              size        latency      what it is
 registers    ~1 KB       0 cycles     inside the core
 L1 cache     ~128 KB     ~4 cycles    per-core, split data/instruction
 L2 cache     ~4-16 MB    ~14 cycles   per-core or per-cluster
 L3 / SLC     ~24-48 MB   ~40 cycles   shared by all cores
 DRAM         GBs         ~300 cycles  the actual memory
```

A "cache hit" = found at that level. A "miss" = go down one level and wait.
The whole game is: what fraction of your loads hit L1?

### Step 2 — the cache line: memory moves in fixed-size chunks

Caches don't store individual bytes. They store **lines** — fixed 64-byte
blocks (128 bytes on Apple M-series). Load 1 byte and the hardware fetches
the whole line it lives in.

Two consequences that shape databases:

- **Touching 8 bytes costs a full line.** Filter on one 8-byte column of a
  wide row and you waste 94% of every transfer:

```
filter on one 8-byte column, 128 B cache lines (M-series):

row layout:  line = [ a │ b  c  d  e  f  g ... padding ... ]   use 8 B / 128 B → 94% wasted
col layout:  line = [ a  a  a  a  a  a  a  a  a  a  a  a  a  a  a  a ]   use 128 B → 0% wasted
```

  That's topic 12 (columnar storage) in one diagram — Drepper's Fig 3.11.

- **Neighbors are free.** Once the line is in L1, the other 120 bytes cost
  nothing. Sequential scans exploit this; pointer chasing throws it away.

### Step 3 — where can a line live? Sets, ways, and conflict misses

A cache can't search all its lines on every load — that would be too slow. So
it's organized like a **hash table with fixed-size buckets**: some middle
bits of the address pick a **set** (the bucket), and each set holds N lines
(**N-way associative**, typically 8–16). A new line evicts one of the N
residents of *its own set only*.

This gives the three miss types a vocabulary:

- **cold** — first touch, unavoidable
- **capacity** — working set simply bigger than the cache
- **conflict** — the set is full even though the cache isn't (bucket
  collision: many hot addresses hash to the same set, e.g. a stride that
  equals the set-index period)

### Step 4 — the prefetcher: hardware that bets on your next load

The memory system watches your access pattern. Sequential or fixed-stride
loads are detected and the *next* lines are fetched before you ask —
hiding DRAM latency entirely. The bet fails on random access: the prefetcher
has nothing to extrapolate, so every miss pays full price.

This is why "sequential vs random" is the single most important distinction
in the topic 0 latency table — same data, same cache, ~10× difference.

### Step 5 — dependent loads: the one latency you cannot hide

A load is **dependent** when the core cannot compute its address until an
earlier load has come back. `chain[idx]` where `idx` was itself loaded from
memory; `node->next->next`; a B-tree descent, where the child pointer lives
inside the parent node you are still waiting for.

Why that one word decides everything: an out-of-order core does not run one
load at a time. It keeps hundreds of instructions in flight and issues *every*
load whose address it already knows, so several misses sit in the memory
system simultaneously. This is **memory-level parallelism (MLP)**, and it
means the cost of a miss is not a property of the miss — it is a property of
how many other misses could keep it company.

```
independent loads — a[0], a[1], a[2]: all three addresses computable right now

  load A  ├──────── ~100 ns ────────┤
  load B   ├──────── ~100 ns ────────┤        three misses overlap in the
  load C    ├──────── ~100 ns ────────┤       memory system
                                      └─► ~105 ns total ⇒ ~35 ns "per miss"

dependent loads — B's address IS the value A returned

  load A  ├──────── ~100 ns ────────┤
  load B                            ├──────── ~100 ns ────────┤
  load C                                                      ├─── ~100 ns ───┤
                                                                              └─► ~300 ns total ⇒ 100 ns each
```

Same cache, same DRAM, same number of misses — 3× apart here, and ~10× apart
in practice once the out-of-order window is full. Note what the diagram
implies: **latency and bandwidth are different questions.** A chase leaves the
memory bus nearly idle (one 128-byte line per ~104 ns ≈ 1.2 GB/s, against the
24–57 GB/s a single core reaches on a streaming scan in topic 12); it is slow
while doing almost nothing. Nothing you can do about the bus will help it.

This repo measured both sides of the diagram on the same machine
([`notes.md`](notes.md)):

| what | working set | ns per access |
|------|------------:|--------------:|
| `lookup_shootout` `hashmap` at n=1e7 — 1024 **independent** probes | ~160 MB | **9.3** |
| `cache_ladder` at 128 MB — a **dependent** chase | 128 MB | **104** |

Both are random DRAM accesses that "should" cost ~100 ns. The 11× gap is
overlap and nothing else. That is why the hash table looked suspiciously flat
at ten million keys, and why a *single* isolated lookup in the capstone would
not enjoy the same number.

The chase is how you measure the un-hidable case. Three properties of
[`cache_ladder`](experiments/benches/cache_ladder.rs) do the work:

```rust
fn chase(chain: &[usize], start: usize, steps: usize) -> usize {
    let mut idx = start;
    for _ in 0..steps {
        idx = chain[idx];   // the value loaded IS the next address
    }
    idx                     // returned so the loop isn't dead code
}
```

1. **The dependency is in the data, not the code.** `idx = chain[idx]` cannot
   be reordered, hoisted, or speculated around by any compiler or any core —
   the next address genuinely does not exist yet. One miss in flight, always.
2. **`chain` is a random cyclic permutation** (Sattolo's algorithm). Random
   kills the prefetcher (Step 4) so nothing arrives early; *cyclic* — one cycle
   through every slot — stops a short sub-cycle from quietly living in L1 and
   flattering the big sizes.
3. **`idx` is carried across criterion iterations.** The first version of this
   benchmark restarted at `idx = 0` each iteration, re-walked the same 65,536
   slots, and reported ~25 ns for "DRAM" — it had measured an ~8 MB hot path
   that the benchmark itself created. The fix is in the source comment;
   the confession is in `notes.md`.

The readout is unusually direct: with no arithmetic between the loads,
`elapsed / steps` **is** the latency of one access at that working-set size.
Sweep the size from 16 KB to 512 MB and the plateaus are the cache levels —
this is Drepper's Fig 3.4, and it is the only number in this repo you can
compare to a datasheet without an argument.

The database consequence is the whole curriculum: pointer-chasing layouts
(linked lists, naive trees, record-per-node graph stores) pay full latency per
hop, while layouts that expose addresses up front (arrays, matrices,
page-sized nodes, batched lookup APIs) convert latency into throughput. When a
later topic says a design "exposes memory-level parallelism", it means: the
addresses are knowable early enough to overlap the waiting.

### Step 6 — virtual memory: every address you use is fake

Every pointer your program holds is a **virtual address**. The physical DRAM
location is decided by the OS — which gives each process its own address space,
maps pages lazily (your `Vec` allocation may have no physical memory behind it
until first touch), shares pages between processes, and backs some of them with
files. So *every* load needs a translation: virtual page → physical frame. The
map is the **page table**, and it lives, awkwardly, in memory itself.

**Why a tree and not an array.** A flat lookup table would be one entry per
page: a 47-bit address space with 16 KB pages is 2³³ pages × 8 bytes = **64 GB
of table per process**. Unaffordable. So it is a **radix tree**: the virtual
address is chopped into fixed-width slices, each slice indexes one level, and
only the sub-tables that actually have mappings are ever allocated. An idle
process's page table is a few KB. You pay for sparsity with depth — and depth
here means *loads*.

**How the address is chopped** (x86-64, 4 KB pages — the paper's case):

```
 47        39 38        30 29        21 20        12 11          0
┌────────────┬────────────┬────────────┬────────────┬─────────────┐
│  9 bits    │   9 bits   │   9 bits   │   9 bits   │  12 bits    │
│  L4 index  │  L3 index  │  L2 index  │  L1 index  │  offset     │
└─────┬──────┴─────┬──────┴─────┬──────┴─────┬──────┴──────┬──────┘
      │            │            │            │             └─ byte inside the page,
      ▼            ▼            ▼            ▼                never translated
   ┌──────┐    ┌──────┐    ┌──────┐    ┌──────┐
   │ PGD  │───►│ PUD  │───►│ PMD  │───►│ PTE  │───► physical frame number
   └──────┘    └──────┘    └──────┘    └──────┘             │
   ▲ load 1      load 2      load 3      load 4             ▼
   │                                              + offset ──► your data, at last
   └── CR3: physical address of this process's top table (swapped on context switch)

why 9 bits: one table is one 4 KB page = 4096 / 8 = 512 entries = 2⁹
why dependent: each entry holds the *physical address of the next table*, so
  load N+1's address is unknown until load N returns — Step 5's chain, in
  silicon, running BEFORE your actual access can even issue
```

The same tree, three vocabularies for the same four levels — you will meet all
three while reading:

| level | x86 manuals | Linux source | ARMv8 |
|-------|-------------|--------------|-------|
| top   | PML4        | PGD          | L0    |
| ↓     | PDPT        | PUD          | L1    |
| ↓     | PD          | PMD          | L2    |
| leaf  | PT          | PTE          | L3    |

**Apple Silicon is shallower, because its pages are bigger.** With a 16 KB
granule a table holds 16384 / 8 = 2048 entries = **11 bits** of index, and the
offset takes 14 bits. So `14 + 11 + 11 + 11 = 47` — a 47-bit user address space
is covered in **three** levels, not four. Bigger pages buy a shorter walk *and*
4× the TLB reach (Step 7) from the same entry count.

**Four things keep this from being catastrophic**, and knowing them is the
difference between fearing the diagram and predicting it:

- **Hardware walks it, not the kernel.** The MMU's page-table walker does those
  loads in silicon, costing nanoseconds. The kernel only gets involved when
  there is no valid entry — a **page fault**, which is microseconds, a
  thousand-fold different event.
- **The tables are ordinary cacheable memory.** The upper levels are touched by
  every access in the region, so they normally sit in L1/L2; only the leaf level
  is likely to be cold.
- **There are dedicated page-walk caches** (x86 paging-structure caches, ARM
  walk caches) holding partial translations, so a walk often skips its first
  levels entirely.
- **Huge pages truncate the walk.** An entry at the PMD/L2 level can be a
  *block* descriptor pointing straight at 2 MB of contiguous physical memory
  (32 MB with a 16 KB granule) instead of at another table — one fewer load,
  and one TLB entry covering 512× more address space.

So: worst case is ~3–4 dependent DRAM loads *added in front of* your access;
typical case is far less. The measurement is in `notes.md` and it lands where
this predicts — `cache_ladder`'s tail rises **87 → 113 ns** from 64 MB to
512 MB, an added ~26 ns per access once 32K pages overflow the TLB. Not the
+400 ns of a fully cold walk, not zero either. That +26 ns is this diagram,
priced.

### Step 7 — the TLB: a cache for translations, with tiny reach

Doing that 4-load walk per access would be absurd, so translations are
cached in the **TLB** (translation lookaside buffer). The catch is
**reach**: ~2K entries × 4 KB pages ≈ only a few MB of address space covered.
Working sets beyond that miss in the TLB *as well as* the caches — the two
penalties stack. This is why databases care about **huge pages** (2 MB/1 GB
pages multiply reach by 512×; Apple's 16 KB base pages already 4× it).

### Step 8 — multiple cores: coherency and false sharing

Each core has its own L1/L2, so hardware keeps copies **coherent**: writing
a line invalidates every other core's copy of it. The pathology is **false
sharing** — two threads writing *different* variables that happen to share
one line. The line ping-pongs between cores at ~100-cycle cost per bounce,
and multi-thread scaling collapses with no visible reason in the source.
Padding each thread's data to its own line fixes it. (This pays off in
topic 9, concurrency.)

## How to read the paper (with the concepts in hand)

The paper is ~114 pages; §3–§4 are the payload.

- **§3.1–3.2** — skim; this is Steps 1–3 with 2007 diagrams.
- **§3.3 — read carefully.** The famous measurements. Fig 3.4 (sequential vs
  random over working-set size) is *exactly* `cache_ladder`; compare his
  plateau shapes with yours before explaining your numbers in `notes.md`.
  You now know why random loses even in DRAM: no prefetch (Step 4) + TLB
  misses (Step 7) + DRAM row activation.
- **§3.3.2** — critical word first / early restart: the CPU resumes as soon
  as the needed word arrives, before the rest of the line does.
- **§3.4** — instruction cache: skim (matters again at topic 19, JIT).
- **§3.5 — read carefully.** Coherency + false sharing (Step 8) with the
  multi-thread scaling-collapse measurements.
- **§4.1–4.3** — Steps 6–7. The key bit is §4.3 on TLB reach.
- **§4.4+, §5, §7** — virtualization and NUMA: skip until a NUMA box matters.
- **§6** — skim for the checklist: sequential > random; hot struct fields
  together, sorted by size; padding audits. §6.2's cache-oblivious matrix
  transpose is worth 10 minutes — the intellectual ancestor of
  blocked/vectorized execution (topic 11).

What's stale vs. forever: DDR2 timings, front-side bus, and Pentium 4
details aged; the organization math, miss taxonomy, and measurement method
didn't. Keep the Apple Silicon deltas in mind while reading: 128-byte lines
(not 64), no inclusive L3 (shared SLC instead), much larger L1 (128–192 KB).

## Finding these concepts in a real program

Steps 1–8 are visible in a microbenchmark you wrote on purpose. The harder
skill is spotting them in a program you did not write, where the pathology is
one loop among thousands. Three instruments, in the order you should reach for
them:

1. **A sampling profiler** (`samply`, `cargo flamegraph`, Instruments →
   Time Profiler) tells you *where* — which line owns the time. It cannot tell
   you *why*, and here is the trap that matters for this whole chapter: a
   sampling profiler attributes stall time to the instruction that is *waiting*.
   A memory-bound loop and a compute-bound loop look identical — one hot
   instruction — because "executing" and "blocked on DRAM" are the same sample.
   This repo hit exactly that: the `lookup_shootout` flamegraph in `notes.md`
   showed 21% in SipHash and ~79% in one inlined probe loop, and no amount of
   staring at it could split "hashing" from "waiting".
2. **Hardware counters** tell you *which wall*. This is the only instrument
   that distinguishes the eight concepts directly. On Linux: `perf stat`. On
   macOS there is no `perf` — Instruments → **CPU Counters** template gives you
   the events, and for real counter work run the same crate in a Linux VM or
   container.
3. **A differential experiment** — change exactly one thing, re-measure —
   is the only fully portable instrument, and the one this repo leans on. Each
   row of the table below has one, because a counter tells you a number is high
   while a differential proves the *causal* link.

Start with the funnel: two counters (`instructions`, `cycles` → IPC) plus a
branch-miss rate narrow eight suspects to one or two.

```mermaid
flowchart TD
    A["IPC = instructions / cycles"] -->|"≥ ~2, and it scales<br/>with more cores"| B["compute-bound<br/>→ algorithm / SIMD (topic 17)"]
    A -->|"IPC under ~1"| C{"branch misses above<br/>~1% of branches?"}
    C -->|yes| D["branch-bound<br/>→ Step 3 of the README, topic 17"]
    C -->|no| E{"achieved GB/s near<br/>the machine's peak?"}
    E -->|yes| F["bandwidth-bound<br/>→ Steps 2, 4: line waste, layout"]
    E -->|"no — bus mostly idle"| G{"dTLB misses<br/>significant?"}
    G -->|yes| H["translation-bound<br/>→ Steps 6, 7: huge pages, smaller reach"]
    G -->|no| I["latency-bound<br/>→ Step 5: dependent loads, no MLP"]
    style B fill:#1f6feb,color:#fff
    style D fill:#8957e5,color:#fff
    style F fill:#bf4b8a,color:#fff
    style H fill:#bf4b8a,color:#fff
    style I fill:#d29922,color:#000
```

The last branch is the one people get wrong: **latency-bound and
bandwidth-bound are opposites.** Both look "memory-bound" in a flamegraph, and
they have opposite fixes — more bandwidth-efficient layouts do nothing for a
pointer chase, and more overlap does nothing for a saturated bus.

| Concept | Signature in a profile | Counters (Linux `perf`) | Differential test that proves it |
|---|---|---|---|
| **1** Hierarchy at all | Low IPC, flat profile, time on loads | `cycles,instructions` | Shrink the dataset with the algorithm unchanged. Time/op drops sharply ⇒ you were paying the hierarchy, not the code. |
| **2** Cache-line waste | Hot loop touches one field of a wide struct | `cache-references,cache-misses`, plus achieved GB/s vs *useful* bytes | Split hot fields out (AoS→SoA) or shrink the struct. Faster with identical instruction count ⇒ you were paying for bytes you never read. |
| **3** Conflict misses | A cliff at a power-of-two size or stride, while the working set still "fits" | `L1-dcache-load-misses` high with a small working set | Pad the stride by one line (row stride 4096 → 4096+128). Faster ⇒ conflict, not capacity. Nothing else moves that. |
| **4** Prefetching | Sequential and random over the *same* data differ ~10× | (vendor-specific prefetch events; weak) | Feed the same loop a sorted vs shuffled index array. The gap *is* the prefetcher's contribution. |
| **5** Dependent loads | One load instruction owns the samples, IPC ≪ 1, **and achieved bandwidth is low** — slow while the bus idles | `cycles,instructions`; on x86 the stall-on-memory events | Run k independent chases interleaved with k cursors. Per-step time falls ~k× until it saturates ⇒ you were latency-bound with spare MLP. Batched/vectorized lookup APIs exist to collect that k×. |
| **6–7** TLB / page walks | A *second*, later cliff after the DRAM plateau has flattened | `dTLB-loads,dTLB-load-misses` (x86: `dtlb_load_misses.walk_completed`) | Enable huge pages (`MADV_HUGEPAGE` / THP) or drop the working set under TLB reach. On macOS: compare above vs below reach — that is `cache_ladder`'s last two rows, 87 → 113 ns. |
| **8** False sharing | Multi-thread scaling collapses; time sits in a *store*; per-thread work is unchanged | `perf c2c` — the purpose-built tool | Pad each thread's datum to its own line (128 B on M-series) and re-plot the scaling curve. Curve straightens ⇒ false sharing. |

Two habits that make this reliable:

- **Always pair a counter with a differential.** "Cache misses are high" is not
  a diagnosis; databases miss cache constantly and are fine. The differential
  answers the only question that matters — *would fixing it help?*
- **Compute the useful-bytes ratio by hand.** Bytes your algorithm needs ÷
  bytes the machine moved. It needs no profiler, catches Step 2 instantly, and
  is the number that decides row-vs-column layouts in topic 12.

## Questions to answer in notes.md when done

1. Why does `cache_ladder` show *gradual* transitions between plateaus rather than
   steps? (Hint: set associativity + random chain touching multiple sets.)
2. Predict: on M-series with 128 B lines, at what stride does a strided-read benchmark
   stop getting faster per element? Verify with a quick experiment.
3. How many memory accesses can a single TLB miss add on a 4-level page table, and why
   don't we see it in `cache_ladder`? (Hint: 16 KB pages, working set vs TLB reach.)
4. Take `lookup_shootout` at n=1e7 — 9.3 ns per probe over a ~160 MB table — and prove
   with the Step 5 differential (not with reasoning) that it is latency-bound with spare
   MLP rather than bandwidth-bound: make the probes *dependent* (each key derived from
   the previous lookup's result) and report the new ns/probe. Which row of the
   profiler table did you just walk down?

## Takeaway

Every table in topic 0 §2 is a compressed version of this paper. Drepper's method —
plot access cost against working-set size and *explain every inflection* — is the
habit; the numbers you regenerate yourself on your own machine.

## Done when

- [ ] You can recite the latency ladder — L1, L2, L3, DRAM — within 2x, and say which numbers from 2007 have aged and which have not.
- [ ] You can explain a conflict miss in terms of sets and ways, and construct a stride that causes one on purpose.
- [ ] You can say why a dependent-load chain is the one latency the prefetcher cannot hide, and why `cache_ladder` is built as a pointer chase for exactly that reason — including all three of its construction choices (data dependency, random *cyclic* permutation, cursor carried across iterations).
- [ ] You can state the difference between latency-bound and bandwidth-bound, name the one measurement that separates them, and say why a flamegraph never can.
- [ ] You can compute a TLB's reach from entry count and page size, and explain why exceeding it looks like a second, later cliff.
- [ ] Given a strange profile, you can name the counter *and* the differential experiment for each of the eight concepts, without re-reading the table.
- [ ] You wrote answers to all four questions in notes.md.

## References

**Papers**
- Drepper — "What Every Programmer Should Know About Memory" (Red Hat,
  2007) — [PDF](https://people.freebsd.org/~lstewart/articles/cpumemory.pdf)
  (~114 pages — read §3–§4 properly, skim §6, skip the rest; the study
  guide's advice stands)

**Tools referenced above**
- Brendan Gregg — [perf examples](https://www.brendangregg.com/perf.html) —
  the counter-event cookbook behind the profiler table's middle column.
- [`perf c2c(1)`](https://man7.org/linux/man-pages/man1/perf-c2c.1.html) —
  purpose-built false-sharing detection (Step 8); no macOS equivalent, so this
  is one of the cases worth a Linux VM.
- Instruments → **CPU Counters** template — the macOS substitute for
  `perf stat`; see topic 0 §4 for the full tool table on this machine.
