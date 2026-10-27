# Topic 6 notes — buffer pool & memory management

## Baseline (provided lane, Apple M3 Pro, measured 2026-07-28)

`cargo run --release --bin pool_vs_mmap` — 1 GiB file, 2 M Zipf(0.99) page
reads, 8 bytes touched per page so the access rather than the copy dominates.

| impl | p50 | p99 | p99.9 | max |
|---|---|---|---|---|
| mmap | 42 ns | 1500 ns | 4459 ns | **181 887 ns** |
| pool (CLOCK, yours) | | | | stub |

**p50 42 ns, max 182 µs: a 4300× spread, and every bit of it is the tail.** The
median is a hit on a page the kernel already had resident — essentially free,
which is exactly why mmap is so tempting. The p99.9 and the max are minor page
faults: a trap into the kernel, a read, a TLB shootdown, and no way for the
database to know it happened, schedule around it, or prefetch ahead of it. That
is CIDR '22's argument in one row.

Read the handicap honestly before you compare your pool to this: the mmap side
gets the *whole* machine's page cache (there is no per-process cap on macOS),
while your pool will be held to a 256 MiB budget against a 1 GiB file. mmap is
playing with an advantage. If your pool still wins the tail, that is conclusive;
if mmap wins the median, that is expected and the interesting question is why
the tail behaves differently from the median at all.

## Baseline (translation_micro, provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin translation_micro` — 2²⁰ pages × 64 B (64 MiB), a
50%-full open-addressing table (32 MiB), 5 M accesses per cell, two runs
(run 1 / run 2), ns per access. The machine was loaded by unrelated builds:
trust ratios, not absolutes.

| miss% | workload | direct | hash | predictive |
|---|---|---|---|---|
| 0 | independent | 4.2 / 4.9 | 14.2 / 10.8 | 10.9 / 8.5 |
| 0 | chained | 101.5 / 102.9 | 213.4 / 198.2 | 107.5 / 107.5 |
| 10 | independent | – | 9.8 / 10.2 | 8.8 / 8.6 |
| 10 | chained | – | 201.1 / 200.9 | 130.6 / 118.5 |
| 50 | independent | – | 11.2 / 10.3 | 16.4 / 16.1 |
| 50 | chained | – | 243.2 / 209.5 | 178.4 / 184.2 |

`miss%` = fraction of pages not in their preferred frame (the guess is wrong).
`direct` is only valid at 0%. The chained lane is a B-tree descent in miniature
(the next page id is read out of the page just fetched); the independent lane
lets the CPU overlap iterations anyway.

**Read:** chained at 0% miss, `hash` ≈ 2 × `direct` (two serial misses) and
`predictive` ≈ `direct` + 6%. The saving erodes with the miss rate and breaks
even near 66% under a two-point linear model (slow path ≈ 255 ns). Independent
accesses gain little (the CPU overlaps them already) and lose at 50% miss. The
first version of this lane showed **no** gain: with the guessed read inside the
`if f == pred` arm the optimizer can reuse the equality and make the read
depend on the lookup again; hoisting the read above the branch fixed it. See
`reading-predictive-translation.md`, Step 8.

## Baseline (heat_scatter_model, provided lane, deterministic)

`cargo run --release --bin heat_scatter_model` — what an *oracle* cache (always
holds the most popular items that fit) achieves under the Bf-Tree paper's
workload (Zipf 0.9, 200 M records of 32 B, 2 GiB cache; paper §6.1), for a
record cache and for a page cache whose popular keys are scattered or clustered.
Seeded; identical output on every run (checked twice). No timing.

```
Part 1 — Zipf(0.9) over N = 200,000,000 records, exact sums
  80% of requests reach the top 15.1% of records (paper §6.1 parenthetical: 33%)
  oracle record cache, full mini-pages: holds 26.1% of records, hit ratio 0.854
  oracle record cache, lone-record mini-pages: holds 7.9% of records, hit ratio 0.739
Part 2 — page cache at 1/10 scale: N = 20,000,000, 281,691 pages, cache holds 52,326 pages
  oracle PAGE cache, heat CLUSTERED : 0.812
  oracle PAGE cache, heat SCATTERED : 0.566
  oracle RECORD cache, full mini-pages  : 0.848
  oracle RECORD cache, lone-record pages: 0.728
```

The paper's §6.5 reports about 50% of reads from cache for page-caching systems
and "almost 75%" for Bf-Tree. The scattered page-cache bound (0.57) and the
record-cache bounds (0.73–0.85) bracket both; the clustered page-cache bound
(0.81) says that if the paper's hot keys were adjacent, a page cache would
already be near the record cache. The paper does not say which holds, and its
"80% of requests access 33% of records" is 15.1% under the textbook pmf (the
exact sum above). See `reading-bftree.md`, Step 8.

## Predictions (fill BEFORE running)

- translation_micro, chained 0% miss: predictive / hash = ___ (the paper's whole-system figure is 0.83: +20.6% throughput, §6.4)
- pool_vs_mmap p50: mmap ___ ns vs pool ___ ns (who wins the median and why?)
- pool_vs_mmap p99.9: mmap ___ vs pool ___ (who wins the tail?)
- CLOCK hit rate vs strict LRU on Zipf(0.99), 16× universe: gap of ___ %
- strict LRU ns/access vs CLOCK: ___× slower

## pool_vs_mmap results

(paste; run warm and note whether the file fit the OS page cache)

| | p50 | p99 | p99.9 | max |
|---|---|---|---|---|
| mmap | | | | |
| pool (CLOCK) | | | | |

Pool hit rate: ___ %. Explanation of the tail difference:

## eviction bench results

| policy | hit rate | ns/access |
|---|---|---|
| CLOCK | | |
| strict LRU | | |
| FIFO | | |

Verdict — is strict LRU's hit-rate edge worth its per-hit cost here?

## Buffer pool build log (src/buffer_pool.rs)

- Where the WAL rule hooks in (which write, which LSN check):
- What a background writer would take off the miss path:
- What a 6-page scan did to usage counts (hot_page_survives_scan_pressure):

## Reading-guide questions

### postgres bufmgr (reading-postgres-bufmgr.md)
1. 18 refcount bits vs 4 usage bits — which cap fails gracefully:
2. Who unpins after a crashed query:
3. Buffer rings (admission) vs cooling stage (eviction) — what each misses:
4. What O_DIRECT buys/costs given double buffering:

### DuckDB buffer manager (reading-duckdb-buffer.md)
1. Why weak_ptr in eviction nodes:
2. Worst-case dead-node ratio; when CLOCK's fixed array is strictly better:
3. Throw-on-pressure vs error-when-all-pinned — capstone choice:

### LeanStore (reading-leanstore.md)
1. One-parent constraint walkthrough; are FalkorDB matrix blocks a tree or DAG:
2. Why eviction must be bottom-up:
3. Random-cooling hit-rate loss vs LRU (estimate, then measure):
4. What survives in vmcache, what dies:

### mmap paper (reading-mmap-paper.md)
1. Which topic-5 crash_test would fail under mmap and why:
2. Why eviction (not fault-in) triggers TLB shootdowns:
3. Reconciling the paper with LMDB's wins:
4. vmcache vs the four problems — solved vs softened:

### LeanStore/vmcache papers (reading-leanstore-paper.md)
1. Classic-pool overhead as arithmetic (topic-0 numbers):
2. Why cooling is a FIFO, not a stack:
3. vmcache state word vs postgres packed state — what colocation buys:
4. Which design is admissible for matrix-tile DAGs:

### Bf-Tree (reading-bftree.md)
1. Growth chain and capacity per class for 100-byte values with 16-byte keys:
2. Scattered vs clustered page-cache bound at Zipf 0.99 (predict, then run):
3. Table entry: paper's 8 bytes vs the source's 24, for 2.8 M leaves:
4. Which promotion rate (1% / 20% / 30%) and what it trades:
5. Where the circular buffer's one `Mutex` would show up:

### redis zmalloc (reading-redis-zmalloc.md)
1. Cost of a shared fetch_add per malloc at 8 threads:
2. Key eviction vs page eviction — why the comparison is unfair:
3. My pool vs turso's page_cache.rs — three design diffs:

## M6 log (capstone)

- [ ] Buffer pool under the persistent backends
- [ ] Per-backend pools vs shared pool + MemoryTag accounting — decision + why:
- [ ] mmap write-back unpredictability reproduced (numbers):
