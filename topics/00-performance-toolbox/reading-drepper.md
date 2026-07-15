# CPU caches and TLBs: the constants aged, the structure didn't

Every latency table in topic 0 §2 is a compressed version of one 2007 paper —
Drepper's "What Every Programmer Should Know About Memory". Before you open
its 114 pages, this chapter builds the eight concepts the paper assumes, one
at a time — then hands you a section-by-section reading lens. The DDR2
numbers are stale; the cache-organization math, the prefetching rules, and
the measurement methodology behind `cache_ladder` are forever.

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

Out-of-order cores can overlap many *independent* misses (memory-level
parallelism: 10 misses in flight ≈ 10× cheaper per miss). But if load N+1's
*address* comes from load N's *result* — a linked list, a tree descent — no
overlap is possible. That's a **pointer chase**, and it measures raw latency:

```rust
// ring[i] holds the index of the next element to visit (a shuffled cycle).
// Because address N+1 is unknown until load N retires, ns/step == the raw
// latency of whatever level the working set lands in — L1, L2, SLC, DRAM.
fn chase(ring: &[usize], steps: usize) -> usize {
    let mut i = 0;
    for _ in 0..steps {
        i = ring[i];            // serialized miss: nothing to prefetch
    }
    i                           // return it so the loop isn't dead code
}
// grow ring.len() from 16 KB to 512 MB and plot ns/step → the plateaus
```

This is the measurement engine behind Drepper's famous Fig 3.4 *and* behind
this topic's `cache_ladder` experiment: plot ns/step against working-set
size, and the plateaus ARE the cache levels.

### Step 6 — virtual memory: every address you use is fake

Your pointers are **virtual addresses**. Hardware translates each one to a
physical DRAM location via the **page table** — a map stored, awkwardly,
*in memory itself*, organized as a 4-level radix tree over 4 KB **pages**
(16 KB on Apple Silicon). Walking it costs up to 4 dependent loads:

```
A TLB miss is pointer chasing in silicon — 4 dependent memory loads:

CR3 ──► PGD entry ──► PUD entry ──► PMD entry ──► PTE ──► finally, your data
        (load 1)      (load 2)      (load 3)     (load 4)
        each load can itself miss cache ⇒ worst case ~4 × DRAM latency
        before the ACTUAL access even starts
```

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

## Questions to answer in notes.md when done

1. Why does `cache_ladder` show *gradual* transitions between plateaus rather than
   steps? (Hint: set associativity + random chain touching multiple sets.)
2. Predict: on M-series with 128 B lines, at what stride does a strided-read benchmark
   stop getting faster per element? Verify with a quick experiment.
3. How many memory accesses can a single TLB miss add on a 4-level page table, and why
   don't we see it in `cache_ladder`? (Hint: 16 KB pages, working set vs TLB reach.)

## Takeaway

Every table in topic 0 §2 is a compressed version of this paper. Drepper's method —
plot access cost against working-set size and *explain every inflection* — is the
habit; the numbers you regenerate yourself on your own machine.

## References

**Papers**
- Drepper — "What Every Programmer Should Know About Memory" (Red Hat,
  2007) — [PDF](https://people.freebsd.org/~lstewart/articles/cpumemory.pdf)
  (~114 pages — read §3–§4 properly, skim §6, skip the rest; the study
  guide's advice stands)
