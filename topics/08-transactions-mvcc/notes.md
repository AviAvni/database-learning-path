# Topic 8 notes — transactions & MVCC

Predict FIRST, then measure.

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-27)

`cargo run --release --bin txn_bench` — 4 threads × 50 000 txns × 4 ops, one
global `Mutex<HashMap>` as the baseline; each transaction's keys are built
before the lock is taken. Median of three runs.

| mix | global-lock txn/s | mvcc txn/s | aborts |
|---|---|---|---|
| read-heavy 95/5, 10K keys | 1 393 945 | stub | stub |
| write-heavy 50/50, 10K keys | 806 155 | stub | stub |
| write-heavy 50/50, 64 keys (HOT) | 1 092 582 | stub | stub |

**Every row is fully serialized, so the throughput is the price of the
critical section.** The read-heavy row is fastest only because a lookup is
cheaper than an insert that allocates — none of its 95% reads ran
concurrently. The HOT row beats the 10K-key write row because 64 keys stay
cache-resident, not because the lock is less contended.

That gives you a sharp prediction to write down before implementing MVCC: your
version should win row 1 (readers never block) and may well *lose* on row 3,
where first-committer-wins turns key contention into aborted work that the
mutex never has to redo. Find the crossover keyspace size.

## Reference numbers for the exercise lanes (Apple M3 Pro, measured 2026-10-26)

A reference `mvcc.rs` — `HashMap<Key, Vec<Version>>` behind one `RwLock`, reads
under the read lock, commit validating and installing under the write lock, a
separate small mutex for the active-snapshot set, `Drop` as abort, `gc()` taking
the minimum active snapshot — passes all eight tests. It is not committed; this
is what `txn_bench` prints with it (4 threads × 50 000 txns × 4 ops, median of
three runs; an aborted transaction retries with the same operations).

| mix | global-lock/s | mvcc txn/s | speed-up | aborts (of 200 000 commits) |
|---|---|---|---|---|
| read-heavy 95/5, 10K keys | 1 393 945 | 1 976 273 | 1.42× | ~3 |
| write-heavy 50/50, 10K keys | 806 155 | 1 320 490 | 1.64× | ~230 |
| write-heavy 50/50, 64 keys (HOT) | 1 092 582 | 1 553 817 | 1.42× | ~31 600 |

Abort counts depend on thread timing and are not reproducible to the digit; the
throughputs are timings.

### Keyspace sweep: where is the crossover?

The prediction in the README was that first-committer-wins aborts would make
MVCC lose the HOT row. It does not. The sweep runs the provided lock (**lock**,
keys built before taking it) beside **lock, keys inside** — the same mutex
with `key(...)` allocating its `String` *inside* the critical section, which is
how an earlier version of the provided lane was written. 50/50 mix unless
stated, median of 3 runs (2 for the smallest keyspaces), txn/s in thousands:

| keys | lock, keys inside | lock | mvcc (RwLock) | mvcc (one mutex) | aborts per commit | mvcc ÷ keys-inside | mvcc ÷ lock | serializable mvcc |
|---|---|---|---|---|---|---|---|---|
| 100 000 | 513 | 642 | 963 | 676 | 0.000 | 1.88× | 1.50× | 883 |
| 10 000 | 533 | 766 | 1 239 | 928 | 0.001 | 2.32× | 1.62× | 1 248 |
| 1 000 | 592 | 786 | 1 430 | 1 017 | 0.011 | 2.42× | 1.82× | 1 362 |
| 256 | 562 | 917 | 1 497 | 1 065 | 0.042 | 2.66× | 1.63× | 1 391 |
| 64 | 591 | 923 | 1 491 | 1 012 | 0.154 | 2.52× | 1.61× | 1 254 |
| 32 | 571 | 939 | 1 439 | 940 | 0.291 | 2.52× | 1.53× | 1 166 |
| 16 | 568 | 960 | 1 284 | 947 | 0.516 | 2.26× | 1.34× | 960 |
| 8 | 585 | 974 | 1 210 | 1 162 | 0.833 | 2.07× | 1.24× | 865 |
| 4 | 572 | 939 | 1 000 | 993 | 1.246 | 1.75× | 1.06× | 750 |
| 2 | 576 | 968 | 872 | 847 | 1.496 | 1.51× | **0.90×** | 726 |
| 1 | 580 | 1 212 | 850 | 868 | 1.673 | 1.47× | **0.70×** | 821 |

Read-only-ish and write-only corners: 95/5 on 10K keys — 606 / 1 221 / 1 950
(keys inside / lock / mvcc), 3.2× the keys-inside lock and 1.6× the provided one;
100% writes on 64 keys — 542 / 720 / 1 508 (2.8× / 2.1×); 100% writes on 8 keys
— 544 / 722 / 783 (1.44× / 1.08×, 1.8 aborts per commit).

**Findings.**

1. **Read-heavy: MVCC wins, as predicted, by 1.4–1.6×** — readers take the
   read lock and never conflict. Against the keys-inside lock it looked like
   3.5×; most of that was allocation under the mutex.
2. **The crossover exists but sits far lower than the README guessed.** Against
   the lock MVCC stops winning between 4 and 2 keys — about one key per
   thread — where each transaction retries 1.2–1.7 times on average. Above that
   it wins everywhere in the sweep, even at 0.5 aborts per commit (16 keys),
   because an abort costs four cheap operations and the commit-time validation
   is short. A workload with *expensive* transactions would cross over at a much
   lower abort rate; this one cannot show it.
3. **How the baseline is written moves the answer by 1.6×.** Compare the
   `lock, keys inside` and `lock` columns: allocation inside the critical section
   halves the lock's throughput, and a speed-up quoted against it ("3.5×") is
   mostly that. Measure concurrency control against a lock whose critical
   section holds only the shared work.
4. **A single metadata mutex gives away a third of the win.** The `one mutex`
   column (every read takes the exclusive lock, the shape `mvcc.rs`' docs call
   "fine") is 0.7× of the `RwLock` version at 64 keys and still above the lock:
   MVCC's gain is mostly "the critical section is short", and the read lock is the
   rest.
5. **Serializable (backward read-set validation) costs 0–25% and aborts more:**
   at 64 keys 1.25 M txn/s with 58 K aborts against 1.49 M with 31 K. It
   converges on snapshot isolation at 10K keys (1 248 vs 1 239) where there is
   nothing to conflict on. This is the false-positive count the implementation log
   asks for, seen as throughput.

### What the two 2026 papers add to the measurement

*Case for SVCC* (CIDR '27) argues that a single-writer primary does not need
versions. The sweep agrees with the one claim it can test: single-version
locking is not slow — the lock holds ~0.9 M txn/s at every hot keyspace here
— and MVCC's margin shrinks to nothing as contention concentrates. It cannot
confirm the paper's complexity argument (lines of code), and the paper's own
TPC-C comparison (127 K vs 119 K txn/s at 8 threads, §3.3) is the same size of
effect as the noise between my three runs. Note what the sweep does *not*
contain: long-running readers. That is MVCC's actual job, and the only workload
on which a 2PL baseline would stall the writer for the length of the scan (topic
32 measures exactly that: 652× fewer writes with full scans).

*cMVBT* is an index design, not a transaction protocol: it does not change this
table, but it changes what a "version" costs when many versions of a key
accumulate — the thing the `gc()` test checks for in miniature.

## Predictions (fill in BEFORE running txn_bench)

| Measurement | Prediction | Actual | Surprised? |
|---|---|---|---|
| read-heavy 95/5, 10K keys: lock vs mvcc txn/s | | | |
| write-heavy 50/50, 10K keys: lock vs mvcc | | | |
| write-heavy 50/50, 64 hot keys: lock vs mvcc | | | |
| abort count on the 64-key mix (out of 200K txns) | | | |
| where's the crossover (keyspace size at which the mutex wins)? | | | |

Reasoning space:
- The mutex serializes even pure readers; MVCC readers only touch the
  metadata lock briefly per op. But your MVCC pays per-version allocation
  + retry loops. At what abort rate does retrying erase the win?
- 4 threads, 4 ops/txn, 64 keys, 50% writes → estimate P(two concurrent
  txns share a written key) before you look at the abort column.

## Implementation log (mvcc.rs design decisions)

- Version storage: append-only newest-to-oldest? Where does your design
  sit in the Wu/Pavlo 5-axis table? (Fill the row.)
- What exactly does your metadata mutex protect, and what would it take
  to shard it (topic 7's SHARDS trick — does it apply cleanly here)?
- Label each test with its Berenson phenomenon (P1/P4/A5A/A5B) in a
  comment — the ansi-critique guide asks for this.
- Serializable = backward read-set validation. Construct one history your
  validator aborts that SSI would have allowed (a false positive).

## Questions — reading-postgres-heapam.md

1. Index-entry deletion before line-pointer recycling: the corruption if flipped?
2. Hint bits make reads write — which topic-6 consequence?
3. XidInMVCCSnapshot at 10K writers vs Hekaton timestamps: costs?
4. Where do old versions live in a matrix-backed graph — append-only or delta?

## Questions — reading-rocksdb-transactions.md

1. Why is memtable-only OCC validation sound (and when does it TryAgain)?
2. Striped key locks vs a super-node's adjacency: the pathology?
3. No read-set validation by default — what isolation, where does skew enter?
4. Single-writer M8: which machinery survives?

## Questions — reading-ansi-critique.md

1. Doctors write skew in history notation; which phenomenon does it evade?
2. Why first-committer-wins can't catch write skew (one sentence).
3. Predicate phantoms for MATCH — label matrix? index range? key locks?
4. Test-to-phenomenon mapping done in mvcc.rs comments?

## Questions — reading-ssi-postgres.md

1. History where the dangerous structure completes after the reader commits?
2. Your read-set granularity vs SIREAD escalation — the graph equivalent?
3. Why can a read-only txn never be the pivot?
4. **The M8 shortcut**: single-writer + SI — prove (with the pivot
   definition) whether it's already serializable. Write the argument here:

## Questions — reading-inmemory-mvcc.md

1. end_ts-as-lock CAS pseudocode; the equivalent check in your mvcc.rs is at line ___.
2. Delta matrices in the Wu/Pavlo taxonomy; predicted read-path cost?
3. Logical node-ids as indirection: which graph updates still touch indexes?
4. Write-only hot key vs cooperative GC — the fix?
5. MVOCC at 40 cores: validation aborts or ts allocation? (Predict, then check §6.)

## The 5-axis placement (fill after all readings)

| System | CC | Version storage | Ordering | GC | Index ptrs |
|---|---|---|---|---|---|
| postgres | SI+SSI | append-only (in heap) | old-to-new (t_ctid) | vacuum+prune | physical (TID) |
| Hekaton | | | | | |
| my mvcc.rs | | | | | |
| M8 design | | | | | |

## M8 log (capstone milestone)

- [ ] mvcc.rs passes all 8 tests (write skew demonstrated AND prevented)
- [ ] txn_bench run; all predictions scored above
- [ ] MVCC graph design written: version unit (matrix/tile/delta), CoW
      granularity, reader visibility rule
- [ ] single-writer/multi-reader argument from SSI Q4 recorded — decides
      whether M8 needs validation at all
- [ ] reference's mvcc_graph.rs / cow.rs studied; diff vs my design noted
