# Reading — Drepper, "What Every Programmer Should Know About Memory" (2007)

PDF: https://people.freebsd.org/~lstewart/articles/cpumemory.pdf (~114 pages — read
§3–§4 properly, skim §6, skip the rest; the study guide's advice stands).

## What's stale vs. what's forever

2007 paper: the *constants* aged, the *structure* didn't. Reading lens:
- Stale: DDR2 timings, front-side bus, Pentium 4/NetBurst details, exact cache sizes.
- Forever: cache organization math, why misses cost what they cost, prefetching rules,
  the measurement methodology (his benchmark plots are the blueprint for `cache_ladder`).
- Apple Silicon deltas to keep in mind while reading: 128-byte cache lines on M-series
  (not 64!), no inclusive L3 (shared SLC instead), much larger L1 (128–192 KB).

## §3 — CPU caches (the core, ~35 pages)

- **3.1–3.2** Skim. Cache hierarchy diagrams + associativity. Know: set-associative =
  hash table with N-way buckets; conflict misses = bucket collisions.
- **3.3 (read carefully)** — the famous measurements. Fig 3.4 (sequential vs random
  access over working-set size) is *exactly* the `cache_ladder` experiment; compare his
  plateau shapes with yours before explaining your numbers in `notes.md`. Understand
  *why* random is worse than sequential even in DRAM: TLB misses + no prefetch + row
  activation.
- **3.3.2** Critical word first / early restart — why the miss cost isn't a full line
  transfer.
- **3.4** Instruction cache — skim (matters again at topic 19, JIT).
- **3.5 (read carefully)** Cache coherency + **false sharing** (Fig 3.27-ish,
  multi-thread scaling collapse). This is the section that pays off in topic 9
  (concurrency) — two atomics on one line = cacheline ping-pong.
- **Fig 3.11** (cache-line utilization) explains why columnar layouts win: touching 8
  bytes of a 128-byte line wastes 94% of the transfer. Topic 12 in one figure.

## §4 — Virtual memory (~10 pages)

- **4.1–4.2** Page tables are a 4-level radix tree walked *in memory* — a TLB miss is
  up to 4 dependent loads. Sound familiar? It's pointer chasing (topic 0 §2).
- **4.3 (the key bit)** TLB reach: 4 KB pages × ~2K entries ≈ a few MB — far smaller
  than working sets. Why databases care about **huge pages** (2 MB/1 GB; 16 KB base
  pages on Apple Silicon already 4x the reach).
- Skim the virtualization part (4.4+).

## §6 — What programmers can do (skim for the checklist)

Sequential access > random; `-O2 -march=native`; struct layout: hot fields together,
sorted by size; `pahole`-style padding audits; NUMA awareness (§5/§7 — skip until a
NUMA box matters). §6.2's cache-oblivious matrix transpose is worth 10 minutes — it's
the intellectual ancestor of blocked/vectorized execution (topic 11).

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
