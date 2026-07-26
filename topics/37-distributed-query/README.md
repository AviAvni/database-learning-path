# Topic 37 — Distributed Query Execution

Topic 36 decided where the data lives; this topic runs queries across
those pieces. Two ideas carry the whole field. **The exchange operator**
(Volcano, ICDE'90/TR 89-007): parallelism is not a property of query
operators but *one more operator* — scan, join, and sort stay
single-threaded and oblivious while `exchange` forks processes, routes
rows between them, and hides all synchronization behind the same
open-next-close iterator interface. **The fan-out tail** (Dean &
Barroso, CACM 2013): once a query scatter-gathers over N machines and
waits for all of them, the *rarest* slowness of one machine becomes the
*common* latency of the service — and no amount of per-machine tuning
fixes it, only tail-tolerant tricks like hedged requests.

## The problem, measured (bench lane 1, provided — runs today)

```
   P(at least one slow) = 1 - (1-p)^n
   n        p=1/100   p=1/1000   p=1/10000
      1        1.0%       0.1%        0.0%
    100       63.4%       9.5%        1.0%
    500       99.3%      39.4%        4.9%
   1000      100.0%      63.2%        9.5%
   2000      100.0%      86.5%       18.1%

   simulated 100-leaf scatter-gather, 1-in-100 slow leaves, 20k queries
   wait for            p50        p95        p99
   one leaf            5.6 ms     9.6 ms    10.0 ms
   95% of 100          9.6 ms     9.9 ms     9.9 ms
   all 100          1000.0 ms  1000.0 ms  1000.0 ms
```

The arithmetic is the paper's Figure: a server slow one time in 100 is
invisible alone (p50 5.6 ms) but a 100-way fan-out that waits for all
leaves hits a stall on 63% of queries — the leaf's p99 became the
service's *median*. Even at 1-in-10,000 slowness, 2,000-way fan-out is
slow 18% of the time. The "95% of 100" row is the paper's good-enough
escape hatch: drop the stragglers and the tail vanishes — Google's
real numbers (Table 1) are 10 ms leaf p99, 140 ms p99 waiting for all,
70 ms waiting for 95%; the slowest 5% of requests cause half the tail.

## The exchange operator: parallelism as an iterator

```
        print                    print          ── demand flows down,
          │                        │               packets flow up
        join                   exchange  ◀─ fork; consumer side
          │            ⇒          │
        scan                     join     ◀─ runs in child process,
                                   │          code unchanged
                               exchange
                                   │
                                 scan
```

Volcano's discipline: every operator is an iterator with
`open/next/close`, inputs are *anonymous* (an operator never knows what
produces its input), so `exchange` slots in anywhere. Its consumer side
is an ordinary iterator; its producer side forks a process group and
ships rows as **packets** through shared-memory queues. Everything the
distributed world argues about is a policy inside it:

- **Vertical parallelism** (pipelining): producer and consumer run
  concurrently. Measured on a 12-CPU Sequent Symmetry: a 4-process
  pipeline of 3 exchanges ran in **16.21 s** vs **20.28 s** for the
  single-process plan — the forked plan beats the sequential one, and
  exchange overhead in no-fork mode prices at **25.73 µs/record/exchange**.
- **Packet economics**: 1 record/packet costs 171 s; 83 records/packet
  (one page) costs 13.7 s — a 12× swing from batching alone. DataFusion's
  batches are the same lesson.
- **Intra-operator parallelism**: the producer's support function picks
  an output queue per record — round-robin, hash, or range — so k
  copies of a join each see one partition. Same trio as topic 36, now
  applied to intermediate results instead of stored data.
- **End-of-stream** is counted, not assumed: each consumer waits for
  one flagged packet from *every* producer (3 producers × 4 consumers =
  12 end-of-stream packets).
- **The merging exchange** (§4.4): for parallel sort, fuse k sorted
  streams — and it *must* keep producers' records separate (merge by
  producer, not one big bag), the detail lane 2's contract test pins.

## Hedged requests: pay 2-5% to delete the tail

```
   t=0        primary ──────────────────────▶ (stalled, 1000 ms)
   t=10 ms    hedge fires ──▶ secondary ────▶ done at ~15 ms
              └─ 95% of requests already finished; hedge never sent
```

Dean & Barroso's production numbers: reading 1,000 keys spread over 100
BigTable servers, hedging after 10 ms (≈ the p95) cut p99.9 from
**1,800 ms to 74 ms while sending only 2% more requests**. Tied
requests go further — enqueue on two servers, each knowing the other,
first to *start* cancels its twin — and cut BigTable's read p99.9 by
38% idle and 32% under a concurrent terasort, with under 1% extra disk
load. Lane 3 reproduces the hedge: unhedged p99.9 is a full stall;
hedged at 10 ms it drops ~50× for +0.5% requests, and the zero-delay
degenerate case shows *why* the delay exists (it doubles every
request). The rest of the paper's toolbox: **micro-partitions** (~20
per machine, so load sheds in 5% steps and one machine's recovery has
many donors), **selective replication** of hot partitions,
**latency-induced probation** (shadow-request a slow server before
readmitting it), and **canary requests** (send the fan-out to 1-2
leaves first — Google does this on *every* large fan-out query).

## Two production shapes

**DataFusion (single process, k cores)** — `RepartitionExec` is
exchange verbatim: `BatchPartitioner` routes batches round-robin or by
hash (seed pinned to 0 — `REPARTITION_RANDOM_STATE` — so same key →
same partition, always), `DistributionSender` queues carry them, and a
**gate** provides flow control: senders park when *all* output buffers
are non-empty, so one slow consumer cannot balloon memory while an
empty channel anywhere keeps data flowing. `preserve_order` is the
merging exchange. The old `EnforceDistribution` optimizer rule that
inserted these was retired into `EnsureRequirements`.

**CockroachDB DistSQL (many nodes)** — the planner walks the plan tree
(`checkSupportForPlanNode`), splits table spans by range ownership
(`PartitionSpans` — placement drives execution, topic 36 literally
becomes the parallelism plan), and ships each node a **flow spec**;
processors connect via **routers** (`PASS_THROUGH`, `MIRROR`,
`BY_HASH`, `BY_RANGE` — Volcano's partitioning/broadcast policies as a
protobuf enum) and **outbox/inbox** pairs that are exchange's two
halves stretched over gRPC.

## Code reading (cloned under ~/repos)

| repo | anchor | what to see |
|---|---|---|
| datafusion | `physical-plan/src/repartition/mod.rs:1150` | `RepartitionExec` — exchange as an ExecutionPlan; `:1160` preserve_order |
| datafusion | `repartition/mod.rs:560` | `BatchPartitioner`; `:592` pinned hash seed; `:825` partition_iter |
| datafusion | `repartition/distributor_channels.rs:55` | `channels()` — per-partition queues; `:62` the flow-control gate |
| datafusion | `physical-expr/src/partitioning.rs:117` | `Partitioning` — RoundRobinBatch / Hash |
| cockroach | `pkg/sql/distsql_physical_planner.go:971` | `PartitionSpans` — ranges → nodes → parallel plan |
| cockroach | `pkg/sql/execinfrapb/data.proto:149` | `OutputRouterSpec` — the four router types |
| cockroach | `pkg/sql/colflow/colrpc/outbox.go:218` / `inbox.go:333` | exchange's two halves over gRPC |

## Reading guides

1. [reading-volcano-exchange.md](reading-volcano-exchange.md) — Graefe (TR 89-007): iterators, exchange, packet economics, the measured numbers.
2. [reading-tail-at-scale.md](reading-tail-at-scale.md) — Dean & Barroso (CACM 2013): fan-out math, hedged/tied requests, the toolbox.
3. [reading-datafusion-repartition.md](reading-datafusion-repartition.md) — code read: RepartitionExec, BatchPartitioner, the gate, preserve_order.
4. [reading-cockroach-distsql.md](reading-cockroach-distsql.md) — code read: physical planning, PartitionSpans, routers, outbox/inbox flows.

## Experiments

```
cd experiments
cargo test              # 3 provided tests pass; 6 fix the contract for your stubs
cargo run --release --bin distq_bench
```

- `fanout.rs` (PROVIDED) — two-mode leaf latency (1-10 ms fast, 1000 ms
  stall), closed-form `p_any_slow`, scatter-gather max and
  95%-of-leaves variants.
- `exchange.rs` (stub) — `Exchange::partition` (round-robin cursor /
  splitmix64 hash routing to k outputs) and `merge_sorted` (k-way merge
  — the merging exchange).
- `hedge.rs` (stub) — `request_with_hedge`: fire a second copy only if
  the primary exceeds the delay; take the winner; count requests.

Bench lanes: 1 = fan-out arithmetic + Table 1's shape (provided,
above). 2 = routing throughput and balance at k=8 plus the 8-run merge.
3 = p50/p99/p99.9 and extra-request % across hedge delays
{0, 5, 10, 20, 50 ms} — the 10 ms row should echo 1,800→74 ms.

## Exercises

1. Implement the stubs until all 9 tests pass and lanes 2-3 print.
2. Derive lane 1's two marked points by hand: 1−0.99¹⁰⁰ ≈ 63.4% and
   1−0.9999²⁰⁰⁰ ≈ 18.1%. Then invert: at n=100, what p_slow keeps
   P(any slow) under 1%? (The answer is why per-machine p99.99 matters.)
3. Extend lane 3 with *tied* requests: both copies sent immediately,
   cancellation after a 1 ms stagger — compare tail and total load
   against the 10 ms hedge (paper's Table 2 shape).
4. Add a `frac` sweep to lane 1 (wait for 90/95/99/100% of leaves) and
   find where good-enough stops helping: at what p_slow does even the
   95% cut stall?
5. Batch-size sweep: change lane 2 to route in chunks of {1, 8, 64,
   1024} rows per call and measure rows/s — reproduce Volcano's
   packet-economics curve (171 s → 13.7 s) in miniature.
6. Sketch M37: scatter-gather over M36's slots — which router type does
   each query shape need (point lookup, hash join, sorted scan), and
   where does the merging exchange sit?

## Cross-topic threads

- **Topic 36 (sharding)**: `PartitionSpans` is the bridge — the
  placement map *is* the parallel plan. Bad balance (topic 36's hot
  shard) becomes a straggler here, and fan-out math amplifies it.
- **Topic 35 (overload)**: hedged requests add load to delete latency —
  exactly the trade admission control polices. The paper's 5% hedge
  budget is a retry budget by another name.
- **Topic 34 (debugging)**: the fan-out tail is invisible in average
  metrics and in per-leaf profiles; it only appears in end-to-end
  percentiles — coordinated omission's distributed cousin.
- **Topic 22 (vectorized execution)**: Volcano's packet economics
  (12× from batching) is the same force that drove one-tuple-at-a-time
  iterators to vectorized batches within a single core.

## Capstone M37 — distributed queries over the sharded engine

- Scatter-gather reads over M36's slot map: fan out to owning shards,
  merge results; merging exchange for ordered scans.
- An exchange operator in the Rust engine: hash routing for the join
  build side, round-robin for scans, end-of-stream counting per
  producer.
- Hedged reads against slot replicas with a p95-based delay and a
  hedge budget (topic 35's admission layer accounts for it).
- Deliverable numbers: scale-up of one query at 1→2→4→8 shards;
  p99.9 with and without hedging while one shard runs a synthetic
  1000 ms stall; extra-request % at the chosen hedge delay.
