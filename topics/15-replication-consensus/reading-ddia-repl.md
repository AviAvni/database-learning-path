# Reading guide — DDIA chapters 5, 8, 9 (Kleppmann)

Designing Data-Intensive Applications. Ch. 5 (Replication), ch. 8
(The Trouble with Distributed Systems), ch. 9 (Consistency and
Consensus). The concepts layer over this topic's code: read ch. 5
alongside valkey's replication.c, ch. 9 alongside the Raft paper.

## Ch. 5 — Replication: the anomaly catalog

The valuable part is the taxonomy of what LAG does to readers:

```
 anomaly                fix
 ──────────────────────────────────────────────────────────
 read-your-writes       session stickiness, or read-after
   (I posted, refresh,    -my-offset (track repl offset per
    it's gone)            session — valkey WAIT-ish)
 monotonic reads        pin session to one replica
   (time goes backward
    across refreshes)
 consistent prefix      causally-ordered delivery (or
   (answer before         single-partition ordering)
    question)
```

Question per anomaly: which does our M15 stage-1 follower exhibit,
and what does the fix cost?

Also from ch. 5: statement vs WAL vs logical (row) replication —
valkey ships statements (post-`propagateNow` rewrite), our M15 ships
the physical WAL, and the tradeoff table maps onto topic 5's logging
choices. Multi-leader and leaderless sections preview topic 31
(CRDTs) — skim.

## Ch. 8 — The trouble: partial failure

The chapter is one argument: in a distributed system you cannot
distinguish {slow node, dead node, slow network, lost packet}, and
clocks lie. Extract:

- **Timeouts are the only failure detector**, and every timeout is a
  guess (our sim.rs makes this concrete: `election_timeout` ticks).
- **Process pauses**: a GC pause makes a live leader dead-then-alive
  — the fencing-token problem. Question: how do Raft terms act as
  fencing tokens? What does valkey have instead? (nothing — hence
  split-brain during failover.)
- **Clock skew**: why leader leases need bounded clock error, while
  ReadIndex needs none (it uses a message round instead of time).

## Ch. 9 — Linearizability and consensus

- **Linearizability** = single-copy illusion: once a read returns a
  value, all later reads return it or newer. Test-worthy definition:
  there is a single total order consistent with real-time.
- Raft gives linearizable WRITES; reads need ReadIndex or leases
  (README §4). Question: why is reading from the leader WITHOUT
  ReadIndex not linearizable? (Deposed leader serving stale reads
  during a partition — walk the timeline.)
- **CAP, properly**: during a Partition choose Available-but-stale
  or Consistent-but-unavailable-on-the-minority-side. valkey chose
  A; Raft chose C. Our `minority_partition_cannot_commit` test IS
  the C choice, executed.
- **Consensus ≡ atomic broadcast ≡ CAS**: the equivalence proofs.
  FLP says async consensus can't be guaranteed to terminate —
  randomized timeouts are the practical dodge, not a refutation.

## Questions for notes.md

1. Build the 2×3 matrix: {async, semi-sync, raft} × {read-your-
   writes, monotonic reads, consistent prefix} — which combos hold?
2. A client's WAIT 1 returns success, then the primary dies and a
   NON-acked replica is promoted. Which ch. 5 guarantee broke, and
   which ch. 9 property would have prevented it?
3. Fencing tokens: sketch how M15's follower rejects a stale
   leader's WAL stream using terms.
4. Why does FLP not doom Raft in practice? One sentence.
5. Linearizable-read options: leader lease vs ReadIndex vs quorum
   read — cost per read of each, and which M22 (the capstone's
   read-path milestone) should pick.
