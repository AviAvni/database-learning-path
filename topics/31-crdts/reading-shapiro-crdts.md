# Reading guide — Shapiro et al., "Conflict-free Replicated Data Types" (SSS 2011) + INRIA comprehensive study (RR-7506)

The founding papers. SSS'11 is the 14-page theory; the INRIA technical
report ("A comprehensive study of Convergent and Commutative Replicated
Data Types") is the 50-page catalog you'll actually keep coming back to.
Read SSS'11 §1-3 first, then treat the report as a reference for each
structure you implement in `experiments/src/`.

## The one picture

```
            Strong Eventual Consistency (SEC)
  ┌──────────────────────────────────────────────────────┐
  │  eventual delivery + termination + CONFLUENCE:       │
  │  same set of updates received ⇒ same state,          │
  │  regardless of order                                 │
  └──────────────────────────────────────────────────────┘
        ▲ guaranteed by either of two sufficient conditions ▲
        │                                                   │
  CvRDT (state-based)                          CmRDT (op-based)
  states form a join semilattice:              concurrent ops commute;
  merge = LUB (assoc, comm, idem);             delivery is causal +
  updates are inflations (s ⊑ update(s))       exactly-once/idempotent
        │                                                   │
  ship state, tolerate any gossip              ship ops, need a smarter
  (counter.rs, orset.rs, lww.rs)               network layer (rga.rs)
  ────────────── §3 of SSS'11 proves these EQUIVALENT ──────────────
       (a CvRDT can emulate a CmRDT and vice versa — the choice
        is an engineering trade, not an expressiveness one)
```

## Reading map

| section | what to extract |
|---|---|
| SSS'11 §2.1 | the system model: no rollback, no consensus, updates applied locally first |
| SSS'11 §2.3 Def. 2.3 | SEC stated precisely — memorize the three clauses |
| SSS'11 §3.1-3.2 | the two sufficient conditions (semilattice / commutativity) and the equivalence proof |
| Report §3.1 | counters: G, PN — why PN needs two G-Counters (your `counter.rs` doc comment) |
| Report §3.2 | registers: LWW and MV-register (multi-value: keep *both* concurrent writes — the honest register LWW isn't) |
| Report §3.3 | sets: G-Set, 2P-Set (remove is forever!), OR-Set (§3.3.5 — your `orset.rs`) |
| Report §4 | graphs! 2P2P-Graph and the remark that concurrent addEdge/removeVertex has *no* universally right answer — the dangling-edge problem M31 inherits |
| Report §5 | garbage collection needs "stability" (Wuu & Bernstein) — ties to exercise 4 |

## Questions

1. State the three clauses of SEC. Which clause does a Raft-replicated
   register satisfy trivially, and which does it *not need* because
   there's a total order?
2. Why is `max()` over a single signed counter not a valid CvRDT merge,
   while per-replica-slot pointwise max is? (Prove non-inflation breaks;
   then check your `counter.rs` PN design against Report §3.1.)
3. The 2P-Set forbids re-adding a removed element; the OR-Set allows it.
   What *metadata* does OR-Set pay for this (look at your `orset.rs`
   tombstones after bench lane 2), and what lets you ever reclaim it?
4. MV-register vs LWW-register: after bench lane 1's ~95% lost-writes
   row, argue when each is right. What does the MV-register push onto
   the application?
5. CvRDT and CmRDT are equivalent in theory (§3). Give two *engineering*
   reasons Yjs/automerge ship ops while Riak shipped state.
6. **M31 mapping**: Report §4's graph CRDTs stop at "concurrent
   addEdge(u,v) ∥ removeVertex(u) is application-specific." Write the
   FalkorDB answer: which of hide/cascade/resurrect did `graph.rs`
   choose, and what would a Cypher user observe in each case?
