# Naiad: the clock that unified batch, streaming, and iteration

Naiad's timely dataflow is one low-level model that expresses batch,
streaming, AND incremental iterative computation — and the only new
mechanism it needs is a smarter clock. This chapter builds that clock
step by step — dataflow, logical time, the completeness problem, the
progress protocol, and what loops do to all of it — then hands you a
reading route through the SOSP '13 paper and its Rust reincarnation
(timely-dataflow, by the same author), the substrate differential
dataflow builds on.

## The problem in one sentence

A streaming operator computing "count per hour" may only emit hour t's
final answer once the system can *prove* no more hour-t records will
ever arrive — and the moment the dataflow contains a loop (iteration),
"no more messages ≤ t" stops even being a statement about a totally
ordered clock.

## The concepts, step by step

### Step 1 — dataflow: the program is a graph, data does the moving

> **In:** a computation you want to run in parallel and incrementally.
> **Out:** a directed graph of stateful operators connected by channels —
> records flow in at sources and out at sinks, with no global controller,
> so parallelism and incremental re-execution both fall out of the shape.

A dataflow system represents a computation as a directed graph of
**operators** (small stateful functions: map, join, count) connected by
channels; input records flow in at sources and results flow out at
sinks, with no global controller sequencing the work. Why this shape:
each operator can run on any worker/thread, parallelism falls out of
partitioning the channels by key, and — crucial for this topic —
*incremental* computation falls out of sending only *changed* records
through the same graph. In 2013 the landscape was fractured: batch
systems (MapReduce/Spark) could iterate but not stream; streaming
systems (Storm) could stream but not iterate; nothing could do
incremental *iterative* computation. Naiad's claim: one dataflow model
covers all three, if the messages carry the right notion of time.

### Step 2 — logical timestamps: every message says which batch it belongs to

> **In:** messages flowing through the graph, processed out of order for
> speed. **Out:** each message tagged with a **logical timestamp** — the
> epoch it derives from (§2.1) — so "results for epoch 7" stays a
> well-defined set even while epochs 8 and 9 are in flight.

In timely dataflow every message carries a **logical timestamp** — not a
wall-clock time but a coordinate naming the unit of input it derives
from, starting with the **epoch** (which round of input the external
source injected: batch 0, batch 1, ...). Operators are free to process
messages out of timestamp order — that's what makes the system fast —
but every derived message keeps (or extends) the timestamp of what it
was derived from. The timestamp is the bookkeeping that makes "results
for epoch 7" a well-defined set even while epochs 8 and 9 are already
in flight.

### Step 3 — the completeness problem: frontiers

> **In:** an operator (count, min) that must not emit epoch t's final
> answer while a message with timestamp ≤ t might still arrive.
> **Out:** the **frontier** — the proven statement "no message with
> timestamp ≤ t will ever arrive at this input" — computed, not guessed.

An operator like count or min cannot emit a *final* answer for time t
while any message with timestamp ≤ t might still arrive — emitting early
means emitting wrong. The system-wide statement "no message with
timestamp ≤ t will ever arrive at this operator input" is called the
**frontier** (elsewhere: watermark), and Naiad's core contribution is
computing it as a *proof*, not a guess. This single mechanism subsumes
batch boundaries (a batch job is one epoch whose frontier passes at
end-of-input), out-of-order data, iteration rounds, and exactly-once
output (emit per closed timestamp). Contrast the industry norm: Flink /
MillWheel-style watermarks are *heuristics* ("probably no events older
than t−5s") that can be violated by stragglers; timely frontiers cannot.

### Step 4 — the protocol: could-result-in, and two counts per pointstamp

> **In:** the set of unprocessed events in the running dataflow.
> **Out:** the frontier, computed by tracking, per active **pointstamp**,
> an *occurrence count* (outstanding events) and a *precursor count*
> (active pointstamps that could-result-in it); a pointstamp is in the
> frontier exactly when its precursor count is zero (§2.3).

The frontier is computed from **pointstamps** — a pointstamp is a
`(timestamp t, location l ∈ Edge ∪ Vertex)` pair naming a place-and-time
where an event exists or could still be produced (Naiad §2.3). The graph
induces an order: pointstamp `(t1,l1)` **could-result-in** `(t2,l2)` iff
some path ψ through the graph adjusts t1 (via ingress/egress/feedback)
so that `Ψ[l1,l2](t1) ≤ t2` — i.e. a message at `(t1,l1)` could cause one
at `(t2,l2)`. The scheduler keeps a set of **active** pointstamps and,
for each, **two** counts (this is the part a plain refcount misses):

- an **occurrence count** — how many outstanding events bear the
  pointstamp. Updated by the vertex methods: `SENDBY` +1, `ONRECV` −1,
  `NOTIFYAT` +1, `ONNOTIFY` −1. A pointstamp leaves the active set when
  its occurrence count hits zero.
- a **precursor count** — how many active pointstamps precede it in the
  could-result-in order. When p becomes active its precursor count is
  initialized to the number of active pointstamps that could-result-in
  it, and it increments the precursor count of everything it
  could-result-in; when p leaves, it decrements them.

A pointstamp is **in the frontier** precisely when its *precursor count
is zero* — "there is no other pointstamp in the active set that
could-result-in p" (§2.3) — and only then may its notification be
delivered. The occurrence count decides when a pointstamp is done; the
precursor count decides when it is *safe*. The `MutableAntichain`
(`frontier.rs:380`, `update_iter` :533) is the code that maintains this
and reports which minimal times appeared or vanished:

```rust
// ILLUSTRATION — the frontier-advance step, sketched; the real
// count arithmetic is timely progress/frontier.rs:533 (update_iter)
// over the (time, ±count) buffer progress/change_batch.rs:16.
fn apply(counts: &mut BTreeMap<Time, i64>, changes: &[(Time, i64)])
    -> Vec<Time> {                        // returns times the frontier passed
    let before = frontier(counts);        // minimal times with count > 0
    for &(t, delta) in changes {          // produced: +1, consumed: -1 —
        *counts.entry(t).or_insert(0) += delta;   // may dip negative, sums safe
        if counts[&t] == 0 { counts.remove(&t); }
    }
    let after = frontier(counts);
    before.into_iter()                    // t left the frontier ⇒ PROVEN:
        .filter(|t| after.iter().all(|f| !(f <= t)))   // nothing ≤ t can
        .collect()                        //   ever arrive — finalize t
}
```

Note the tolerance for disorder: counts may transiently go negative
(a consume heard before its produce), and the protocol stays safe
because only *sums* matter — question 3 below chases the invariant.

### Step 5 — loops: timestamps become tuples, order becomes partial

> **In:** an iterative computation — a cycle in the dataflow graph that
> would make could-result-in circular. **Out:** timestamps extended to
> `(epoch, loop counters…)` by ingress/egress/feedback vertices (§2.1),
> so order becomes a partial order (a lattice) and the frontier becomes
> an antichain.

Iteration is a cycle in the dataflow graph, and a cycle would deadlock
the could-result-in analysis — everything could result in everything.
Naiad's fix (§2.1): entering a loop *pushes a new counter onto the
timestamp* (ingress appends a `0`), each trip around the loop's feedback
edge increments the last counter, and exiting (egress) pops it:

```
  timestamp in Naiad:  (epoch, loop1_counter, loop2_counter, ...)
                        ^ input batch  ^ iteration rounds, one per nested loop
  partial order: pointwise ≤   — this lattice is what "differential" will
                                 exploit for incremental iteration
```

Two timestamps now compare **pointwise** — (1, 5) ≤ (2, 6), but (1, 5)
and (2, 3) are *incomparable* — so time forms a **lattice** (a partial
order where any two elements have a least upper bound), not a line, and
a frontier is an *antichain* (a set of mutually incomparable minimal
times) rather than a single number. The payoff: round 5 of epoch 1 and
round 2 of epoch 2 can be in flight simultaneously, correctly — the
property differential dataflow will exploit for incremental iteration.

### Step 6 — what timely deliberately is not

> **In:** the expectation that a dataflow engine manages state, windows,
> and retractions. **Out:** timely does *none* of that — it only moves
> data and proves frontiers; everything database-shaped lives one layer
> up, in differential dataflow.

Timely only moves data and proves frontiers — there is no state
management, no retractions, no windows in the substrate. That division
of labor is the design: everything database-shaped (indexed state,
weighted deltas, incremental operators) lives a layer up in differential
dataflow ([reading-differential-dataflow.md](reading-differential-dataflow.md)),
built out of nothing but timely operators plus the frontier guarantee.
When you read the code and wonder where the tables are — that's the
point.

### Step 7 — the database rosetta

> **In:** every timely concept built up so far. **Out:** its database
> twin — epoch ≈ transaction id, frontier ≈ completed-snapshot watermark,
> could-result-in ≈ dependency tracking for safe truncation — so reading
> timely as a database person is mostly translation.

Every concept above has a database twin — reading timely as a database
person is mostly translation:

| timely concept | database concept |
|---|---|
| timestamp/epoch | transaction id / batch boundary |
| frontier passes t | watermark: txn t's snapshot is complete |
| could-result-in | dependency tracking for safe truncation |
| loop counter coordinate | recursive CTE iteration depth |
| `step()` cooperative scheduling | topic 7's event loop, one layer up |

## How to read the paper (with the concepts in hand)

- **§2 — read fully.** The model: Steps 1–3 and 5 in the authors'
  words. §2.1 gives the loop timestamp rules (Step 5); §2.3
  ("Achieving timely dataflow") is the pointstamp / could-result-in /
  occurrence-and-precursor-count protocol of Step 4 — keep the `apply`
  sketch above next to the prose.
- **§3 — read carefully.** The distributed implementation; §3.3
  ("Distributed progress tracking") is how the counts survive reordering
  across workers (question 3's sums invariant lives here).
- **Eval — skim.** 2013 cluster numbers; the model is the payload.

Then the Rust reincarnation — the code anchors, by step:

| anchor | step | what it is |
|---|---|---|
| `progress/change_batch.rs:16` `ChangeBatch` | 4 | the (time, ±count) buffer — progress updates are themselves Z-set-shaped |
| `progress/frontier.rs:380` `MutableAntichain` | 4–5 | the frontier: minimal elements of outstanding times; `update_iter` :533 applies count changes and reports which minimal times appeared/vanished |
| `progress/reachability.rs` | 4 | the static could-result-in analysis over the dataflow graph |
| `progress/subgraph.rs` | 5 | scopes: nested dataflow whose inner timestamp adds a coordinate |
| `worker.rs:235` `step` | 1, 6 | the whole runtime: drain channels, schedule operators, exchange progress — cooperative, no threads-per-operator |

## Questions to answer in notes.md

1. Why must loop ingress/egress/feedback nodes edit the timestamp (push a
   counter, pop it, increment it)? Show that without the feedback
   increment, could-result-in has a cycle and no frontier ever advances.
2. `MutableAntichain` keeps counts per time and exposes only the
   *antichain* of minimal ones. Why antichain and not the full set? (What
   query do operators actually ask — and how does this echo topic 8's
   "oldest active txn" watermark for vacuum?)
3. Progress messages are counts that may go negative transiently (consume
   before the produce is heard). Why is the protocol still safe — what
   invariant over SUMS keeps it correct? Naiad states the distributed
   property in §3.3; the formal safety proof is in the companion technical
   report, not the SOSP paper. (Same shape as escrow /
   commutative-counter arguments in topic 29's world.)
4. Kafka Streams / Flink watermarks are *heuristic* ("probably no events
   older than t-5s"); timely frontiers are *proofs*. What does each buy?
   Where does FalkorDB's single-writer serialization make the proof
   trivial? (That's why M27 can skip most of §3.3's distributed
   machinery.)

## Done when

Answer each before unfolding it.

- [ ] What does a logical timestamp say about a message?
  <details><summary>answer</summary>

  Not wall-clock time but the unit of input the message derives from —
  the epoch (which round of input a source injected), extended with loop
  counters inside iteration. It makes "results for epoch t" a
  well-defined set even while later epochs are in flight.

  </details>
- [ ] State the completeness problem and what a frontier answers.
  <details><summary>answer</summary>

  A count/min operator must not emit epoch t's final answer while a
  message with timestamp ≤ t could still arrive. The frontier is the
  proven statement "no message with timestamp ≤ t will ever arrive at
  this input," so the operator knows when it may finalize t.

  </details>
- [ ] Explain could-result-in and progress tracking — and why one count isn't enough.
  <details><summary>answer</summary>

  `(t1,l1)` could-result-in `(t2,l2)` iff a graph path adjusts t1 to
  `Ψ[l1,l2](t1) ≤ t2`. Each active pointstamp carries an occurrence count
  (outstanding events) and a precursor count (active pointstamps ahead of
  it in could-result-in order). A pointstamp is in the frontier when its
  precursor count is zero — occurrence count alone can't tell you it is
  safe (§2.3).

  </details>
- [ ] Why must loop nodes edit the timestamp, and what does order become inside a loop?
  <details><summary>answer</summary>

  Ingress appends a loop coordinate `0`, feedback increments the last
  coordinate, egress pops it (§2.1). Without the feedback increment,
  could-result-in has a cycle and no frontier ever advances. Inside a
  loop, timestamps compare pointwise — a partial order (lattice) — and
  the frontier is an antichain, not a single number.

  </details>
- [ ] Why can progress counts transiently go negative, and why is that safe?
  <details><summary>answer</summary>

  A consume (−1) can be observed before the matching produce (+1) under
  reordering, so a per-time count can dip below zero. Only the *sums*
  determine the frontier and they converge to the true occurrence counts,
  so no frontier is ever advanced prematurely — the distributed property
  Naiad states in §3.3.

  </details>
- [ ] Contrast this with heuristic watermarks and say what each guarantees.
  <details><summary>answer</summary>

  Flink/MillWheel watermarks are heuristics ("probably nothing older than
  t−5s") that stragglers can violate; timely frontiers are proofs derived
  from could-result-in and cannot be wrong. Heuristics buy low latency
  with occasional error; frontiers buy exactness at the cost of
  coordination.

  </details>
- [ ] You wrote answers to all questions in notes.md.
  <details><summary>answer</summary>

  Including where FalkorDB's single-writer serialization makes the frontier
  proof trivial (one clock, one writer — most of §3.3's distributed
  machinery is unnecessary for M27).

  </details>

## References

**Papers**
- Murray, McSherry, Isaacs, Isard, Barham, Abadi — "Naiad: A Timely
  Dataflow System" (SOSP 2013) — read §2 fully (the model), §3
  (distributed implementation, esp. §3.3 progress tracking) carefully,
  skim eval

**Code**
- [timely-dataflow](https://github.com/TimelyDataflow/timely-dataflow)
  `timely/src/` — `progress/change_batch.rs`, `progress/frontier.rs`
  (:380 `MutableAntichain`), `progress/reachability.rs`,
  `progress/subgraph.rs`, `worker.rs` (:235 `step`)
