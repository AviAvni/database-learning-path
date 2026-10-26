# Topic 32 notes — HTAP architectures

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin htap_bench` — lane 1 runs a fixed 2 s window per
mode:

```
=== lane 1: OLTP/OLAP interference on one copy ===
                  mode     p50 ns     p99 ns     p99.9 ns  writes/2s   scans
          writes alone         84        209          292   14052932       0
   writes + full scans        167    2247875      2369166      21543     889
(coarse lock on purpose: every full scan is a write outage — the HTAP problem)

=== lane 2: columnar replica — scan speedup + freshness ===
[stub — implement the todo!()s in src/ to unlock this lane]

=== lane 3: learner reads — the price of freshness ===
[stub — implement the todo!()s in src/ to unlock this lane]
```

Every figure is a timing or a count of work done in a fixed window, so none
is seeded; three consecutive runs gave p99 2.24–2.25 ms and 21,349–21,946
writes with scans, i.e. one outage per scan of ~2.25 ms (2 s / 889 scans).

**The lane used to measure the scheduler, not the scan.** Until 2026-10-26
the store sat behind `std::sync::Mutex`, which on macOS is `os_unfair_lock`:
the scanner finished a scan and re-acquired the lock before the parked
writer woke, so a single write starved for as long as luck allowed, and
the 2 s window (checked only between writes) stretched with it. Recorded
2026-07-11: p99 333 ns → 7.49 s, 11,438,647 → 69 writes; on 2026-10-26 the
same code gave 5.32 s, then 20.3 s, 37.5 s, 3.2 s and 22.2 s on consecutive
runs, with the lane taking 5–40 s of wall clock. A p99 longer than the
measurement window is the repo's "implausible number" rule firing. The
store is now a `parking_lot::FairMutex` (FIFO hand-off), which makes each
scan exactly one write outage — the HTAP problem itself — and reproduces;
starvation under an unfair lock is now exercise 2.

Methodology note: lane 1 originally did a fixed 200K writes; with the
scanner holding ~0.6 ms locks that serializes into minutes. Switched to a
fixed 2 s window per mode — the writes-completed collapse became the
headline number instead of the runtime.

## Predictions (fill BEFORE implementing the stubs)

| question | predicted | measured |
|---|---|---|
| lane 1: write p99, scans on vs off | | **209 ns → 2.25 ms** |
| lane 1: write throughput hit | | 14,052,932 → **21,543** writes in 2 s (652×) |
| lane 1: scans completed in 2 s | | 889 |
| lane 2: merged vs delta-heavy scan | | |
| lane 2: max lsn gap vs batch | | |
| lane 3: wait p50 at interval 100 | | |

<details>
<summary>Author's prediction</summary>

| question | author's prediction |
|---|---|
| lane 1: write p99, scans on vs off | 10–100x worse |
| lane 1: write throughput hit | ~2–5x fewer writes |
| lane 1: scans completed in 2 s | ~100 |
| lane 2: merged vs delta-heavy scan | 5–10x |
| lane 2: max lsn gap vs batch | ≈ batch size |
| lane 3: wait p50 at interval 100 | ~50 ticks |

The lane-1 predictions were wrong in kind, not degree: the author expected
degradation and, under the old unfair lock, got writer starvation (see the
paragraph under the baseline). With a fair lock, throughput still falls
~650× — far beyond the predicted 2–5×.

</details>

## Reference numbers for the exercise lanes

No reference run of the stub lanes is recorded for this topic; the stub tests
are the specification.

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

- Cloned this topic: [~/repos/tiflash](https://github.com/pingcap/tiflash), [~/repos/tidb.](https://github.com/pingcap/tidb)
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
