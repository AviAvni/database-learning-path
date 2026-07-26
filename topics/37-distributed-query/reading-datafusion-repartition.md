# DataFusion's RepartitionExec: Volcano's Exchange Operator, One Process at a Time

RepartitionExec is DataFusion's in-process exchange operator: it takes N input partitions and redistributes their
batches across M output partitions, using either round-robin (for balance) or hash (for key affinity) routing.
It is the direct descendant of Volcano's exchange — the operator that made parallelism invisible to every other
operator — squeezed into one process with tokio tasks instead of OS processes. This guide walks the routing core,
the deterministic hash contract that joins depend on, and a flow-control design that is deliberately not a bounded queue.

## The problem in one sentence

**Every other operator in the plan wants to pretend it is single-threaded, so one operator must own all
data movement between partitions — routing, buffering, backpressure, and order preservation.**
Volcano called this "anonymous inputs": a hash join reading partition 3 neither knows nor cares that its rows
came from eight different producers. RepartitionExec is where that pretense is manufactured.

## The concepts, step by step

### Step 1 — The N-to-M fan: what the operator actually is

`RepartitionExec` (mod.rs:1150) maps N input partitions to M output partitions. `execute` (:1329) is called once
per *output* partition; on first call it spawns one `pull_from_input` task (:1742) per *input* partition. Each task
drains its input stream, routes every batch, and pushes into per-output channels. Consumers just poll their channel.

```text
 input part 0 ──┐                       ┌── output part 0
 input part 1 ──┤   pull_from_input     ├── output part 1
 input part 2 ──┤ ──► BatchPartitioner ─┤       ...
     ...        │   (route each batch)  │
 input part N-1─┘                       └── output part M-1

 N producer tasks          N x M channel matrix          M consumer streams
```

The key structural fact: there are N×M logical channels, not one shared queue. That matrix is what makes both
flow control (Step 5) and order-preserving merge (Step 6) possible. It also localizes contention: a producer
and a consumer only ever synchronize on their own channel plus one shared gate, never on a global lock over all
in-flight batches. `consume_input_streams` (:393) is the entry point on the producing side that wires this up.

### Step 2 — Routing policy is a plan-time property

The `Partitioning` enum (partitioning.rs:117) declares the contract: `RoundRobinBatch(usize)` (:119) says
"any batch anywhere, just balance the load"; `Hash(Vec&lt;Arc&lt;dyn PhysicalExpr&gt;&gt;, usize)` (:122) says
"rows with equal key expressions must land in the same output partition." Round-robin is what you use to widen
a single-partition scan to all cores; hash is what a partitioned hash join or hash aggregate demands, because
correctness — not just balance — depends on co-locating keys.

```mermaid
graph LR
    A["Plan requires distribution"] --> B["RoundRobinBatch"]
    A --> C["Hash on key exprs"]
    B --> D["balance only - any row anywhere"]
    C --> E["key affinity - equal keys same partition"]
    E --> F["hash join both sides agree"]
    E --> G["hash aggregate no cross-partition merge"]
```

### Step 3 — Deterministic hashing: the seeds are zero on purpose

`BatchPartitioner` (mod.rs:560) is constructed for hash mode at :667 and :689 using
`REPARTITION_RANDOM_STATE` (:592) — a `RandomState` built with FIXED seeds of 0. This is not laziness; it is a
correctness contract. A hash join repartitions *both* inputs by the join keys: if the build side and the probe
side hashed with different per-instance random seeds, key `42` could go to partition 1 on one side and partition 5
on the other, and the join would silently drop matches. Fixed seeds make routing deterministic across runs, across
operators, and (in a distributed setting) across nodes. The partition index is then `hash % partition_count`
(:675 — a strength-reduced modulo, since M is a runtime value, not a power of two you can mask with).

The flip side of determinism: adversarial or pathological key sets that collide will collide *every* run — skew is
reproducible, which is good for debugging and bad if your workload hits it.

### Step 4 — Batch economics: whole batches round-robin, per-row scatter for hash

`partition_iter` (mod.rs:825) is the routing loop, and it treats the two policies asymmetrically:

- **Round-robin** forwards the *entire* RecordBatch to the next output in rotation. Zero per-row work, zero copies —
  the batch is just a bundle of Arc'd arrays changing queues.
- **Hash** must look at every row: `create_hashes` (:854) computes one hash per row over the key columns, the loop
  builds an index list per target partition, and arrow's `take` kernel gathers one new batch per partition.

This is Volcano's packet-economics lesson restated. Graefe measured 171 s at 1 record per exchange packet versus
13.7 s at 83 records per packet — a 12x swing purely from amortizing per-transfer cost. DataFusion's RecordBatch
*is* the packet; round-robin keeps the packet intact, while hash pays a per-row scatter but immediately
re-materializes results as full columnar batches so every downstream operator still sees good packets.

```text
 round-robin:  [batch 8192 rows] ──────────────► output (i mod M)   no row work

 hash:         [batch 8192 rows]
                   │ create_hashes per row
                   ▼
               indices: p0:[0,5,9...] p1:[2,3...] ... pM-1:[1,4...]
                   │ arrow take per partition
                   ▼
               M smaller batches, one per output   per-row cost, then columnar again
```

### Step 5 — Flow control: unbounded channels behind one global gate

The channels are built by `channels()` (distributor_channels.rs:55) — N linked per-output buffers sharing one
`Gate` with an `empty_channels` counter (:62). `DistributionSender::send` (:121, :131) implements the unusual
semantics: each per-output buffer is individually *unbounded*, and a sender parks only when *all* M output buffers
are non-empty. If any single channel is empty, every send proceeds.

Why not M bounded queues? Deadlock and starvation in join plans. With per-channel bounds, one slow consumer
(say, a join output partition blocked on its other input) would block the producer, which would then stop feeding
the *fast* consumers too — and in cyclic-ish plan shapes that stall can become a distribution deadlock. The global
gate inverts the condition: backpressure engages only when *everyone* has data waiting, so a fast consumer can
never be starved by pressure aimed at a slow one, while total buffering stays bounded in the common case where at
least one consumer keeps draining.

The trade-off is honest: in the worst case (all consumers stalled except one that never fills), buffering can
grow past what a bounded design would allow. DataFusion accepts that memory risk in exchange for liveness — the
same call Volcano made when it favored keeping producers running over strict per-flow limits.

```mermaid
graph TD
    S["sender with routed batch"] --> Q["is any output buffer empty"]
    Q -->|"yes"| P["send proceeds - buffers are unbounded"]
    Q -->|"no - all M non-empty"| W["sender parks on the gate"]
    W --> R["a consumer drains its buffer to empty"]
    R --> P
```

### Step 6 — preserve_order: the merging exchange

When each input partition is already sorted and the plan needs that order, the `preserve_order` flag (mod.rs:1160)
switches RepartitionExec into merge mode — Volcano's MERGING exchange. Records from different producers must be
kept *distinct* per producer and merged by sort key, never dumped into one bag. The machinery at :398-538 keeps a
dedicated channel per (input, output) pair — including spill-to-disk channel variants for memory pressure — and
`consume_input_streams` (:393) drives the input side. Each output partition then runs a streaming k-way merge over
its N per-input channels instead of interleaving arrival order. Cost model: you buy a global sort order for the
price of a per-output merge heap and stricter buffering (a merge cannot emit until every input channel has shown
its next head), which is exactly why the flag is opt-in rather than default.

```text
 plain mode:   in0, in1, in2  ──►  one channel per output  ──►  arrival-order interleave
 merge mode:   in0 ─► ch(0,j) ─┐
               in1 ─► ch(1,j) ─┼─►  k-way streaming merge  ──►  output j, globally sorted
               in2 ─► ch(2,j) ─┘    per-input identity preserved
```

### Step 7 — Who inserts RepartitionExec: EnforceDistribution is retired

You will read blog posts saying "the EnforceDistribution rule inserts RepartitionExec." That rule was RETIRED and
folded into `EnsureRequirements` (physical-optimizer/src/ensure_requirements/mod.rs:159). The old file
`enforce_distribution.rs` still exists but now holds helper functions; its doc comments (:18, :76) say so
explicitly. When a plan node declares a required input distribution — hash-partitioned on join keys, or a single
partition — EnsureRequirements inserts the RepartitionExec that satisfies it. Do not be confused when you grep for
the rule the guides mention and find only helpers.

## Where each step lives in the code

| Step | What | Anchor |
|------|------|--------|
| 1 | Operator struct, N→M | datafusion/physical-plan/src/repartition/mod.rs:1150 |
| 1 | Per-output execute | datafusion/physical-plan/src/repartition/mod.rs:1329 |
| 1 | Per-input pull task | datafusion/physical-plan/src/repartition/mod.rs:1742 |
| 2 | Partitioning enum | datafusion/physical-expr/src/partitioning.rs:117 |
| 2 | RoundRobinBatch / Hash variants | datafusion/physical-expr/src/partitioning.rs:119, :122 |
| 3 | Fixed-seed RandomState | datafusion/physical-plan/src/repartition/mod.rs:592 |
| 3 | Hash constructor paths | datafusion/physical-plan/src/repartition/mod.rs:667, :689 |
| 3 | hash % partition_count | datafusion/physical-plan/src/repartition/mod.rs:675 |
| 4 | Routing core struct | datafusion/physical-plan/src/repartition/mod.rs:560 |
| 4 | Round-robin constructor | datafusion/physical-plan/src/repartition/mod.rs:699 |
| 4 | partition_iter routing loop | datafusion/physical-plan/src/repartition/mod.rs:825 |
| 4 | Per-row create_hashes | datafusion/physical-plan/src/repartition/mod.rs:854 |
| 5 | channels constructor | datafusion/physical-plan/src/repartition/distributor_channels.rs:55 |
| 5 | Gate with empty_channels | datafusion/physical-plan/src/repartition/distributor_channels.rs:62 |
| 5 | DistributionSender::send | datafusion/physical-plan/src/repartition/distributor_channels.rs:121, :131 |
| 6 | preserve_order flag | datafusion/physical-plan/src/repartition/mod.rs:1160 |
| 6 | Merge-mode channel machinery | datafusion/physical-plan/src/repartition/mod.rs:398-538 |
| 6 | consume_input_streams | datafusion/physical-plan/src/repartition/mod.rs:393 |
| 7 | EnsureRequirements rule | datafusion/physical-optimizer/src/ensure_requirements/mod.rs:159 |
| 7 | Retirement notes | datafusion/physical-optimizer/src/enforce_distribution.rs:18, :76 |

## Questions to answer in notes.md

1. Why must `REPARTITION_RANDOM_STATE` use fixed seeds — walk through exactly how a hash join breaks if the
   build-side and probe-side RepartitionExec instances used per-instance random seeds.
2. Round-robin forwards whole batches while hash scatters per row and rebuilds batches with `take`. What are the
   CPU and memory costs of each, and why does the local stub's row-at-a-time model show hash routing *faster*
   (543.0 M rows/s vs 229.6 M rows/s at k=8) while DataFusion's batch model favors round-robin for cheapness?
3. Describe a concrete plan shape where M per-channel *bounded* queues could deadlock or starve a fast consumer,
   and explain how the single Gate with the `empty_channels` counter avoids it. What is the worst-case buffering
   the gate design permits?
4. In preserve_order mode, why does correctness require a channel per (input, output) pair rather than one channel
   per output? What extra latency does the k-way merge introduce before the first row can be emitted?
5. After the EnforceDistribution → EnsureRequirements consolidation, trace how a hash join's required input
   distribution causes a `Hash` RepartitionExec to appear — which rule sees the requirement, and what does it insert?

## Done when

- [ ] You can sketch the N×M channel matrix from memory and explain why it is not one shared MPMC queue.
- [ ] You can state the deterministic-hashing contract and name two operators whose correctness depends on it.
- [ ] You can explain the Gate flow-control rule ("park only when all buffers are non-empty") and its failure mode
      trade-off versus per-channel bounds.
- [ ] You can explain when preserve_order forces merge mode and what it costs relative to interleaving.
- [ ] You have run the local exchange stub and compared its routing throughput numbers against your own reasoning
      about DataFusion's batch-level costs.

## References

- Repo: `~/repos/datafusion` — `datafusion/physical-plan/src/repartition/` (operator + channels),
  `datafusion/physical-expr/src/partitioning.rs` (policy enum),
  `datafusion/physical-optimizer/src/ensure_requirements/` (insertion rule).
- Goetz Graefe, *Encapsulation of Parallelism in the Volcano Query Processing System*, TR CS/E 89-007 — the
  exchange operator, anonymous inputs, packet economics (171 s at 1 record/packet vs 13.7 s at 83), and the
  MERGING exchange variant.
- Local stub: `experiments/src/exchange.rs` — reimplements the routing (round-robin + deterministic hash) and
  merge contracts in miniature; reference numbers at k=8: 229.6 M rows/s round-robin, 543.0 M rows/s hash.
