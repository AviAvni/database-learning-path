# Reading guide — TiDB: a Raft-based HTAP database (VLDB 2020)

Paper: "TiDB: A Raft-based HTAP Database", Huang et al., VLDB 2020.
Code: ~/repos/tidb (planner), ~/repos/tiflash (learner reads).

## The one move

TiDB's answer to the interference you measured in bench lane 1 is
*separation inside the consensus group*: the columnar copy is a *Raft
learner* — it receives the log like any follower but never votes, so
adding it costs no write-quorum latency and its scans touch OLTP nodes
zero times.

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
