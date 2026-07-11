# Naiad: the clock that unified batch, streaming, and iteration

Naiad's timely dataflow is one low-level model that expresses batch,
streaming, AND incremental iterative computation — and the only new
mechanism it needs is a smarter clock. This chapter reads the SOSP '13
paper's progress-tracking protocol, then its Rust reincarnation
(timely-dataflow, by the same author), which is the substrate
differential dataflow builds on.

## 1. What problem Naiad actually solved

2013's landscape: batch systems (MapReduce/Spark) could iterate but not
stream; streaming systems (Storm) could stream but not iterate; nothing
could do *incremental iterative* computation. Naiad's claim: ONE
low-level model — timely dataflow — expresses all three, and the only new
mechanism needed is a smarter clock.

```
  timestamp in Naiad:  (epoch, loop1_counter, loop2_counter, ...)
                        ^ input batch  ^ iteration rounds, one per nested loop
  partial order: pointwise ≤   — this lattice is what "differential" will
                                 exploit for incremental iteration
```

## 2. The core protocol: could-result-in

An operator may only finalize output for time t when the system proves no
message with timestamp ≤ t can ever arrive. Naiad §3.2: track, per
(location, timestamp), counts of outstanding *pointstamps*; a pointstamp
is in the frontier when no other could-result-in it. Every produced or
consumed message decrements/increments counts — progress is just a
distributed refcount over the lattice.

**Q1.** Why must loop ingress/egress/feedback nodes edit the timestamp
(push a counter, pop it, increment it)? Show that without the feedback
increment, could-result-in has a cycle and no frontier ever advances.

The frontier advance, mechanically — progress is count arithmetic:

```rust
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

## 3. timely code anchors

| anchor | what it is |
|---|---|
| `progress/change_batch.rs:16` `ChangeBatch` | the (time, ±count) buffer — progress updates are themselves Z-set-shaped |
| `progress/frontier.rs:380` `MutableAntichain` | the frontier: minimal elements of outstanding times; `update_iter` :533 applies count changes and reports which minimal times appeared/vanished |
| `progress/reachability.rs` | the static could-result-in analysis over the dataflow graph |
| `progress/subgraph.rs` | scopes: nested dataflow whose inner timestamp adds a coordinate |
| `worker.rs:235` `step` | the whole runtime: drain channels, schedule operators, exchange progress — cooperative, no threads-per-operator |

Note what is NOT here: no state management, no retractions, no windows.
Timely only moves data and proves frontiers. Everything database-shaped
lives a layer up in differential.

**Q2.** `MutableAntichain` keeps counts per time and exposes only the
*antichain* of minimal ones. Why antichain and not the full set? (What
query do operators actually ask — and how does this echo topic 8's
"oldest active txn" watermark for vacuum?)

**Q3.** Progress messages are counts that may go negative transiently
(consume before the produce is heard). Why is the protocol still safe —
what invariant over SUMS does Naiad §4.1 prove? (Same shape as escrow /
commutative-counter arguments in topic 29's world.)

## 4. The database rosetta

| timely concept | database concept |
|---|---|
| timestamp/epoch | transaction id / batch boundary |
| frontier passes t | watermark: txn t's snapshot is complete |
| could-result-in | dependency tracking for safe truncation |
| loop counter coordinate | recursive CTE iteration depth |
| `step()` cooperative scheduling | topic 7's event loop, one layer up |

**Q4.** Kafka Streams / Flink watermarks are *heuristic* ("probably no
events older than t-5s"); timely frontiers are *proofs*. What does each
buy? Where does FalkorDB's single-writer serialization make the proof
trivial? (That's why M27 can skip most of §4.)

## References

**Papers**
- Murray, McSherry, Isaacs, Isard, Barham, Abadi — "Naiad: A Timely
  Dataflow System" (SOSP 2013) — read §1-3 fully (the model), §4
  (distributed progress) carefully, skim eval

**Code**
- [timely-dataflow](https://github.com/TimelyDataflow/timely-dataflow)
  `timely/src/` — `progress/change_batch.rs`, `progress/frontier.rs`
  (:380 `MutableAntichain`), `progress/reachability.rs`,
  `progress/subgraph.rs`, `worker.rs` (:235 `step`)
