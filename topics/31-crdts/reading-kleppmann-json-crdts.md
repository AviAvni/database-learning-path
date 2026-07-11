# JSON CRDTs & the move op: identity beats paths

Three papers by the Kleppmann line, one arc: (1) generalize CRDTs from
flat sets/lists to arbitrary nested JSON; (2) discover that *moving*
things is the hard op the 2017 paper punted on; (3) the manifesto for
why any of this matters. Automerge is the running implementation of the
first two.

## The one picture — why JSON is harder than a list

```
  doc = { "todo": [ {"title": "buy milk", "done": false} ] }

  replica A: todo[0].done = true          replica B: delete todo[0]
             └── mutates INSIDE an element     └── removes the element

  after merge, what wins?  three composable sub-problems:
  ┌─────────────────────────────────────────────────────────────┐
  │ map keys   → per-key registers (concurrent set = MV or LWW) │
  │ list order → sequence CRDT (topic's rga.rs)                 │
  │ nesting    → every value has an identity (op id = our Dot); │
  │              mutations address identities, not paths;       │
  │              delete hides subtree, concurrent edit revives  │
  └─────────────────────────────────────────────────────────────┘
  (automerge: rust/automerge/src/op_set2/op.rs:52 — `succ` lists the
   ops that overwrote/deleted this op; visibility = "has no succ")
```

## JSON CRDT (2017) — reading map

| section | extract |
|---|---|
| §2 | the two editors / shopping-list examples — run them mentally against your orset.rs + lww.rs semantics |
| §3.1-3.2 | ops address *identifiers* (Lamport timestamps ≈ Dots), never indices or paths |
| §4 | the formal semantics: presence sets, the `clear` trick for assigning over a subtree |
| §5 | the interleaving anomaly figure — the flaw Fugue later fixes (see `reading-sequence-crdts.md`) |

## The move op (2021, "A highly-available move operation for replicated trees")

The 2017 paper has insert/delete/assign — no move. Naive move =
delete+re-insert, and two concurrent moves of the same node *duplicate
it* (or cycle the tree: move A under B ∥ move B under A).

```
  fix: moves form a TOTAL order (Lamport ts). apply = log op.
  to add op O out of order:  UNDO all ops after O, apply O, REDO them.
  ── each redo re-checks "would this create a cycle? then skip" ──
  safety from the total order; availability kept because undo/redo
  is local replay, not coordination.
```

That undo/redo replay is the same shape as diamond-types' retreat/advance
over its time DAG — one mechanism, two papers. In code:

```rust
// Moves live in a TOTAL order (Lamport ts). Integrating an op that
// arrives out of order = undo everything newer, apply, redo.
fn integrate_move(log: &mut Vec<MoveOp>, tree: &mut Tree, op: MoveOp) {
    let pos = log.partition_point(|o| o.ts < op.ts);
    for o in log[pos..].iter().rev() { tree.undo(o); }  // roll back newer ops
    tree.apply_unless_cycle(&op);                       // "would this create a
    for o in &log[pos..] {                              //  cycle? then skip" —
        tree.apply_unless_cycle(o);                     //  re-checked at every redo,
    }                                                   //  identically on all replicas
    log.insert(pos, op);
    // safety from the total order; availability because replay is LOCAL
}
```

## Local-First (Onward! 2019)

The "why": seven ideals (no spinners, multi-device, offline, collab,
longevity, privacy, ownership). Read §3's assessment table — every sync
architecture graded against them; CRDTs are the only column that clears
offline + collab + ownership simultaneously. This is M31's product spec:
active-active FalkorDB is "local-first for graphs."

## Questions

1. In the 2017 semantics, why must ops reference identifiers instead of
   JSON paths? Construct the concurrent-edit anomaly a path-based op
   causes (hint: two inserts shift indices).
2. Concurrent assignment of `{"a":1}` and `[1,2]` to the same map key:
   what does the paper's MV-semantics keep, and what does automerge's
   LWW-flavored choice keep? Which lane-1 number says how often you'd care?
3. Why does delete-as-hide (presence sets) fall out *necessarily* from
   wanting "concurrent edit into deleted subtree revives it"? Relate to
   your graph.rs hide-not-delete edges.
4. Two concurrent moves of the same tree node: show how delete+reinsert
   duplicates it, then walk the 2021 undo/redo algorithm on that exact
   interleaving.
5. The move paper's cycle check happens at *redo* time on every replica
   identically. Why does this give convergence without coordination, and
   what's the cost as the op log grows (what bounds the replay window)?
6. **M31 mapping**: FalkorDB graphs have no tree constraint, but "move" ≈
   re-parenting via edge delete+add. Does the duplicate/cycle problem
   survive? Design the graph analogue: which concurrent edge rewirings
   need move-op-style total ordering, and which are safe under plain
   OR-Set semantics?

## References

**Papers**
- Kleppmann & Beresford — "A Conflict-Free Replicated JSON Datatype"
  (IEEE TPDS 2017, [arXiv:1608.03960](https://arxiv.org/abs/1608.03960))
  — §2-4; §5's interleaving figure is the flaw Fugue later fixes
- Kleppmann, Mulligan, Gomes, Beresford — "A Highly-Available Move
  Operation for Replicated Trees" (IEEE TPDS 2021) — the undo/redo
  algorithm and the cycle check
- Kleppmann, Wiggins, van Hardenberg, McGranaghan — "Local-First
  Software: You Own Your Data, in Spite of the Cloud" (Onward! 2019) —
  read §3's assessment table

**Code**
- [automerge](https://github.com/automerge/automerge)
  `rust/automerge/src/op_set2/op.rs` — the `succ` field is
  deletion-as-successor-ops; visibility = "has no succ"
