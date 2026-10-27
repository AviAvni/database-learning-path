# Topic 27 — Notes & measurements

Machine: Apple M3 Pro, macOS. `cargo run --release --bin ivm_bench`
(50K nodes / 500K edges; 10 churn batches of +90/−10 edges; reach lane:
50 insert-only chunks of 10K edges).

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin ivm_bench`:

```
graph: 50000 nodes, 500000 edges; 10 batches of +90/-10
triangles, full recompute:      100.5 ms/batch   (count after last batch: 1365)
triangles, incremental:    [stub — implement tri::IncrementalTriangles]
wedge join, full recompute:     923.5 ms/batch   (wedges incl. trivial: 21063140)
wedge join, incremental:   [stub — implement djoin::IncrementalJoin]
reach, re-BFS per batch:         25.9 ms/batch   (|reached| 50000)
reach, semi-naive:         [stub — implement reach::SemiNaiveReach]
```

The counts are seeded and identical on every run. That depends on one design
choice in `ChurnGen::new`: the deletion pool `present_vec` is built from the
base ZSet's sorted order, not from a std `HashSet`'s iteration order. The
seeded rng picks each deletion *by index into that pool*, so the pool's order
is part of the seed — and std's `HashSet` draws a fresh hash key every
process, which would make the same seed delete different edges each run (the
triangle count would wander by a few, the wedge weight by a few hundred).

| standing query | full recompute / batch | notes |
|---|---|---|
| triangle count | 100.5 ms | O(m·d̄) sorted-intersect sweep; count 1365 after last batch |
| 2-hop wedge join | 923.5 ms | full self-join, 21,063,140 wedge weight — rebuilt per batch |
| reachability (re-BFS) | 25.9 ms | Σ over batches ≈ 1.3 s for what should cost O(m) total |

Timings vary from run to run; the counts do not. The README and FINDINGS row
27 quote this run.

## Predictions (fill BEFORE implementing the stubs)

| stub lane | your prediction | measured |
|---|---|---|
| incremental triangles | | |
| incremental wedge join | | |
| semi-naive reach | | |
| relaxations | | |

<details><summary>Author's prediction</summary>

| stub lane | prediction | reasoning |
|---|---|---|
| incremental triangles | 5-30 µs/batch, ~3000-10000× | 100 changes × d̄=20 probes × ~15 ns/BTreeSet probe |
| incremental wedge join | 1-5 ms/batch, ~200-800× | delta keyed both directions = 200 rows joined vs 1M-row state via hash index... but our ZSet state is a sorted Vec — merge cost O(state) per step may dominate; watch integrate cost, not join cost |
| semi-naive reach | ~500 µs/batch early, ~ns late | each edge relaxed ≤ 2× ever; late batches mostly intra-component = free |
| relaxations | ≤ 2m ≈ 1M | frontier discipline; test bound is 4m |

Honest flag on the wedge lane: `IncrementalJoin` integrates by
`ZSet::merge` = full re-sort of a 1M-entry state per batch. If measured
speedup disappoints, the fix is an indexed/spine state (an arrangement!)
— which would be the lesson demonstrating itself: deltas are cheap,
*state maintenance* is where arrangements earn their keep.

</details>

## Reference numbers for the exercise lanes

Read these after filling the worksheet. A scratch reference solution passing
every stub test (triangles: per-edge `|N(u) ∩ N(v)|` against current
`HashMap<u32, BTreeSet<u32>>` state; wedge join: `delta_join` composed from
`join_oracle` over the integrated `ZSet` state, then `merge`; reach: frontier
BFS from newly-connected endpoints) printed, from `cargo run --release --bin
ivm_bench` (Apple M3 Pro, 2026-10-27, one run on an idle machine; the
full-recompute lines of that same run were 100.8 / 885.3 / 29.2 ms):

| lane | incremental | speedup in that run | check |
|---|---|---|---|
| triangles, +90/-10 batch | 69.5 µs/batch | 1450x | count 1365 == oracle |
| wedge join | 41,175 µs/batch | 21x | wedges 21,063,140 == oracle |
| reach, 50 insert chunks | 655.2 µs/batch | 44x | |reached| 50000; 110,608 relaxations for 500,000 edges |

- **A "3-5 orders of magnitude" claim does not
  reproduce.** The best lane is 1450x (3.2 orders) and the median is 44x
  (1.6 orders). The wedge join, whose incremental form is the textbook
  bilinear identity, reaches only 21x because `ZSet::merge` re-sorts the whole
  1 M-entry state every batch and `join_oracle` rebuilds a hash index over the
  whole other side: the delta is 200 rows, the state work is 2 M. The fix is
  the arrangement the author's prediction flagged, and it is left as the
  exercise's stretch.
- The triangle `probes` counter reads 3,144,377 after the whole run because it
  includes loading the 500 K-edge base through `apply`; the per-batch work is
  the test's `< 4000` probes bound.
- Semi-naive relaxations: 110,608 for 500,000 edges, 0.22 per edge — far under
  the 2m bound, because most late chunks land inside the already-reached
  component and cost nothing.

The stub tests remain the specification (incremental results equal full recompute
after every batch, and the relaxation bound of 4m).

## Questions to answer while reading (from the guides)

- [ ] Naiad Q1: why must feedback nodes increment the loop counter?
- [ ] Naiad Q3: progress counts transiently negative — why safe?
- [ ] DD Q2: which frontier does a ΔA batch join B's trace at, and how does the wrong answer double-count ΔA⋈ΔB?
- [ ] DD Q4: build the case where incremental recursion needs the lattice, not a total order.
- [ ] DBSP Q1: derive the bilinear rule from Q^Δ = D∘Q∘I.
- [ ] DBSP Q2: where exactly does the theory need negative weights; why is distinct the troublemaker?
- [ ] Mz/RW Q2: degree tables vs diff arithmetic — what does hand-rolled state buy RisingWave?
- [ ] Kafka Q2: what must a compacted topic keep that an LSM needn't?
- [ ] Kafka Q4: raw-log vs result-delta subscriptions for M27; the retention trade.

## Cross-topic threads

- Topic 20: DP/DM delta matrices are ±Z-sets; `wait` = integrate. The
  M27 gap is pushing Q through the deltas (Q^Δ) instead of integrating
  first. tri.rs is the scalar rehearsal of ΔA·A + A·ΔA + ΔA·ΔA.
- Topic 4: arrangement spine = LSM; `advance` = compaction horizon;
  consolidation = tombstone drop. Same structure, third appearance
  (LSM, GIN pending list, arrangements).
- Topic 24: semi-naive frontier = "never re-derive settled facts" =
  delta-stepping's settled buckets.
- Topic 7: differential's join fuel (join.rs:348-395) = cooperative
  yielding inside an operator — the event-loop lesson at a new layer.
- Topic 15/5: Kafka offset = LSN; consumer group = replica set; log
  compaction = per-key checkpoint+truncate.

## Capstone M-log (M27, per PLAN)

Target: standing Cypher queries — register a query, keep its result
incrementally maintained under graph mutations via delta matrices, push
changes to subscribers.

- Scope v1 to the auto-incrementalizable fragment: linear ops (filters,
  projections) + bilinear joins (pattern edges) + count/sum aggregates.
  `distinct`-shaped and top-k queries need per-operator state — defer.
- The circuit compiler is topic 10's planner with a new backend: plan →
  per-tick delta program of masked SpGEMM terms. Wedges: Δ(A²) =
  ΔA·A + A·ΔA + ΔA·ΔA where A is post-previous-tick state (order per DD Q2).
- Tick = writer batch (single writer ⇒ no barriers, no frontier protocol
  — the parts of Naiad we get to skip, per reading-naiad-timely Q4).
- Subscriber protocol: result deltas (Materialize SUBSCRIBE shape) with a
  bounded replay buffer; disconnect > buffer ⇒ full re-materialize
  (Kafka Q4's retention trade, decided).
- Deletions in recursive/variable-length patterns: NOT in v1 — that's
  differential's lattice territory; document the cliff explicitly.

## Infra notes

- Provided lanes always print: bench survives stubs via catch_unwind.
- 6 provided tests pass (zset consolidation/nonlinearity, churn set
  semantics, K4 oracle, BFS oracle); 9 stub tests fail as todo!() panics.
- `distinct_is_not_linear` in zset.rs is the theory's load-bearing test:
  deleting the last copy must retract, a stateless delta pass can't know.
- ChurnGen guards same-batch insert-after-delete of one edge so weights
  stay in {0,1} — the oracles assume set semantics (debug_assert'd).

## Done when

- [ ] All 15 tests green (`cargo test --release`).
- [ ] ivm_bench speedup columns filled; wedge-join integrate-cost
  suspicion confirmed or refuted (if confirmed: write the two-sentence
  argument for why arrangements exist).
- [ ] Can state from memory: the linear/bilinear/nonlinear operator
  classification and Q^Δ = D∘Q∘I with the three join terms.
- [ ] One paragraph: why insert-only reachability is easy, why deletion
  is hard, and what differential stores to make it tractable.
- [ ] M27 design sketch reviewed against reading-dbsp Q4's wedge circuit.
