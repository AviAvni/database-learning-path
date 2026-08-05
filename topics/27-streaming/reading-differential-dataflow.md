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
surviving paths — without falling back to the 31.2 ms full re-BFS our
insert-only stub would need (this topic's measured reachability lane —
`../../FINDINGS.md` row 27 / `README.md` "The problem, measured").

## The concepts, step by step

### Step 1 — the delta discipline: streams of weighted, timestamped updates

> **In:** a changing collection (a table under inserts, deletes,
> updates). **Out:** a stream of `(data, time, diff)` updates whose
> implicit collection at time t is the sum of all updates at times ≤ t —
> kept canonical by consolidation (sort, sum diffs, drop zeros).

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

> **In:** a stream of updates that operators need to look up by key.
> **Out:** an `Arranged` collection whose trace is an LSM-of-batches
> index of `(key, val, time, diff)`, shared by reference across every
> operator that reads that key, and compacted against the frontier.

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

> **In:** two arranged, changing inputs A and B. **Out:** the output
> delta ΔA⋈B + A⋈ΔB + ΔA⋈ΔB, computed by joining each new batch against
> the *other* input's trace — work metered by a fuel loop so a large
> delta never stalls the worker.

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

> **In:** a recursive loop body (e.g. BFS relaxation) over changing
> input. **Out:** every derived fact stamped with an **(outer, round)**
> lattice time, so deleting an input edge retracts exactly the
> round-and-epoch-dependent facts it produced — no support counting.

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

`examples/bfs.rs:98-109` is the whole algorithm (real code, pinned at
`3f279da` — the closure takes `(scope, inner)`, and the final `reduce`
keeps the minimum distance, it is not a `...min...` placeholder):

```rust
// differential-dataflow/examples/bfs.rs @3f279da
98      let nodes = roots.map(|x| (x, 0));
101     nodes.clone().iterate(|scope, inner| {
103         let nodes = nodes.enter(scope);
104         let edges = edges.enter(scope);
106         inner.join_map(edges, |_k,l,d| (*d, l+1))   // relax: one hop
107              .concat(nodes)                          // keep roots
108              .reduce(|_, s, t| t.push((*s[0].0, 1))) // keep shortest
109      })
```

### Step 5 — semi-naive evaluation falls out for free

> **In:** a recursive query run round by round. **Out:** semi-naive
> behavior — each round joins only the *newly derived* diffs against the
> full relation — with no special code, because unchanged facts emit no
> updates.

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

> **In:** the topic's three stubs (delta_join, IncrementalTriangles,
> SemiNaiveReach) — differential with the general machinery deleted.
> **Out:** a clear ledger of what the real system pays (arrangements,
> lattice times, compaction) and what that buys (retractions inside
> recursion — the one thing the stubs cannot do).

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
| `examples/bfs.rs:98-109` | 4–5 | 12 lines that do what our reach.rs stub cannot |

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

## Done when

Answer each before unfolding it.

- [ ] Explain the delta discipline: weighted, timestamped updates.
  <details><summary>answer</summary>

  A collection is a stream of `(data, time, diff)` updates; the collection
  at time t is the sum of all updates at times ≤ t and never materializes
  except inside arrangements. One consolidation path (sort, sum diffs,
  drop zeros) handles inserts, deletes, and updates uniformly.

  </details>
- [ ] What is an arrangement, and why does sharing one across queries matter?
  <details><summary>answer</summary>

  An arrangement is an indexed, compacted trace (an LSM of `(key, val,
  time, diff)` batches) built by `arrange`. It is shared by reference, so
  two queries joining the same collection on the same key reuse one
  index instead of each building its own — Materialize's main memory win.

  </details>
- [ ] Explain the incremental join as the bilinear rule on traces, and what "fuel" is for.
  <details><summary>answer</summary>

  `join_traces` computes ΔA⋈B + A⋈ΔB + ΔA⋈ΔB by joining each new batch
  against the other input's trace up to the frontier. Fuel meters the
  work (`Deferred` state, effort accounting) so a huge delta yields
  cooperatively instead of stalling the worker.

  </details>
- [ ] Why are lattice timestamps *required* for retractable recursion, not merely convenient?
  <details><summary>answer</summary>

  Each derived fact carries an (outer-epoch, round) time. Deleting an
  input edge must retract exactly the facts derived through it at each
  round while facts re-derived by surviving paths persist. A total order
  can't keep a mid-flight iteration from epoch 1 separate from a new
  change at epoch 2; the product order can, so retractions stay exact.

  </details>
- [ ] Show how semi-naive evaluation falls out for free.
  <details><summary>answer</summary>

  At round r+1 the join's inputs are exactly the diffs produced at round
  r, because unchanged facts emit no updates. So the "join only the new
  facts" discipline is a consequence of the update representation, not a
  hand-written optimization.

  </details>
- [ ] You wrote answers to all questions in notes.md, including the ordering issue in `IncrementalJoin::step`.
  <details><summary>answer</summary>

  The batch of A must join B's trace as of the frontier *before* B's
  matching delta is folded in (and vice versa); fold both first and the
  ΔA⋈ΔB cross term is counted twice. Record the correct order and why.

  </details>

## References

**Papers**
- McSherry, Murray, Isaacs, Isard — "Differential Dataflow"
  (CIDR 2013) — short; read all of it, twice

**Code**
- [differential-dataflow](https://github.com/TimelyDataflow/differential-dataflow)
  `differential-dataflow/src/` — `consolidation.rs`,
  `operators/arrange/arrangement.rs`, `operators/join.rs`,
  `operators/iterate.rs`; plus `examples/bfs.rs` — a dozen lines that do
  what our reach.rs stub cannot
