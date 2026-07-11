# Materialize vs RisingWave: two production IVM bets

Both systems sell "materialized views that stay fresh," built on
opposite bets: Materialize productionizes differential dataflow (one
delta algebra, arrangements in RAM), RisingWave hand-writes incremental
executors with explicit state in an LSM on S3. Reading them side by
side shows which parts of IVM are theory and which are operations —
and which parts a single-writer graph engine gets for free.

## 1. Materialize: differential dataflow, productionized

The compute layer (`src/compute/src/render/`) compiles SQL plans into
differential dataflows. The parts worth reading:

| anchor | what it is |
|---|---|
| `render/join/delta_join.rs:47` | "dogs^3" delta-query joins: an n-way join becomes n dataflows, each starting from one input's changes — the bilinear rule generalized so NO intermediate arrangements are built |
| `delta_join.rs:315/:402` | `half_join` construction (and the newer `half_join2`): ΔA against B's arrangement, time-stamped so the n paths don't double-count — our stub's "state BEFORE the delta" rule, industrial edition |
| `render/reduce.rs` | the nonlinear ops, each with its arrangement |
| `src/compute/src/arrangement/` | arrangement sharing across dataflows — one index, many standing queries |
| `src/persist-client/` | the durable shard log: compute is stateless-ish; state rehydrates from persist (topic 28's disaggregation, applied to IVM) |

The signature idea: **indexes are arrangements are memory**. A Materialize
"index" is an arrangement pinned in RAM shared by every query that can use
it; capacity planning is arrangement accounting.

**Q1.** Delta joins need an arrangement per input per join key but no
intermediate state. Linear (binary-tree) joins need intermediate
arrangements but fewer per-input ones. Materialize chooses delta joins
when the arrangements already exist. Map this onto topic 10's
join-ordering cost model: what's the analogue of "interesting orders"?

## 2. RisingWave: streaming executors + LSM state on S3

No differential core — hand-written incremental executors
(`src/stream/src/executor/`), each managing explicit state in Hummock
(a shared LSM over object storage):

| anchor | what it is |
|---|---|
| `common/src/array/stream_chunk.rs:45` | `enum Op { Insert, Delete, UpdateDelete, UpdateInsert }` — Z-set weights as a protocol; Update split into paired Delete+Insert so downstream operators never need "modify" |
| `stream/src/executor/hash_join.rs:158` | `HashJoinExecutor`: both sides' rows in state tables; `need_degree_table` :117 + degree tables :269 track match counts so outer joins can retract NULLs correctly — hand-rolled weight bookkeeping |
| `executor/barrier_align.rs` | two-input operators align on barriers before emitting — the consistency unit |
| `executor/aggregate/`, `top_k/` | each nonlinear op = explicit state table schema in Hummock |

Barriers (Chandy-Lamport) flow from sources; when an operator has a
barrier from all inputs it flushes state to Hummock — a globally
consistent checkpoint per epoch. Recovery = reload from S3 + replay the
log since the checkpoint. Compare topic 15's replication story: the
checkpoint interval IS the replay window.

**Q2.** RisingWave's degree table vs differential's diff arithmetic: both
solve "when the last matching row leaves, retract the outer-join NULL row."
One is a schema and code per operator; the other is one consolidation
rule for all operators. What does RisingWave get in exchange? (Hint:
per-operator state schemas are legible to S3 spill, per-key TTL, and
elastic scaling of a SINGLE operator.)

## 3. The comparison that matters for M27

| axis | Materialize | RisingWave | M27 (FalkorDB standing queries) |
|---|---|---|---|
| delta algebra | diffs everywhere (differential) | Op enum per chunk | delta matrices (DP/DM) |
| join state | shared arrangements, RAM | per-join Hummock tables | the graph matrices themselves |
| consistency unit | timestamp + frontier | barrier/epoch | writer tick (single writer!) |
| recovery | rehydrate from persist | checkpoint + replay | topic 5's WAL replay |

The single-writer graph engine gets the hard parts free: no barrier
alignment (one clock), no distributed frontier (one writer). What M27
inherits from this guide is the *shape*: standing query = compiled
circuit + explicit per-operator state + delta in/delta out per tick.

**Q3.** Both systems separate compute from durable state (persist / S3).
For M27 inside FalkorDB, state lives in the same process as the graph.
Name one thing that gets easier (no rehydration protocol) and one that
gets harder (memory pressure from arrangements competes with the graph
itself — who evicts?).

## References

**Code**
- [materialize](https://github.com/MaterializeInc/materialize) `src/` —
  compute (differential): `src/compute/src/render/join/delta_join.rs`,
  `render/reduce.rs`, `src/compute/src/arrangement/`; persist (durable
  log): `src/persist-client/`; plus the in-repo architecture docs
  (`doc/developer/` — skim "formalism" and "platform")
- [risingwave](https://github.com/risingwavelabs/risingwave) `src/` —
  stream executors: `src/stream/src/executor/` (hash_join.rs,
  barrier_align.rs, aggregate/, top_k/); the Op enum:
  `common/src/array/stream_chunk.rs:45`; Hummock state store
