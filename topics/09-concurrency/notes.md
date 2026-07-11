# Topic 9 notes — latches, lock-free & epochs

Predict FIRST, then measure.

## Measured already (false_sharing, provided binary — this Mac, 8 threads)

| layout | time | rate |
|---|---|---|
| packed | 636 ms | 63 M inc/s |
| pad64 | 24 ms | 1697 M inc/s |
| pad128 | 11 ms | 3707 M inc/s |

- **59×** packed → pad128. "Independent" counters in one line are not
  independent — this is the whole reason redis pads `used_memory`.
- **pad64 is still 2.2× slower than pad128**: Apple M-series coherence
  granularity is 128 B. `#[repr(align(64))]`, the x86 default, HALF-fixes
  false sharing on this machine. Check every CachePadded assumption.

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
