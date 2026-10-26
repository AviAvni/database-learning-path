# Topic 37 notes — distributed query execution

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin distq_bench`:

```
== lane 1: the fan-out tail — P(any leaf slow) and Table 1's shape ==
   P(at least one slow) = 1 - (1-p)^n
   n        p=1/100   p=1/1000   p=1/10000
      1        1.0%       0.1%        0.0%
    100       63.4%       9.5%        1.0%
    500       99.3%      39.4%        4.9%
   1000      100.0%      63.2%        9.5%
   2000      100.0%      86.5%       18.1%

   simulated 100-leaf scatter-gather, 1-in-100 slow leaves, 20k queries
   (the paper's Table 1: waiting for all vs 95% of leaves)
   wait for            p50        p95        p99
   one leaf            5.6 ms     9.6 ms    10.0 ms
   95% of 100          9.6 ms     9.9 ms     9.9 ms
   all 100          1000.0 ms  1000.0 ms  1000.0 ms

== lane 2: exchange — routing throughput and the merging exchange ==
[stub — implement the todo!()s to unlock lane 2]

== lane 3: hedged requests — p99.9 vs extra load, by hedge delay ==
   leaf: 1-10 ms fast, 1000 ms stall with p = 0.005 (paper: 1800 -> 74 ms at +2%)
[stub — implement the todo!()s to unlock lane 3]
```

The table is closed-form and the simulation is seeded (simulated latencies,
not wall time), so every figure — 63.4% at p = 1/100, n = 100; 18.1% at
p = 1/10000, n = 2000; one-leaf p50 5.6 ms; all-100 p50 1000.0 ms;
95%-of-leaves p99 9.9 ms — is identical on every run.

The lane-1 mechanics, worth memorizing: waiting for all of n leaves
takes the max of n draws, so P(slow) = 1−(1−p)ⁿ — fan-out *exponentiates*
rarity into certainty. The component's p99 becomes the service's median
at n≈70 already (0.99⁷⁰ ≈ 0.50). The two outs are structural, not
tuning: stop waiting for everyone (good-enough / hedges), or shrink n's
exposure (canary requests, micro-partitions).

## Predictions (fill BEFORE implementing the stubs)

| question | predicted | measured |
|---|---|---|
| P(any slow), p=1/100, n=100 | | **63.4% analytic; 63.4%±2% simulated** (20k trials) |
| P(any slow), p=1/10000, n=2000 | | **18.1%** (closed form, test-pinned to 1e-3) |
| 100-leaf p50 waiting for all | | **1000.0 ms** (one leaf p50: 5.6 ms) |
| 95%-of-leaves p99 | | **9.9 ms** vs 1000 ms waiting for all |
| lane 2: hash routing at k=8 | | |
| lane 3: hedge at 10 ms | | |

<details>
<summary>Author's prediction</summary>

| question | author's prediction |
|---|---|
| P(any slow), p=1/100, n=100 | 1−0.99¹⁰⁰ = 63.4% |
| P(any slow), p=1/10000, n=2000 | 18.1% |
| 100-leaf p50 waiting for all | the leaf's tail → a stall |
| 95%-of-leaves p99 | tail gone |
| lane 2: hash routing at k=8 | balanced, deterministic |
| lane 3: hedge at 10 ms | p99.9 ~50× down, +≲1% requests |

</details>

## Reference numbers for the exercise lanes

Read these after filling the worksheet. What the reference solution printed
(measured 2026-07-26, Apple M3 Pro):

- **Lane 2 — exchange**: round-robin **229.6 M rows/s**; hash
  **543.0 M rows/s** (balance 1.002); 8×500k merging exchange
  **80.6 M rows/s**.
- **Lane 3 — hedging**: hedge at 10 ms takes p99.9 **1000 → 18.3 ms** at
  **+0.5%** requests; hedge at 0 costs +100%.

## Guide-question checklist

- [ ] reading-volcano-exchange.md Q1–Q5
- [ ] reading-tail-at-scale.md Q1–Q5
- [ ] reading-datafusion-repartition.md Q1–Q5
- [ ] reading-cockroach-distsql.md Q1–Q5

## Cross-topic threads (worked)

- Topic 36 ↔ 37: `PartitionSpans` turns the placement map into the
  parallel plan; a hot shard (36's Zipf row) is a straggler here, and
  the fan-out math multiplies its cost by every query that touches it.
- Topic 35 ↔ 37: a hedge is deliberate extra load — the paper's ~5%
  budget at a p95 delay is a retry budget; M37 routes hedges through
  the admission layer as low-priority work.
- Topic 22 ↔ 37: Volcano's packet sweep (171 s at 1 rec/packet → 13.7 s
  at 83) is vectorization's argument made with processes instead of
  CPU pipelines.

## Capstone M37 log

- Surface: scatter-gather over M36 slots; exchange operator (hash /
  round-robin / merging) in the Rust engine; hedged reads on replicas.
- Targets: near-linear scale-up 1→2→4→8 on a partitionable scan;
  p99.9 with one stalled shard within 2× no-stall p99.9 when hedging;
  hedge overhead ≤ 5% extra requests.
- Order of work: scatter-gather + merge first (needs only the slot
  map), then the in-engine exchange, then hedging (needs replicas).

## Infra notes

- No new clones: datafusion and cockroach already under ~/repos.
- DataFusion anchors (`350ca7650`; paths under `datafusion/`):
  physical-plan/src/repartition/mod.rs:1526 (RepartitionExec), :1536
  (preserve_order), :446 (consume_input_streams; :484-492 per-(input,
  output) channels, :495-500 per-output channels, :524-545 spill
  channels, :547-558 output coalescer), :649 (BatchPartitioner), :677
  (REPARTITION_RANDOM_STATE, seed 0), :939-951 (StrengthReducedU64:
  mask for powers of two, reciprocal otherwise), :1024
  (new_hash_partitioner; :1036 StrengthReducedU64::new), :1055
  (new_round_robin_partitioner), :1172 (try_new), :1234
  (partition_iter), :1263 (create_hashes), :1273
  (partition_reducer.partition_indices), :1303 (range route_indices),
  :1344 (partition_grouped_take), :1740 (execute), :2280
  (pull_from_input); repartition/range.rs:36 (RangeRouter), :418
  (primitive binary search), :585 (row-format binary search);
  distributor_channels.rs:54 (channels()), :61 (Gate empty_channels),
  :120 (DistributionSender), :130 (send), :220 (parks when ALL buffers
  non-empty); physical-expr/src/partitioning.rs:126 (Partitioning),
  :128/:131/:133 (RoundRobinBatch/Hash/Range), :216
  (RangePartitioning), :347 (scale). EnsureRequirements
  (physical-optimizer/src/ensure_requirements/mod.rs:175, motivation
  :31-35) is the one rule; ensure_requirements/enforce_distribution.rs
  holds its helpers (:18/:80 retirement notes; :1532
  ensure_distribution_with_stats; :1776-1822 Range re-cut or hash;
  :1287 co-partitioned adaptation).
- Cockroach anchors verified: distsql_check.go:214
  (checkSupportForPlanNode); distsql_physical_planner.go:312
  (mustWrapNode — "no DistSQL-processor equivalent"), :971
  (PartitionSpans), :3604 (createPhysPlan), :3632
  (createPhysPlanForPlanNode); physicalplan/physical_plan.go:125
  (PhysicalPlan); execinfrapb/data.proto:72 (StreamEndpointSpec), :149
  (OutputRouterSpec), :152/:154/:157/:160 (PASS_THROUGH / MIRROR /
  BY_HASH / BY_RANGE); flowinfra/flow.go:72 (Flow), :272 (Setup), :463
  (StartInternal), :566 (Run); colflow/colrpc/outbox.go:50/:218/:323
  (Outbox, Run, sendBatches); inbox.go:57/:212/:333 (Inbox,
  RunWithStream, Next); rowflow/routers.go:538 (hashRouter);
  colflow/routers.go:443 (HashRouter).
- Papers verified from PDFs: Volcano exchange (TR CS/E 89-007,
  /tmp/volcano-exchange.pdf, read in full) — anonymous inputs,
  exchange fork/packet/semaphore mechanics, end-of-stream counting
  (3×4=12), §4.4 variants (broadcast by pinning, merging exchange
  keeps producers separate, exchange-in-the-middle removes flow
  control), §4.5 two-level buffer locking + restart = deadlock-free,
  §5 numbers: 20.28 s single-process / 28.00 s no-fork →
  25.73 µs/record/exchange; 16.21 s forked pipeline; packet sweep 171 /
  94 / 15.0 / 13.7 s at 1/2/50/83 rec/packet. Tail at Scale (CACM
  2013, /tmp/tail-at-scale.pdf, read in full) — 63%/18% fan-out math;
  Table 1 (1/5/10 ms leaf → 40/87/140 ms at 100%, 12/32/70 ms at 95%);
  hedged 1,800→74 ms +2%; tied requests Table 2 (99.9%: 98→61 ms idle
  −38%, 159→108 ms with terasort −32%; tied+terasort ≈ idle-unhedged);
  micro-partitions ~20/machine; canary requests on every fan-out.
- Crate: 3 provided tests green (fanout.rs — 63.4%/18.1% arithmetic,
  simulation within ±2%, leaf-tail-becomes-median). 6 stub tests fix
  contracts for exchange.rs (3) and hedge.rs (3). Reference solution
  verified 9/9 then reverted; reference lane numbers: round-robin
  229.6 M rows/s, hash 543.0 M rows/s (balance 1.002), 8×500k merge
  80.6 M rows/s; hedge@10ms p99.9 1000→18.3 ms at +0.5% requests,
  hedge@0 +100%. Lanes 2-3 print `[stub …]` banners via catch_unwind.
  Zero warnings on the pristine crate.

## Done when

- [ ] All 9 tests pass; lanes 2-3 print real numbers.
- [ ] Exercise 2 inversion done: p_slow for P(any)<1% at n=100
      recorded here (≈1e-4).
- [ ] Tied-request variant (exercise 3) measured against the hedge.
- [ ] Good-enough sweep (exercise 4): the p_slow where 95%-cut stalls.
- [ ] Packet-economics sweep (exercise 5): rows/s vs chunk size table.
- [ ] All 20 guide questions answered in writing.
- [ ] M37 sketch (exercise 6) upgraded to a design note: router type
      per query shape chosen and justified.
