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

> **In:** the same path-vs-identity failure the JSON chapter met, now one
> level down — inside a single ordered list.
> **Out:** the fix every sequence CRDT shares: elements carry a permanent
> identity and inserts are expressed relative to it, and deletes tombstone.

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

> **In:** per-element identity and after-parent anchoring from Step 1.
> **Out:** the one deterministic ordering rule (RGA's) that every codebase
> in this chapter shares — the fixed point the storage escalations vary around.

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

The shared rule at `rga.rs` granularity — everything else in this chapter
is storage. The block below is illustration: the real `rga.rs` names the
parent on the `Op::Insert` variant (not on `Element`) and leaves `apply`
as a `todo!()` for you to fill, so treat this as the *shape*, not a quote:

```rust
// ILLUSTRATION — not quoted from the crate; the topic's rga.rs:1 leaves
// `apply` a todo!() stub and stores the parent on Op::Insert, not on
// Element. This is the RGA integration rule the stub asks you to build.
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

> **In:** the correct-but-naive `Vec<Element>` RGA of Step 2.
> **Out:** the per-character metadata bill that makes it unusable at scale,
> and the three production storage strategies that keep the rule but pay less.

The textbook representation — one struct per character in a
`Vec<Element>` — makes everything O(n). Price it on the topic's own
`rga.rs`: an `Element` carries its own dot (`(u64 replica, u64 counter)`
≈ 16 B), a parent dot (≈ 16 B), the `char` (4 B) and a `deleted` flag
(1 B, padded) — call it ~40 B of struct per 1 B of text, and every
`integrate` is an O(n) scan of that vector. A 1-million-character
document is ~1 million elements and tens of MB of metadata for 1 MB of
text. (Per-character overhead is implementation-specific — this is the
topic's unpacked-struct `rga.rs`; the run-coalesced stores in Steps 4–6
cut it by orders of magnitude, which is the whole point.) Production
systems keep Step 2's *rule* and replace the *storage* — three ways:

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

> **In:** the O(n)-per-character cost from Step 3.
> **Out:** yrs's first escalation — store contiguous typing as one Item,
> and the contiguity invariant that makes addressing mid-run sound.

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

> **In:** the run-coalesced stores of Step 4, which converge but can still
> weave two users' text together.
> **Out:** the interleaving anomaly, the exact algorithms it hits (Fugue's
> Table 1), and the two escalations — YATA's origin fences and Fugue's tree.

Convergence (Step 2) only says replicas agree — it doesn't say the
agreed order is *good*. The anomaly: two users type multi-character runs
at the same cursor while offline; RGA's skip-larger-siblings rule can
weave the runs together character by character. Fugue's Figure 1 is the
worked case: a document holds "milk\n", user A appends "eggs", user B
concurrently appends "bread", and a spec-conformant merge can produce
"milk\n\n**ebgrgesad**" — the two words shredded together:

```
  interleaving anomaly (why Fugue exists) — Fugue Fig. 1:
  doc = "milk\n"; A appends "eggs", B appends "bread", offline.
  bad merge:  milk \n \n e b g r g e s a d   (eggs ∥ bread, letter soup)
  fugue:      milk \n \n eggs bread          (runs stay contiguous)
```

Which algorithms actually interleave is a published, checkable result —
Fugue's **Table 1** grades each on three columns (forward insertion,
backward insertion on one replica, backward insertion across replicas):

- **RGA**: *proven not* to interleave forward-typed text (`○✓`), but it
  **does** interleave backward insertions — both single-replica and
  multi-replica (`●`). So "RGA interleaves backward typing" is the precise
  claim, not "RGA interleaves everything."
- **YATA (Yjs/yrs)**: each Item records *both* neighbors at insert time —
  `origin` (left) and `right_origin` — and integration keeps concurrent
  Items from crossing each other's origin fences. Table 1: Yjs is proven
  not to interleave forward (`○✓`) *and* avoids single-replica backward
  interleaving (`○`), but it **still interleaves in the multi-replica
  backward case** (`●`). (So it is *not* "forward interleaving in corner
  cases" — forward is exactly the column Yjs is safe on.)
- **Fugue / FugueMax** (Weidner & Kleppmann 2023, the design Loro
  implements): the paper first proves it is *impossible* to avoid
  interleaving in every situation, then defines **maximal
  non-interleaving** — keep concurrent runs contiguous except where some
  interleaving is provably unavoidable. **FugueMax is proven** to satisfy
  it (all three Table 1 columns `○✓`); plain Fugue is simpler and may
  interleave more than necessary only in those unavoidable cases. The
  mechanism is the left+right origin pair read as a tree: each element is
  a left or right child of its origin, and the read order is
  left-subtree, node, right-subtree.

Loro implements Fugue on a `generic-btree` (tree beats linked list for
random access into large documents) with run-length-encoded runs, plus a
standalone `fractional_index` crate for non-text ordered containers.

### Step 6 — the biggest escalation: don't run the CRDT at all

> **In:** the run-coalesced-but-always-live CRDT stores of Steps 4–5.
> **Out:** diamond-types' bet — store only the op log and *run* the CRDT
> solely when branches merge, so the 99% lone-writer case pays nothing.

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
Fugue paper. Clone each under `~/repos` (the pin table in
`resources/codebases.md` records the commit each was read at); the
diamond-types anchors below are the pinned `ad48b9c`.

**Steps 1, 2, 4 — yrs ([~/repos/y-crdt](https://github.com/y-crdt/y-crdt))**

y-crdt is not in this topic's pin table; the line numbers below were
verified against `y-crdt/y-crdt` **`main` @ 3074c84** — check them out or
`grep` if `main` has since drifted.

| anchor | what to see |
|---|---|
| `yrs/src/block.rs:160` | `pub struct ID { client, clock }` — literally your `Dot` (Step 1) |
| `yrs/src/block.rs:439` | `pub struct ItemPtr(NonNull<Item>)` — pointer-heavy linked structure, the cost of O(1) local edits (Step 4) |
| `yrs/src/block.rs:1302` | `pub struct Item` — note `origin` (:1322) AND `right_origin` (:1326): Yjs (YATA) uses *both* neighbors at insert time, not just RGA's single parent (Step 5) |
| `yrs/src/block.rs:984`, `:995` | `integrate` / `integrate_item` dispatch (Step 2) |
| `yrs/src/block.rs:1415` | `Item::integrate` — the conflict-resolution loop. Map each branch onto your rga.rs `apply`: the scan for the insert position, the (client-id) tiebreak, splitting a run when the insert lands mid-Item (Steps 2 + 4) |

**Step 6 — diamond-types ([~/repos/diamond-types](https://github.com/josephg/diamond-types), pinned `ad48b9c`)**

| anchor | what to see |
|---|---|
| `src/listmerge/merge.rs:142` | `integrate()` — the RGA-style rule re-hosted onto a range tree: it scans forward over concurrent inserts (`current_state == NOT_INSERTED_YET`, asserted at :175) and breaks ties **by agent name** (:193–197), the diamond-types spelling of `(counter, replica)` (Step 2, re-hosted) |
| `src/listmerge/yjsspan.rs:16-17` | `NOT_INSERTED_YET` and `INSERTED` `SpanState` constants (`:14` defines `SpanState`, `:47` the per-span `current_state` field). retreat/advance flips a span's state as the engine walks the time DAG — Kleppmann's move-op undo/redo, industrialized (Step 6) |

The headline: diamond-types doesn't *store* a CRDT structure at rest —
it stores the op log and *runs* the CRDT only when branches actually
merge. Sequential editing (the 99% case) never pays CRDT overhead.

**Step 5 — Loro & Fugue**

- Fugue paper ("The Art of the Fugue", Weidner & Kleppmann 2023):
  defines *maximal non-interleaving* and proves FugueMax satisfies it.
  Per its Table 1, RGA interleaves backward insertions (single- and
  multi-replica) while proven safe on forward text; Yjs is safe on
  forward *and* single-replica backward, and interleaves only in the
  multi-replica backward case. Fugue's fix is the left+right origin pair
  read as a tree-order rule.
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
4. `NOT_INSERTED_YET` (yjsspan.rs:16): why does merging branch B into
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

Answer each before unfolding it.

- [ ] You can state the one integration rule all three codebases share, from memory.

  <details><summary>Answer</summary>

  RGA's rule: every element has a permanent identity (a dot) and is
  inserted *after a named parent element*; when concurrent siblings share a
  parent, walk right from the parent and skip any sibling whose dot is
  larger, then insert — larger `(counter, replica)` sits closer to the
  parent, deterministically, on every replica. yrs, diamond-types, and
  Loro all implement this ordering; they differ only in storage (Step 2).

  </details>

- [ ] You can name, per codebase, what the rule keeps and what it replaces.

  <details><summary>Answer</summary>

  **yrs**: keeps the rule, replaces one-struct-per-char with run-coalesced
  `Item`s (`block.rs:1302`) carrying `origin` + `right_origin`, split only
  on an edit inside a run (Step 4). **diamond-types**: keeps the rule
  (`merge.rs:142`), but stores only the op log in a time DAG at rest and
  *runs* the CRDT via retreat/advance over `SpanState`
  (`yjsspan.rs:16-17`) only when branches merge (Step 6). **Loro**: keeps
  the rule but strengthens the ordering to Fugue's tree (maximal
  non-interleaving) on a `generic-btree` with RLE runs (Step 5).

  </details>

- [ ] You can explain why the single-parent RGA rule can interleave where YATA's origin pair does better, and state which columns of Fugue's Table 1 each is safe on.

  <details><summary>Answer</summary>

  RGA anchors an insert only to its left parent, so two concurrent
  backward-typed runs sharing anchors can be woven together — Fugue's
  Table 1 marks RGA `●` on both backward-insertion columns (safe, `○✓`,
  only on forward). YATA records *both* neighbors (`origin` and
  `right_origin`) and forbids concurrent Items from crossing each other's
  fences, which buys it `○✓` forward and `○` single-replica backward; it
  still shows `●` in the multi-replica backward column. FugueMax is proven
  `○✓` on all three (Step 5).

  </details>

- [ ] You can say what forces diamond-types to "become" a CRDT again, and name the function that does it.

  <details><summary>Answer</summary>

  A lone writer only ever appends to one branch of the time DAG, so there
  is nothing to reconcile and the stored op log *is* the document. The CRDT
  machinery runs only when two branches must be merged: the engine
  retreats its cursor to the branches' common ancestor (marking applied
  spans `NOT_INSERTED_YET`) and advances through both, re-running the RGA
  integration in `integrate()` (`merge.rs:142`). That function is the
  "becoming" — sequential editing never calls it (Step 6).

  </details>

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
