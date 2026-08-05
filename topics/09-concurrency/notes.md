# Topic 9 notes — latches, lock-free & epochs

Predict FIRST, then measure.

## Baseline (provided lanes, Apple M3 Pro, measured 2026-07-28)

### false_sharing — 8 threads, 5 M increments each, own counter per thread

| layout | time | rate | vs pad128 |
|---|---|---|---|
| packed | 202.7 ms | 197.4 M inc/s | 17.8× slower |
| pad64 | 20.4 ms | 1957.6 M inc/s | 1.8× slower |
| pad128 | 11.4 ms | 3502.9 M inc/s | — |

- **17.8× packed → pad128.** "Independent" counters sharing a line are not
  independent — this is the whole reason redis pads `used_memory`.
- **pad64 is still 1.8× slower than pad128**: Apple M-series coherence
  granularity is 128 B. A hand-written `#[repr(align(64))]` — the textbook "pad
  to one cache line" — only HALF-fixes false sharing on this machine. Note that
  crossbeam's `CachePadded` is *not* the thing being caught out here: it is
  `repr(align(128))` on x86-64 and aarch64 alike. Check the 64-byte assumption
  wherever you wrote it yourself.
- **Run-to-run variance is large on the packed row** and worth knowing about:
  an earlier run of this same binary recorded 636 ms / 63 M inc/s, i.e. a 59×
  ratio rather than 17.8×. Contended-line throughput depends on how the threads
  happen to interleave, so treat the *order of magnitude* as the finding and
  quote a range, not a point, if you cite it.

### scaling — 90/10 read/write mix, keyspace 100 000, Mops/s total

| impl | 1t | 2t | 4t | 8t | 16t |
|---|---|---|---|---|---|
| global mutex | 8.65 | 5.32 | 2.84 | 2.86 | 2.96 |
| sharded ×16 | 11.63 | 8.40 | 8.66 | 11.22 | 12.65 |
| crossbeam SkipSet | 4.21 | 9.07 | 14.39 | 14.82 | 19.28 |
| mine | | | | | stub |

**The global mutex gets 2.9× SLOWER going from 1 thread to 16.** Not "stops
scaling" — actually negative: 8.65 → 2.96 Mops/s. Adding cores to a
single-lock structure removes throughput, because the cores spend their time
transferring the lock's cache line and parking/unparking instead of working.
That line shape (peak at 1 thread, decay after) is the signature to recognise in
production: it means the fix is never "more threads".

Two more shapes worth naming: sharding recovers most of it but is *non-monotonic*
(dips at 2t, recovers by 8t) because 16 shards with few threads is mostly
uncontended luck; and the lock-free skip set is the only one that is slowest at
1 thread (4.21, vs the mutex's 8.65) and fastest at 16 — atomics and epoch
bookkeeping cost real single-threaded performance to buy a slope. That trade is
the whole topic.

## Predictions (fill in BEFORE running scaling.rs)

| Measurement | Prediction | Actual | Surprised? |
|---|---|---|---|
| global mutex: shape 1→16t | | | |
| sharded-16: where does it stop scaling? | | | |
| crossbeam SkipSet 16t vs global 16t (×?) | | | |
| my ConcurrentSet vs crossbeam at 16t | | | |
| my set at 1 thread vs topic-2 sequential skiplist | | | |

Reasoning space:
- 90/10 mix: the global mutex serializes READS too — estimate its ceiling
  from one uncontended lock/unlock (~20 ns?) per op.
- 16 shards, 16 threads, uniform keys: collision probability per op ⇒
  expected stall fraction (birthday-ish). Where's the knee?
- Lock-free reads scale with cores until… what? (memory bandwidth,
  allocator, epoch advance O(threads) scans)

## Implementation log (concurrent_set.rs)

- Which school did you pick (CAS-lazy hybrid?) and what does level-0-CAS-
  as-linearization-point simplify vs memgraph's lock-preds-validate?
- Where exactly is Release/Acquire load-bearing? List each ordering and
  the test that fails on this ARM Mac if it were Relaxed (try it — flip
  one and run `same_key_race` 50×).
- Tag-bit marking via `Shared::with_tag`: bit-smuggling ledger update —
  where else this repo has seen it (SwissTable meta, swips, valkey jobs).
- `cargo miri test` result (readers_survive_concurrent_removal_churn is
  the UAF canary):

## Questions — reading-postgres-lwlock.md

1. Shared count + exclusive bit in ONE word: the race if split in two?
2. Lost-wakeup timeline that recheck-after-enqueue prevents?
3. Why are latches non-recursive by design?
4. What does rolling their own buy over pthread rwlock?

## Questions — reading-crossbeam-epoch.md

1. Why 3 epochs, not 2 (interleaving)?
2. What bug class does `Shared<'g>`'s lifetime delete?
3. Reader pins then blocks on I/O 100 ms — consequence and fix?
4. Epoch-per-operation vs per-morsel for second-long graph queries?

## Questions — reading-concurrent-skiplists.md

1. Arena-per-memtable dodge → does M8 CoW give M9 the same dodge?
2. Lost-insert without validate-after-lock (construct it)?
3. Splice cache: bulk-load vs random edges?
4. Comparison table filled from memory?

## Questions — reading-bwtree.md

1. 6-delta point read: cache misses vs OLC B+tree (topic-0 numbers)?
2. Why must helpers finish others' SMOs?
3. OLC restart probability, 4 levels, 1% write rate — and the hot-leaf case?
4. Why do deltas win for sparse matrices but lose for B-tree nodes?
5. CAS-the-matrix-pointer: which Bw-tree lesson transfers to FalkorDB?

## scaling.rs results (after implementing)

| impl | 1t | 2t | 4t | 8t | 16t |
|---|---|---|---|---|---|
| global | | | | | |
| sharded | | | | | |
| crossbeam | | | | | |
| mine | | | | | |

## M9 log (capstone milestone)

- [ ] concurrent_set.rs passes all 5 tests + miri clean
- [ ] scaling table recorded; predictions scored
- [ ] threadpool.rs designed: work queue, steal or not, who owns threads
      when GraphBLAS is also parallel (ONE pool decision written down)
- [ ] single-writer/multi-reader graph: epoch-pinned readers + Release-
      published matrix versions — sketch matches M8's CoW design
- [ ] one real false-sharing site found & padded (128 B!) in my code
- [ ] reference threadpool.rs studied; diff noted
