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

## Predictions (fill BEFORE running)

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

### redis zmalloc (reading-redis-zmalloc.md)
1. Cost of a shared fetch_add per malloc at 8 threads:
2. Key eviction vs page eviction — why the comparison is unfair:
3. My pool vs turso's page_cache.rs — three design diffs:

## M6 log (capstone)

- [ ] Buffer pool under the persistent backends
- [ ] Per-backend pools vs shared pool + MemoryTag accounting — decision + why:
- [ ] mmap write-back unpredictability reproduced (numbers):
