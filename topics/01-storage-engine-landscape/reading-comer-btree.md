# The B-tree: the memory hierarchy turned into a data structure

Node size = transfer unit, fanout = whatever fits, height = the IO budget —
that's the whole design, and Comer's 1979 survey is still the cleanest
exposition of it in print. This chapter reads it as the theory half of the
topic's B-tree thread: before the paper, it builds the six ideas Comer
assumes — the disk cost model, why binary trees drown in it, and the
invariants that fix it — one step at a time. Everything in turso's
`btree.rs` is a footnote to this paper, and §3's B+ variant is the shape
every real engine actually shipped.

## The problem in one sentence

Find one record among a million on a 1979 disk, where every disk access
costs ~30 ms: a balanced binary search tree needs ~20 accesses (**600 ms per
lookup**); a B-tree needs 3 (~90 ms then, 3 cached-or-not page reads now) —
and the structure that closes that gap still sits under nearly every
database shipped since.

## The concepts, step by step

### Step 1 — the disk access model: cost = blocks touched

A disk does not hand you bytes; it hands you fixed-size **blocks** (a few
KB), and in 1979 each block fetch costs a mechanical seek plus rotation —
tens of milliseconds — while comparing keys already in memory costs
microseconds. So the only number that matters for a disk-resident structure
is **how many distinct blocks it touches**, not how many comparisons it
does. The RAM model (count comparisons) prices algorithms in the wrong
currency here; the disk model (count block reads) is off by a factor of
~10,000 per operation.

This is the same observation the turso chapter's Step 1 makes for pages —
"one disk IO" always means "one block/page" — and the same block-transfer
logic as CPU cache lines in topic 0, three orders of magnitude up the
hierarchy.

### Step 2 — why binary trees fail on disk

A binary search tree stores one key and two child pointers per node, so
finding one key among n takes ~log₂(n) pointer hops — and on disk, every
hop lands on a different block:

```
binary tree, 1M keys, nodes scattered on disk:

  hop 1  → block read (~30 ms)
  hop 2  → block read (~30 ms)          height = log₂(1,000,000) ≈ 20
  ...                                    ⇒ ~20 block reads ≈ 600 ms/lookup
  hop 20 → block read (~30 ms)

  and each read fetches a ~4 KB block to use ~16 bytes of it → 99.6% wasted
```

Two independent failures: the *height* is 20 (each level is one IO), and
the *transfer* is wasted (one tiny node per big block). Any disk structure
must fix both at once.

### Step 3 — the fix: one node = one block, packed with keys

The B-tree's move is to make one tree node exactly one disk block and pack
it with as many sorted keys as fit; the number of children a node can have
is its **fanout**. Now each block read consumes the *entire* transfer, and
the height shrinks from log-base-2 to log-base-fanout:

- 4 KB block ÷ ~40 bytes per key+child-pointer ≈ **100 keys per node**;
- height = log₁₀₀(1,000,000) = **3** — versus the binary tree's 20;
- at 100-way fanout, 4 levels already index 100⁴ = 100 million keys.

Fanout is derived, not chosen: **fanout ≈ block size ÷ entry size**. Bigger
blocks or smaller keys ⇒ flatter tree. The turso chapter's Step 2 draws
this exact tree-of-pages picture (and its Step 3 covers how one page
physically stores variable-length entries — the slotted layout — which
Comer doesn't need and this chapter won't re-explain).

### Step 4 — the invariants: what "B-tree" actually promises

A B-tree of order d enforces three rules at all times: (1) every node
except the root holds between d and 2d keys — **at least half full**; (2)
all leaves sit at the same depth — **perfectly balanced, always**; (3) keys
within a node are sorted, and child subtrees fall strictly between adjacent
keys.

What the rules buy: the height bound of Step 3 is *worst-case*, not
average-case — there is no insertion order that degrades a B-tree the way
sorted input turns a naive binary tree into a linked list. And the
half-full rule caps wasted space: pages are 50–100% full, ~69% (ln 2) on
average in practice — that gap is the B-tree's space overhead, and it's
bounded.

### Step 5 — search, insert, split: the mechanics

Search descends one block per level: read the root, binary-search its keys,
follow the child pointer that brackets your key, repeat until a leaf —
height block reads, exactly the Step 3 budget. Insert descends the same
way, then places the key in a leaf; the interesting case is a full leaf
(2d+1 keys): **split** it into two d-key nodes and push the middle key up
into the parent. The push can overflow the parent too, so splits propagate
upward; splitting the root is the *only* way the tree gets taller — it
grows from the top, which is what keeps all leaves level (invariant 2 for
free). Deletion mirrors it: a node under d keys **borrows** a key from a
sibling or **merges** with one.

Map to turso: `balance_non_root` (btree.rs:2995) is the "borrow from
siblings first" refinement — Comer explicitly calls out redistribution as
reducing splits, and turso's ≤3-sibling rebalance is that idea implemented.
Cost gradient: one insert usually dirties 1 block, occasionally a split
chain of O(height) blocks.

### Step 6 — B-tree vs B+-tree: the variant everyone shipped

In Comer's original B-tree every node stores full records; in the
**B+-tree** variant, interior nodes store only keys (pure routing
information), all records live in the leaves, and the leaves are chained
into a linked list:

```
B-tree:  keys+values in ALL nodes          B+tree: values ONLY in leaves
         ┌─────k,v─────┐                          ┌──────k──────┐  routing only
      ┌─k,v─┐       ┌─k,v─┐                    ┌──k──┐       ┌──k──┐
      ...                                     [k,v|k,v] ↔ [k,v|k,v]  linked leaves
                                                     └── range scan = list walk
```

Why every real engine chose B+: (a) interior nodes hold only keys → higher
fanout (Step 3's formula with smaller entries) → shorter tree; (b) the
leaf-level linked list → range scans walk sideways without re-descending;
(c) uniform "all data at leaf depth" simplifies everything.

The paper's core loop, in the B+ shape §3 argues for — note that the cost of
this function is *exactly* its iteration count:

```rust
// height = number of page reads = ceil(log_fanout(n)) — the whole game
fn lookup(pager: &Pager, root: PageId, key: u64) -> Option<Value> {
    let mut page = pager.read(root);                 // each read: 1 potential IO
    loop {
        match page.kind() {
            Interior => {
                let i = page.keys().partition_point(|&k| k <= key);
                page = pager.read(page.child(i));    // descend one level
            }
            Leaf => return page.find(key),           // B+: values ONLY here;
        }                                            // leaf link → range scans
    }
}
// 4 KB page ≈ 100 keys ⇒ 1 billion rows at height 5, top 3–4 levels cached
```

And the modern payoff of Steps 3–6 combined: 1 billion rows fit in height
5, and the root plus interior levels are ~1–2% of the data — they stay in
the buffer pool, so a point lookup is typically **one actual disk IO**.

## How to read the paper (with the concepts in hand)

Read in this order:

1. **§1–2 (the problem + the structure)** — Steps 1–4 in Comer's words: why
   balanced trees on disk need high fanout — tree height = number of IOs,
   and height = log_fanout(n). A 4 KB page holding ~100 keys ⇒ 1 billion
   rows in height 5, of which 3–4 levels cache-resident. This is the whole
   game.
2. **§2.1–2.2 (insertion/deletion)** — Step 5's mechanics: split on
   overflow, merge/borrow on underflow. Map to turso as you read:
   `balance_non_root` (btree.rs:2995) is the "borrow from siblings first"
   refinement — Comer calls redistribution out as reducing splits.
3. **§3 (B+-tree, B*-tree variants)** — Step 6; the section that matters
   most, because B+ is what every real engine shipped. The B*-tree's
   deferred split (redistribute into a sibling before splitting) is
   question 2 below.
4. **§4 (applications: VSAM, etc.)** — skim for flavor; 1979's product
   landscape.

## Questions to answer in notes.md

1. Why do B-trees guarantee ≥50% page occupancy, and what's the *measured average*
   (~69%, ln 2)? Connect to space amplification in the README.
2. B*-tree defers splits by redistributing into siblings. What does turso implement —
   B+, B*, or a hybrid?
3. Comer's B-trees assume one page write is atomic. It isn't (torn writes). Which
   later machinery patches this hole? (WAL — topic 5; checksums — topic 3.)

## The one-line takeaway

The B-tree is the memory hierarchy turned into a data structure: node size = transfer
unit, fanout = whatever fits, height = the IO budget.

## Done when

- [ ] You can state the disk access model in one line — cost is blocks touched, not comparisons made — and use it to explain why a binary search tree is the wrong shape.
- [ ] You can list the B-tree invariants and say which one forces the >=50% occupancy guarantee.
- [ ] You can narrate a split and say where the separator key ends up in a B-tree versus a B+-tree.
- [ ] You can compute fanout and height for a given page size and key width, and check yourself against topic 3's measured table (185 leaf cells and fanout 255 for 8 B keys).
- [ ] You wrote answers to both questions in notes.md, including what turso actually implements.

## References

**Papers**
- Comer — "The Ubiquitous B-Tree" (ACM Computing Surveys 1979) — ~15
  pages, 2 h; read §1–3 in order, §3 (the B+/B* variants) matters most,
  skim §4

**Code**
- [turso](https://github.com/tursodatabase/turso)
  `core/storage/btree.rs` — the living counterpart; walked in
  [reading-turso-btree.md](reading-turso-btree.md)
