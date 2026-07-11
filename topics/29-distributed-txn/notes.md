# Topic 29 — Notes

## Measured: the workload's conflict probability (lane 1, provided)

100K transfers over 100K accounts, batches of 8, `cargo run --release --bin txn_bench`:

| zipf θ | batches containing a key collision |
|---|---|
| 0.5 | 0.3% |
| 0.9 | 29.9% |
| 1.1 | 86.2% |
| 1.3 | 99.6% |

The jump from θ=0.9 to 1.1 is the whole story: real workloads sit exactly
where contention goes from "occasionally" to "usually". Any protocol
evaluated only at uniform keys is being evaluated on the easy case.

## Predictions (before implementing the stubs)

| lane | prediction | reasoning |
|---|---|---|
| 2: Percolator abort rate θ=0.5 | < 0.5% | collisions are rare and txns are 2 keys |
| 2: abort rate θ=0.9 | ~5-10% | 29.9% of *batches* collide, but a batch is 8 txns and only pairwise overlaps abort; within-batch sequential execution here means only lock/conflict windows that persist (committed newer writes) count |
| 2: abort rate θ=1.3 | 30-60% | nearly every batch collides, often on the same hot head keys; each committed hot-key write Conflicts every later same-batch reader that took an older snapshot |
| 2: throughput | ~1M txn/s order, dropping mildly with θ | in-process HashMaps; aborts are cheap (locks cleaned eagerly) |
| 3: 2PC storm | committed ≈ 19.4K, crashed = 200, invariant holds | 1 crash per 100 txns × 20K; recovery every 4th crash leaves lock wreckage windows |
| 3: blocked_aborts | few hundred | between a crash and its recovery, θ=0.9 traffic keeps landing on the 2 locked keys |

Record actuals next to these after implementing `percolator.rs` / `tpc.rs`.

## Things that surprised me while designing the experiments

- The blocking window is *quantifiable*: lane 3's `blocked_aborts` counts
  txns that aborted specifically because a dead coordinator's locks were
  still staged. That number is the empirical cost of "participants cannot
  decide locally" — the sentence every textbook writes and no textbook
  measures.
- Percolator needs *no separate recovery procedure at all* — `resolve_lock`
  run by any inconvenienced reader IS recovery. The tests for "crash
  after primary commit" and "crash before primary commit" are just reads.
- HLC's subtlety is not the max() rules but the *bound*: `l` never exceeds
  the largest physical time seen anywhere, so it can't drift like a
  Lamport clock under message bursts. The `l_is_bounded_by_max_physical_time_seen`
  test hammers 1000 messages through a node whose clock reads ~10 to prove it.
- A duplicate commit in TiKV returns `Ok` on purpose (commit.rs:57's
  match arm) — every step of Percolator must be idempotent because the
  client is the coordinator and clients retry.

## Guide questions (work through per reading guide)

- [ ] reading-percolator-tikv.md — 6 questions (prewrite-fails-on-any-lock; atomicity substrate; TTL vs immediate Locked; Rollback records; abort-rate prediction; M29 snapshot reads)
- [ ] reading-spanner-hlc.md — 6 questions (commit-wait throughput; HLC bound induction; uncertainty interval; STAGING resolution; tiebreak; M29 TSO-vs-HLC)
- [ ] reading-calvin.md — 6 questions (deterministic locking; mid-txn node death; OLLP livelock; log-vs-WAL replication; sequencer lineage; M29 reconnaissance traversal)
- [ ] reading-foundationdb.md — 6 questions (durability point; false aborts; recovery-vs-decision-log; read-only serializability; ResolverBug vs crash points; M29 graph resolver)

## Cross-topic threads

- **Topic 15 (consensus)**: 2PC and Raft/Paxos are orthogonal — 2PC makes
  *different* shards atomic, consensus makes *copies* of one shard agree.
  Spanner's fix for the blocking window is literally "run the coordinator
  on topic 15."
- **Topic 16 (DST)**: lane 3's `CrashPoint` enum is a baby FDB simulator;
  ResolverBug.cpp shows the grown-up version injects *wrong answers*, not
  just crashes.
- **Topic 9 (MVCC)**: the write CF's `(key, commit_ts) -> start_ts` is
  xmin/xmax vocabulary — Percolator is Postgres snapshot rules with the
  counter moved to a TSO.
- **Topic 27 (IVM/CDC)**: TiKV resolved-ts (the min start_ts of open txns)
  is what makes a consistent changefeed cut possible — this topic's locks
  are exactly what CDC must wait out.
- **Topics 24/25**: cross-shard pattern matching = distributed join over
  partitioned adjacency; the delta-join shapes apply once M29 shards the
  graph.

## Capstone M29 log

- Shard by node id (hash). Node properties + outgoing adjacency co-located
  ⇒ single-shard for 1-hop writes; edges (u,v) with u,v on different
  shards are the 2PC/Percolator case — edge insert = prewrite {u's
  adjacency, v's in-adjacency}, primary = u's side.
- Reads: traversals want a *snapshot*, not locks — Percolator-style
  `get`-at-`start_ts` per shard suffices if all shards share a timestamp
  domain ⇒ start with a TSO (single-region assumption, à la PD); HLC +
  uncertainty is the multi-region upgrade path (reading-spanner-hlc.md Q6).
- Contention profile for graphs: supernodes are the Zipf head — a hot
  node's adjacency key will serialize all its edge inserts. Mitigation to
  explore: split adjacency into per-shard segments (write to your local
  segment; readers union) — turns a WW hotspot into a scatter-gather read.
- Crash matrix from lane 3 must become a test suite: every M29 protocol
  step gets a kill point, invariant = no dangling half-edges (the graph
  version of "money conserved").

## Infra notes

- Crate: `distributed-txn-experiments`; kv.rs provided (3 column
  families, TSO, Zipf, 2-shard cluster), tpc.rs / percolator.rs / hlc.rs
  stubs. 4 provided tests pass; 14 stub tests fail as `todo!()` panics
  (`grep -c "not yet implemented"` = 14).
- txn_bench lanes 2 and 3 are `catch_unwind`-wrapped: they print
  `[stub — implement …]` until the stubs are done, then the bank
  invariant + atomicity asserts arm themselves.

## Done when

- [ ] `tpc.rs`: all 4 tests green — every crash point preserves atomicity,
      logged decisions roll forward, blocking window demonstrated.
- [ ] `percolator.rs`: all 5 tests green — snapshot reads repeatable,
      cross-shard atomic, no lock leaks, roll-forward/roll-back via primary.
- [ ] `hlc.rs`: all 5 tests green — monotonic under backward clocks, `l`
      bounded by max pt, causal chains ordered.
- [ ] txn_bench full run: abort-rate table filled in above next to
      predictions; 2PC storm invariant holds; blocked_aborts recorded.
- [ ] Can explain, without notes: why the primary key's commit is a commit
      *point*, what 2PC's blocking window is and the three distinct escapes
      (replicate it / move it into data / delete runtime agreement), and
      why HLC needs an uncertainty interval where TrueTime needs a sleep.
