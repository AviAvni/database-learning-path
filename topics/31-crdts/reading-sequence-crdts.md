# Sequence CRDTs: what a decade of engineering does to RGA

Your `rga.rs` is the textbook version. The three production codebases
here — yrs, diamond-types, Loro — all share its integration rule and
disagree about everything else: storage layout, when the CRDT machinery
runs at all, and how to stop two users' words interleaving. Read in this
order: yrs (the canonical Item/integrate design), diamond-types (same
rule, radically different storage), Loro blogs + Fugue paper (fixing
interleaving, plus the b-tree/rle machinery).

## The one picture — three storage strategies, one integration rule

```
  rga.rs        Vec<Element>, one entry per char       O(n) everything, honest
  ─────────────────────────────────────────────────────────────────────────
  yrs           doubly-linked Items, RUN-COALESCED:    typing "hello" = ONE
                Item{id, left, right, origin,          Item spanning 5 chars
                right_origin, content}                 (split on edit inside)
  ─────────────────────────────────────────────────────────────────────────
  diamond-types ops in a TIME DAG, run-length          replay/merge engine:
                encoded; document rebuilt by           retreat/advance marks
                retreat/advance over spans             spans INSERTED /
                                                       NOT_INSERTED_YET
  ─────────────────────────────────────────────────────────────────────────
  loro          Fugue semantics on a generic-btree,    tree beats linked list
                rle runs, fractional_index for         for random access;
                (non-text) ordered containers          same origin-pair idea
```

The shared rule, at rga.rs granularity — everything else is storage:

```rust
// Insert after the parent, skipping concurrent siblings with a
// larger id — the same deterministic scan on every replica.
fn integrate(&mut self, el: Element) {
    let mut pos = self.index_of(el.parent) + 1;
    while let Some(sib) = self.elems.get(pos) {
        if sib.parent != el.parent { break; }   // left the sibling block
        if sib.dot > el.dot {                   // larger (counter, replica)
            pos += 1;                           // sits closer to the parent —
        } else { break; }                       // skip it (and its subtree,
    }                                           // the detail rga.rs handles)
    self.elems.insert(pos, el);                 // tombstones stay: deleted
}                                               // elements still anchor children
```

## yrs walk ([~/repos/y-crdt](https://github.com/y-crdt/y-crdt))

| anchor | what to see |
|---|---|
| `yrs/src/block.rs:160` | `ID { client, clock }` — literally your `Dot` |
| `yrs/src/block.rs:439` | `ItemPtr` — pointer-heavy linked structure, the cost of O(1) local edits |
| `yrs/src/block.rs:1302` | `Item` — note `origin` AND `right_origin`: Yjs (YATA) uses *both* neighbors at insert time, not just RGA's single parent |
| `yrs/src/block.rs:984`, `:995` | `integrate`/`integrate_item` dispatch |
| `yrs/src/block.rs:1415` | `Item::integrate` — the conflict-resolution loop. Map each branch onto your rga.rs `apply`: the scan for the insert position, the (client-id) tiebreak, splitting a run when the insert lands mid-Item |

## diamond-types walk ([~/repos/diamond-types](https://github.com/josephg/diamond-types))

| anchor | what to see |
|---|---|
| `src/listmerge/merge.rs:142` | `integrate()` — "This is a bastardization of the sequence CRDT algorithm" per its own comment; same skip-larger-siblings loop over a range tree |
| `src/listmerge/yjsspan.rs:29` | `INSERTED` / `NOT_INSERTED_YET` — spans have a *current* state relative to the merge frontier; retreat/advance flips them as the engine walks the time DAG. Kleppmann's move-op undo/redo, industrialized |

The headline: diamond-types doesn't *store* a CRDT structure at rest —
it stores the op log and *runs* the CRDT only when branches actually
merge. Sequential editing (the 99% case) never pays CRDT overhead.

## Loro & Fugue

- Fugue paper ("The Art of the Fugue", Weidner & Kleppmann): defines
  *maximal non-interleaving*. RGA interleaves backward typing; Yjs
  interleaves forward in corner cases. Fugue's fix is the left+right
  origin pair with a tree-order rule.
- Loro blog "Introduction to Loro's Rich Text Format" + "Movable Tree"
  posts: crates to skim — `crates/loro-internal/src/{dag, diff_calc,
  handler, encoding}`, plus standalone `fractional_index`,
  `generic-btree`, `rle`.

```
  interleaving anomaly (why Fugue exists):
  A types "milk eggs", B types "bread jam" at the SAME cursor, offline.
  bad merge:  m b i r l e k a ...   (RGA worst case: letter soup)
  fugue:      milk eggs bread jam   (runs stay contiguous, order by tiebreak)
```

## The PLAN's automerge-vs-loro bench

This crate's deps convention (rand only) can't host automerge/loro, so
run it as a scratch project (README exercise 2): replay
`diamond-types/benchmark_data/` traces through both, record apply time +
peak memory + serialized size. Loro's claims to verify: order-of-magnitude
faster load via its "shallow snapshot" encoding.

## Questions

1. Yjs Items carry `origin` + `right_origin`; your rga.rs carries only
   `parent`. Construct the concurrent scenario where the single-parent
   rule produces a different (worse) order than YATA's pair rule.
2. In `Item::integrate` (block.rs:1415), when does an insert *split* an
   existing Item? What invariant about `ID.clock` contiguity makes run
   coalescing sound in the first place?
3. Why can diamond-types skip CRDT overhead entirely for a lone writer,
   and what specifically forces it to "become" a CRDT again (which
   function have you read that does the becoming)?
4. `NOT_INSERTED_YET` (yjsspan.rs:29): why does merging branch B into
   the frontier require marking some *already-typed* spans as
   not-yet-inserted? Connect to the move-op paper's undo/redo.
5. Define maximal non-interleaving. Show a two-user trace where RGA
   interleaves but Fugue doesn't, using (counter, replica) tiebreaks
   explicitly.
6. **M31 mapping**: FalkorDB properties can hold long strings. When is a
   sequence CRDT per string property worth it vs LWW-whole-string?
   Propose the cutover heuristic and what the write path stores in each
   mode (think: Loro's rle runs vs one register).

## References

**Papers**
- Weidner & Kleppmann — "The Art of the Fugue: Minimizing Interleaving
  in Collaborative Text Editing"
  ([arXiv:2305.00583](https://arxiv.org/abs/2305.00583), 2023) — the
  definition of maximal non-interleaving and the left+right origin rule

**Code**
- [y-crdt](https://github.com/y-crdt/y-crdt) `yrs/src/block.rs` — ID,
  Item, and `Item::integrate` at :1415 are the canonical design
- [diamond-types](https://github.com/josephg/diamond-types)
  `src/listmerge/merge.rs`, `src/listmerge/yjsspan.rs` — the op-log-at-
  rest, CRDT-only-on-merge architecture
- [loro](https://github.com/loro-dev/loro)
  `crates/loro-internal/src/{dag, diff_calc, handler, encoding}` plus
  the standalone `fractional_index`, `generic-btree`, `rle` crates —
  skim alongside the Loro blog posts ("Introduction to Loro's Rich Text
  Format", "Movable Tree")
