# Topic 31 — working notes

## Predict before you measure

Fill the predictions BEFORE implementing the stubs / running lanes 2-4.

| lane | metric | prediction | measured |
|---|---|---|---|
| 1 | lost% — 10 keys, sync_every=1 | — | **94.98%** |
| 1 | lost% — 1000 keys, sync_every=100 | — | **88.34%** |
| 1 | lost% — 100K keys, sync_every=10000 | — | **12.45%** |
| 2 | tombstones per live dot after storm | | |
| 2 | rounds of random gossip to converge (8 replicas) | | |
| 3 | sequential inserts/s (Vec-backed RGA, 50K chars) | | |
| 3 | tombstone bloat after deleting half | | |
| 4 | dangling (hidden) edges after 100 node-removes ∥ 500 edge-adds | | |
| 4 | edges resurrected after re-adding the 100 nodes | | |

Lane 1 measured notes (2026-07-11, M-series MBP, release):

- Hot keyspace + constant sync ≈ every write races → LWW keeps one of
  each pair: ~95% loss is the *floor of the birthday collision*, not a bug.
- Subtle: lane 1 counts **merge-time discards only**. With rare sync +
  tiny keyspace (10 keys / sync_every=10000 → 0.05%) most shadowed
  writes were overwritten *locally* before ever syncing, so they don't
  show up as "lost to concurrency" — the divergence loss is a lower bound.
- Full 100K-writes × sync_every=1 config was quadratic (state-based sync
  ships the whole map): shrunk to 20K writes and wrote the cost into the
  bench comment. Delta-CRDTs exist for exactly this (README exercise 3).

## Stub order that worked on paper

counter → orset → graph (composes orset+lww) → rga. Graph before RGA:
graph reuses semantics you just built; RGA is the genuinely new mechanism.

## Guide-question checklist

- [ ] reading-shapiro-crdts.md 1-6
- [ ] reading-kleppmann-json-crdts.md 1-6
- [ ] reading-sequence-crdts.md 1-6
- [ ] reading-cr-sqlite.md 1-6

## Cross-topic threads

- Raft (15) vs CRDT is *where* coordination happens: before the write
  (consensus) vs never (merge must absorb it). M31 = run both, same workload.
- LWW timestamps want the HLC from topic 29; cr-sqlite shows the pure-
  Lamport alternative (db_version) and pays with "offline week still wins
  nothing" semantics — good interview question for M31 design review.
- OR-Set tombstones ↔ MVCC dead versions (topic 5): both need a horizon
  (causal stability ↔ oldest snapshot) before GC is safe.
- diamond-types' "only be a CRDT at merge time" rhymes with topic 27's
  incremental view maintenance: store the log, derive the state.

## Capstone M31 log

- Node/edge identity must be a Dot (replica, counter), NOT user-visible
  ids — cr-sqlite question 5 is the same trap as auto-increment PKs.
- Dangling-edge policy locked: hide-not-delete, edge visible iff both
  endpoints visible, re-add resurrects (graph.rs tests pin this).
- Properties: LwwMap keyed by node id (survive remove/re-add). Automerge
  would key by creation-op — README exercise 6 argues the difference.
- Anti-entropy v1: whole-state merge like lane 1; v2: db_version-style
  watermark deltas (cr-sqlite question 6 sketches the change feed).
- Deliverable comparison vs M15: write latency histogram + a table of
  concrete conflicts (same node created twice, edge to deleted node,
  property race) and what each mode did about them.

## Infra notes

- rand 0.8 + rand_chacha 0.3 only, per repo convention; seeded ChaCha
  permutation shuffles stand in for proptest in all convergence tests.
- 6 provided tests green (clock 3, lww 3); 18 stub tests todo-panic
  until implemented; lanes 2-4 wrapped in catch_unwind print their
  stub banner.
- automerge-vs-loro bench deliberately NOT in this crate (deps
  convention) — README exercise 2, precedent topic 14 (helix-db).

## Done when

- [ ] all 24 tests pass
- [ ] lanes 2-4 print real numbers; predictions table filled + surprises noted
- [ ] all 24 guide questions answered
- [ ] M31 design sketch reviewed against cr-sqlite question 6
