# Topic 32 notes — HTAP architectures

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| lane 1: write p99, scans on vs off | 10–100x worse | **333 ns → 7.49 s** (2.2e7x); not slowdown — *starvation* |
| lane 1: write throughput hit | ~2–5x fewer writes | 11,438,647 → **69** writes in 2 s |
| lane 1: scans completed in 2 s | ~100 | 3261 (~0.6 ms/scan; scanner re-grabs the unfair lock back-to-back) |
| lane 2: merged vs delta-heavy scan | 5–10x | (stub — measure after implementing replica.rs) |
| lane 2: max lsn gap vs batch | ≈ batch size | (stub) |
| lane 3: wait p50 at interval 100 | ~50 ticks | (stub) |

The lane-1 surprise: I expected degradation, got **writer starvation**.
std::sync::Mutex is unfair; the scanner finishes a ~0.6 ms scan and wins
the lock again before the parked writer wakes. p50 stayed 125 ns because
the handful of writes that got through ran uncontended. Interference at
its worst isn't slower writes — it's *no* writes. (Exercise 2: RwLock
changes fairness, not the fight.)

Methodology note: lane 1 originally did a fixed 200K writes; with the
scanner holding ~0.6 ms locks that serializes into minutes. Switched to a
fixed 2 s window per mode — the writes-completed collapse became the
headline number instead of the runtime.

## Guide-question checklist

- [ ] reading-tidb-htap.md Q1–Q6 (Q6: what plays commit index in M32)
- [ ] reading-tiflash-deltatree.md Q1–Q6 (Q6: delta index for matrices)
- [ ] reading-hyper-hana.md Q1–Q6 (Q6: fork() vs delta-matrix replica)
- [ ] reading-f1-lightning.md Q1–Q6 (Q6: safe timestamp in M32 router)

## Cross-topic threads (worked)

- The same fold, four costumes: topic 4 LSM minor compaction, HANA delta
  merge, TiFlash segmentMergeDelta, FalkorDB delta-matrix flush. All pin
  the identical invariant: scans unchanged, write side emptied — which is
  literally `merge_preserves_scans_and_sorts_main`.
- Freshness has exactly two prices: a wait (TiFlash `doLearnerRead`,
  lane 3) or staleness (Lightning safe timestamp, lane 2's gap table).
  Every HTAP design picks one; there is no third option.
- Topic 27's changelog is the load-bearing wall: `RowStore.log` here,
  Changepump at Google, Raft log at PingCAP. "The log is the database,"
  third appearance.

## Capstone M32 log

- Architecture choice: Lightning-shaped (CDC from M27's changelog into a
  matrix replica), not TiFlash-shaped — FalkorDB has no consensus group
  until M15 lands, and decoupling means zero primary changes.
- Router contract sketched in README exercise 6 + f1-lightning Q6:
  replicas advertise `applied_lsn` (safe timestamp); queries carry a
  freshness bound; router serves stale-but-consistent, waits, or falls
  back to primary.
- Before-shot recorded: 69 writes/2 s when analytics shares the copy.
  M32's success metric is restoring the 11.4M while scans run elsewhere.

## Infra notes

- Cloned this topic: ~/repos/tiflash, ~/repos/tidb.
- Anchors verified: LearnerRead.cpp:35/:61, DeltaMergeStore.h:107/:668,
  Segment.h:84/:715, DeltaValueSpace.h:65, DeltaIndex.h:27,
  find_best_task.go:535/:1841/:1878.
- Crate: 5 provided tests green (row.rs oracle + bench helpers), 7 stub
  tests fix contracts for replica.rs (4) and learner.rs (3). Lanes 2-3
  print `[stub …]` banners via catch_unwind until implemented.

## Done when

- [ ] All 12 tests pass; lanes 2-3 print real numbers.
- [ ] Lane 2 crossover found (exercise 3) and compared to TiFlash's
      delta-merge trigger.
- [ ] Lane 3 re-run with bounded staleness (exercise 4).
- [ ] All 24 guide questions answered in writing.
- [ ] M32 router sketch upgraded to a design note with `read_wait`
      analogue signed off.
