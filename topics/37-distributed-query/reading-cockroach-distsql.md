# CockroachDB DistSQL: Volcano's Exchange Operator, Stretched Over a Cluster

DistSQL is what happens when you take Volcano's exchange operator — the one trick that made
single-box parallelism invisible to every other operator — and replace its shared-memory packet
queues with gRPC streams, its `fork()` with flow specs shipped to remote nodes, and its support
functions with protobuf router enums. This guide walks the code path from "can this plan
distribute?" through span partitioning, physical planning, flows, and the Outbox/Inbox pair that
is exchange's producer and consumer half. Everything below is anchored to the CockroachDB source
cloned at `~/repos/cockroach`.

## The problem in one sentence

**A query's data lives on many nodes, so the plan must be cut into per-node fragments that scan
locally and exchange rows over the network — without any join or aggregation operator ever
knowing the network exists.** Volcano solved this on one machine with the exchange operator;
DistSQL's bet is that the same encapsulation survives a network hop if the receiving end still
looks like an ordinary iterator.

## The concepts, step by step

### Step 1 — First, decide whether the plan can distribute at all

Not every logical operator has a distributed-processor equivalent. `checkSupportForPlanNode`
walks the logical plan and votes on each node: distributable, local-only, or somewhere in
between. Nodes with no DistSQL processor equivalent are not a dead end — `mustWrapNode` wraps
the local planNode so it can be embedded inside a distributed flow as an opaque row source. The
decision is per-node, so a mostly-distributable plan with one awkward operator still distributes
around it.

```mermaid
graph LR
    LP["logical plan"] --> CHK["checkSupportForPlanNode"]
    CHK -->|"has processor"| PHYS["physical planning"]
    CHK -->|"no equivalent"| WRAP["mustWrapNode embeds local planNode"]
    WRAP --> PHYS
```

### Step 2 — Data placement becomes the parallelism plan

This is the bridge from topic 36. `PartitionSpans` takes the table spans a scan needs, consults
range ownership — the placement map you built in the sharding topic — and partitions the spans
by the node holding each range's leaseholder. There is no separate scheduling decision: each
node scans exactly what it already owns, so the sharding layout literally *is* the degree and
shape of parallelism.

```
table spans      [a ──────── e) [e ──────── m) [m ──────── z)
leaseholder           node 1         node 2         node 3
                        │              │              │
PartitionSpans          ▼              ▼              ▼
per-node work    node1: scan a-e  node2: scan e-m  node3: scan m-z
```

The consequence cuts both ways: co-located data means zero data movement for the scan, but
fan-out width now equals the number of nodes owning relevant ranges — Step 7 collects the bill.

### Step 3 — The physical plan: processors connected by streams

`createPhysPlan` and `createPhysPlanForPlanNode` recursively turn the logical plan into a
`PhysicalPlan` — the under-construction distributed plan, which is nothing more than a set of
processors (typed by spec) plus the streams wiring their outputs to inputs. A stream endpoint is
described by `StreamEndpointSpec` and is deliberately location-agnostic: it may resolve to a
local in-memory queue or a remote gRPC stream, and no processor can tell the difference. That is
Volcano's anonymous-input discipline encoded in the plan representation itself.

```
PhysicalPlan (under construction, gateway-side)

  node1: [TableReader]──router──┐
                                ├── stream ──▶ [Aggregator] on gateway
  node2: [TableReader]──router──┤
                                │        each edge = StreamEndpointSpec
  node3: [TableReader]──router──┘        local queue OR remote gRPC — same spec
```

The recursion in `createPhysPlanForPlanNode` mirrors the logical plan bottom-up: scans become
per-node TableReaders positioned by Step 2's span partitioning, and each parent operator either
merges the partial results on the gateway or — when a router can repartition by key — stays
distributed and runs a copy on every node that already has a flow.

### Step 4 — Routers: Volcano's partitioning policies as protobuf enums

Where Volcano's exchange took C support functions to decide which consumer gets each row,
DistSQL declares the policy in `OutputRouterSpec` and the wire format enumerates exactly the
classic options:

```
Volcano exchange policy          OutputRouterSpec enum
-----------------------          ---------------------
single consumer, no routing  →   PASS_THROUGH
broadcast to all consumers   →   MIRROR
hash of key picks consumer   →   BY_HASH   (joins, aggregations)
range of key picks consumer  →   BY_RANGE
```

The runtime implementations live twice — `hashRouter` in the row-based engine and `HashRouter`
in the vectorized engine — because routing a whole column batch amortizes per-row costs the
row engine pays every time. `BY_HASH` is the workhorse: it is how a distributed hash join gets
matching keys onto the same node without any join-side awareness.

### Step 5 — Flows: one fragment per node replaces fork()

A `Flow` is the set of processors and streams scheduled on ONE node for one query — the unit the
gateway ships out instead of forking worker processes. `Setup` instantiates processors from the
spec, `StartInternal` launches the internal goroutines, and `Run` executes the *last* processor
synchronously in the caller's goroutine while the rest run async — saving one goroutine per flow
on the hottest path.

```
gateway node                         remote node 2          remote node 3
┌─────────────────────────┐          ┌───────────────┐      ┌───────────────┐
│ flow: final Aggregator  │  specs   │ flow: scan +  │      │ flow: scan +  │
│       + Inboxes         │ ───────▶ │ router+Outbox │      │ router+Outbox │
│ Run: last proc inline   │          │ Setup / Start │      │ Setup / Start │
└─────────────────────────┘          └───────────────┘      └───────────────┘
```

### Step 6 — The exchange's two halves: Outbox and Inbox over gRPC

The vectorized engine splits exchange across the wire. The producer half is `Outbox`: its `Run`
dials the consumer node and opens a FlowStream RPC, then `sendBatches` serializes record batches
onto the stream. The consumer half is `Inbox`: `RunWithStream` is where the gRPC handler hands
the incoming stream to the reader, and `Next` is a plain operator iterator — the downstream join
or aggregator pulls batches from the Inbox exactly as it would from a local scan. Volcano's
encapsulation survives the network hop intact.

Read the two halves in this order, and the symmetry becomes obvious:

1. `Outbox` struct (`outbox.go:50`) — what state the producer half carries.
2. `Outbox.Run` (`outbox.go:218`) — dial the consumer node, open the FlowStream RPC.
3. `sendBatches` (`outbox.go:323`) — the serialize-and-ship loop.
4. `Inbox` struct (`inbox.go:57`) — the mirror image on the consumer side.
5. `RunWithStream` (`inbox.go:212`) — the gRPC handler hands the stream to the reader.
6. `Inbox.Next` (`inbox.go:333`) — and here the network disappears: it is just an operator.

What replaced what, relative to the 1990 single-machine design:

```
Volcano exchange, 1990              DistSQL, over a cluster
----------------------              ------------------------
fork a producer process        →    ship a FlowSpec, node runs Setup + StartInternal
shared-memory packet queue     →    gRPC FlowStream carrying record batches
support function per policy    →    OutputRouterSpec enum value
consumer's anonymous input     →    Inbox.Next — still anonymous, now remote
```

```mermaid
graph TD
    subgraph "producer flow on node 2"
        SC["TableReader"] --> HR["HashRouter"]
        HR --> OB["Outbox Run then sendBatches"]
    end
    subgraph "consumer flow on gateway"
        IB["Inbox RunWithStream feeds Next"] --> AG["Aggregator calls Next"]
    end
    OB -->|"gRPC FlowStream"| IB
```

### Step 7 — The price: fan-out width is tail-latency exposure

Because `PartitionSpans` derives fan-out from placement, a query over ranges on N nodes waits
for its slowest flow. This is exactly topic 37's fanout lane from "The Tail at Scale": if each
node is slow 1 time in 100, a 100-way fan-out query is slow 63% of the time — the per-node p99
becomes roughly the query median. DistSQL buys locality and parallel scan bandwidth at the cost
of multiplying your exposure to every straggling node; hedging, smaller fan-out via better
placement, and per-flow admission control are the countermeasures, not faster operators.

## Where each step lives in the code

Paths are relative to `~/repos/cockroach`.

| Step | Anchor | What to look at |
|---|---|---|
| 1 | `pkg/sql/distsql_check.go:214` | `checkSupportForPlanNode` — per-node distributability walk |
| 1 | `pkg/sql/distsql_physical_planner.go:312` | `mustWrapNode` — embedding planNodes with no processor equivalent |
| 2 | `pkg/sql/distsql_physical_planner.go:971` | `PartitionSpans` — spans partitioned by leaseholder node |
| 3 | `pkg/sql/distsql_physical_planner.go:3604` | `createPhysPlan`; `createPhysPlanForPlanNode` at `:3632` |
| 3 | `pkg/sql/physicalplan/physical_plan.go:125` | `PhysicalPlan` — processors + streams under construction |
| 4 | `pkg/sql/execinfrapb/data.proto:72` | `StreamEndpointSpec` — local queue or remote gRPC stream |
| 4 | `pkg/sql/execinfrapb/data.proto:149` | `OutputRouterSpec`; enum at `:152` `PASS_THROUGH`, `:154` `MIRROR`, `:157` `BY_HASH`, `:160` `BY_RANGE` |
| 4 | `pkg/sql/rowflow/routers.go:538` | `hashRouter` — row-based engine |
| 4 | `pkg/sql/colflow/routers.go:443` | `HashRouter` — vectorized engine |
| 5 | `pkg/sql/flowinfra/flow.go:72` | `Flow` interface; `Setup` at `:272`, `StartInternal` at `:463`, `Run` at `:566` |
| 6 | `pkg/sql/colflow/colrpc/outbox.go:50` | `Outbox`; `Run` dials at `:218`, `sendBatches` at `:323` |
| 6 | `pkg/sql/colflow/colrpc/inbox.go:57` | `Inbox`; `RunWithStream` at `:212`, `Next` at `:333` |
| 7 | `pkg/sql/distsql_physical_planner.go:971` | `PartitionSpans` — fan-out width falls out of placement |

## Questions to answer in notes.md

1. Which planNodes does `checkSupportForPlanNode` reject, and what does `mustWrapNode`'s wrapping
   cost at runtime — where does the wrapped local planNode execute, and what parallelism is lost?
2. Trace `PartitionSpans` for a three-node table: what happens when the gateway's leaseholder
   cache is stale and a flow is scheduled on a node that no longer holds the range's lease?
3. Why does `Flow.Run` execute the last processor synchronously in the caller's goroutine instead
   of spawning it like the others — what does that save per query, and per node, under high QPS?
4. Follow `sendBatches` in the Outbox: what provides backpressure so a fast producer cannot
   overrun a slow Inbox — gRPC stream flow control, an explicit window, or buffering in between?
5. Using the Step 7 model: with 1-in-100 per-node slowness, what fraction of queries are slow at
   20-way fan-out versus 100-way, and what does that imply for range placement of hot tables?

## Done when

- [ ] You can narrate the full path — logical plan → `checkSupportForPlanNode` →
      `PartitionSpans` → `PhysicalPlan` → per-node `Flow` — without looking at the code.
- [ ] You can point at the line where the consumer side of a network exchange becomes an
      ordinary iterator, and explain why that keeps every other operator network-oblivious.
- [ ] You can map all four `OutputRouterSpec` policies to their Volcano exchange ancestors and
      name which one a distributed hash join uses and why.
- [ ] You have answered all five questions above in `notes.md`, with file:line evidence.

## References

- **Code**: `~/repos/cockroach` — all anchors above are relative to the repo root.
- **Conceptual ancestor**: Goetz Graefe, *Encapsulation of Parallelism in the Volcano Query
  Processing System* (SIGMOD 1990) — the exchange operator DistSQL stretches over gRPC.
- **The placement side**: [topic 36's sharding guide](../36-sharding/README.md) — where the
  range/leaseholder map that `PartitionSpans` consults comes from.
- **Local stub**: [`experiments/src/exchange.rs`](experiments/src/exchange.rs) — build the
  single-process exchange first; DistSQL is that plus serialization and a dial.
