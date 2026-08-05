# Raphtory: the graph IS the event log, views are lenses

Raphtory (Pometry) is the event-log-first pole of topic 33's storage
menu, in Rust: where AeonG starts from objects and catches their
versions on the way to the garbage collector, Raphtory never has
objects to begin with — every fact is a timestamped event in an
append-only log, and "the graph" (current, AT TIME, BETWEEN) is a lens
over that log. This is a **code** read, ~1.5 h, focused on five types
that carry the whole design.

The repo is cloned at `~/repos/raphtory` and **pinned at commit
`5d0d286`** (recorded in `resources/codebases.md`); every `file:line`
anchor and every gutter number below was re-checked at that SHA with
`tools/pinned-source.py show raphtory …`, so a later `main` may have
moved them. Before opening files, this chapter builds the ideas in
order; the anchor table maps each step to an exact line.

## The problem in one sentence

Serve `AT TIME t` and `BETWEEN t1 AND t2` over millions of timestamped
edge events without ever materializing a snapshot — in Raphtory a
window over the entire history costs exactly two optional timestamps,
zero bytes of graph copied.

## The concepts, step by step

### Step 1 — event-log-first: the log is primary, "now" is a window

> **In:** nothing yet — this step fixes the storage philosophy (log is
> primary) that every type below is an instance of.
> **Out:** the claim "the current graph is just the view over [−∞, ∞)",
> which Steps 3–5 make cost-free by indexing the log.

An **event-log-first** engine stores the stream of timestamped changes
as its primary representation and derives every graph state from it —
the inverse of Memgraph/AeonG, where the current object graph is
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

> **In:** the append-only log of Step 1, whose events must be totally
> ordered even when two land on the same millisecond.
> **Out:** the 16-byte key every timeline in Steps 3–5 sorts on.

Every temporal fact in the system is keyed by one pair — an i64
timestamp and a `usize` event id:

```rust
// raphtory-api/src/core/storage/timeindex.rs:27 @ 5d0d286
27  #[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Ord, PartialOrd, Eq, Hash)]
28  pub struct EventTime(pub i64, pub usize);   // (timestamp, event_id)
```

The `usize` is a per-event sequence number that totally orders events
sharing the same millisecond. Worked size: on a 64-bit target the i64
is 8 bytes and the `usize` is 8 bytes, so **`EventTime` is 16 bytes**,
and because the derive lists `Ord` *after* `PartialEq` on a tuple
struct, comparison is lexicographic — timestamp first, event id only as
the tiebreaker. You met exactly this subtlety in Wu et al.'s one-pass
temporal-path algorithms (reading-temporal-paths.md): with λ = 0
contacts at equal `t`, correctness of a single sorted pass hinges on a
deterministic tie order. Raphtory bakes the tie order into the key type
itself, so every BTree in the engine sorts events identically.

Why it matters: "time" in a temporal engine is never just i64 — the
moment two events collide on a timestamp, reproducibility of every
window and every path answer depends on the tiebreaker.

### Step 3 — TimeIndex and TCell: size-adaptive timelines

> **In:** the EventTime key from Step 2.
> **Out:** two per-entity timelines (existence and property history) that
> turn a window into a range probe instead of a global scan.

WHEN an entity existed and WHAT VALUE a property had are both
*timelines* — sorted collections keyed by EventTime — and both enums
ladder up by history length, because event counts per entity are
power-law (most entities: one event; the same trick as Memgraph's
small_vector in topic 13). First, existence:

```rust
// raphtory-core/src/storage/timeindex.rs:12 @ 5d0d286 — WHEN an entity existed
12  #[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
13  pub enum TimeIndex<T: Ord + Eq + Copy + Debug> {
14      #[default]
15      Empty,
16      One(T),
17      Set(BTreeSet<T>),
18  }
```

Then property values over time — note the in-source comment on line 9
states the contract outright:

```rust
// raphtory-core/src/entities/properties/tcell.rs:8 @ 5d0d286
 8  #[derive(Debug, PartialEq, Default, Clone, Serialize, Deserialize)]
 9  // TCells represent a value in time that can be set at multiple times and keeps a history
10  pub enum TCell<A> {
11      #[default]
12      Empty,
13      TCell1(EventTime, A),            // one event: stored inline, no allocation
14      TCellCap(SVM<EventTime, A>),     // a few: SVM = small-vector map, inline array
15      TCellN(BTreeMap<EventTime, A>),  // many: a real balanced tree
16  }
```

A property is a timeline, not a value; reading it *requires* saying at
what time. **SVM** here is a *small-vector map* — an association list
kept in a stack-inlined array while it stays short, so a property that
changes a handful of times pays no heap allocation and no tree
overhead. Worked ladder: a node with **1** existence event is
`TimeIndex::One` (16 bytes, no alloc); a property set **once** is
`TCell1` (inline); one edited **~8** times is `TCellCap` (one small
inline array); one edited **thousands** of times spills to `TCellN`'s
`BTreeMap`, paying `O(log n)` lookup only where the history actually
warrants it.

Why it matters: per-entity time indexes are what make a window a probe
instead of a replay — `BETWEEN t1 AND t2` on one node is a range query
on its TimeIndex, not a scan of the global log.

### Step 4 — properties: columnar log + time→offset index

> **In:** the `TCell` timeline from Step 3.
> **Out:** the split between the *time* index (small, per-entity) and the
> *value* log (dense, columnar) that Step 5's scans stream over.

Property *values* don't live inside the TCell — they live in a columnar
log, and the TCell stores an *offset* into it, so the timeline indexes
`Option<usize>` (an offset) rather than the value itself:

```rust
// raphtory-core/src/entities/properties/tprop.rs:21 @ 5d0d286
21  #[derive(Copy, Clone, Debug, Default)]
22  pub struct TPropCell<'a> {
23      t_cell: Option<&'a TCell<Option<usize>>>,   // time → offset
24      log: Option<&'a PropColumn>,                // the columnar value store
25  }
26  // new(t_cell, log: Option<&PropColumn>) at tprop.rs:28 wires the two together
```

Both fields are borrows and the struct is `Copy`: a `TPropCell` is a
*pair of pointers* — the per-entity time index (`t_cell`) and the shared
column (`log`) — not owned data. So one property read is: probe the
`t_cell` for the newest EventTime ≤ t, get a `usize` offset, index the
`PropColumn`.

Why it matters: this splits the two access patterns cleanly — temporal
navigation stays in small per-entity indexes (cache-friendly, Step 3's
enums), while values sit in dense columns (topic 12's layout) that
scans and analytics can stream. It's the same time-vs-payload
separation AeonG gets from KV key-vs-value, done in-memory and
columnar.

### Step 5 — WindowedGraph + TimeOps: views as composable zero-copy lenses

> **In:** the per-entity time indexes of Steps 3–4, which make time
> filtering cheap per node.
> **Out:** a graph-level `WindowedGraph` view and the `TimeOps` algebra
> that turns M33's `FOR TT` clauses into ordinary constructors.

A **view** is a struct that wraps a graph and reinterprets every read
through a filter — here, a time filter — and it is `Copy`:

```rust
// raphtory/src/db/graph/views/window_graph.rs:85 @ 5d0d286
85  /// A struct that represents a windowed view of a `Graph`.
86  #[derive(Copy, Clone)]
87  pub struct WindowedGraph<G> {
88      /// The underlying `Graph` object.
89      pub graph: G,
90      /// The inclusive start time of the window.
91      pub start: Option<EventTime>,
92      /// The exclusive end time of the window.
93      pub end: Option<EventTime>,
94  }
```

It derives `Copy`: a BETWEEN view is the wrapped graph handle plus two
`Option<EventTime>` fields (start inclusive, end exclusive), nothing in
the log copied. The `TimeOps` trait declares `window` and `at` — the
former takes two `IntoTime` bounds, the latter one, and `at(t)` is a
degenerate window:

```rust
// raphtory/src/db/api/view/time.rs:115 @ 5d0d286
115      /// Create a view including all events between `start` (inclusive) and `end` (exclusive)
116      fn window<T1: IntoTime, T2: IntoTime>(&self, start: T1, end: T2) -> Self::WindowedViewType;
117
118      /// Create a view that only includes events at `time`
119      fn at<T: IntoTime>(&self, time: T) -> Self::WindowedViewType;
```

Every view type — graph, node, edge — implements `TimeOps` (default
impls around `time.rs:245`), so views *compose*: a window of a window
intersects the ranges. Downstream, even existence is windowed — an
edge's presence is an iterator over its addition times per layer, not a
boolean (`additions_iter` at
`raphtory-storage/src/graph/edges/edge_storage_ops.rs:110`, `additions`
at `:140`).

Why it matters: this is AT TIME/BETWEEN done as *algebra* — M33's
`FOR TT`-style clauses become constructors of a view type the whole
query engine already runs on, instead of a special mode threaded
through every operator.

### Step 6 — where it's going: db4 segments and Cypher

> **In:** the per-entity model of Steps 3–5, which is elegant but
> allocation-heavy at scale.
> **Out:** the roadmap crates (`raphtory-cypher`, `db4-*`) that batch it
> into segments — the recurring "arrays beat objects" arc of this path.

The workspace tells you the roadmap: `raphtory-cypher` runs Cypher over
these temporal views (the same "bolt a query language onto a time
model" move as AeonG's `FOR TT`), and `db4-graph` + `db4-storage` are a
newer segmented storage engine:

```rust
// db4-storage/src/segments/edge/segment.rs:57 @ 5d0d286
57  #[derive(Debug)]
58  pub struct MemEdgeSegment {
59      layers: Vec<SegmentContainer<EdgeEntry>>,
60      est_size: usize,
```

It replaces per-entity allocations with segment-grained storage. Why it
matters: the pure event-log model is allocation-heavy at scale for the
same reason Memgraph is (Step 3's per-entity enums are still per-entity
objects); segments are the "batch it into arrays" correction — the
recurring arc of this whole learning path.

## Where each step lives in the code

All paths relative to `~/repos/raphtory` at `5d0d286`. Workspace crates:
`raphtory` (main API), `raphtory-api`, `raphtory-core`,
`raphtory-storage`, `raphtory-cypher`, `raphtory-graphql`, `db4-graph`
+ `db4-storage`.

| Step | Anchor | What to see |
|---|---|---|
| 2 | `raphtory-api/src/core/storage/timeindex.rs:28` | `EventTime(pub i64, pub usize)` — the universal 16-byte key (derive at :27) |
| 3 | `raphtory-core/src/storage/timeindex.rs:13` | `TimeIndex { Empty, One, Set }` — when an entity existed |
| 3 | `raphtory-core/src/entities/properties/tcell.rs:10` | `TCell` enum ladder + the "value in time" comment at :9 |
| 4 | `raphtory-core/src/entities/properties/tprop.rs:22` | `TPropCell { t_cell, log }` — time→offset index + `PropColumn` (`new` at :28) |
| 5 | `raphtory/src/db/graph/views/window_graph.rs:87` | `WindowedGraph{graph, start, end}`, derives `Copy` (at :86) |
| 5 | `raphtory/src/db/api/view/time.rs:116` | `TimeOps::window` decl; `at` at :118; default impls ~:245 |
| 5 | `raphtory-storage/src/graph/edges/edge_storage_ops.rs:110,:140` | `additions_iter` / `additions` — edge existence as an iterator |
| 6 | `db4-storage/src/segments/edge/segment.rs:58` | `MemEdgeSegment` — the newer segmented engine |

Read order: EventTime → TimeIndex → TCell (read the whole enum and its
comment) → TPropCell → WindowedGraph → TimeOps (trace `window` from
declaration to one default impl) → skim a db4 segment. Resist reading
more; these eight anchors are the design.

## Questions (answer in notes.md)

1. M33: what would a `WindowedGraph` over FalkorDB's GraphBLAS matrices
   be? Two timestamps can't lazily filter a dense SpMV — do BETWEEN
   views become masks (topic 20), materialized submatrices, or
   per-operation time predicates, and what does each cost?
2. The experiments crate's `events.rs::replay_at_time` answers AT TIME
   by replaying the log from t = 0. Which Raphtory structures replace
   the replay, and what is the probe cost per node in their terms
   (Step 3)?
3. EventTime's `usize` tiebreaker vs the λ = 0 tie-order stream from Wu
   et al. (exercise 2 in the README): show how a total order on events
   makes the one-pass earliest-arrival deterministic where bare i64
   timestamps aren't.
4. Contrast with Memgraph (topic 13): both end up with per-entity
   small-then-spill collections (small_vector vs TCell's ladder), yet
   one is object-first and one log-first. What query does each answer in
   O(1) that costs the other a scan?
5. Raphtory has no GC question — nothing is ever superseded — but that
   means the log only grows. Steal AeonG's vocabulary: what would an
   "anchor" be in an event-log-first engine, and where would you put it?
   (Hint: your `snapshot.rs` is exactly this hybrid.)

## Done when

Answer each before unfolding it.

- [ ] You can trace `g.window(t1, t2).node(n).properties()` naming the concrete type at each hop.

  <details><summary>Answer</summary>

  `window(t1, t2)` constructs a **`WindowedGraph<G>`** (window_graph.rs:87)
  — the graph handle plus `start`/`end: Option<EventTime>`, `Copy`, nothing
  copied. `.node(n)` yields a node view still carrying those bounds.
  `.properties()` reads through a **`TPropCell`** (tprop.rs:22): it probes
  the node's **`TCell`** timeline (tcell.rs:10) for the newest
  `EventTime ≤ t2` (and `≥ t1`), which is a range query on a per-entity
  index — `TimeIndex`/`TCell` from Step 3 — yielding an `Option<usize>`
  offset, then indexes the shared **`PropColumn`** (`log`) at that offset.
  Time navigation stays in small per-entity structures; only the value
  read touches the dense column.

  </details>

- [ ] You can say which hop `at(t)` on a never-updated node skips, and why it costs nothing.

  <details><summary>Answer</summary>

  A node whose property was set exactly once stores it as
  **`TCell1(EventTime, A)`** (tcell.rs:13) — the value is inline in the
  enum, no `SVM` array and no `BTreeMap`. So `at(t)` skips the
  `TCellN::BTreeMap` `O(log n)` seek entirely: there is one event, it is
  either `≤ t` or not, an `O(1)` inline check with zero allocation. The
  size-adaptive ladder means the common case (power-law: most entities
  have one event) pays nothing.

  </details>

- [ ] You can explain why a BETWEEN view copies zero bytes of the graph.

  <details><summary>Answer</summary>

  `WindowedGraph<G>` (window_graph.rs:86–93) derives `Copy` and holds only
  the wrapped `graph: G` (itself a handle/reference in practice) plus two
  `Option<EventTime>` fields — `start` inclusive, `end` exclusive. A window
  is therefore ~two 16-byte timestamps beside the handle, constructed by
  value; the append-only event log is never duplicated. Filtering happens
  lazily at read time by comparing each event's `EventTime` against the
  bounds (Step 5), so `BETWEEN` is an algebra of cheap wrappers, not a
  materialization.

  </details>

- [ ] You can name the tiebreaker that makes equal-timestamp events reproducible, and where it lives.

  <details><summary>Answer</summary>

  `EventTime(pub i64, pub usize)` (timeindex.rs:28): the second field is a
  per-event `usize` sequence id, and because `Ord` is derived on the tuple
  struct after the i64, comparison is lexicographic — equal timestamps fall
  back to the event id. Every `BTreeSet`/`BTreeMap` in the engine
  (`TimeIndex::Set`, `TCellN`) keys on `EventTime`, so all of them order
  colliding events identically. That is the type-system answer to Wu et
  al.'s λ = 0 tie-order requirement (reading-temporal-paths.md): the
  one-pass algorithms need a deterministic order among same-`t` contacts,
  and Raphtory makes bare i64 collisions impossible to observe.

  </details>

## References

**Code**
- [Raphtory](https://github.com/Pometry/Raphtory) — cloned at
  `~/repos/raphtory`, pinned `5d0d286` (resources/codebases.md); the eight
  anchors above are the read. Re-verify any anchor with
  `python3 tools/pinned-source.py show raphtory <path> -r A:B`.
- [memgraph](https://github.com/memgraph/memgraph) — topic 13 clone; the
  object-first pole to hold this against.
- This topic's `experiments/src/events.rs` (`replay_at_time`) and
  `snapshot.rs` — the naive and anchor+delta baselines Raphtory's indexes
  replace.

**Related guides**
- [reading-aeong.md](reading-aeong.md) — the object-first counterpoint
  built on Memgraph.
- [reading-temporal-paths.md](reading-temporal-paths.md) — Wu et al.'s
  one-pass algorithms, whose tie-order subtlety EventTime solves in the
  type system.
