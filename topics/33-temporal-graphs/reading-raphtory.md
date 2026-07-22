# Raphtory: the graph IS the event log, views are lenses

Raphtory (Pometry) is the event-log-first pole of topic 33's storage
menu, in Rust: where AeonG starts from objects and catches their
versions on the way to the garbage collector, Raphtory never has
objects to begin with — every fact is a timestamped event in an
append-only log, and "the graph" (current, AT TIME, BETWEEN) is a lens
over that log. The repo is cloned at `~/repos/raphtory`; this is a
code-read, ~1.5h, focused on five types that carry the whole design.
Before opening files, this chapter builds the ideas in order; the
anchor table below maps each step to an exact file:line.

## The problem in one sentence

Serve `AT TIME t` and `BETWEEN t1 AND t2` over millions of timestamped
edge events without ever materializing a snapshot — in Raphtory a
window over the entire history costs exactly two optional timestamps,
zero bytes of graph copied.

## The concepts, step by step

### Step 1 — event-log-first: the log is primary, "now" is a window

An **event-log-first** engine stores the stream of timestamped changes
as its primary representation and derives every graph state from it —
the inverse of memgraph/AeonG, where the current object graph is
primary and history hangs off it in chains. The current graph is not
special: it is merely the view windowed to [−∞, ∞).

```
 object-first (memgraph/AeonG):        event-log-first (Raphtory):

 Vertex ──delta──delta──delta          e1 e2 e3 e4 e5 e6 e7 e8 ...  (primary)
   ▲ primary       (history bolted on)      └────┬────┘
                                          view [t1,t2)  = "a graph"
                                          view [−∞,∞)   = "the current graph"
```

Why it matters: this is topic 24's streaming stance promoted to a
storage philosophy, and it dissolves M33's hardest design question —
there is no migration, no second tier, no GC-vs-history tension,
because nothing is ever superseded; the cost moves to indexing the log
so views aren't full replays (Steps 3–4).

### Step 2 — EventTime: a timestamp plus a tiebreaker

Every temporal fact in the system is keyed by one 16-byte pair:

```rust
pub struct EventTime(pub i64, pub usize);   // timestamp + event_id
```

(`raphtory-api/src/core/storage/timeindex.rs:28`.) The `usize` is a
per-event sequence number that totally orders events sharing the same
millisecond. You met exactly this subtlety in Wu et al.'s one-pass
temporal path algorithms (reading-temporal-paths.md): with λ=0
contacts at equal t, correctness of a single sorted pass hinges on a
deterministic tie order. Raphtory bakes the tie order into the key
type itself, so every BTree in the engine sorts events identically.
Why it matters: "time" in a temporal engine is never just i64 — the
moment two events collide on a timestamp, reproducibility of every
window and every path answer depends on the tiebreaker.

### Step 3 — TimeIndex and TCell: size-adaptive timelines

WHEN an entity existed and WHAT VALUE a property had are both
*timelines* — sorted collections keyed by EventTime — and both enums
ladder up by history length, because event counts per entity are
power-law (most entities: one event; same trick as memgraph's
small_vector in topic 13):

```rust
// raphtory-core/src/storage/timeindex.rs:13 — WHEN an entity existed
pub enum TimeIndex<T> { Empty, One(T), Set(BTreeSet<T>) }

// raphtory-core/src/entities/properties/tcell.rs:10 — a value in time
pub enum TCell<A> {
    Empty,
    TCell1(EventTime, A),               // one event: no allocation
    TCellCap(SVM<EventTime, A>),        // few: small-vector map
    TCellN(BTreeMap<EventTime, A>),     // many: real tree
}
```

The in-source comment says it plainly: "TCells represent a value in
time that can be set at multiple times and keeps a history" — a
property is a timeline, not a value; reading it *requires* saying at
what time. Why it matters: per-entity time indexes are what make a
window a probe instead of a replay — `BETWEEN t1 AND t2` on one node
is a range query on its TimeIndex, not a scan of the global log.

### Step 4 — properties: columnar log + time→offset index

Property *values* don't live inside the TCell — they live in a
columnar log, and the TCell maps time to an offset into it:

```rust
// raphtory-core/src/entities/properties/tprop.rs:22
pub struct TPropCell<'a> {
    t_cell: Option<&'a TCell<Option<usize>>>,   // time → offset
    // ... new(t_cell, log: Option<&PropColumn>) at :28
}
```

So one property read is: probe the TCell for the newest EventTime ≤ t,
get a `usize`, index the PropColumn. Why it matters: this splits the
two access patterns cleanly — temporal navigation stays in small
per-entity indexes (cache-friendly, Step 3's enums), while values sit
in dense columns (topic 12's layout) that scans and analytics can
stream. It's the same time-vs-payload separation AeonG gets from KV
key-vs-value, done in-memory and columnar.

### Step 5 — WindowedGraph + TimeOps: views as composable zero-copy lenses

A **view** is a struct that wraps a graph and reinterprets every read
through a filter — here, a time filter:

```rust
// raphtory/src/db/graph/views/window_graph.rs:87 — derives Copy, Clone
pub struct WindowedGraph<G> {
    pub graph: G,
    pub start: Option<EventTime>,
    pub end: Option<EventTime>,
}
```

It derives `Copy`: a BETWEEN view is two optional timestamps wrapping
the graph, nothing copied. The `TimeOps` trait
(`raphtory/src/db/api/view/time.rs:116`) declares
`fn window<T1: IntoTime, T2: IntoTime>(&self, start, end) ->
Self::WindowedViewType` with default impls (~:245), and every view
type — graph, node, edge — implements it, so views *compose*: a window
of a window intersects the ranges; `at(t)` is a degenerate window.
Downstream, even existence is windowed — an edge's presence is an
iterator over its addition times per layer, not a boolean
(`additions_iter`/`additions`,
`raphtory-storage/src/graph/edges/edge_storage_ops.rs:110,:140`). Why
it matters: this is AT TIME/BETWEEN done as *algebra* — M33's `FOR
TT`-style clauses become constructors of a view type the whole query
engine already runs on, instead of a special mode threaded through
every operator.

### Step 6 — where it's going: db4 segments and Cypher

The workspace tells you the roadmap: `raphtory-cypher` runs Cypher
over these temporal views (the same "bolt a query language onto a time
model" move as AeonG's `FOR TT`), and `db4-graph` + `db4-storage` are
a newer segmented storage engine — see `pub struct MemEdgeSegment`
(`db4-storage/src/segments/edge/segment.rs:58`) — replacing per-entity
allocations with segment-grained storage. Why it matters: the pure
event-log model is allocation-heavy at scale for the same reason
memgraph is (Step 3's per-entity enums are still per-entity objects);
segments are the "batch it into arrays" correction — the recurring
arc of this whole learning path.

## Where each step lives in the code

All paths relative to `~/repos/raphtory`. Workspace crates: `raphtory`
(main API), `raphtory-api`, `raphtory-core`, `raphtory-storage`,
`raphtory-cypher`, `raphtory-graphql`, `db4-graph` + `db4-storage`.

| Step | Anchor | What to see |
|---|---|---|
| 2 | `raphtory-api/src/core/storage/timeindex.rs:28` | `EventTime(pub i64, pub usize)` — the universal key |
| 3 | `raphtory-core/src/storage/timeindex.rs:13` | `TimeIndex { Empty, One, Set }` — when an entity existed |
| 3 | `raphtory-core/src/entities/properties/tcell.rs:10` | `TCell` enum ladder + the "value in time" comment |
| 4 | `raphtory-core/src/entities/properties/tprop.rs:22` | `TPropCell` — TCell holds time→offset into a `PropColumn` (`new` at :28) |
| 5 | `raphtory/src/db/graph/views/window_graph.rs:87` | `WindowedGraph{graph, start, end}`, derives `Copy` |
| 5 | `raphtory/src/db/api/view/time.rs:116` | `TimeOps::window` declaration; default impls ~:245 |
| 5 | `raphtory-storage/src/graph/edges/edge_storage_ops.rs:110,:140` | `additions_iter` / `additions` — edge existence as an iterator |
| 6 | `db4-storage/src/segments/edge/segment.rs:58` | `MemEdgeSegment` — the newer segmented engine |

Read order: EventTime → TCell (read the whole enum and its comment) →
TPropCell → WindowedGraph → TimeOps (trace `window` from declaration
to one default impl) → skim a db4 segment. Resist reading more; these
eight anchors are the design.

## Questions (answer in notes.md)

1. M33: what would a `WindowedGraph` over FalkorDB's GraphBLAS
   matrices be? Two timestamps can't lazily filter a dense SpMV — do
   BETWEEN views become masks (topic 20), materialized submatrices, or
   per-operation time predicates, and what does each cost?
2. The experiments crate's `events.rs::replay_at_time` answers AT TIME
   by replaying the log from t=0. Which Raphtory structures replace
   the replay, and what is the probe cost per node in their terms
   (Step 3)?
3. EventTime's `usize` tiebreaker vs the λ=0 tie-order stream from Wu
   et al. (exercise 2 in the README): show how a total order on events
   makes the one-pass earliest-arrival deterministic where bare i64
   timestamps aren't.
4. Contrast with memgraph (topic 13): both end up with per-entity
   small-then-spill collections (small_vector vs TCell's ladder), yet
   one is object-first and one log-first. What query does each answer
   in O(1) that costs the other a scan?
5. Raphtory has no GC question — nothing is ever superseded — but that
   means the log only grows. Steal AeonG's vocabulary: what would an
   "anchor" be in an event-log-first engine, and where would you put
   it? (Hint: your `snapshot.rs` is exactly this hybrid.)

## Done when

You can trace, naming the concrete types at each hop, what
`g.window(t1, t2).node(n).properties()` touches — WindowedGraph →
TimeOps → TimeIndex range → TCell probe → PropColumn offset — and say
which single hop `at(t)` on a never-updated node skips (TCell1: no
tree, no allocation).

## References

**Code**
- [Raphtory](https://github.com/Pometry/Raphtory) — cloned at
  `~/repos/raphtory`; the eight anchors above are the read
- [memgraph](https://github.com/memgraph/memgraph) — topic 13 clone;
  the object-first pole to hold this against
- This topic's `experiments/src/events.rs` (`replay_at_time`) and
  `snapshot.rs` — the naive and anchor+delta baselines Raphtory's
  indexes replace

**Related guides**
- [reading-aeong.md](reading-aeong.md) — the object-first counterpoint
  built on memgraph
- [reading-temporal-paths.md](reading-temporal-paths.md) — Wu et al.'s
  one-pass algorithms, whose tie-order subtlety EventTime solves in
  the type system
