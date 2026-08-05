# The B-tree: the memory hierarchy turned into a data structure

Node size = transfer unit, fanout = whatever fits, height = the IO budget —
that's the whole design, and Comer's 1979 survey is still the cleanest
exposition of it in print. This chapter reads it as the theory half of the
topic's B-tree thread: before the paper, it builds the six ideas Comer
assumes — the disk cost model, why binary trees drown in it, and the
invariants that fix it — one step at a time. Everything in turso's
`btree.rs` is a footnote to this paper, and §3's B+ variant is the shape
every real engine actually shipped.

Every section reference below was checked against the PDF of *ACM Computing
Surveys* Vol. 11, No. 2 (June 1979), pp. 121–137. **The previous version of
this chapter had the section map wrong** in two places: insertion and deletion
are taught in §1 (under the subheads "Balancing", "Insertion", "Deletion"),
not §2 — §2 is the *cost* analysis — and VSAM is §5, not §4, because §4 is the
multiuser chapter. The corrected reading order is below. Code anchors are
turso at `dd775bc`, the commit this repo's pin table records.

## The problem in one sentence

Find one record among a million on a disk where, in Comer's own framing, "the
time required to access secondary storage is the main component of the total
time required to process the data" (Introduction, *Operations on a File*): a
balanced binary search tree needs ~20 accesses, while Comer's Table I shows a
B-tree of order 50 needs **4 in the worst case** — "later we will see that this
estimate is too high; simple implementation techniques lower the worst case
cost to 3, and the average cost to less" (§2) — and the structure that closes
that gap still sits under nearly every database shipped since.

## The concepts, step by step

### Step 1 — the disk access model: cost = blocks touched

> **In:** nothing yet — this step fixes the cost currency every later step
> prices things in.
> **Out:** one number, "distinct blocks touched", which Steps 2, 3 and 5 all
> minimize and Step 6 finally drives to ~1.

A disk does not hand you bytes; it hands you fixed-size **blocks** — a
contiguous run of bytes that the device transfers as one unit, a few KB on
modern hardware. Comer's Introduction states the model and the reason for it in
two sentences: "with current hardware technology, the time required to access
secondary storage is the main component of the total time required to process
the data. Furthermore, most random access devices transfer a fixed amount of
data per read operation, so that the total time required is linearly related to
the number of reads. Therefore, the number of secondary storage accesses serves
as a reasonable cost measure for evaluating index methods."

Read that carefully, because it is doing two things. It declares the **cost
model** — the thing you count when you compare two algorithms — to be block
accesses rather than comparisons, and it justifies the swap by an *empirical*
claim about the hardware, not a mathematical one. Comer never prints a
millisecond figure anywhere in the paper; a previous version of this chapter
attributed "~30 ms per access, so 600 ms per lookup" to him, and that number is
not his. The honest form of the claim is his: accesses dominate, so count
accesses.

Comer does list what the model deliberately ignores, and it is worth having the
list: "other less important costs include the time to process data once it has
been placed in main memory, the secondary storage space utilization, and the
ratio of the space required by the index to the space required by the
associated information." Two of those three come back to bite — space
utilization is Step 4, and the index-to-data ratio is why Step 6's B+ shape
won.

The **RAM model** — count comparisons, assume every memory access costs the
same — prices algorithms in the wrong currency here. This is the same
observation the turso chapter's Step 1 makes for pages ("one disk IO" always
means "one page"), and the same block-transfer logic as CPU cache lines in
topic 0, three orders of magnitude up the hierarchy.

### Step 2 — why binary trees fail on disk

> **In:** the block-counting cost model from Step 1.
> **Out:** two independent failures of the binary tree under that model —
> height and transfer waste — which Step 3 has to fix simultaneously.

A **binary search tree** stores one key and two child pointers per node, and
the branch taken at a node depends on comparing the query key against the
node's key — Comer's Figure 2 shows exactly this, with the path for the query
"15" darkened. Finding one key among n takes about log₂(n) pointer hops, and
under Step 1's model every hop lands on a different block:

```
binary tree, 1,000,000 keys, nodes scattered on disk:

  hop 1  → block read          height ≈ log2(1,000,000)
  hop 2  → block read                = ln(1e6)/ln(2)
  ...                                = 13.8155 / 0.6931 = 19.93  ⇒  20 reads
  hop 20 → block read

  and each read fetches a 4096 B block to use 16 bytes of it:
      16 / 4096 = 0.39%   used
      4080 / 4096 = 99.6% of the transfer thrown away
```

Two independent failures, and both are Step 1's currency: the *height* is 20,
and each of those 20 transfers is 99.6% waste. Fixing only the second (pack
several binary nodes per block) still leaves a tree whose height is set by
log₂. Fixing only the first is what Step 3 does — and it fixes the second for
free, which is the elegance.

Comer builds the fix the same way, as a generalization rather than a
replacement: §1 says it presents the B-tree "as a generalization of the binary
search tree in which more than two paths leave a given node", and Figure 3
shows the intermediate case — two keys and three branches per node, where "the
query, 15, is less than 42 so the leftmost would be taken at the root."

### Step 3 — the fix: one node = one block, packed with keys

> **In:** the two failures from Step 2.
> **Out:** the fanout formula and a height in single digits — the budget Step 5's
> mechanics have to preserve and Step 6 finally halves again.

The B-tree's move is to make one tree node exactly one disk block and pack it
with as many sorted keys as fit. The number of children a node can have is its
**fanout**; Comer's parameter is the **order** *d*, defined in §1 as: "each
node in a B-tree of order d contains at most 2d keys and 2d + 1 pointers…
each must have at least d keys and d + 1 pointers." So order d means fanout
between d + 1 and 2d + 1, and the two ends of that range give two different
heights — a guaranteed one and a typical one. Both are worth computing.

**The formula, with its symbols named.** Comer derives the height bound in §2
(*Retrieval Costs*) by counting the minimum number of nodes at each depth —
"the number of nodes at depths 0, 1, 2, … must be at least 2, 2d, 2d², 2d³ …"
— and arrives at

```
    h  ≤  log_d ( (n + 1) / 2 )

    h  the height: the number of nodes visited by a find, i.e. block reads
    d  the order: the MINIMUM number of keys in a non-root node
    n  the number of keys in the file
```

The `/2` and the `log_d` rather than `log_2d` are the worst case being paid
for: the root may hold as few as one key, and every other node may be only
half full. Run it on Comer's own example, order 50 indexing 10⁶ records:

```
h ≤ log_50(500,000.5) = ln(500,000.5) / ln(50) = 13.1224 / 3.9120 = 3.354  ⇒  h = 4
```

which is exactly the `4` in Table I's row for node size 50, column 10⁶. Table I
in full, recomputed from the formula to check the transcription — every cell
below matches the paper:

| order d | n = 10³ | 10⁴ | 10⁵ | 10⁶ | 10⁷ |
|---|---|---|---|---|---|
| 10 | 3 | 4 | 5 | 6 | 7 |
| 50 | 2 | 3 | 3 | 4 | 4 |
| 100 | 2 | 2 | 3 | 3 | 4 |
| 150 | 2 | 2 | 3 | 3 | 4 |

**Now derive the fanout from a real page format**, which is the arithmetic this
repo's own topic 3 records and the half Comer leaves to the implementer.
Fanout is not chosen, it is what fits:

```
    F  = floor( (P - H) / (c + s) )      maximum entries in one page
    P  page size in bytes
    H  page-header bytes
    c  bytes per cell (the key plus whatever rides with it)
    s  bytes per cell pointer in the slot array
```

Topic 3's page format (`topics/03-btree-internals/experiments/src/page.rs`,
module docs at lines 1–20) is 4096-byte pages, an 8-byte header, a 2-byte cell
pointer, and an interior cell of `child u32 ∥ key_len u16 ∥ key`. For an 8-byte
key:

```
interior cell   4 + 2 + 8            =   14 B
plus its slot   14 + 2               =   16 B per entry
fanout          (4096 - 8) / 16      = 4088 / 16 = 255.5   ⇒ F = 255

leaf cell       2 + 2 + 8 + 8        =   20 B  (key_len, val_len, key, value)
plus its slot   20 + 2               =   22 B per entry
leaf capacity   4088 / 22            = 185.8                ⇒ L = 185
```

`255` and `185` are exactly the numbers in topic 3's recorded fanout table
(`topics/03-btree-internals/notes.md`, *Fanout arithmetic*). Now the height, in
the two-part form a real engine has — leaves first, then interior levels above
them:

```
leaves      n / L      = 1,000,000 / 185          =     5,406 leaf pages
interior    log_F(...) = ln(5406)/ln(255)
                       = 8.5952 / 5.5413 = 1.551  ⇒       2 interior levels
height                                              2 + 1 = 3
```

and at n = 10⁹: 10⁹/185 = 5,405,406 leaves, log₂₅₅(5,405,406) = 15.5030/5.5413
= 2.798 ⇒ 3 interior levels ⇒ **height 4**. Both match topic 3's table. Widen
the key to 32 bytes and the same two formulas give F = 4088/40 = 102 and
L = 4088/46 = 88, hence 11,364 leaves and log₁₀₂(11,364) = 9.3383/4.6250 =
2.019 ⇒ 3 interior levels ⇒ **height 4 at a million rows**, one more than the
8-byte key needs. That is the whole cost of a wide key, and it is why suffix
truncation exists.

Bigger blocks or smaller keys ⇒ flatter tree. Comer's §2 warns that you cannot
simply keep growing the node: "most hardware systems bound the amount of data
that can be transferred with one access", the constant factor grows with the
transfer size, and "each device has some fixed track size which must be
accommodated to avoid wasting large amounts of space", so "optimum node size
depends critically on the characteristics of the system and the devices".

The turso chapter's Step 2 draws this exact tree-of-pages picture, and its
Step 3 covers how one page physically stores variable-length entries — the
slotted layout — which Comer does not need and this chapter will not
re-explain.

### Step 4 — the invariants: what "B-tree" actually promises

> **In:** the order d and the fanout F from Step 3.
> **Out:** three rules, and the two guarantees they buy — a worst-case height
> and a bounded space overhead — which Step 5's algorithms must maintain on
> every single insert.

A B-tree of order d enforces three rules at all times:

1. **Occupancy.** Every node except the root holds between d and 2d keys, so
   in Comer's words "each node is at least ½ full" (§1).
2. **Balance.** All leaves sit at the same depth. Comer calls this the point of
   the whole structure: "the beauty of B-trees lies in the methods for
   inserting and deleting records that always leave the tree balanced" (§1,
   *Balancing*).
3. **Order.** Keys within a node are sorted, and each child subtree holds
   exactly the keys falling between its two bracketing separators — the
   generalization of the binary search tree's left/right split (§1).

What the rules buy, and it is two distinct things:

- **The height bound of Step 3 is worst-case, not average-case.** Rule 1 is
  what puts the `d` under the logarithm: with a *minimum* of d children per
  node, depth i has at least 2dⁱ⁻¹ nodes no matter what order the keys arrived
  in. There is no insertion sequence that degrades a B-tree the way sorted
  input turns a naive binary search tree into a linked list.
- **Wasted space is capped.** **Storage utilization** — the fraction of the
  allocated bytes that hold live entries — is at least 50% by rule 1. The
  expected value is better: Comer reports in §3 (*2-3 Trees and Theoretical
  Results*) that "extending the analysis to B-trees of higher order, Yao has
  shown that the expected storage utilization is ln 2 [≈] 69%" [YAO78].

Turn 69% into this topic's currency. **Space amplification** is physical bytes
on disk divided by logical bytes stored, so a structure sitting at ln 2
occupancy has, from slack alone,

```
    1 / 0.6931 = 1.443×      space amplification from page slack, in steady state
```

Hold that number next to what this topic actually measured: redb, a
copy-on-write B-tree fed 108 MB of records in random key order, wrote **6.8 GB**
— space amplification **63.28×** ([FINDINGS.md](../../FINDINGS.md) row 1). The
gap between 1.44× and 63.28× is the measure of how much of a real B-tree's
space cost is *not* the thing Comer analysed. Page slack is bounded and
predictable; copy-on-write page versions retained across 1080 commits are
neither. Comer's 1979 B-tree updates in place, so 1.44× is the whole story
there — and that is precisely the assumption Step 6 of the
[LSM chapter](reading-lsm-paper.md) and this topic's README both attack.

### Step 5 — search, insert, split: the mechanics

> **In:** the invariants from Step 4 and the tree shape from Step 3.
> **Out:** three algorithms that all cost O(h) blocks, and the one operation —
> the root split — that is allowed to change h.

**Search** descends one block per level: read the root, search its keys,
follow the child pointer that brackets your key, repeat until a leaf. That is
h block reads, exactly Step 3's budget. What happens *inside* the node is
Comer's "less important cost" from Step 1, and §3 notes the options anyway:
Clampet suggests binary search rather than linear scan, Knuth's refinement is
that "a binary search might be useful if the node is large, while a sequential
search might be best for small nodes."

**Insert** descends the same way and places the key in a leaf. The interesting
case is a full leaf, holding its maximum 2d keys: **split** it into two nodes
of d keys each and push the middle key — the **separator**, the key that
divides which subtree a search descends into — up into the parent. The push can
overflow the parent too, so splits propagate upward. Splitting the root is the
*only* way the tree gets taller, which is why it grows from the top, and why
invariant 2 (all leaves level) holds for free: every leaf gains a level at the
same instant.

**Delete** mirrors it. A node dropping below d keys **borrows** a key from a
sibling (redistribution) or **merges** with one (concatenation).

The cost, per §2 (*Insertion and Deletion Costs*): an insert or delete "may
require additional secondary storage accesses beyond the cost of a find
operation as it progresses back up the tree. Overall, the costs are at most
doubled, so the height of the tree still dominates". So the gradient is: one
insert usually dirties 1 block, occasionally a split chain of O(h) blocks, and
never more than 2h accesses.

**Map to turso.** Comer's §3 opens with the refinement turso implements:
"instead of splitting a node as soon as it fills up, keys could merely be
distributed into a neighboring node, splitting only when two neighbors fill."
turso's `balance_non_root` is that idea, and the number of siblings it will
consider is a named constant:

```rust
// core/storage/btree.rs at turso dd775bc — the constant at 136, and the two
// lines of balance_non_root (2995) that consume it. Elided between them:
// 137-2994, the page/cell format, the cursor and the seek machinery.
   136  pub const MAX_SIBLING_PAGES_TO_BALANCE: usize = 3;
// ... 137-2994: page layout, cursor, seek ...
  2995      fn balance_non_root(&mut self) -> Result<IOResult<()>> {
// ... 2996-3073: state machine, parent bookkeeping, assertions ...
  3074                      let mut pages_to_balance: [Option<PinGuard>; MAX_SIBLING_PAGES_TO_BALANCE] =
  3075                          [const { None }; MAX_SIBLING_PAGES_TO_BALANCE];
```

The line that carries the argument is **136**: the redistribution window is
three pages wide, fixed at compile time. Line 3074 is where that width becomes
the actual array of pages the balance operates on. A wider window would pack
pages fuller (Step 4's utilization rises toward Step 6's B\*-tree bound) at the
cost of reading more siblings per split — the same tradeoff Comer describes in
prose, with a number attached.

### Step 6 — B-tree vs B+-tree: the variant everyone shipped

> **In:** everything above — a tree whose interior nodes carry records.
> **Out:** the shape every shipped engine uses instead, and the one operation
> (`next`) whose cost it changes by a factor of h.

In Comer's original B-tree every node stores full records. In the **B+-tree**,
"all keys reside in the leaves. The upper levels, which are organized as a
B-tree, consist only of an index, a roadmap to enable rapid location of the
index and key parts" (§3, *B+-Trees*), and the leaves are "usually linked
together left-to-right". Comer names that chain: "the linked list of leaves is
referred to as the **sequence set**."

```
B-tree:  keys+values in ALL nodes          B+tree: values ONLY in leaves
         ┌─────k,v─────┐                          ┌──────k──────┐  routing only
      ┌─k,v─┐       ┌─k,v─┐                    ┌──k──┐       ┌──k──┐
      ...                                     [k,v|k,v] ↔ [k,v|k,v]  sequence set
                                                     └── range scan = list walk
```

A terminology warning Comer spends a footnote on: "perhaps the most misused
term in B-tree literature is B\*-tree." Knuth's actual **B\*-tree** is a
different thing from the B+-tree — see below — and Comer adopts "B+-tree" for
"Knuth's unnamed implementation" precisely to stop the confusion. So when a
codebase says "B\*", check which one it means.

Why every real engine chose B+, in the order Comer argues it:

1. **`next` gets cheap, by a factor of h.** This is the headline, and it is the
   problem §2 (*Sequential Processing*) leaves hanging. In a plain B-tree, a
   preorder walk "requires space for at least h = log_d(n + 1) nodes in main
   memory since it stacks the nodes along a path", and finding the smallest key
   means descending from the root to the leftmost leaf (Figure 12). In a
   B+-tree, §3 says the structure "retains the logarithmic cost properties for
   operations by key, but gains the advantage of requiring **at most 1 access
   to satisfy a next operation**. Moreover, during the sequential processing of
   a file, no node will be accessed more than once, so space for only 1 node
   need be available in main memory."
2. **Higher fanout, shorter tree.** Interior entries carry a separator and a
   child pointer instead of a whole record. Run Step 3's formula on topic 3's
   format with an 8-byte key and a 100-byte value: the *leaf* capacity falls to
   `(2 + 2 + 8 + 100 + 2) = 114 B` per entry, `4088/114 = 35` records per leaf,
   while the interior fanout stays at **255** because the value never enters an
   interior cell. A B-tree that stored those records in interior nodes too
   would have a fanout of 35 there as well — `log₃₅(1e6/35) = 10.259/3.555 =
   2.89 ⇒ 3` interior levels instead of 2, a whole extra IO per lookup on the
   same data. Topic 3 records the height as 3 for this shape, which is the
   B+ answer.
3. **Uniformity.** All data at leaf depth means deletion never has to hunt for
   a record inside an interior node — §3: "the key to be deleted must always
   reside in a leaf so its removal is simple", and a stale separator left
   behind in the index still routes searches correctly (Comer's Figure 14).

And the variant that *is* called B\*, since Step 5 already met its mechanism:
Knuth's B\*-tree keeps every node "at least 2/3 full (instead of just 1/2
full)" by delaying a split until two siblings are full and then "the 2 nodes
are divided into 3, each 2/3 full. This scheme guarantees that storage
utilization is at least 66%" (§3, *B\*-Trees*). Compare it against Step 4's
numbers in this topic's currency:

```
B-tree  guaranteed 50%  ⇒ space amp ≤ 2.000×      expected ln 2 = 69% ⇒ 1.443×
B*-tree guaranteed 66%  ⇒ space amp ≤ 1.515×
```

Comer adds the second-order win: "increasing storage utilization has the side
effect of speeding up the search since the height of the resulting tree is
smaller."

The paper's core loop, in the B+ shape §3 argues for — note that the cost of
this function is *exactly* its iteration count:

```rust
// ILLUSTRATION — not quoted from any engine. The real descent this sketches is
// turso's cursor seek in core/storage/btree.rs:2995 (the balance path) and the
// page/cell accessors above it; the height arithmetic in the comment is Step 3's.
fn lookup(pager: &Pager, root: PageId, key: u64) -> Option<Value> {
    let mut page = pager.read(root);                 // each read: 1 potential IO
    loop {
        match page.kind() {
            Interior => {
                let i = page.keys().partition_point(|&k| k <= key);
                page = pager.read(page.child(i));    // descend one level
            }
            Leaf => return page.find(key),           // B+: values ONLY here;
        }                                            // sequence set → range scans
    }
}
// 4 KB page, 8 B key, topic 3's format ⇒ F = 255, L = 185
// 1e9 rows ⇒ 5,405,406 leaves ⇒ 3 interior levels ⇒ height 4
```

The modern payoff of Steps 3–6 combined is a caching argument, and Comer makes
it himself in §3 (*Virtual B-Trees*): under an LRU policy "the most active
nodes are those close to the root; these tend to stay in memory", and "at
least, the root should remain in main memory since it is accessed for each
search." Size it with the numbers above, at n = 10⁹ and 4 KB pages:

```
leaf level      5,405,406 pages × 4096 B    = 22.14 GB
interior levels 5,405,406/255 = 21,198
              + 21,198/255    =     84
              + 84/255        =      1      = 21,283 pages × 4096 B = 87.2 MB
interior share  87.2 MB / 22,140 MB         = 0.39% of the file
```

Under half a percent of the file is routing, so the top three levels fit in any
plausible buffer pool and a point lookup costs **one actual disk IO**. Topic 3
measures the catch: lookups still climb **862 → 1101 ns** from 1e6 to 4e6 keys
with height pinned at 3 ([FINDINGS.md](../../FINDINGS.md) row 3), because
"resident in the buffer pool" and "resident in CPU cache" are different
questions and Comer's model has no term for the second.

## How to read the paper (with the concepts in hand)

~15 pages, 2 h. The order below is the corrected one — §4 is concurrency, not
applications.

1. **Introduction, *Operations on a File*** — Step 1. The four operations
   (`insert`, `delete`, `find`, `next`), and the two sentences that declare
   block accesses to be the cost measure. `next` is the one to keep in mind;
   §3 is where it gets fixed.
2. **§1 The Basic B-Tree** (subheads *Balancing*, *Insertion*, *Deletion*) —
   Steps 2, 4 and 5 in Comer's words: the generalization from the binary search
   tree (Figures 2 and 3), the order-d definition, and the split/merge
   algorithms. Map to turso as you read: `balance_non_root`
   (`core/storage/btree.rs:2995`) with `MAX_SIBLING_PAGES_TO_BALANCE = 3`
   (`:136`) is §3's "redistribute before splitting" refinement, implemented.
3. **§2 The Cost of Operations** (*Retrieval Costs*, *Insertion and Deletion
   Costs*, *Sequential Processing*) — Step 3's height bound and **Table I**, the
   single most quotable artifact in the paper. Read *Sequential Processing*
   last and notice it ends by deferring `next` to the next section — that
   deferral is the reason B+ exists.
4. **§3 B-Tree Variants** — Step 6, and the section that matters most. Read
   *B\*-Trees* and *B+-Trees* adjacently so the naming confusion lands, then
   *Virtual B-Trees* (the caching argument) and *2-3 Trees and Theoretical
   Results* (Yao's ln 2). Skim *Prefix B+-Trees* and *Compression* — they are
   topic 3's suffix-truncation exercise.
5. **§4 B-Trees in a Multiuser Environment** — skim now, return at topics 8–9;
   this is lock coupling before it had that name.
6. **§5 A General Purpose Access Method Using B+-Trees** — IBM's VSAM. Skim for
   flavour; 1979's product landscape.

## Questions to answer in notes.md

1. Comer's height bound is `h ≤ log_d((n+1)/2)` where `d` is the *minimum*
   fanout, but Step 3's page arithmetic computes the *maximum*, F. Work both for
   topic 3's 8-byte-key format (F = 255, so d = 127) at n = 10⁶ and say how far
   apart the guaranteed and typical heights are. Which one does a latency SLO
   care about?
2. Why do B-trees guarantee ≥50% page occupancy, and what is the expected
   value? (Yao's ln 2 ≈ 69%, §3.) Convert both to space amplification and put
   them beside this topic's measured 63.28× for redb — what accounts for the
   rest?
3. Comer's §3 describes redistribution-before-split as a way to "delay
   splitting and eliminate the associated overhead", and Knuth's B\*-tree as
   the 2/3-full version of it. Read turso's `balance_non_root`
   (`core/storage/btree.rs:2995`) and decide: is turso B+, B\*, or a hybrid?
   Name the line that decides it.
4. Comer's B-trees assume one page write is atomic. It is not — a **torn
   write** is a page that hit the disk half-updated after a crash. Which later
   machinery patches this hole, and which topic measures its cost?
5. §2's Table I stops at 10⁷ records. Extend it: with topic 3's F = 255 and
   L = 185, at what n does the height reach 5, and how big is the file then?
   (Step 3 gives you both formulas.)

## The one-line takeaway

The B-tree is the memory hierarchy turned into a data structure: node size =
transfer unit, fanout = whatever fits, height = the IO budget.

## Done when

Answer each before unfolding it.

- [ ] You can state the disk access model in one line — cost is blocks touched, not comparisons made — and use it to explain why a binary search tree is the wrong shape.

  <details><summary>Answer</summary>

  Comer's Introduction: "most random access devices transfer a fixed amount of
  data per read operation, so that the total time required is linearly related
  to the number of reads. Therefore, the number of secondary storage accesses
  serves as a reasonable cost measure." Count block reads, not comparisons.

  The binary search tree fails that model twice over, and the two failures are
  independent. Its height is log₂(n) — 19.93, so 20 reads at a million keys —
  because each node has two children. And each of those reads pulls a 4096-byte
  block to consume roughly 16 bytes of it, throwing away 99.6% of the transfer.
  Packing several binary nodes into one block fixes the second and leaves the
  first; only making one node *be* one block fixes both, which is Step 3.

  </details>

- [ ] You can list the B-tree invariants and say which one forces the >=50% occupancy guarantee.

  <details><summary>Answer</summary>

  Occupancy, balance, order. Occupancy is the one that does the work: §1's
  definition is that a node of order d holds "at most 2d keys and 2d + 1
  pointers… each must have at least d keys and d + 1 pointers. As a result,
  each node is at least ½ full." A node is sized for 2d keys and never allowed
  below d, so the floor is d/2d = 50% by construction.

  That same rule is what makes Step 3's height a *guarantee*: with a minimum of
  d children per node, depth i holds at least 2dⁱ⁻¹ nodes regardless of
  insertion order, which is the inequality §2 turns into `h ≤ log_d((n+1)/2)`.
  Occupancy and worst-case height are the same rule seen from two ends. The
  expected occupancy is better than the guarantee — Yao's ln 2 ≈ 69% (§3) — but
  the guarantee is what you can quote in a capacity plan: at 50%, space
  amplification from slack alone is at most 2.000×, and at 69% it is 1.443×.

  </details>

- [ ] You can narrate a split and say where the separator key ends up in a B-tree versus a B+-tree.

  <details><summary>Answer</summary>

  A leaf holding its maximum 2d keys receives one more. It splits into two
  nodes of d keys, and the middle key becomes the separator pushed into the
  parent. If the parent is also full it splits too, so splits propagate upward;
  splitting the root is the only operation that increases the height, and
  because it lifts every leaf at once, the "all leaves at the same depth"
  invariant is maintained for free.

  The difference is what happens to the middle key itself. In a plain B-tree it
  **moves** up: it now lives in the parent and nowhere else, so a search that
  matches a key in an interior node stops there. In a B+-tree the algorithm
  "promotes a copy of the key, retaining the actual key in the right leaf" (§3),
  so the search "does not stop if a key in the index equals the query value.
  Instead, the nearest right pointer is followed, and the search proceeds all
  the way to a leaf." The consequence Comer draws out is a deletion
  simplification: the copy in the index can be left behind as a pure separator
  even after the real key is deleted, and searches still land correctly
  (Figure 14).

  </details>

- [ ] You can compute fanout and height for a given page size and key width, and check yourself against topic 3's measured table (185 leaf cells and fanout 255 for 8 B keys).

  <details><summary>Answer</summary>

  `F = floor((P - H) / (c + s))` — page size, header, cell bytes, slot-pointer
  bytes. Topic 3's format is P = 4096, H = 8, s = 2, interior cell
  `child u32 ∥ key_len u16 ∥ key` and leaf cell
  `key_len u16 ∥ val_len u16 ∥ key ∥ val`. For an 8-byte key and 8-byte value:

  ```
  interior  4 + 2 + 8  = 14, + 2 slot = 16   ⇒ 4088 / 16 = 255.5  ⇒ F = 255
  leaf      2+2+8+8    = 20, + 2 slot = 22   ⇒ 4088 / 22 = 185.8  ⇒ L = 185
  ```

  Then height in two parts: `n / L` leaves, and `ceil(log_F(leaves))` interior
  levels above them. At n = 10⁶: 5,406 leaves, log₂₅₅(5406) = 1.551 ⇒ 2 interior
  levels ⇒ height 3. At n = 10⁹: 5,405,406 leaves, log₂₅₅(…) = 2.798 ⇒ 3 ⇒
  height 4. Both match `topics/03-btree-internals/notes.md`.

  The check that you have understood rather than memorized: widen the key to 32
  bytes and the interior entry becomes 40 B, so F drops to 102 and the height at
  10⁶ rises to 4 — one extra IO on identical data, bought entirely with 24 bytes
  of key. Topic 3 records that row too.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including what turso actually implements.

  <details><summary>Answer</summary>

  Nothing to unfold — the questions are the exercise, and they go under
  `## Papers → Comer '79` in this topic's `notes.md`.

  The bar for question 3, since it is the one with a checkable answer in code:
  turso is a B+-tree with the redistribution refinement, not a B\*-tree. The
  deciding evidence is `MAX_SIBLING_PAGES_TO_BALANCE = 3`
  (`core/storage/btree.rs:136`, turso `dd775bc`) consumed at
  `core/storage/btree.rs:3074` — a fixed three-page redistribution window, which
  is §3's "distribute into a neighbouring node" rather than Knuth's 2-into-3
  split with its 66% floor. An answer that says "B\*, because it redistributes"
  has confused the mechanism with the guarantee: B\* is defined by the
  occupancy bound, and turso never promises one.

  </details>

## References

**Papers**
- Comer — "The Ubiquitous B-Tree" (*ACM Computing Surveys*, Vol. 11, No. 2,
  June 1979, pp. 121–137) — ~15 pages, 2 h; read the Introduction, §1, §2 and
  §3 in order, §3 (the B+/B\* variants) matters most, skim §4 (multiuser) and
  §5 (VSAM).

| Section | What this chapter took from it |
|---|---|
| Introduction, *Operations on a File* | the four operations; "the number of secondary storage accesses serves as a reasonable cost measure"; the three costs the model ignores |
| §1 | the B-tree as a generalization of the binary search tree (Figures 2–3); order d = "at most 2d keys and 2d + 1 pointers… at least d keys", hence "each node is at least ½ full"; balancing, insertion, deletion |
| §2, *Retrieval Costs* | the node counts 2, 2d, 2d², 2d³ …; the bound `h ≤ log_d((n+1)/2)`; **Table I**, and "a B-tree of order 50 which indexes a file of one million records can be searched with only 4 disk accesses in the worst case… simple implementation techniques lower the worst case cost to 3" |
| §2, *Insertion and Deletion Costs* | costs "at most doubled" over a find; the practical limits on node size (transfer bound, constant factor, track size) |
| §2, *Sequential Processing* | a plain B-tree's `next` may cost log_d n accesses and needs h nodes stacked in memory |
| §3, opening | redistribute into a neighbour before splitting — the refinement turso implements |
| §3, *B\*-Trees* | Knuth's definition: ≥2/3 full, split 2 nodes into 3, "storage utilization is at least 66%"; and the warning that the term is "perhaps the most misused" in the literature |
| §3, *B+-Trees* | all keys in the leaves, upper levels a "roadmap"; the **sequence set**; a promoted *copy* of the separator; "at most 1 access to satisfy a next operation" and space for only 1 node |
| §3, *Virtual B-Trees* | LRU keeps the nodes closest to the root resident; "at least, the root should remain in main memory" |
| §3, *2-3 Trees and Theoretical Results* | Yao's result: "the expected storage utilization is ln 2 [≈] 69%" |

**Code**
- [turso](https://github.com/tursodatabase/turso) at `dd775bc` —
  `core/storage/btree.rs:136` (`MAX_SIBLING_PAGES_TO_BALANCE = 3`) and
  `core/storage/btree.rs:2995` (`balance_non_root`), the living counterpart;
  walked in [reading-turso-btree.md](reading-turso-btree.md).

**This repo's measurements cited above**
- `topics/03-btree-internals/notes.md` — the fanout table (255/185 for 8 B keys,
  102/88 for 32 B keys, 255/35 for a 100 B value) that Step 3's formula
  reproduces, and `topics/03-btree-internals/experiments/src/page.rs:1-20` for
  the cell format it is derived from.
- [FINDINGS.md](../../FINDINGS.md) row 1 (redb's 63.28× space amplification) and
  row 3 (862 → 1101 ns at constant height).
