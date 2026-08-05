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

> **In:** the flat CRDTs of the previous chapter — registers, sets,
> sequences — each solving one shape of data.
> **Out:** the decomposition of a nested JSON merge into three known
> sub-problems plus one new one (nesting via stable identity).

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

> **In:** the "identity" row from Step 1 — the sub-problem the flat CRDTs
> did not solve.
> **Out:** the rule every JSON CRDT op obeys — address values by a stable
> identifier, never by a path — and why a path-addressed op corrupts.

A JSON path like `todo[1]` names a value by its *position*, but positions
change under concurrent edits — so an op addressed by path can land on
the wrong value. Concretely: A sends "set `todo[1].done = true`" while B
concurrently inserts a new item at index 0. By the time A's op arrives at
B, index 1 is a *different* task — A's checkmark lands on the wrong
grocery run. With 2 replicas and even 1% of ops racing, that's a
corruption every ~50 edits, silently.

The fix: every value created gets a permanent, globally unique
**identifier** — a Lamport timestamp `(counter, replica)`, which is
exactly this topic's `Dot` — and every subsequent op addresses that
identifier, never a path or index. The 2017 paper motivates this with the
concurrent-edit examples of §3.1 (Figures 1–6) and then defines the
identifier machinery formally in §4.2.1 (Lamport timestamps) and §4.2.2
(operation structure). Identity is immune to your neighbors moving;
position is not. This is the chapter's title in one line: identity beats
paths.

### Step 3 — delete must mean hide: presence sets and revival

> **In:** the identifier-addressing rule from Step 2.
> **Out:** why "delete" must mean *hide* — presence sets — and the
> concurrent delete-vs-edit outcome the paper itself flags as surprising.

Once ops address identifiers, "delete" cannot physically remove the
value — a concurrent op addressing that identifier may still be in
flight, and dropping the value would leave that op dangling. So the 2017
semantics makes delete *hide*: §4.3 gives each value a **presence set**
`pres(k)` (the set of operation ids that keep it visible), delete empties
it, and a concurrent edit *inside* the deleted subtree re-populates it —
the subtree revives. Run Step 1's example under this rule: A's
`done = true` races B's `delete todo[0]`; after merge the item is back,
with `done: true`. The paper does not celebrate this — it is exactly
**Figure 6**, which §5 (Conclusions) singles out as the "surprising"
outcome: the resurrected item reappears *without its title*, and the paper
notes it may be more desirable to discard one of the concurrent updates.
So the honest reading is "the concurrent edit *forces* revival," not "the
edit deservedly won."

This is the same shape twice over in this topic: the OR-Set's add-wins
(a concurrent add's fresh dot survives remove) and `graph.rs`'s
hide-not-delete dangling edges. Cost: tombstoned subtrees linger until
causal stability lets you collect them. In automerge the mechanism is
inverted but equivalent: `op_set2/op.rs:52`'s `succ` field lists the
ops that overwrote/deleted an op, and `visible()` (`op.rs:105`) returns
true exactly when an op `has_succ()` is false — deletion is recorded as
*successor ops*, not flags. The `clear` trick that assigns over a whole
subtree is §4.3.1.

### Step 4 — the missing op: move, and how naive move duplicates

> **In:** the identity + hide semantics from Steps 2–3, which handle set,
> assign, and delete.
> **Out:** the one operation the 2017 paper lacks — *move* — and the two
> ways a naive delete+reinsert encoding breaks under concurrency.

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

> **In:** the duplication and cycle failures of naive move from Step 4.
> **Out:** the move paper's mechanism — a Lamport total order plus a local
> undo-do-redo replay — that restores safety without any coordination.

The move paper's insight: you need a total order over moves, but you do
*not* need coordination to get one — Lamport timestamps already give
every replica the same total order, just not the same *arrival* order.
So each replica keeps a log of `LogMove` records sorted by timestamp
(descending in the paper's presentation), each record remembering the
node's *old parent* so the move can be undone; integrating a
late-arriving op means: undo every op newer than it, apply it, redo the
newer ones —

```
  fix: moves form a TOTAL order (Lamport ts). apply = log op.
  to add op O out of order:  UNDO all ops after O, apply O, REDO them.
  ── each redo re-checks "would this create a cycle? then skip" ──
  safety from the total order; availability kept because undo/redo
  is local replay, not coordination.
```

```rust
// ILLUSTRATION — not quoted from any crate; this is the move paper's
// undo-do-redo integration in miniature. The real algorithm is Fig. 3 of
// "A highly-available move operation for replicated trees" (do_op/undo_op/
// redo_op over a log of LogMove records); this topic has no move.rs:1 stub.
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
late costs `O(k)` undos + redos, so the replay window must be bounded (by
causal stability, again). The move paper measures this directly — in its
worst concurrent scenario it reports **~200 undos and redos per remote
operation** (a figure the paper suggests amortising by batching). That
undo/redo replay is the same shape as diamond-types' retreat/advance over
its time DAG (next chapter) — one mechanism, two papers.

### Step 6 — Local-First: the product argument for all of this

> **In:** the technical machinery of Steps 1–5 — identity, hide, move.
> **Out:** the product thesis that motivates it: seven ideals a sync
> architecture should meet, and why only CRDTs clear the hard ones.

"Local-First Software" (Onward! 2019) is the why: **seven ideals**, one
per subsection §2.1–§2.7 — no spinners / instant local writes (§2.1),
your work not trapped on one device (§2.2), the network is optional
(§2.3), seamless collaboration (§2.4), the long now / longevity (§2.5),
security and privacy by default (§2.6), and you retain ultimate ownership
and control (§2.7). Read §3's assessment table (Table 1) — every sync
architecture (files, web apps, Git, Dropbox, CRDTs, …) scored against all
seven with ✓ / partial / ✗ — and note that the CRDT/local-first column is
the one that clears offline (§2.3) *and* real-time collaboration (§2.4)
*and* ownership (§2.7) at once. This is M31's product spec: active-active
FalkorDB is "local-first for graphs" — Step 2's identity-not-path
discipline becomes node identity, Step 3's revival becomes the
dangling-edge policy, Step 4's move problem becomes concurrent edge
rewiring (question 6).

## How to read the papers (with the concepts in hand)

Read in arc order — 2017, 2021, then the manifesto:

**JSON CRDT (2017, arXiv:1608.03960)**

| section | extract |
|---|---|
| §3.1 | the concurrent-editing examples (Figures 1–6) — run them mentally against your orset.rs + lww.rs semantics (Step 1) |
| §4.2.1–§4.2.2 | the identifier machinery: Lamport timestamps (≈ Dots) and operation structure — ops address identifiers, never indices or paths (Step 2) |
| §4.3 | the formal semantics: presence sets `pres(k)`; §4.3.1 is the `clear` trick for assigning over a subtree (Step 3) |
| §5 | Conclusions — it flags **Figure 6** (a to-do item removed while concurrently updated, resurrected *without its title*) as the surprising limitation, and suggests discarding one update instead. This is a delete/update race, *not* interleaving — the character-interleaving anomaly is Fugue's concern, in `reading-sequence-crdts.md` (Step 3) |

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

Answer each before unfolding it.

- [ ] You can construct the path-anomaly that forces ops to address identifiers, not indices.

  <details><summary>Answer</summary>

  A sends "set `todo[1].done = true`" while B concurrently inserts a new
  item at index 0. When A's op reaches B, index 1 now names a *different*
  task — the checkmark lands on the wrong item, silently. Because positions
  shift under concurrent inserts, any path- or index-addressed op can hit
  the wrong value. The 2017 fix (§4.2.1–§4.2.2): every value gets a
  permanent Lamport-timestamp identifier (this topic's `Dot`) and every op
  addresses that identifier, which no neighbor's insert can move (Step 2).

  </details>

- [ ] You can show how naive move (delete + reinsert) duplicates a node, then walk the 2021 undo/redo fix on that trace.

  <details><summary>Answer</summary>

  Two replicas concurrently move the same node N. Encoded as delete+insert,
  each replica deletes N at its old place and inserts a fresh copy at its
  new place; merging both keeps *both* inserts — N is duplicated, one copy
  per mover. The 2021 fix gives moves a Lamport total order and a log of
  `LogMove` records (each remembering N's old parent). To integrate the
  lower-timestamped move after the higher one already applied: **undo** the
  newer move (restore N's old parent from the record), **apply** the
  arriving move, then **redo** the newer move — whose redo now re-checks
  "would this create a cycle?" against the new tree. Both replicas run the
  same log in the same order, so N ends in one place on both (Steps 4–5).

  </details>

- [ ] You can say why the cycle check at redo time yields convergence without coordination, and what bounds the replay cost.

  <details><summary>Answer</summary>

  The check is a pure function of `(tree state, move op)`, and every
  replica reaches the same tree state before each redo because it replays
  the same total-ordered log. So all replicas skip exactly the same
  cycle-forming moves — convergence with zero coordination. The cost is the
  undo-do-redo replay: an op arriving `k` positions late costs `O(k)` undos
  plus redos (the paper measures ~200 per remote op in its worst case), so
  the window of reorderable ops must be bounded — by causal stability,
  after which older ops can never be contradicted (Step 5).

  </details>

- [ ] You can explain why delete-as-hide (presence sets) falls out *necessarily* from wanting revival, and relate it to `graph.rs`.

  <details><summary>Answer</summary>

  If you want "a concurrent edit into a deleted subtree revives it," the
  deleted value's identifier must still exist for the edit to address —
  which means delete cannot physically remove it. Presence sets encode
  exactly this: delete empties `pres(k)`, a concurrent edit re-adds an op
  id to it, and the value reappears. It is the same hide-not-delete shape
  as `graph.rs`'s dangling edges (an edge whose endpoint is absent is
  retained but invisible, and reappears if the node is re-added) and the
  OR-Set's add-wins (Step 3). The honest caveat: the 2017 paper's Figure 6
  shows this revival can resurrect an item missing its title (Step 3).

  </details>

## References

**Papers**
- Kleppmann & Beresford — "A Conflict-Free Replicated JSON Datatype"
  (IEEE TPDS 2017, [arXiv:1608.03960](https://arxiv.org/abs/1608.03960))
  — §3.1 examples, §4.2–§4.3 semantics; §5 (Conclusions) flags Figure 6's
  delete/update resurrection, *not* interleaving
- Kleppmann, Mulligan, Gomes, Beresford — "A Highly-Available Move
  Operation for Replicated Trees" (IEEE TPDS 2021) — the undo/redo
  algorithm and the cycle check
- Kleppmann, Wiggins, van Hardenberg, McGranaghan — "Local-First
  Software: You Own Your Data, in Spite of the Cloud" (Onward! 2019) —
  the seven ideals (§2.1–§2.7) and §3's assessment table (Table 1)

**Code**
- [automerge](https://github.com/automerge/automerge)
  `rust/automerge/src/op_set2/op.rs` — the `succ` field is
  deletion-as-successor-ops; visibility = "has no succ"
