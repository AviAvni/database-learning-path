# TiDB HTAP: the columnar replica is a Raft learner

TiDB's fix for the interference you measured in bench lane 1 is
separation *inside* the consensus group: a columnar copy that receives
the Raft log but never votes. This chapter pairs the VLDB '20 paper with
the two code paths that carry the design — TiFlash's learner read
(freshness as a wait) and TiDB's planner (one optimizer pricing two
engines). Before either, it builds the design step by step: what a
replica inherits from Raft, why *learner* is the load-bearing word, and
how a read buys back freshness.

Every code anchor below is verified against the two clones this repo pins:
TiFlash at `pingcap/tiflash@b5093dd` (2026-07-09) and TiDB at
`pingcap/tidb@b94006d` (2026-07-10) — the pin table at the end of
`../../resources/codebases.md` — quoted with the line numbers those files
occupy at that commit. The paper is Huang et al., *TiDB: A Raft-based HTAP
Database* (VLDB 2020); section numbers cite that PDF.

## The problem in one sentence

Bench lane 1 showed scans and writes on one copy starving each other —
adding a free-running scanner took writes from **11,438,647 in 2 s to 69**
and p99 write latency from **333 ns to 7.49 s** (the topic README's
provided-lane output, echoed in `notes.md`). TiDB's answer is a physically
separate columnar copy for the scans, and the question that decides
everything is: how does that copy stay fresh without slowing the writes?

## The concepts, step by step

### Step 1 — separate the copies: scans get their own machines

> **In:** nothing yet — this step names the one cure the whole topic circles,
> so the later steps can price it. **Out:** the decision to keep a *second,
> columnar copy* on separate hardware, which Steps 2–4 then have to keep
> fresh without taxing the writers.

The only cure for one-copy interference that survives every workload is
a second copy on separate hardware: OLTP point-writes hit row-format
nodes (TiKV — TiDB's distributed key-value layer), analytical scans hit
columnar-format nodes (TiFlash), and the scans touch OLTP nodes zero
times. Isolation: total. Cost: an extra full copy plus its nodes. What's
left of the trilemma is freshness — a second copy is only as good as the
mechanism that keeps it current, which is Steps 2–4.

### Step 2 — the feed is the Raft log itself, not a bolt-on pipeline

> **In:** the second-copy decision from Step 1. **Out:** the feed that fills
> that copy — the Raft log the primary already produces — and the reason an
> *inside-the-group* feed can be bounded where the outside pipeline of Step 5's
> contrast (F1 Lightning) cannot.

TiDB already replicates every write through Raft (topic 15's consensus
protocol: a leader appends each write to a replicated **log**, and once
a majority — the **quorum** — acknowledges it, the write is committed
and every replica applies the log in the same order). So the columnar
copy doesn't need a new pipeline: let it consume the *same log*. Every
write is already ordered, already durable, already numbered by its log
index — the columnar copy just applies the entries into columnar form
instead of row form. Compare the alternative (F1 Lightning,
`reading-f1-lightning.md`): a CDC changelog bolted outside the system,
paying seconds of lag. Being inside the consensus group is what makes
*bounded* freshness even possible (Step 4).

### Step 3 — the learner: receives everything, votes never

> **In:** the Raft-log feed from Step 2. **Out:** the one word — *learner* —
> that lets the columnar copy consume that feed without ever sitting in the
> write quorum, so Step 4 can charge the whole freshness bill to the read side.

A Raft **learner** is a replica that receives the log like any follower
but does not vote in the quorum. That one word carries the OLTP-latency
guarantee: commit waits only on voters, so adding TiFlash learners adds
**zero** to write-quorum latency — even when a learner is busy building
column files or falls minutes behind, no write ever waits for it. The
paper states this outright: a learner "does not participate in leader
elections, nor is it part of a quorum for log replication," and log
replication to it is asynchronous, so "the leader does not need to wait
for success before responding to the client" (§2).

A **Region** — the unit the code below operates on — is TiKV's key-range
shard: one contiguous slice of the key space, replicated by its own Raft
group. A learner holds the columnar copy of a Region's rows.

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

If TiFlash were a voting follower, every commit's p99 would inherit the
columnar apply path's tail — the slowest, burstiest work in the system
would sit inside the write quorum (question 1).

### Step 4 — freshness is a wait: the learner read

> **In:** a learner (Step 3) that lags the leader by whatever it has not yet
> applied. **Out:** the per-read operation that buys a consistent snapshot
> back — a read-index plus a wait — and the timeout that bounds it.

A learner lags by whatever it hasn't applied yet, so a consistent read
must buy freshness back explicitly. The learner read does it in two
moves: ask the leader for the current **commit index** (the log position
of the newest committed write — one cheap RPC, Raft's ReadIndex from
topic 15), then *block* until the local replica has applied at least
that far. Freshness is not a config flag — it's a **wait**, paid per
read, sized by the current apply lag:

```rust
// ILLUSTRATION — not quoted from TiFlash. The real control flow is
// doLearnerRead (LearnerRead.cpp:35), which calls waitUntilDataAvailable
// (LearnerRead.cpp:58) under config.waitIndexTimeout() (:61); the M32
// analogue you build is learner.rs:22 (read_wait). freshness = read-index
// + wait-for-apply.
fn learner_read(region: &Region, leader: &Leader, timeout: Duration) -> Option<Snapshot> {
    let commit_idx = leader.read_index();       // "how far is committed, right now?"
    let deadline = Instant::now() + timeout;
    while region.applied_index() < commit_idx { // block until local apply catches up
        if Instant::now() > deadline {
            return None;                        // real code raises RegionException (:121)
        }
        wait_for_apply_progress();
    }
    Some(region.snapshot_at(commit_idx))        // now as fresh as any leader read
}
```

The real thing is `doLearnerRead`
(`dbms/src/Storages/KVStore/Read/LearnerRead.cpp:35`). It builds the
regions' snapshot and calls `waitUntilDataAvailable` (`:58`) with two
budgets — `batchReadIndexTimeout()` for the read-index RPC and
`waitIndexTimeout()` for the apply wait (`:60-61`) — stamping the wait's
start and end onto the query context (`:66-68`). When a Region cannot reach
its read index inside `waitIndexTimeout()`, its status is left non-OK and
`doLearnerRead` throws a `RegionException` for the unavailable regions
(`:121`); TiDB catches that, retries, and with fallback enabled can rerun
the query on TiKV — always safe, but back on the row store the scan and
writes share one copy again, re-importing exactly the interference this
architecture exists to remove. Your `learner.rs::read_wait` (`learner.rs:22`)
is this function reduced to arithmetic; bench lane 3 is its wait
distribution, and lane 2's batch-size table is the pressure that makes
waits grow (question 3).

### Step 5 — one planner prices both engines

> **In:** two live copies — TiKV rows (Step 1) and a fresh-on-read TiFlash
> learner (Step 4). **Out:** the per-query decision of which copy to scan,
> made by one cost-based optimizer rather than a static routing rule.

With two copies live, something must decide per query which one to hit —
and TiDB makes it the *same* cost-based optimizer, pricing row and
columnar paths together rather than routing by rule. In
`pkg/planner/core/find_best_task.go` (a **cop task** is a *coprocessor
task*: one pushed-down read request the planner sends to a storage node,
tagged for either a TiKV or a TiFlash target):

- `:535` — building cop tasks, distinguishing TiKV vs TiFlash targets.
- `:1841`, `:1878` — candidate-path retention keeps TiFlash paths alive
  alongside index paths so cost, not topology, decides.

So a point lookup goes to TiKV (row, indexed), a `SUM ... GROUP BY` over
50M rows goes to TiFlash (columnar, learner-read first) — and a query can
mix both. That's the planner deciding the trilemma point per query. A
rule like "big table → TiFlash" guesses wrong as soon as an index makes
the row path cheaper than the scan (question 4).

## Where each step lives in the code

| anchor | step | what to see |
|---|---|---|
| tiflash `dbms/src/Storages/KVStore/Read/LearnerRead.cpp:35` | 4 | `doLearnerRead` — read-index then wait-for-apply, freshness as a wait |
| tiflash `LearnerRead.cpp:58`, `:60-61` | 4 | `waitUntilDataAvailable` under `batchReadIndexTimeout()` / `waitIndexTimeout()` — the two budgets |
| tiflash `LearnerRead.cpp:66-68` | 4 | wait-index start/end timestamps stamped on the query context |
| tiflash `LearnerRead.cpp:121` | 4 | `RegionException` thrown for regions that miss the read index — the timeout-and-fallback path |
| tidb `pkg/planner/core/find_best_task.go:535` | 5 | building cop tasks, TiKV vs TiFlash targets |
| tidb `find_best_task.go:1841`, `:1878` | 5 | candidate-path retention — TiFlash paths kept alive so cost decides |

For the paper: read the VLDB '20 architecture sections with Steps 2–4 in
hand (learner, log apply, read index), and save the DeltaTree storage
appendix for [reading-tiflash-deltatree.md](reading-tiflash-deltatree.md)
— that chapter is where the columnar copy's own write problem gets
solved.

## Questions

1. Why does the learner *not* voting matter for OLTP write latency? What
   would happen to commit p99 if TiFlash were a voting follower doing
   columnar apply?
2. `read_wait` returns `None` on timeout. What does TiDB do then, and why
   is falling back to TiKV safe but expensive? (On timeout the real code
   throws `RegionException` at `LearnerRead.cpp:121`.)
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

## Done when

Answer each before unfolding it.

- [ ] You can say why adding TiFlash learners costs write latency nothing.
  <details><summary>Answer</summary>

  A learner receives the log but never votes, and replication to it is
  asynchronous — the leader "does not need to wait for success before
  responding to the client" (paper §2). Commit waits only on the voting
  quorum (Step 3), so a learner that is minutes behind or busy building
  column files never sits on the write path. This is the mechanism that keeps
  the bench lane 1 outage (11,438,647 writes/2 s → 69) from happening once the
  scans move to a separate voting-exempt copy.

  </details>

- [ ] You can say how a learner read still returns committed data despite lag.
  <details><summary>Answer</summary>

  Two moves (Step 4): ask the leader for the current commit index (Raft
  ReadIndex, one RPC), then block until the local replica's applied index
  reaches it — `doLearnerRead` → `waitUntilDataAvailable`
  (`LearnerRead.cpp:35`, `:58`). Freshness is a per-read *wait*, sized by the
  current apply lag, not a background setting.

  </details>

- [ ] You can say what happens when that wait times out, and its cost.
  <details><summary>Answer</summary>

  When a Region misses its read index within `waitIndexTimeout()` (`:60-61`),
  `doLearnerRead` throws a `RegionException` for the unavailable regions
  (`LearnerRead.cpp:121`). TiDB retries and, with fallback enabled, can rerun
  on TiKV. Safe — the row store is always current — but expensive: the scan
  is back on the same copy as the writes, re-creating the one-copy
  interference (bench lane 1) the split existed to remove.

  </details>

- [ ] You can say why one optimizer prices both engines instead of a rule.
  <details><summary>Answer</summary>

  `find_best_task.go` builds cop tasks tagged TiKV-vs-TiFlash (`:535`) and
  *retains* TiFlash candidate paths beside index paths (`:1841`, `:1878`) so
  cost, not topology, decides. A rule like "big table → TiFlash" guesses wrong
  the moment an index makes the row path cheaper than a full columnar scan
  (question 4); the cost model catches that per query.

  </details>

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
