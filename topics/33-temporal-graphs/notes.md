# Topic 33 notes — temporal graphs

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin temporal_bench`:

```
=== lane 1: static condensation vs temporal reachability ===
     nodes   contacts   static-reach temporal-reach  false positives
      2000       3999          25031            137            99.5%
      2000       7998          36956            925            97.5%
      2000      15996          39980          17229            56.9%
      2000      63969          39980          39980             0.0%
(every false positive is a path that exists in space but not in time)

=== lane 2: one-pass earliest-arrival vs fixpoint oracle ===
  contacts       oracle     one-pass   speedup
[stub — implement the todo!()s in src/ to unlock this lane]

=== lane 3: AT TIME latency vs checkpoint spacing ===
  anchor every   anchors     p50 read     p99 read avg replay len
[stub — implement the todo!()s in src/ to unlock this lane]
```

Lane 1 is a seeded count; every figure first recorded on 2026-07-22 (static
reach 25,031 vs temporal 137 = 99.5% false positives; 56.9% at 16K contacts;
0.0% at 64K, saturating at 39,980 pairs) is reproduced exactly.

The lane-1 surprise: I expected the static condensation to be a loose
upper bound; at realistic sparsity it is *pure noise* — 99.5% of its
"reachable" answers have no time-respecting witness. The transition is
sharp: 56.9% wrong at 16K contacts, exactly 0 at 64K. Dense streams give
every hop a later contact to board; sparse ones don't. Static reach is
the T→∞ limit, and most real event streams live nowhere near it.

Methodology note: the oracle is a deliberate Bellman-Ford-shaped
fixpoint (no ordering assumptions, obviously correct) so the one-pass
implementation has independent ground truth; lane 2's speedup is then
the measured value of the sorted-stream insight, not a tautology.

## Predictions (fill BEFORE implementing the stubs)

| question | predicted | measured |
|---|---|---|
| lane 1: false positives at 2K nodes / 4K contacts | | **99.5%** — static reach 25,031 pairs, temporal 137 |
| lane 1: density where the lie vanishes | | **0.0% at 64K contacts** (temporal saturates to all 39,980 static pairs) |
| lane 2: one-pass vs fixpoint oracle speedup | | |
| lane 3: p99 AT TIME, anchors every 1K vs none | | |
| lane 3: avg replay len ≈ every/2? | | |

<details>
<summary>Author's prediction</summary>

| question | author's prediction |
|---|---|
| lane 1: false positives at 2K nodes / 4K contacts | ~30–50% |
| lane 1: density where the lie vanishes | never fully |
| lane 2: one-pass vs fixpoint oracle speedup | ~10x |
| lane 3: p99 AT TIME, anchors every 1K vs none | ~100x |
| lane 3: avg replay len ≈ every/2 | yes |

</details>

## Reference numbers for the exercise lanes

No reference run of the stub lanes is recorded for this topic; the stub tests
are the specification.

## Guide-question checklist

- [ ] reading-temporal-paths.md Q1–Q6
- [ ] reading-temporal-motifs.md Q1–Q6
- [ ] reading-aeong.md Q1–Q5
- [ ] reading-raphtory.md Q1–Q5

## Cross-topic threads (worked)

- Anchor+delta = checkpoint+redo (topic 5), fourth appearance of the
  trade: WAL checkpoints, LSM compaction, M30 snapshots, now AeonG
  anchors — always "bound the replay, pay in materialization."
- Topic 8's begin_ts/end_ts version chains already store a
  transaction-time temporal graph; AeonG's contribution is refusing to
  GC it into oblivion and giving it a query surface (FOR TT AS OF).
- Raphtory vs memgraph is topic 13's mutation-vs-scan spectrum rotated
  90°: object-first keeps "now" fast and reconstructs the past;
  log-first keeps the past free and reconstructs "now."

## Capstone M33 log

- Storage choice pending lane 3: anchor+delta over M30's versioned
  store, spacing set where p99 AT TIME crosses 2× the dense-anchor
  floor (README exercise 4).
- Semantics decision: `temporalPath()` defaults to earliest-arrival
  (matches Wu et al.'s one-pass; fastest/shortest as options);
  time-respecting MATCH adds non-decreasing timestamps + WITHIN δ.
- The before shot: lane 1's 99.5% false-positive column is what static
  MATCH silently returns on temporal data.

## Infra notes

- Cloned this topic: [~/repos/raphtory](https://github.com/Pometry/Raphtory)
  (memgraph already cloned for topic 9/13; AeonG read as paper+spec, not cloned).
- Anchors verified: EventTime timeindex.rs:28, TimeIndex :13 (core),
  TCell tcell.rs:10, TPropCell tprop.rs:22, WindowedGraph
  window_graph.rs:87, TimeOps::window time.rs:116,
  edge_storage_ops.rs:110/:140, MemEdgeSegment segment.rs:58.
- AeonG paper facts verified against arXiv:2304.12212v2: Memgraph base,
  5.73× storage / 2.57× latency / 9.74% degradation, per-version ω,
  VP/VE/EP split, adaptive anchoring (Eq. 1), async migration during
  MVCC GC, FOR TT AS OF / FROM..TO syntax.
- Crate: 3 provided tests green (events.rs oracles), 6 stub tests fix
  contracts for temporal_reach.rs (3) and snapshot.rs (3). Lanes 2-3
  print `[stub …]` banners via catch_unwind until implemented.

## Done when

- [ ] All 9 tests pass; lanes 2-3 print real numbers.
- [ ] One-pass correctness argument written (README exercise 2),
      including the λ=0 tie-order counterexample.
- [ ] Lane 3 crossover found (exercise 4) and compared to AeonG's
      adaptive anchoring bands.
- [ ] All 20 guide questions answered in writing.
- [ ] M33 semantics sketch (exercise 6) upgraded to a design note.
