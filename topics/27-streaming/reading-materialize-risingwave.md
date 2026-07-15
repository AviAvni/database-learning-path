# Materialize vs RisingWave: two production IVM bets

Both systems sell "materialized views that stay fresh," built on
opposite bets: Materialize productionizes differential dataflow (one
delta algebra, arrangements in RAM), RisingWave hand-writes incremental
executors with explicit state in an LSM on S3. This chapter builds the
engineering questions the theory leaves open — where state lives, what
the consistency unit is, how queries share indexes — then walks each
system's answer through its source, and shows which parts a
single-writer graph engine gets for free.

## The problem in one sentence

The calculus says "keep an integral per nonlinear operator" — production
asks the three questions the calculus doesn't: where do a thousand
standing queries' integrals *live* (RAM vs S3), what unit makes their
outputs *consistent* (frontier vs barrier), and who pays when two
queries need the same index — and Materialize and RisingWave answer all
three in opposite directions.

## The concepts, step by step

### Step 1 — what production adds to the theory

An IVM engine in production is the delta algebra plus three systems
decisions. **State placement**: every nonlinear operator's integral
(join state, aggregate counts) must live somewhere with a cost —
RAM is fast and evaporates on crash; object storage survives but adds
milliseconds. **Consistency unit**: outputs from different operators
must correspond to the *same* input prefix, or a dashboard joins hour-7
counts against hour-9 sums. **Sharing and recovery**: 1000 standing
queries over the same tables must not keep 1000 copies of the same
index, and a restarted node must rebuild its state from something.
Everything in the two codebases below is one of these three, answered.

### Step 2 — Materialize: indexes are arrangements are memory

Materialize's bet is to change as little theory as possible: the compute
layer (`src/compute/src/render/`) compiles SQL plans into differential
dataflows, and its signature idea is **indexes are arrangements are
memory** — a Materialize "index" is a differential arrangement (the
shared, compacted, indexed update log from the differential guide)
pinned in RAM and shared by every query that can use it. Sharing is the
memory model: one arrangement, many standing queries; capacity planning
*is* arrangement accounting. Durability is delegated: `src/persist-client/`
keeps a durable shard log, compute is stateless-ish, and state
rehydrates from persist on restart — topic 28's disaggregation applied
to IVM. Consistency comes free from timely: outputs are correct as of a
timestamp when the frontier passes it, and reads are strict
serializable.

### Step 3 — delta joins: the bilinear rule scaled to n inputs

An n-way incremental join done as a binary tree needs an arrangement for
every *intermediate* result — state that exists only to serve the join.
Materialize's "dogs^3" **delta joins**
(`render/join/delta_join.rs:47`) avoid that: the n-way join becomes n
dataflows, each starting from one input's changes and looking up the
other n−1 inputs' *existing* arrangements — the bilinear rule
generalized so NO intermediate arrangements are built. The correctness
subtlety is double-counting: the n paths must not each claim the same
joint update, so `half_join` (:315, and the newer `half_join2` :402)
time-stamps lookups — ΔA joins B's arrangement *as of the time just
before* the delta — our stub's "state BEFORE the delta" rule, industrial
edition. The cost: delta joins need an arrangement per input per join
key, so they're chosen when those arrangements already exist (question 1
maps this onto topic 10's "interesting orders").

### Step 4 — RisingWave: hand-written executors, state in an LSM on S3

RisingWave's bet is the opposite: no differential core, no general delta
algebra — each relational operator is a hand-written incremental
executor (`src/stream/src/executor/`) that manages *explicit, schema'd
state tables* in **Hummock**, a shared LSM over object storage. The
Z-set shows up wearing protocol clothing: every stream chunk's rows
carry an `Op` (`common/src/array/stream_chunk.rs:45` — `enum Op {
Insert, Delete, UpdateDelete, UpdateInsert }`), weights ±1 as an enum,
with Update split into paired Delete+Insert so downstream operators
never need "modify". Where differential gets retraction from diff
arithmetic, RisingWave hand-rolls it per operator:
`HashJoinExecutor` (hash_join.rs:158) keeps both sides' rows in state
tables plus **degree tables** (:117 `need_degree_table`, :269) tracking
match counts, so outer joins can retract their NULL rows when the last
match leaves. What the per-operator schemas buy: state that is legible
to S3 spill, per-key TTL, and elastic scaling of a *single* operator
(question 2).

### Step 5 — barriers: consistency and recovery by checkpoint

RisingWave's consistency unit is the **barrier** — a Chandy-Lamport-style
marker injected at sources that flows through the dataflow with the
data. Two-input operators align on barriers before emitting
(`executor/barrier_align.rs`), and when an operator has the barrier from
all inputs it flushes its state tables to Hummock — a globally
consistent checkpoint per epoch. Recovery = reload the checkpoint from
S3 + replay the source log since it; the checkpoint interval IS the
replay window (compare topic 15's replication story). Contrast the
Materialize column: timely frontiers give consistency continuously and
recovery means rehydrating from persist — one mechanism per system, both
subsumed by "know which input prefix your output reflects."

### Step 6 — the comparison that matters for M27

| axis | Materialize | RisingWave | M27 (FalkorDB standing queries) |
|---|---|---|---|
| delta algebra | diffs everywhere (differential) | Op enum per chunk | delta matrices (DP/DM) |
| join state | shared arrangements, RAM | per-join Hummock tables | the graph matrices themselves |
| consistency unit | timestamp + frontier | barrier/epoch | writer tick (single writer!) |
| recovery | rehydrate from persist | checkpoint + replay | topic 5's WAL replay |

The single-writer graph engine gets the hard parts free: no barrier
alignment (one clock), no distributed frontier (one writer). What M27
inherits from this guide is the *shape*: standing query = compiled
circuit + explicit per-operator state + delta in/delta out per tick —
and one honest warning about memory (question 3): arrangements compete
with the graph itself for RAM, and somebody has to be the evictor.

## Where each step lives in the code

Materialize — Steps 2–3
([materialize](https://github.com/MaterializeInc/materialize) `src/`):

| anchor | what it is |
|---|---|
| `render/join/delta_join.rs:47` | "dogs^3" delta-query joins: an n-way join becomes n dataflows, each starting from one input's changes — the bilinear rule generalized so NO intermediate arrangements are built |
| `delta_join.rs:315/:402` | `half_join` construction (and the newer `half_join2`): ΔA against B's arrangement, time-stamped so the n paths don't double-count — our stub's "state BEFORE the delta" rule, industrial edition |
| `render/reduce.rs` | the nonlinear ops, each with its arrangement |
| `src/compute/src/arrangement/` | arrangement sharing across dataflows — one index, many standing queries |
| `src/persist-client/` | the durable shard log: compute is stateless-ish; state rehydrates from persist (topic 28's disaggregation, applied to IVM) |

Also skim the in-repo architecture docs (`doc/developer/` —
"formalism" and "platform").

RisingWave — Steps 4–5
([risingwave](https://github.com/risingwavelabs/risingwave) `src/`):

| anchor | what it is |
|---|---|
| `common/src/array/stream_chunk.rs:45` | `enum Op { Insert, Delete, UpdateDelete, UpdateInsert }` — Z-set weights as a protocol; Update split into paired Delete+Insert so downstream operators never need "modify" |
| `stream/src/executor/hash_join.rs:158` | `HashJoinExecutor`: both sides' rows in state tables; `need_degree_table` :117 + degree tables :269 track match counts so outer joins can retract NULLs correctly — hand-rolled weight bookkeeping |
| `executor/barrier_align.rs` | two-input operators align on barriers before emitting — the consistency unit |
| `executor/aggregate/`, `top_k/` | each nonlinear op = explicit state table schema in Hummock |

## Questions to answer in notes.md

1. Delta joins need an arrangement per input per join key but no
   intermediate state. Linear (binary-tree) joins need intermediate
   arrangements but fewer per-input ones. Materialize chooses delta joins
   when the arrangements already exist. Map this onto topic 10's
   join-ordering cost model: what's the analogue of "interesting orders"?
2. RisingWave's degree table vs differential's diff arithmetic: both
   solve "when the last matching row leaves, retract the outer-join NULL
   row." One is a schema and code per operator; the other is one
   consolidation rule for all operators. What does RisingWave get in
   exchange? (Hint: per-operator state schemas are legible to S3 spill,
   per-key TTL, and elastic scaling of a SINGLE operator.)
3. Both systems separate compute from durable state (persist / S3). For
   M27 inside FalkorDB, state lives in the same process as the graph.
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
