# Topic 8 notes — transactions & MVCC

Predict FIRST, then measure.

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
