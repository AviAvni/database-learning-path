# CRDT foundations: convergence without coordination

Consensus agrees on an order, then applies; CRDTs design the data so
order doesn't matter, then never coordinate. This chapter distills the
two founding documents — Shapiro et al.'s 14-page SSS'11 theory and the
50-page INRIA catalog (RR-7506) you'll keep coming back to. Read SSS'11
§1-3 first, then treat the report as a reference for each structure you
implement in `experiments/src/`.

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

The catalog's flagship (Report §3.3.5, our `orset.rs`) in one screen —
every property SEC needs falls out of set union:

```rust
struct OrSet<T> { adds: HashMap<T, HashSet<Dot>>, removed: HashSet<Dot> }

fn add(&mut self, x: T, dot: Dot) { self.adds.entry(x).or_default().insert(dot); }

fn remove(&mut self, x: &T) {                 // kill only dots we have OBSERVED —
    self.removed.extend(&self.adds[x]);       // a concurrent add's fresh dot
}                                             // survives: add-wins

fn contains(&self, x: &T) -> bool {
    self.adds.get(x).is_some_and(|ds| ds.iter().any(|d| !self.removed.contains(d)))
}

fn merge(&mut self, other: &Self) {           // join = union of everything:
    for (x, ds) in &other.adds { self.adds.entry(x.clone()).or_default().extend(ds); }
    self.removed.extend(&other.removed);      // assoc + comm + idem ⇒ SEC for free
}
```

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

## References

**Papers**
- Shapiro, Preguiça, Baquero, Zawirski — "Conflict-free Replicated Data
  Types" (SSS 2011) — the 14-page theory; read §1-3 first
- Shapiro, Preguiça, Baquero, Zawirski — "A comprehensive study of
  Convergent and Commutative Replicated Data Types" (INRIA RR-7506,
  2011) — the 50-page catalog; use as a reference per structure, not a
  cover-to-cover read

**Code**
- Paper-only chapter — the catalog's structures map one-to-one onto this
  topic's `experiments/src/` stubs
