# Differential dataflow: retractions that survive recursion

Differential dataflow is the system that made incremental computation
work *inside* iteration: deltas carry lattice timestamps, so deleting
an input edge correctly retracts everything derived through it, round
by round. This chapter builds the machinery step by step — timestamped
deltas, arrangements, the incremental join, and the lattice trick that
makes recursion retractable — then maps each step onto the short
CIDR '13 paper and the modern Rust code (arrangements, join_traces,
iterate) that our topic-27 stubs are simplified excerpts of.

## The problem in one sentence

Delete one edge from a 500K-edge graph and a maintained reachability
view must retract every fact derived *through* that edge — across
however many BFS rounds derived them, while other facts re-derive via
surviving paths — without falling back to the 24.7 ms full re-BFS our
insert-only stub would need.

## The concepts, step by step

### Step 1 — the delta discipline: streams of weighted, timestamped updates

A differential **Collection** is not a table — it is a stream of
`(data, time, diff)` updates: the record, the logical timestamp it
changed at (Naiad's lattice time, from the timely guide), and an integer
weight (`+1` insert, `−1` delete — our Z-set weights with a timestamp
attached). Every operator consumes and produces *updates only*; the
"current collection" at time t exists only implicitly, as the sum of all
updates at times ≤ t, and never materializes except inside arrangements
(Step 2). The one primitive that keeps this representation canonical is
**consolidation**: sort updates, sum the diffs of identical
(data, time) pairs, drop zeros — `consolidation.rs:24 consolidate`,
`:88 consolidate_updates`, our `ZSet::from_updates` verbatim. Why it
matters: a deletion is just more data, so *one* code path handles
inserts, deletes, and updates — no per-operator retraction logic.

### Step 2 — arrangements: the indexed update log, shared and compacted

Operators like join need to look up "all updates for key k" — so
differential builds **arrangements**: `arrange`
(operators/arrange/arrangement.rs:311, core at :336) turns an update
stream into an `Arranged` (:45), whose **trace** is an LSM-of-batches of
(key, val, time, diff), shared *by reference* among every operator that
needs that index. This is the topic-4 rhyme made literal:

```
  batch     = immutable sorted run of updates       (an SST)
  spine     = the merging hierarchy of batches      (leveled compaction)
  advance   = "no reader needs times < f anymore":
              times collapse, diffs consolidate      (tombstone GC below
              — the WEIGHT-level merge               the horizon)
```

Two things to hold: an arrangement is built once and shared (two queries
joining the same collection on the same key reuse one trace — the
"build one index, use it in many plans" move, and Materialize's main
memory optimization), and it is *compacted against the frontier* — once
timely proves no reader needs times before f, distinct historical times
collapse and their diffs consolidate, bounding state.

### Step 3 — the incremental join: the bilinear rule on traces, with fuel

The join of two changing inputs updates by the product rule — new output
= ΔA⋈B + A⋈ΔB + ΔA⋈ΔB — and `join_traces` (operators/join.rs:69) is
that rule executed against arrangements: each input is arranged; when a
new batch of A arrives, join it against B's *trace* (all of B's history
up to the frontier), and vice versa — exactly our stub's three terms,
with the cross term ΔA⋈ΔB handled by careful batch/trace ordering
(question 2 makes you find why the wrong order double-counts it). The
production detail our stub skips: the `Deferred` state (:311) and the
`work`/`fuel` loop (:348, effort accounting :355-395) — a huge delta
must not stall the worker, so join work is metered and yields.
Cooperative scheduling at the operator level: topic 7's lesson, again.

### Step 4 — iteration: lattice timestamps make recursion retractable

This is where differential earns its name. `iterate`
(operators/iterate.rs:192 `Variable`, `set` :262) runs a loop body
inside a nested scope where every update carries an **(outer, round)**
timestamp — which input epoch it belongs to *and* which iteration round
derived it. Because each derived fact is stored with the full lattice
time at which it held, deleting an input edge retracts exactly the
(round, edge)-dependent updates: facts derived through the edge at round
r get −1s at round r, may re-derive at round r+2 via another path — and
it is all the *same consolidation arithmetic* from Step 1. No support
counting, no over-deletion bug — the two failure modes every hand-rolled
incremental-recursion scheme hits. This is the machinery our insert-only
`reach.rs` deliberately lacks (the topic README's scope cut).

`examples/bfs.rs:101-107` is the whole algorithm:

```rust
nodes.iterate(|inner| {
    inner.join_map(&edges, |_k, l, d| (*d, l + 1))   // relax
         .concat(&nodes)                              // keep roots
         .reduce(...min...)                           // keep shortest
})
```

### Step 5 — semi-naive evaluation falls out for free

Semi-naive evaluation — the classic Datalog optimization of joining only
the *newly derived* facts against the full relation each round, instead
of re-joining everything — is not implemented anywhere in differential;
it *falls out*: at round r+1 the join's input updates are exactly the
diffs at round r, because unchanged facts have no updates to send. Our
`reach.rs` hand-rolls the same discipline as "BFS from the new frontier
only" and enforces it with a relaxation counter (≤ 4 relaxations per
edge across ALL batches); differential gets the guarantee from the
representation itself — question 3 asks you to line the two up.

### Step 6 — what the generality costs, and what it buys

Our three stubs are differential with the general machinery deleted:
`delta_join` = join_traces without times/fuel; `IncrementalTriangles` =
a 3-way delta join specialized by hand; `SemiNaiveReach` = iterate for
monotone inserts only. The point of reading the real thing is to see
*what the generality costs* — arrangements to maintain, lattice
timestamps on every update, compaction machinery — and what it buys:
retractions inside recursion, the one thing none of our stubs can do,
and the reason "delete an edge from a reachability view" is a solved
problem here and an open one in most hand-built IVM systems.

## Where each step lives in the code

[differential-dataflow](https://github.com/TimelyDataflow/differential-dataflow)
`differential-dataflow/src/`:

| anchor | step | what it is |
|---|---|---|
| `consolidation.rs:24` `consolidate`, `:88` `consolidate_updates` | 1 | sort, sum diffs, drop zeros — our `ZSet::from_updates` verbatim |
| `operators/arrange/arrangement.rs:311` (core :336), `Arranged` :45 | 2 | update stream → shared trace (LSM of batches) |
| `operators/join.rs:69` `join_traces`; `Deferred` :311; fuel :348, :355-395 | 3 | the bilinear rule against traces, work-metered |
| `operators/iterate.rs:192` `Variable`, `set` :262 | 4 | nested scope, (outer, round) timestamps |
| `examples/bfs.rs:101-107` | 4–5 | 40 lines that do what our reach.rs stub cannot |

Paper route: the CIDR '13 paper is short — read all of it, twice. First
pass after Steps 1–3 (collections, arrangements as "indexed
differences"); second pass after Step 4, when the lattice-timestamp
section stops reading like notation and starts reading like the fix to a
bug you can now name.

## Questions to answer in notes.md

1. Two queries join against the same collection on the same key. In
   postgres you'd build one index used by two plans. What is the
   differential equivalent, and why does Materialize describe arrangement
   sharing as its main memory optimization?
2. Our `IncrementalJoin::step` integrates deltas into state *after*
   emitting. join_traces must pick an order too: a batch of A joins B's
   trace *as of which frontier*? Work out why getting this wrong
   double-counts the ΔA⋈ΔB term.
3. Semi-naive evaluation falls out: at round r+1, the join only sees
   *diffs* at round r. Verify against our reach.rs relaxation counter:
   what does differential's per-round diff discipline guarantee that our
   "BFS from new frontier" hand-rolls?
4. **(the hard one)** Why does incremental recursion need the *lattice*
   (product partial order) rather than a total order? Construct the case:
   input change at epoch 2 while iteration from epoch 1 is still running —
   which updates must NOT be merged?

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
