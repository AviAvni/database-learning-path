# Jepsen & elle: isolation anomalies are cycles

Jepsen believes nothing you tell it: it drives real concurrent
clients against a real cluster while breaking the network, records
the history, and only afterwards decides whether that history was
even possible under the claimed consistency model. Before the
analyses, this chapter builds the machinery step by step — the
black-box method, why checking a history is NP-complete, and elle's
workload-design trick that makes anomalies show up as graph cycles —
then routes you through two reports worth reading in full:
Redis-Raft (the catalog of consensus-plumbing bugs) and Dgraph (the
graph-DB cautionary tale).

## The problem in one sentence

Databases routinely claim "serializable" or "linearizable" and lose
acked writes the first time a network partition lands mid-failover —
Jepsen's redis-raft analysis alone found acked-write loss in a
system built directly on the Raft paper's math.

## The concepts, step by step

### Step 1 — the method: real cluster, real faults, recorded history

Jepsen is black-box testing: it needs no source code, no
instrumentation — just client access to an unmodified binary running
on a real cluster. It spawns concurrent clients issuing operations,
while a **nemesis** process injects real environmental faults, and
records everything into a **history** — a timestamped log of every
operation's start, end, and result:

```
 generators → concurrent client ops (read/write/cas/txn)
            → against a REAL cluster
            → while nemesis injects: partitions, clock skew,
              process kills/pauses (SIGSTOP = the GC-pause stand-in)
            → record HISTORY: [{op, start, end, result}, ...]
            → checker: is this history linearizable / serializable?
```

Note the fault menu is topic 15's failure catalog made physical:
iptables rules for partitions, SIGSTOP for the process that's alive
but not responding (the GC-pause / VM-migration stand-in a crash
doesn't model).

### Step 2 — the checker problem: verifying a history is the hard part

**Linearizability** (every operation appears to take effect
atomically at some instant between its start and end) sounds
checkable — but given a history of concurrent operations, deciding
whether *any* legal ordering explains it is NP-complete in general:
each concurrent window multiplies the orderings to try. Jepsen's
first checker, Knossos, did exactly this search and exploded on long
histories — histories had to stay short, which is the opposite of
what fault-finding wants. elle is the escape.

### Step 3 — elle's trick: design the workload so dependencies are visible

Don't check arbitrary histories — DESIGN the operations so the
outcome itself reveals what ordered what. elle's workload is
**list-append**: every write is `append(k, v)` with a globally
unique v, and every read returns the *entire list* for k. Now a
single read of `[1,3]` on k is loaded with facts: 1 preceded 3
(a write-write dependency, **ww**), this read saw 3's write (a
write-read dependency, **wr**), and any transaction appending 4
must come after this read (a read-write anti-dependency, **rw**,
inferred). Plain registers (get/set of a single value) hide all of
this — each write destroys the evidence of the previous one; lists
keep the whole lineage.

### Step 4 — the serialization graph: a cycle IS an anomaly

Collect those ww/wr/rw facts into a directed graph over transactions
(the **serialization graph** — an edge T1 → T2 means T1 must come
before T2 in any serial order). If the graph has a cycle, no serial
order exists — and the cycle's *edge types* name the anomaly from
the isolation literature: G0 (dirty write, ww cycle), G1c (cyclic
information flow), G-single (read skew), pure-rw cycles (write
skew). The whole checker, structurally:

```rust
// a read of k = [1, 3] by txn T makes dependency edges OBSERVABLE:
fn check(history: &History) -> Result<(), Cycle> {
    let mut g = Graph::new();
    for read in history.reads() {
        for w in read.list.windows(2) {
            g.add(writer(w[0]), writer(w[1]), Ww);   // list order = write order
        }
        if let Some(&last) = read.list.last() {
            g.add(writer(last), read.txn, Wr);       // T saw last's write
        }
        // and T -> writer(v) for any v appended after: an rw anti-dep
    }
    g.find_cycle()   // a cycle = an anomaly; its edge types NAME it
}
```

Cycle detection is polynomial — the NP-complete search of Step 2 is
gone, bought entirely by Step 3's workload design. And the
counterexample is human-readable ("this txn read state that implies
it ran both before and after that one"). Question: why do unique
values + list semantics make wr/ww edges *directly observable* where
plain registers hide them?

### Step 5 — what the method finds: redis-raft, 2020

The Redis-Raft analysis is the catalog of consensus-*integration*
bugs — none were in the Raft paper's math, ALL were in the plumbing:

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

### Step 6 — Jepsen vs DST: complements, not competitors

The comparison that matters for M16:

| | Jepsen | DST (turso/FDB) |
|---|---|---|
| SUT | unmodified binary | instrumented / DI'd |
| faults | real (iptables, SIGSTOP) | simulated |
| reproducibility | statistical, flaky | perfect (seed) |
| finds | integration + env bugs | logic bugs, deep interleavings |
| checker | elle (history-based) | model/invariant (state-based) |

DST explores deeper (millions of seeded interleavings), Jepsen
believes nothing you told it (real kernel, real network, real
binary). A serious engine wants both: the bug classes barely
overlap.

## How to read the analyses (with the concepts in hand)

1. **elle paper (VLDB 2020)** — read §on the dependency-graph
   construction with Steps 3–4 in hand; the anomaly taxonomy section
   is a topic-8 isolation refresher with better names.
2. **"Redis-Raft 1b3fbf6" (2020)** — read in full. For every finding,
   identify which Step 4 edge types formed the cycle, and which
   plumbing layer (election, log, membership) produced it.
3. **"Dgraph 1.0.2" (2018)** — read as the graph-DB case: watch how
   per-key Raft groups turn single-system anomalies into
   distributed-transaction ones.
4. **elle README** — the anomaly taxonomy (G0/G1/G2) is the fastest
   refresher when the reports start naming cycles.

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

## References

**Papers**
- Kingsbury & Alvaro — "Elle: Inferring Isolation Anomalies from
  Experimental Observations" (VLDB 2020,
  [arXiv:2003.10554](https://arxiv.org/abs/2003.10554))
- Jepsen analyses ([jepsen.io/analyses](https://jepsen.io/analyses))
  — read TWO: "Redis-Raft 1b3fbf6" (2020) and a graph one,
  "Dgraph 1.0.2" (2018)

**Code**
- [elle](https://github.com/jepsen-io/elle) — the checker itself;
  the README's anomaly taxonomy is the fastest G0/G1/G2 refresher
