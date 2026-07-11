# TiDB HTAP: the columnar replica is a Raft learner

TiDB's fix for the interference you measured in bench lane 1 is
separation *inside* the consensus group: a columnar copy that receives
the Raft log but never votes. This chapter pairs the VLDB '20 paper with
the two code paths that carry the design — TiFlash's learner read
(freshness as a wait) and TiDB's planner (one optimizer pricing two
engines).

## The one move

The columnar copy is a *Raft learner* — it receives the log like any
follower but never votes, so adding it costs no write-quorum latency and
its scans touch OLTP nodes zero times.

```
   client writes                        analytical query
        │                                      │
        ▼                                      ▼
   TiKV leader ──log──► TiKV follower     "what's the commit index?" ──► leader
        │                   (votes)                                        │
        └───────log───► TiFlash learner ◄── wait until applied ≥ index ◄──┘
                        (never votes,        LearnerRead.cpp:35
                         columnar)           doLearnerRead
```

Freshness is not a config flag — it's a **wait**. `doLearnerRead`
(`dbms/src/Storages/KVStore/Read/LearnerRead.cpp:35`) asks the leader for
the current commit index, then blocks until the local region has applied
that far, with `waitIndexTimeout` at `:61` (and the wait-index timestamps
at `:66-68`). Your `learner.rs::read_wait` is this function reduced to
arithmetic; bench lane 3 is its wait distribution.

```rust
// doLearnerRead, reduced: freshness = read-index + wait-for-apply
fn learner_read(region: &Region, leader: &Leader, timeout: Duration) -> Option<Snapshot> {
    let commit_idx = leader.read_index();       // "how far is committed, right now?"
    let deadline = Instant::now() + timeout;
    while region.applied_index() < commit_idx { // block until local apply catches up
        if Instant::now() > deadline {
            return None;                        // caller falls back to the leader:
        }                                       // safe but expensive
        wait_for_apply_progress();
    }
    Some(region.snapshot_at(commit_idx))        // now as fresh as any leader read
}
```

## One planner, two engines

The second half of the trick: the *same* cost-based optimizer prices row
and columnar paths together. In `pkg/planner/core/find_best_task.go`:

- `:535` — building cop tasks, distinguishing TiKV vs TiFlash targets.
- `:1841`, `:1878` — candidate-path retention keeps TiFlash paths alive
  alongside index paths so cost, not topology, decides.

So a point lookup goes to TiKV (row, indexed), a `SUM ... GROUP BY` over
50M rows goes to TiFlash (columnar, learner-read first) — and a query can
mix both. That's the planner deciding the trilemma point per query.

## Questions

1. Why does the learner *not* voting matter for OLTP write latency? What
   would happen to commit p99 if TiFlash were a voting follower doing
   columnar apply?
2. `read_wait` returns `None` on timeout. What does TiDB do then, and why
   is falling back to the leader safe but expensive? (LearnerRead.cpp:61.)
3. The paper claims fresh analytics, but lane 3 shows waits grow with
   apply-batch size. What pressure pushes TiFlash toward larger batches
   anyway? (Think lane 2's freshness-vs-batch table.)
4. In `find_best_task.go:1841`, why must TiFlash paths be *retained* as
   candidates rather than chosen by a rule like "big table → TiFlash"?
   Give a query where the rule guesses wrong.
5. Raft learners get the log, CDC (see `reading-f1-lightning.md`) gets a
   changelog. Both are "replay the writes" — what does being *inside* the
   consensus group buy, and what does it cost?
6. **M32 mapping**: FalkorDB has no Raft group (until M15). Which piece
   substitutes for the commit index in M32's `read_wait` — and what is
   the "leader" the router must ask?

## References

**Papers**
- Huang et al. — "TiDB: A Raft-based HTAP Database" (VLDB 2020) — the
  learner architecture and the freshness argument; the DeltaTree storage
  appendix pairs with
  [reading-tiflash-deltatree.md](reading-tiflash-deltatree.md)

**Code**
- [tidb](https://github.com/pingcap/tidb)
  `pkg/planner/core/find_best_task.go` — one optimizer pricing TiKV vs
  TiFlash paths together
- [tiflash](https://github.com/pingcap/tiflash)
  `dbms/src/Storages/KVStore/Read/LearnerRead.cpp` — `doLearnerRead`,
  freshness as a wait with a timeout
