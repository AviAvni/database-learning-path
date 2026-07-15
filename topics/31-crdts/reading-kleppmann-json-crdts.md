# JSON CRDTs & the move op: identity beats paths

Three papers by the Kleppmann line, one arc: (1) generalize CRDTs from
flat sets/lists to arbitrary nested JSON; (2) discover that *moving*
things is the hard op the 2017 paper punted on; (3) the manifesto for
why any of this matters. Automerge is the running implementation of the
first two. Before you open the papers, this chapter builds the argument
step by step — why paths break under concurrency, why stable identity
fixes them, why delete must mean hide, and why move needs a total order
after all.

## The problem in one sentence

Merge two independently edited copies of a nested JSON document — one
user set `todo[0].done = true` while the other *deleted* `todo[0]` —
so that every replica converges to the same, defensible state with zero
coordination; the flat CRDTs of the previous chapter (registers, sets,
sequences) each solve a third of it.

## The concepts, step by step

### Step 1 — the composition problem: JSON is three CRDTs stacked

A JSON document is maps, lists, and primitive values, nested arbitrarily
— so a JSON CRDT must compose three already-solved sub-problems and add
one genuinely new one (nesting):

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

Map keys are the easy row: each key is a register (LWW or MV, previous
chapter's Step 5). List order is `rga.rs` / the next chapter. The rest of
this chapter is the third row — identity — and the op it turns out to be
missing.

### Step 2 — why paths break: an address must survive concurrent edits

A JSON path like `todo[1]` names a value by its *position*, but positions
change under concurrent edits — so an op addressed by path can land on
the wrong value. Concretely: A sends "set `todo[1].done = true`" while B
concurrently inserts a new item at index 0. By the time A's op arrives at
B, index 1 is a *different* task — A's checkmark lands on the wrong
grocery run. With 2 replicas and even 1% of ops racing, that's a
corruption every ~50 edits, silently.

The fix (2017 paper §3.1–3.2): every value created gets a permanent,
globally unique **identifier** — a Lamport timestamp `(counter, replica)`,
which is exactly this topic's `Dot` — and every subsequent op addresses
that identifier, never a path or index. Identity is immune to your
neighbors moving; position is not. This is the chapter's title in one
line: identity beats paths.

### Step 3 — delete must mean hide: presence sets and revival

Once ops address identifiers, "delete" cannot physically remove the
value — a concurrent op addressing that identifier may still be in
flight, and dropping the value would leave that op dangling. So the 2017
semantics (§4) makes delete *hide*: each value carries a **presence set**
(the set of ops that keep it visible), delete empties it, and a
concurrent edit *inside* the deleted subtree re-populates it — the
subtree revives. Run Step 1's example under this rule: A's
`done = true` races B's `delete todo[0]`; after merge the item is back,
with `done: true` — the edit won, because someone demonstrably still
cared about the item.

This is the same shape twice over in this topic: the OR-Set's add-wins
(a concurrent add's fresh dot survives remove) and `graph.rs`'s
hide-not-delete dangling edges. Cost: tombstoned subtrees linger until
causal stability lets you collect them. In automerge the mechanism is
inverted but equivalent: `op_set2/op.rs:52`'s `succ` field lists the
ops that overwrote/deleted an op — visibility = "has no succ", i.e.
deletion is recorded as *successor ops*, not flags.

### Step 4 — the missing op: move, and how naive move duplicates

A **move** relocates an existing value (drag a task to another list,
re-parent a folder) — and the 2017 paper simply doesn't have it. The
obvious encoding, delete + re-insert, is broken under concurrency: two
replicas concurrently moving the *same* node each delete the original
and insert their own copy — merge both and the node is **duplicated**,
one copy per mover. Worse, with tree re-parenting, "move A under B"
concurrent with "move B under A" merges into a **cycle** — A and B
orbit each other, detached from the root, and the tree invariant is
gone. No commutativity trick fixes this: the two outcomes ("A under B"
vs "B under A") are mutually exclusive, so *some* order must win.

### Step 5 — the 2021 fix: a total order plus local undo/redo replay

The move paper's insight: you need a total order over moves, but you do
*not* need coordination to get one — Lamport timestamps already give
every replica the same total order, just not the same *arrival* order.
So each replica keeps a log of move ops sorted by timestamp, and
integrating a late-arriving op means: undo every op newer than it, apply
it, redo the newer ones —

```
  fix: moves form a TOTAL order (Lamport ts). apply = log op.
  to add op O out of order:  UNDO all ops after O, apply O, REDO them.
  ── each redo re-checks "would this create a cycle? then skip" ──
  safety from the total order; availability kept because undo/redo
  is local replay, not coordination.
```

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

The cycle check runs at *apply and every redo*, identically on every
replica — so all replicas skip the same ops and converge, still with
zero coordination. The price is replay cost: an op arriving `k` positions
late costs `O(k)` undos + redos, so the replay window must be bounded
(by causal stability, again). That undo/redo replay is the same shape as
diamond-types' retreat/advance over its time DAG (next chapter) — one
mechanism, two papers.

### Step 6 — Local-First: the product argument for all of this

"Local-First Software" (Onward! 2019) is the why: seven ideals for
software — no spinners (instant local writes), multi-device, offline,
collaboration, longevity, privacy, ownership. Read §3's assessment
table — every sync architecture (cloud apps, Git, Dropbox, CRDTs) graded
against all seven; CRDTs are the only column that clears offline +
collab + ownership simultaneously. This is M31's product spec:
active-active FalkorDB is "local-first for graphs" — Step 2's
identity-not-path discipline becomes node identity, Step 3's revival
becomes the dangling-edge policy, Step 4's move problem becomes
concurrent edge rewiring (question 6).

## How to read the papers (with the concepts in hand)

Read in arc order — 2017, 2021, then the manifesto:

**JSON CRDT (2017, arXiv:1608.03960)**

| section | extract |
|---|---|
| §2 | the two editors / shopping-list examples — run them mentally against your orset.rs + lww.rs semantics (Step 1) |
| §3.1-3.2 | ops address *identifiers* (Lamport timestamps ≈ Dots), never indices or paths (Step 2) |
| §4 | the formal semantics: presence sets, the `clear` trick for assigning over a subtree (Step 3) |
| §5 | the interleaving anomaly figure — the flaw Fugue later fixes (see `reading-sequence-crdts.md`) |

**The move op (2021, "A highly-available move operation for replicated
trees")** — read for Steps 4–5: the duplication/cycle counterexamples
first, then the undo/redo algorithm; check that the cycle test is
deterministic given the total order.

**Local-First (Onward! 2019)** — read §3's assessment table (Step 6);
skim the rest as motivation.

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

## Done when

You can construct the path-anomaly and the move-duplication
counterexamples from memory, and explain why the 2021 algorithm's
undo/redo replay preserves availability while its total order restores
safety.

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
