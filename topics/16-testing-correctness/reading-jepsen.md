# Reading guide — Jepsen & elle

Sources: jepsen.io/analyses — read TWO: "Redis-Raft 1b3fbf6"
(2020) and a graph one, "Dgraph 1.0.2" (2018). Plus the elle paper:
"Elle: Inferring Isolation Anomalies from Experimental Observations"
(VLDB '20) and github.com/jepsen-io/elle.

## The method

Jepsen is black-box and brutal: real cluster, real network, real
clients.

```
 generators → concurrent client ops (read/write/cas/txn)
            → against a REAL cluster
            → while nemesis injects: partitions, clock skew,
              process kills/pauses (SIGSTOP = the GC-pause stand-in)
            → record HISTORY: [{op, start, end, result}, ...]
            → checker: is this history linearizable / serializable?
```

The checker is the hard part. Linearizability checking is
NP-complete in general (Knossos exploded on long histories); elle
is the escape.

## elle's trick

Don't check arbitrary histories — DESIGN the workload so the
serialization graph is recoverable:

- ops are list-appends: `append(k, v)` with unique v, reads return
  the whole list
- a read of `[1,3]` on k tells you: 1 preceded 3 (ww), this read
  saw 3 (wr), and any txn appending 4 comes after (rw, inferred)
- build the dependency graph from these facts; **a cycle = an
  isolation anomaly**, and the cycle TYPE names it (G0 dirty write,
  G1c cyclic info flow, G-single = read skew...)

Polynomial time, and the counterexample is human-readable ("this
txn read state that implies it ran both before and after that
one"). Question: why do unique values + list semantics make wr/ww
edges *directly observable* where plain registers hide them?

## The redis-raft analysis (2020)

Read for the catalog of consensus-integration bugs — none were in
the Raft paper's math, ALL were in the plumbing:

- acked writes lost on failover (stale-leader window)
- reads served by deposed leaders (no ReadIndex — topic 15 §4!)
- log divergence after membership changes
- the infamous "Raft on top of a system with its own replication"
  impedance

Question: for each finding, which of our topic-15 raft.rs tests (or
which MISSING test) covers it?

The Dgraph analysis is the graph-DB cautionary tale: per-key Raft
groups + cross-group txns = lost writes and read skew — a preview
of topic 29's distributed-transaction problems.

## Jepsen vs DST (the comparison that matters for M16)

| | Jepsen | DST (turso/FDB) |
|---|---|---|
| SUT | unmodified binary | instrumented / DI'd |
| faults | real (iptables, SIGSTOP) | simulated |
| reproducibility | statistical, flaky | perfect (seed) |
| finds | integration + env bugs | logic bugs, deep interleavings |
| checker | elle (history-based) | model/invariant (state-based) |

They're complements: DST explores deeper, Jepsen believes nothing
you told it.

## Questions for notes.md

1. Why does Jepsen use SIGSTOP/SIGCONT instead of kill -9 for one
   nemesis class — which production failure does a *pause* model
   that a crash doesn't (fencing! DDIA ch. 8)?
2. elle needs append+read-full-list ops. What can it NOT check about
   a system that only exposes get/set registers?
3. An elle cycle of pure rw edges (write skew) — which isolation
   level permits it and which forbids it? (Topic 8 refresher.)
4. Redis-raft served stale reads from deposed leaders. Write the
   ReadIndex fix in one sentence and its cost per read.
5. For M15+M16: sketch a mini-elle for our sim: unique-value
   appends via propose(), reads of committed(), cycle check over
   the history. What does the deterministic sim make TRIVIAL that
   real Jepsen fights (total real-time order is known!)?
