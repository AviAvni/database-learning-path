# Sequence CRDTs: what a decade of engineering does to RGA

Your `rga.rs` is the textbook version. The three production codebases
here — yrs, diamond-types, Loro — all share its integration rule and
disagree about everything else: storage layout, when the CRDT machinery
runs at all, and how to stop two users' words interleaving. Before the
code, this chapter builds the ideas step by step — why list indices
fail, the one integration rule everyone shares, and the three
engineering escalations built on top of it — then hands you the exact
file:line anchors where each idea lives.

## The problem in one sentence

Two users type at the same position of a shared text concurrently and
every replica must converge to the same character order — and the naive
convergent order can interleave their words letter-by-letter
(`m b i r l e k a ...`), so "converges" alone isn't good enough, and
doing all of it at less than ~1 µs per keystroke over million-character
documents is the actual engineering.

## The concepts, step by step

### Step 1 — indices lie: a sequence needs per-element identity

A list index names a position, and concurrent inserts shift positions —
so "insert at index 5" means different things on replicas that have seen
different edits (the same path-vs-identity failure as the JSON chapter's
Step 2, one level down). The fix every sequence CRDT shares: give every
inserted element a permanent unique identity — a `Dot`
`(counter, replica)` — and express inserts *relative to another
element's identity*: "insert 'X' after the element with dot (17, A)".
Identity never shifts; the element you anchored to is the element you
meant, even if 500 characters arrived around it.

Deletion gets the same treatment as everywhere in this topic: a deleted
element becomes a **tombstone** (kept, marked dead) — it must survive
because later concurrent inserts may still anchor to it.

### Step 2 — the shared integration rule: insert after parent, skip larger siblings

Anchoring alone isn't enough: two replicas can concurrently insert
*different* elements after the *same* parent, and both replicas must
place them in the same order. RGA's rule (the one rule all three
codebases share): walk right from the parent, skip over any concurrent
sibling whose dot is larger, insert there — larger `(counter, replica)`
sits closer to the parent, deterministically, on every replica.

```
insert 'X' after 'a' (parent = a's dot):

  a ──► c              a ──► X ──► c        concurrent 'Y' same parent:
        integrate:           tombstone ok:   a ──► Y ──► X ──► c
        walk after a,        deleted elems   (larger (counter,replica)
        skip larger-id       still anchor    sits closer to parent —
        siblings             children        both replicas agree)
```

The shared rule, at rga.rs granularity — everything else in this chapter
is storage:

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

This is an op-based CRDT (ships Insert/Delete ops, needs causal
delivery) — the `rga.rs` row of the previous chapters' CvRDT/CmRDT
table.

### Step 3 — the cost problem: one entry per character doesn't scale

The textbook representation — one struct per character in a
`Vec<Element>` — makes everything O(n): a 1 MB document is ~1 million
elements, each carrying a dot (~12 B), a parent dot, and a tombstone
flag, so ~30 MB of metadata for 1 MB of text, with O(n) scans per
integrate. Production systems keep Step 2's *rule* and replace the
*storage* — three different ways:

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

Steps 4–6 take these one at a time.

### Step 4 — run coalescing: yrs stores runs, not characters

Typing is overwhelmingly sequential, and sequential typing mints
*contiguous* dots — replica A typing "hello" creates dots
`(A,1)..(A,5)`, each parented on the previous. Run coalescing exploits
this: store the whole run as **one Item** with a starting ID and a
length, and split it only when an edit lands *inside* it. Five
characters, one node; a 10K-word typed document collapses from ~60K
elements to a few hundred Items. The invariant that makes it sound:
within a run, `ID.clock` values are contiguous and each element's parent
is its left neighbor — so any element of the run can be addressed as
`(start_id + offset)` without materializing it.

The costs: Items live in a doubly-linked list (O(1) local edits, but
pointer-chasing for random access — topic 0's dependent-load problem),
and every remote edit inside a run pays a split.

### Step 5 — interleaving: convergent is not the same as sensible

Convergence (Step 2) only says replicas agree — it doesn't say the
agreed order is *good*. The anomaly: two users type multi-character runs
at the same cursor while offline; RGA's skip-larger-siblings rule can
weave the runs together character by character:

```
  interleaving anomaly (why Fugue exists):
  A types "milk eggs", B types "bread jam" at the SAME cursor, offline.
  bad merge:  m b i r l e k a ...   (RGA worst case: letter soup)
  fugue:      milk eggs bread jam   (runs stay contiguous, order by tiebreak)
```

Two escalating fixes:

- **YATA (Yjs/yrs)**: each Item records *both* neighbors at insert time
  — `origin` (left) and `right_origin` — and integration keeps concurrent
  Items from crossing each other's origin fences. Kills most
  interleaving, but corner cases remain (forward interleaving when
  origins coincide asymmetrically).
- **Fugue** (Weidner & Kleppmann 2023, implemented by Loro): defines
  **maximal non-interleaving** as a spec — concurrent runs must stay
  contiguous, ordered by one tiebreak — and achieves it with the
  left+right origin pair interpreted as a tree-order rule. RGA
  interleaves backward typing; Yjs interleaves forward in corner cases;
  Fugue provably neither.

Loro implements Fugue on a `generic-btree` (tree beats linked list for
random access into large documents) with run-length-encoded runs, plus a
standalone `fractional_index` crate for non-text ordered containers.

### Step 6 — the biggest escalation: don't run the CRDT at all

diamond-types' observation: 99% of editing is a lone writer, and a lone
writer needs zero conflict resolution — so don't store a CRDT structure
at rest at all. Store the **op log** (run-length encoded, arranged in a
time DAG — a graph of ops ordered by causality, same idea as a commit
graph), and *rebuild* CRDT state only when branches actually merge. The
merge engine walks the DAG with **retreat/advance**: to merge branch B,
it rolls its cursor back to the common ancestor by marking
already-applied spans `NOT_INSERTED_YET`, then advances through both
branches flipping spans to `INSERTED` — Kleppmann's move-op undo/redo
replay (previous chapter, Step 5), industrialized. Sequential editing
never pays CRDT overhead; only actual concurrency does.

## Where each step lives in the code

Read in this order: yrs (the canonical Item/integrate design),
diamond-types (same rule, radically different storage), Loro blogs +
Fugue paper. All cloned under `~/repos`.

**Steps 1, 2, 4 — yrs ([~/repos/y-crdt](https://github.com/y-crdt/y-crdt))**

| anchor | what to see |
|---|---|
| `yrs/src/block.rs:160` | `ID { client, clock }` — literally your `Dot` (Step 1) |
| `yrs/src/block.rs:439` | `ItemPtr` — pointer-heavy linked structure, the cost of O(1) local edits (Step 4) |
| `yrs/src/block.rs:1302` | `Item` — note `origin` AND `right_origin`: Yjs (YATA) uses *both* neighbors at insert time, not just RGA's single parent (Step 5) |
| `yrs/src/block.rs:984`, `:995` | `integrate`/`integrate_item` dispatch (Step 2) |
| `yrs/src/block.rs:1415` | `Item::integrate` — the conflict-resolution loop. Map each branch onto your rga.rs `apply`: the scan for the insert position, the (client-id) tiebreak, splitting a run when the insert lands mid-Item (Steps 2 + 4) |

**Step 6 — diamond-types ([~/repos/diamond-types](https://github.com/josephg/diamond-types))**

| anchor | what to see |
|---|---|
| `src/listmerge/merge.rs:142` | `integrate()` — "This is a bastardization of the sequence CRDT algorithm" per its own comment; same skip-larger-siblings loop over a range tree (Step 2, re-hosted) |
| `src/listmerge/yjsspan.rs:29` | `INSERTED` / `NOT_INSERTED_YET` — spans have a *current* state relative to the merge frontier; retreat/advance flips them as the engine walks the time DAG. Kleppmann's move-op undo/redo, industrialized (Step 6) |

The headline: diamond-types doesn't *store* a CRDT structure at rest —
it stores the op log and *runs* the CRDT only when branches actually
merge. Sequential editing (the 99% case) never pays CRDT overhead.

**Step 5 — Loro & Fugue**

- Fugue paper ("The Art of the Fugue", Weidner & Kleppmann): defines
  *maximal non-interleaving*. RGA interleaves backward typing; Yjs
  interleaves forward in corner cases. Fugue's fix is the left+right
  origin pair with a tree-order rule.
- Loro blog "Introduction to Loro's Rich Text Format" + "Movable Tree"
  posts: crates to skim — `crates/loro-internal/src/{dag, diff_calc,
  handler, encoding}`, plus standalone `fractional_index`,
  `generic-btree`, `rle`.

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

## Done when

You can state the one shared integration rule from memory, then name —
per codebase — what it keeps and what it replaces: yrs (runs, two
origins), diamond-types (op log at rest, retreat/advance), Loro (Fugue
on a b-tree).

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
