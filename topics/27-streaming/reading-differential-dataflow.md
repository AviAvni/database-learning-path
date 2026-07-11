# Differential dataflow: retractions that survive recursion

Differential dataflow is the system that made incremental computation
work *inside* iteration: deltas carry lattice timestamps, so deleting
an input edge correctly retracts everything derived through it, round
by round. This chapter reads the short CIDR '13 paper alongside the
modern Rust code — arrangements, join_traces, iterate — which our
topic-27 stubs are simplified excerpts of.

## 1. The delta discipline

A differential Collection is a stream of `(data, time, diff)` updates —
our Z-set entries with a timestamp attached. Every operator consumes and
produces *updates only*; the "current collection" never materializes
except inside arrangements. Consolidation
(`consolidation.rs:24 consolidate`, `:88 consolidate_updates`) is our
`ZSet::from_updates` verbatim: sort, sum diffs, drop zeros.

## 2. Arrangements — the indexed update log

`arrange` (operators/arrange/arrangement.rs:311, core at :336) turns an
update stream into an `Arranged` (:45): a **trace** = LSM-of-batches of
(key, val, time, diff), shared by reference among all operators that need
that index. This is the topic-4 rhyme made literal:

```
  batch     = immutable sorted run of updates       (an SST)
  spine     = the merging hierarchy of batches      (leveled compaction)
  advance   = "no reader needs times < f anymore":
              times collapse, diffs consolidate      (tombstone GC below
              — the WEIGHT-level merge               the horizon)
```

**Q1.** Two queries join against the same collection on the same key.
In postgres you'd build one index used by two plans. What is the
differential equivalent, and why does Materialize describe arrangement
sharing as its main memory optimization?

## 3. join_traces — the bilinear rule with fuel

`join_traces` (operators/join.rs:69): each input is arranged; when a new
batch of A arrives, join it against B's *trace* (all of B's history up to
the frontier), and vice versa — exactly our stub's ΔA⋈B + A⋈ΔB + ΔA⋈ΔB,
with the cross term handled by batch/trace ordering. The `Deferred` state
(:311) and the `work`/`fuel` loop (:348, effort accounting :355-395) are
the production detail our stub skips: a huge delta must not stall the
worker, so join work is metered and yields — cooperative scheduling at
the operator level (topic 7's lesson, again).

**Q2.** Our `IncrementalJoin::step` integrates deltas into state *after*
emitting. join_traces must pick an order too: a batch of A joins B's
trace *as of which frontier*? Work out why getting this wrong
double-counts the ΔA⋈ΔB term.

## 4. Iteration — where differential earns its name

`iterate` (operators/iterate.rs:192 `Variable`, `set` :262) runs a loop
body inside a nested scope; updates carry (outer, round) timestamps. The
magic our insert-only reach.rs cannot do: when an *input* edge is deleted,
differential re-derives only the (round, edge)-dependent updates, because
each derived fact is stored with the full lattice time at which it held.
Deletion of an edge retracts facts derived *through* it at round r, which
may re-derive at round r+2 via another path — all handled by the same
consolidation arithmetic, no support counting, no over-deletion bug.

`examples/bfs.rs:101-107` is the whole algorithm:

```rust
nodes.iterate(|inner| {
    inner.join_map(&edges, |_k, l, d| (*d, l + 1))   // relax
         .concat(&nodes)                              // keep roots
         .reduce(...min...)                           // keep shortest
})
```

**Q3.** Semi-naive evaluation falls out: at round r+1, the join only sees
*diffs* at round r. Verify against our reach.rs relaxation counter: what
does differential's per-round diff discipline guarantee that our
"BFS from new frontier" hand-rolls?

**Q4 (the hard one).** Why does incremental recursion need the *lattice*
(product partial order) rather than a total order? Construct the case:
input change at epoch 2 while iteration from epoch 1 is still running —
which updates must NOT be merged?

## 5. Tie back to the stubs

Our three stubs are differential with the general machinery deleted:
`delta_join` = join_traces without times/fuel; `IncrementalTriangles` =
a 3-way delta join specialized by hand; `SemiNaiveReach` = iterate for
monotone inserts only. The point of reading the real thing is to see
*what the generality costs* (arrangements, lattice times, compaction) and
what it buys (retractions inside recursion — the thing none of our stubs
can do).

## References

**Papers**
- McSherry, Murray, Isaacs, Isard — "Differential Dataflow"
  (CIDR 2013) — short; read all of it, twice

**Code**
- [differential-dataflow](https://github.com/TimelyDataflow/differential-dataflow)
  `differential-dataflow/src/` — `consolidation.rs`,
  `operators/arrange/arrangement.rs`, `operators/join.rs`,
  `operators/iterate.rs`; plus `examples/bfs.rs` — 40 lines that do
  what our reach.rs stub cannot
