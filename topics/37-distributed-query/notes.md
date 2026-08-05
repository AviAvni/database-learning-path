# Topic 37 notes — distributed query execution

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| P(any slow), p=1/100, n=100 | 1−0.99¹⁰⁰ = 63.4% | **63.4% analytic; 63.4%±2% simulated** (20k trials) |
| P(any slow), p=1/10000, n=2000 | 18.1% | **18.1%** (closed form, test-pinned to 1e-3) |
| 100-leaf p50 waiting for all | the leaf's tail → a stall | **1000.0 ms** (one leaf p50: 5.6 ms) |
| 95%-of-leaves p99 | tail gone | **9.9 ms** vs 1000 ms waiting for all |
| lane 2: hash routing at k=8 | balanced, deterministic | (stub — implement exchange.rs) |
| lane 3: hedge at 10 ms | p99.9 ~50× down, +≲1% requests | (stub — implement hedge.rs; reference run: 1000→18.3 ms, +0.5%) |

The lane-1 mechanics, worth memorizing: waiting for all of n leaves
takes the max of n draws, so P(slow) = 1−(1−p)ⁿ — fan-out *exponentiates*
rarity into certainty. The component's p99 becomes the service's median
at n≈70 already (0.99⁷⁰ ≈ 0.50). The two outs are structural, not
tuning: stop waiting for everyone (good-enough / hedges), or shrink n's
exposure (canary requests, micro-partitions).

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
- DataFusion anchors verified by grep this session:
  physical-plan/src/repartition/mod.rs:1150 (RepartitionExec), :1160
  (preserve_order), :398-538 (merge mode, per-(input,output) spill
  channels), :560 (BatchPartitioner), :592 (REPARTITION_RANDOM_STATE,
  seed 0), :679 (new_hash_partitioner; :667 is its doc comment,
  :689 the Hash state literal, :691 StrengthReducedU64::new), :710
  (new_round_robin_partitioner; :699 is its doc comment), :825
  (partition_iter), :854 (create_hashes), :862
  (partition_reducer.partition_indices — the strength-reduced
  modulo; :675 is only a doc comment), :1329 (execute), :1742
  (pull_from_input);
  distributor_channels.rs:55 (channels()), :62 (Gate empty_channels),
  :121 (DistributionSender), :131 (send — parks when ALL buffers
  non-empty); physical-expr/src/partitioning.rs:117 (Partitioning),
  :119/:122 (RoundRobinBatch/Hash). Real finding: EnforceDistribution
  was retired into EnsureRequirements
  (physical-optimizer/src/ensure_requirements/mod.rs:166, doc comment
  :157-164; ensure_requirements/enforce_distribution.rs:18/:76 are
  helpers + the retirement note).
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
