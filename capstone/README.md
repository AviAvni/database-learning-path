# minidb — the capstone

A multi-model database built incrementally in Rust, one milestone per curriculum topic.
By the end it is a small but *real* database: pluggable storage engines (in-memory,
B+tree, LSM), WAL + crash recovery, buffer pool, MVCC transactions, a RESP-compatible
server, a vectorized query engine, and graph + vector layers — all covered by
deterministic simulation and property tests, all benchmarked.

The point is not to compete with anything. The point is that **you cannot deeply
understand a mechanism you have not built and benchmarked.**

## Architecture (target end-state)

```
              +------------------ RESP server (tokio) ------------------+
              |                                                          |
   query language ──► parser ──► planner ──► vectorized executor         |
              |                                                          |
   models:    KV        graph (property graph)        vector (HNSW)      |
              |                                                          |
              +──────────────── MVCC transaction layer ─────────────────+
              |                                                          |
   trait StorageEngine:   [ in-memory ]  [ B+tree + buffer pool ]  [ LSM ]
              |                                                          |
              +──────────────── WAL + recovery ── replication (Raft) ───+
```

## Ground rules

- Cargo workspace; one crate per major component (`minidb-storage`, `minidb-txn`,
  `minidb-server`, `minidb-query`, `minidb-bench`, ...) — added as milestones demand,
  not upfront.
- Every milestone lands with: tests + a criterion benchmark + a short `notes.md` entry
  on what was measured and learned.
- Unsafe allowed where the lesson requires it (epochs, memory layout) — with Miri runs.
- Keep old engines working when adding new ones; the engine shootout (M4) is a
  recurring benchmark, rerun as the system grows.

## Milestone map

See `PROGRESS.md` for status. Milestones M0–M20 map 1:1 to curriculum topics 0–20 in
`PLAN.md` — each topic's "Capstone milestone" line defines the scope.

Workspace is created at M0 (topic 0). Nothing lives here until then.
