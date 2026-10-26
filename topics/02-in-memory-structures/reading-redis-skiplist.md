# The redis skiplist: spans make rank queries free

The canonical readable skiplist — `zskiplist` in redis's `t_zset.c` — with
one addition the textbooks skip: every forward link records how many level-0
nodes it jumps over, so summing spans during an ordinary descent yields a
node's rank at no extra cost. This chapter builds the structure from a plain
sorted list upward — express lanes, the descent, spans, the insert
bookkeeping — anchors each piece in the source, and then follows the same
idea into the B+ tree that stores a large sorted set in redis today. Read it
before the RocksDB memtable chapter to see what a skiplist looks like when
concurrency is not allowed to take features away.

Every redis anchor below is `unstable` at commit `e87dda2a4` (2026-10-26),
the revision this repo pins; Valkey anchors are `unstable` at `668d875ca`.
Two facts about that code frame the chapter. First, in redis the skiplist is
a *working* structure, not a storage format — the file says so in its opening
comment:

```c
// redis@e87dda2a4 — src/t_zset.c:20-23, the file's opening comment
    20  /* Large ZSETs use the packed B+ tree and compact member hash index
    21   * implemented in zset_btree.c. The skiplist below is only used as a
    22   * temporary structure by ZUNION/ZINTER/ZDIFF and their STORE variants:
    23   * a SKIPLIST-encoded zset is never stored in the keyspace.
```

A large sorted set in the keyspace is a packed B+ tree (`src/zset_btree.c`,
`OBJ_ENCODING_BTREE` at `object.h:81`, reported by `OBJECT ENCODING` as
`btree` — `object.c:1338`); small ones stay listpack. Valkey has no skiplist
at all: its sorted sets pair a hashtable with an `OrderedIndex` implemented
as a B+ tree (`src/fbtree.c`). Second, the skiplist that *is* there is not the
2009 skiplist most write-ups describe: the node no longer holds an `sds ele`
pointer (the string is *embedded* in the node allocation), the `span` field
at level 0 is **repurposed** to hold node metadata, and the dict beside it
stores skiplist *node pointers* rather than member/score copies. Steps 1-7
teach the skiplist, because it is where "the search structure carries counts,
so rank is free" is easiest to see; Step 8 shows both B+ trees keeping exactly
that property with a different shape. Where this chapter contradicts an older
account, check it yourself:

```
tools/pinned-source.py show redis src/server.h -r 1863:1889
tools/pinned-source.py show redis src/t_zset.c  -r 75:114
```

## The problem in one sentence

A sorted set needs insert, lookup, range-by-score *and* "what is element
#4,217?" — all in O(log n) — and a plain sorted linked list does every one of
them in O(n): at 1 M elements that is up to 1 M dependent pointer hops, at
~100 ns each when cold ([FINDINGS.md](../../FINDINGS.md) row 0), which is
milliseconds per query.

## The concepts, step by step

### Step 1 — a sorted linked list, and why it is too slow

> **In:** nothing yet — this step establishes the baseline structure and the
> exact quantity every later step attacks.
> **Out:** ordered iteration for free, search in O(n) *dependent* loads. Step
> 2 attacks the O(n).

The simplest ordered structure is a linked list kept in key order: each node
holds a key and a pointer to the next. Ordered iteration and range scans are
trivial. But *finding* anything means walking from the head one node at a
time, and each hop is a **dependent load** — the address of the next node is
inside the current one, so the CPU cannot begin the second fetch until the
first has landed (topic 0's pointer chase). A search at n = 1 M costs up to
1 M serialized cache misses; no amount of memory bandwidth helps, because
there is only ever one outstanding request.

The array alternative fixes search (binary search, O(log n)) and breaks
insert (O(n) memmove). What we want is list-like insert with search that can
skip ahead.

### Step 2 — express lanes: give random nodes extra levels

> **In:** the sorted list from Step 1.
> **Out:** a tower of progressively sparser lists over the same nodes, with
> the height distribution and its two constants. Step 3 turns that tower into
> a search algorithm.

A **skiplist** keeps the sorted level-0 list and adds sparser "express lanes"
above it. Each node is assigned a random **height** h and appears in levels
0..h−1. Heights follow a geometric distribution — flip a biased coin until it
fails:

```c
// redis@e87dda2a4 — src/t_zset.c:250-260, zslRandomLevel
   250  /* Returns a random level for the new skiplist node we are going to create.
   251   * The return value of this function is between 1 and ZSKIPLIST_MAXLEVEL
   252   * (both inclusive), with a powerlaw-alike distribution where higher
   253   * levels are less likely to be returned. */
   254  static int zslRandomLevel(void) {
   255      static const int threshold = ZSKIPLIST_P*RAND_MAX;
   256      int level = 1;
   257      while (random() < threshold)
   258          level += 1;
   259      return (level<ZSKIPLIST_MAXLEVEL) ? level : ZSKIPLIST_MAXLEVEL;
   260  }
```

Line 256 is why every node has at least level 0; line 257 is the coin, with
`ZSKIPLIST_P = 0.25` (`server.h:694`); line 259 is the cap,
`ZSKIPLIST_MAXLEVEL = 32` (`server.h:693`, commented "Should be enough for
2^64 elements"). That comment is checkable arithmetic: with p = 1/4 a level-k
lane holds about n·p^k nodes, so 32 levels covers 4³² = 18,446,744,073,709,551,616
= 2⁶⁴ elements exactly.

```
L3 ──────────────────────────────► 42 ─────────────────────────► ∅
L2 ─────────► 17 ─────────────────► 42 ─────────► 71 ──────────► ∅
L1 ─► 8 ────► 17 ────► 29 ────────► 42 ─► 55 ───► 71 ─► 88 ────► ∅
      search 55: move right while next < target, else drop a level
```

The pointer budget follows from the same distribution. A node reaches level k
with probability p^k, so its expected number of forward pointers is

```
E[levels per node] = Σ p^k  for k = 0,1,2,…  =  1/(1 − p)

   at p = 0.25:  1 / (1 − 0.25) = 1 / 0.75 = 1.333 forward pointers per node
```

Cheaper than a binary tree's two child pointers — and there is no rebalancing
code at all. Balance is probabilistic, not maintained. That absence is the
skiplist's real selling point: `t_zset.c` implements insert, delete, range and
rank in about 900 lines with no rotation logic anywhere.

### Step 3 — the descent: one search algorithm for everything

> **In:** the tower from Step 2.
> **Out:** the single traversal pattern every zsl function opens with, and its
> cost priced against topic 0's ladder. Step 4 hangs rank on it for free.

Every skiplist operation starts identically: begin at the header's top level,
move right while the next node's key is still less than the target, and when
it is not, drop down one level. At the bottom you are standing immediately
before the target position. In the source this is the loop at
`t_zset.c:277-285` (inside insert), `:651-660` (`zslGetRank`), `:694-703`
(`zslGetElementByRankFromNode`) and `:415-420` (`zslUpdateScore`) — the same
seven lines, four times.

The cost, with p = 1/4 and the divisions performed:

```
levels to descend:        log_{1/p}(n) = log₄(1,000,000)
                          = ln(1e6)/ln(4) = 13.8155 / 1.3863 = 9.97 levels

forward steps per level:  (1 − p)/p = 0.75 / 0.25 = 3.0
                          (you expect to pass ~3 nodes before the next one
                           overshoots, since each has probability p of being
                           tall enough to have appeared on the lane above)

forward hops, total:      9.97 × 3.0 = 29.9  ≈  30 dependent loads
   at n = 1e7:            11.63 × 3.0 = 34.9  ≈  35
```

Only the *forward* hops are dependent loads. Dropping a level is free: `level[]`
is a flexible array inside the node's own allocation (`server.h:1880`), so
`level[i]` and `level[i−1]` are 16 bytes apart in a line you already have.

Price the 30 hops with topic 0's ladder. If every one missed to DRAM:
30 × ~100 ns = **~3.0 µs** — a hard upper bound, and a bad model, because the
top of the tower is traversed by *every* search and stays in L1 while the
bottom levels are cold. The honest statement is: a skiplist lookup costs tens
of dependent misses where hashbrown costs two (its
[chapter](reading-hashbrown.md), Step 3), which is why topic 0's
`lookup_shootout` shows HashMap at 8.8 ns and the ordered structures 3-5×
worse at n = 1e6 (BTreeMap 26.6 ns, sorted-vec binary search 25.8 ns —
[topic 0 notes](../00-performance-toolbox/notes.md)). You do not choose a
skiplist for point-lookup speed. You choose it for what Steps 4 and 5 add on
top of a search you were doing anyway.

### Step 4 — spans: count what you skip, and rank is free

> **In:** the descent from Step 3, which already visits O(log n) links.
> **Out:** rank queries in O(log n) with no auxiliary structure — plus the
> level-0 encoding trick that pays for the node metadata. Step 6 pays the
> maintenance bill.

A **rank** query ("what index is 55?", "give me elements 100-110") needs to
know *how many* level-0 nodes each express-lane jump flew over. Redis stores
exactly that: each forward link carries a **span**, the number of level-0
nodes it skips.

```c
// redis@e87dda2a4 — src/server.h:1863-1882, the node and its info word
  1863  /* ZSETs use a specialized version of Skiplists */
  1864
  1865  /* Node info placed in level[0].span since it's unused at level 0 (static assert verified) */
  1866  typedef struct zskiplistNodeInfo {
  1867      uint16_t sdsoffset;  /* Offset from node start to sds data (after sds header) */
  1868      uint8_t levels;      /* Number of levels in this node (1-32) */
  1869      uint8_t reserved;
  1870  } zskiplistNodeInfo;
  1871
  1872  typedef struct zskiplistNode {
  1873      double score;
  1874      struct zskiplistNode *backward;
  1875      struct zskiplistLevel {
  1876          struct zskiplistNode *forward;
  // ... 1877-1878: comment reproduced in the prose below ...
  1879          unsigned long span;
  1880      } level[];
  1881      /* sds ele is embedded after level[] array (assist zslGetNodeElement(node) to access it) */
  1882  } zskiplistNode;
```

Note what is *not* there: no `sds ele` field. The member string lives inside
the same allocation, past the end of `level[]` (line 1881), reached through
the byte offset stored in `zskiplistNodeInfo.sdsoffset`. One `zmalloc` per
node holds score, backward pointer, the whole level array and the string —
which is the difference between one cache miss and two when you finally
compare the key.

And the trick that pays for that offset: **level 0's span is always 1**, so
the field is dead weight, so redis puts the node metadata there instead
(the elided comment at 1877-1878 says exactly this).

```c
// redis@e87dda2a4 — src/t_zset.c:75-81 and 101-105, span synthesised at level 0
    75  static inline unsigned long zslGetNodeSpanAtLevel(zskiplistNode *x, int level) {
    // ... 76-77: comment — at level 0, span stores node info instead of distance ...
    78      if (level > 0) return x->level[level].span;
    79      /* For level 0, if regular node, span is 1. If tail node, span is 0. */
    80      return x->level[0].forward ? 1 : 0;
    81  }
   // ... 83-99: Set / Incr / Decr, each a no-op when level == 0 ...
   101  /* Get zskiplistNodeInfo from node (stored in level[0].span). */
   102  static_assert(sizeof(zskiplistNodeInfo) <= sizeof(((zskiplistNode *)0)->level[0].span), "Must fit in level[0].span");
   103  static inline zskiplistNodeInfo *zslGetNodeInfo(const zskiplistNode *node) {
   104      return (zskiplistNodeInfo *)&node->level[0].span;
   105  }
```

Line 80 *computes* the level-0 span instead of reading it; lines 85, 91 and 97
silently skip writes at level 0; line 102 is the static assertion that the
4-byte info struct fits in the 8-byte span slot it is squatting in. Free
metadata, at the cost of every span access going through an accessor — which
is exactly why the insert code in Step 6 never touches `.span` directly.

With spans present, the ordinary descent computes rank as a side effect:

```rust
// ILLUSTRATION — not quoted from redis; the shape of t_zset.c:645-662.
// The real zslGetRank compares {score, ele} via zslCompareWithNode
// (t_zset.c:120) and reads spans through zslGetNodeSpanAtLevel (t_zset.c:75).
fn rank_of(list: &SkipList, target: &Key) -> u64 {
    let mut node = &list.head;
    let mut rank = 0u64;
    for lvl in (0..list.level).rev() {           // express lanes: top → bottom
        while let Some(next) = node.forward(lvl) {
            if next.key < *target {
                rank += node.span(lvl);          // spans sum to the rank — free
                node = next;
            } else {
                break;                           // too far: drop one level
            }
        }
    }
    rank        // ZRANK in O(log n), no auxiliary structure, no re-walk
}
```

The real `zslGetRank` is `t_zset.c:645-662`; it also checks at every level
whether it has *landed on* the target (line 657) so it can return early. The
inverse operation, ZRANGE-by-index, is the same walk with the comparison
replaced by a running total against the wanted rank
(`zslGetElementByRankFromNode`, `t_zset.c:688-705`).

The bill for all this is one `unsigned long` per forward link and the
requirement that every insert and delete keep every affected span exact.

### Step 5 — backward pointers, and rank without comparisons

> **In:** the level-0 list and the spans from Step 4.
> **Out:** reverse ranges as a plain walk, and a second rank algorithm that
> exploits both. Step 7 explains why neither survives concurrency.

Level 0 is doubly linked: `backward` (`server.h:1874`) makes a reverse range
a plain walk from `zsl->tail`, with no descent and no cleverness. Only level 0
gets it; higher levels would double the pointer cost for no query redis runs.

The backward pointer plus spans also enable a rank algorithm the textbooks do
not have — one that avoids string comparison entirely:

```c
// redis@e87dda2a4 — src/t_zset.c:672-685, zslGetRankByNode
   672  unsigned long zslGetRankByNode(zskiplist *zsl, zskiplistNode *x) {
   673      unsigned long distance_to_end = 0;
   674      int level;
   // ... 675-676: comment — walk forward to the end, jumping at each node's top level ...
   677      while (x) {
   678          level = zslGetNodeInfo(x)->levels - 1;
   679          distance_to_end += zslGetNodeSpanAtLevel(x, level);
   680          x = x->level[level].forward;
   681      }
   682      
   683      /* Rank = total nodes - nodes after this one */
   684      return zsl->length - distance_to_end;
   685  }
```

Given a node pointer (which, per Step 7, is what the zset's dict hands you),
this walks *forward* to the tail always taking the current node's tallest
lane, sums the spans it crosses, and subtracts from `zsl->length` — line 684,
the line to focus on, is where the rank appears. Same O(log n), but zero
`sdscmp` calls — the doc comment at 664-671 says so outright. It is only
possible because `levels` is stored in the node (Step 4's repurposed word) and
because spans are exact. Hold on to its shape: Step 8's B+ tree computes rank
the same way, from the element's leaf upward.

There is a warning hidden here for Step 7: `backward` is a *second* pointer
that must change in the same logical instant as the corresponding `forward`.
Trivial when one thread owns the structure; very hard to make atomic without
locks.

### Step 6 — insert: remember the splice points on the way down

> **In:** the descent (Step 3), spans (Step 4), and a node whose height was
> already drawn by `zslRandomLevel`.
> **Out:** the two arrays that make a single descent sufficient, and the three
> distinct span updates an insert must perform. Step 7 explains what makes
> them safe.

`zslInsertNode` (`t_zset.c:265-321`) is the heart. Note the split: the public
`zslInsert` (`t_zset.c:326-339`) only draws a height (line 335), allocates the
node (336) and delegates to `zslInsertNode` (337), which does the descent and
the splice — `zslUpdateScore` reuses that second half on a node it already
owns.

One descent records, per level i:

- `update[i]` — the rightmost node at level i preceding the insert point (the
  nodes whose forward pointers must be spliced), `t_zset.c:284`;
- `rank[i]` — the cumulative level-0 distance travelled to reach `update[i]`,
  `t_zset.c:279-281`, so new spans can be computed without re-walking.

```
insert 55, height 2:                       update[]/rank[] captured on the way down
L2 ──────► 17 ────────────────► 71        update[2]=17  rank[2]=2
L1 ──────► 17 ────► 42 ─[55]──► 71        update[1]=42  rank[1]=3   splice
L0 ─► 8 ─► 17 ─► 29 ─► 42 ─[55]► 71       update[0]=42  rank[0]=3   splice
                                           levels above height 2: span += 1 only
```

Then the splice, which is where the span algebra lives:

```c
// redis@e87dda2a4 — src/t_zset.c:298-311, splice and span fix-up
   298      /* Insert the node at the found position */
   299      for (i = 0; i < level; i++) {
   300          node->level[i].forward = update[i]->level[i].forward;
   301          update[i]->level[i].forward = node;
   302
   303          /* update span covered by update[i] as node is inserted here */
   304          zslSetNodeSpanAtLevel(node, i, zslGetNodeSpanAtLevel(update[i], i) - (rank[0] - rank[i]));
   305          zslSetNodeSpanAtLevel(update[i], i, (rank[0] - rank[i]) + 1);
   306      }
   307
   308      /* increment span for untouched levels */
   309      for (i = level; i < zsl->level; i++) {
   310          zslIncrNodeSpanAtLevel(update[i], i, 1);
   311      }
```

Read `rank[0] − rank[i]` as "how many level-0 nodes lie between `update[i]`
and the insert point". Line 305 sets `update[i]`'s new span to that distance
plus one (the new node itself); line 304 gives the new node the remainder of
the old span. Their sum is the old span plus one, which is the invariant a
verification routine would check — and redis ships one, `zslDebugVerifyStruct`
at `t_zset.c:5482`, worth reading before you debug your own.

Lines 309-311 are the case people get wrong first: levels *above* the new
node's height get no new link, but a node now exists underneath them, so
their spans still grow by one — a separate loop from the splice arithmetic at
304-305. There is a third case at `t_zset.c:288-296`: when the new node is
taller than the list has ever been, the header's links at the new levels are
created with span `zsl->length` — they leap the entire existing list.

Finally, note what `zslUpdateScore` (`t_zset.c:396-430`) does with all this.
If the new score keeps the node between its current neighbours (the two-line
test at 400-401), it writes `node->score` and returns — no splice, no span
touched, O(1). Otherwise it unlinks and reinserts *the same node*, so the
dict's pointer stays valid (comment at 425-426). A structure whose identity
survives repositioning is what makes Step 7's design possible.

### Step 7 — what single-threading buys: features, and one allocation

> **In:** every mechanism above — spans, backward pointers, the descent.
> **Out:** the reason all of them are affordable here, and the two design
> choices that follow from it. The RocksDB chapter is the contrast.

No locks, no CAS (compare-and-swap — the atomic primitive lock-free structures
are built from). Redis is single-threaded on the data path, so this skiplist
can afford operations that touch several pointers at once: an insert writes
`update[i]->level[i].forward`, `node->level[i].forward`, two spans per level,
and two backward pointers. Making that sequence appear atomic to concurrent
readers is exactly the problem RocksDB's `InlineSkipList` avoids by *deleting
the features* — no backward pointers, no spans, no deletes (next chapter).
Concurrency removes features; topic 9 makes that precise.

The same freedom shows up in the memory layout. `zslCreateNode`
(`t_zset.c:169-205`) computes `node_size + sds_buf_size` and makes **one**
allocation (line 181) holding the node, its level array, and a copy of the
member string placed with `sdsnewplacement` (line 193):

```
one zmalloc:  [ score 8 | backward 8 | level[0..h-1] 16h | sds hdr | member bytes ]
                                       ▲ level[0].span holds {sdsoffset, levels}
              h = 1 (75% of nodes):  8 + 8 + 16          = 32 B + string
              h = 1.333 (expected):  8 + 8 + 16 × 1.333  ≈ 37 B + string
```

And the second structure beside the skiplist is cheaper than the folklore
says. The dict does not store a copy of the member and a copy of the score; it
stores the **node pointer**:

```c
// redis@e87dda2a4 — src/t_zset.c:54-65, the zset's dict is a set of node pointers
    54  /* dictType for zset's dict (maps sds to zskiplistNode*) */
    55  dictType zsetDictType = {
    56      dictSdsHash,        /* hash function */
    // ... 57-58: key dup / val dup, both NULL ...
    59      dictSdsKeyCompare,  /* compares embedded sds by keyFromStoredKey */
    60      NULL,               /* key destructor - skiplist owns the node memory */
    // ... 61-62: val destructor, allow-to-expand ...
    63      .no_value = 1,      /* no values stored (only nodes) */
    64      .keyFromStoredKey = zslGetNodeElementForDict,  /* extract embedded sds from node */
    65  };
```

`dictAdd(dstzset->dict, znode, NULL)` at `t_zset.c:3429` — inside
`zunionInterDiffGenericCommand`, building a ZINTER result — inserts the
*node* as the key; `keyFromStoredKey` (line 64) tells the dict how to find the
sds inside it (`zslGetNodeElement`, `t_zset.c:129-133`); line 60 records that
the skiplist owns the memory. Because `.no_value = 1`, the dict can store that
pointer directly in the bucket with no `dictEntry` allocation whenever the
bucket holds one key or the key sits at a chain tail (`dict.h:15-23`). So the
index costs roughly the bucket array plus tag bits — not a second copy of
every member — and a score lookup returns the node, from which the score is
one field away.

Even so, the pair is two indexes over one set: a bucket slot per member, a
node per member with its own `backward` pointer and 1.33 levels of 16-byte
links, and every member compared by string inside nodes scattered across the
heap. That residue is what Step 8's encoding removes.

### Step 8 — the same counts in a B+ tree: how a sorted set is stored

> **In:** the rank-by-counts idea from Steps 4-5, and the dict-plus-skiplist
> pair from Step 7.
> **Out:** the two B+ trees that store sorted sets in redis and Valkey, where
> their counts live, the two rank algorithms over them, and what one insert
> costs in counter updates — so the chapter's claim ("rank is free because the
> search structure carries counts") is checked against the code that serves
> ZRANK today.

A **B+ tree** is a balanced search tree whose **leaves** hold all the
elements, in order and linked to their neighbours, and whose **inner nodes**
hold only routing information: for each child, a bound on the keys below it
and a pointer to it. Its **fanout** is the maximum number of children an inner
node has; with fanout F and n elements the tree is about log_F(n) levels tall,
and every leaf is at the same depth (topic 3 builds one on disk). Redis's
`zset_btree.c` states the job in its opening comment:

```c
// redis@e87dda2a4 — src/zset_btree.c:23-31, the two questions
    23  /* A large sorted set needs to answer two different questions efficiently:
    24   *
    25   *  1. Given a member, what is its score?
    26   *  2. Given a score and member, where is it in the sorted order?
    27   *
    28   * The traditional Redis encoding uses a hash table for the first question
    29   * and a skiplist for the second. This file uses a small hash table and one B+
    30   * tree. Both are made for this exact pair of indexes, so they share
    31   * information and omit everything the sorted set does not need.
```

**The score tree.** Ordered by (score, member). A leaf is a single allocation
of at most `ZBT_SCORE_LEAF_BYTES` = 4095 bytes and `ZBT_SCORE_LEAF_MAX` = 96
elements (`zset_btree.c:144-145`), packing scores, 16-bit record offsets, one
hash-tag byte per member, and the member bytes themselves (comment at 41-58;
members of 320 bytes or more get their own allocation, line 178). An inner
page has at most `ZBT_SCORE_INNER_MAX` = 32 children (line 147). The counts
are in the inner page:

```c
// redis@e87dda2a4 — src/zset_btree.c:212-246, the node header and the inner page
   212  struct zbtScoreNode {
   213      zbtScoreInner *parent;   /* NULL only for the root. */
   214      uint64_t subtree;        /* Elements represented by this node. */
   215      uint8_t parent_index;    /* Our position in parent->child[]. */
   216      uint8_t count;           /* Elements in a leaf, children in an inner node. */
   217      uint8_t isleaf;          /* Non-zero for zbtScoreLeaf. */
   218      /* Only score leaves use these. They live in this header's padding so that
   219       * a score leaf still costs 56 bytes before its packed arrays. */
   220      uint8_t score_shift;
   221      uint8_t has_external;
   222      uint8_t moved_out;       /* Page share moved out by score changes. */
   223      uint16_t index_resize;   /* Resize that copied this leaf, zero if none. */
   224  };
   // ... 225-239: zbtScoreLeaf, and its comment ...
   240  /* max_score[] and child_count[] describe the child at the same index. */
   241  struct zbtScoreInner {
   242      zbtScoreNode n;
   243      zbtScoreNode *child[ZBT_SCORE_INNER_MAX];
   244      uint64_t child_count[ZBT_SCORE_INNER_MAX];
   245      double max_score[ZBT_SCORE_INNER_MAX];
   246  };
```

Line 244 is the one to focus on: `child_count[i]` is the number of elements
under `child[i]` — a span, moved from a skiplist link to a B+ tree slot.
Line 214's `subtree` is the same count kept in the child's own header, and
`parent_index` (215) lets a node find its slot in the parent without
searching. Compiled on this machine (arm64, LP64) the header is 24 bytes, the
leaf header 56 (matching the comment at 218-219) and an inner page
24 + 3 × 32 × 8 = 792 bytes.

Rank, given the element's leaf and position, is Step 5's `zslGetRankByNode`
turned upward — add up everything to the left, one level at a time:

```c
// redis@e87dda2a4 — src/zset_btree.c:3316-3328, zbtScoreRank
  3316  /* Compute a zero-based rank by adding complete left siblings while walking
  3317   * from a score leaf to the root. */
  3318  static unsigned long zbtScoreRank(zbtScoreLeaf *leaf, unsigned int pos) {
  3319      unsigned long rank = pos;
  3320      zbtScoreNode *node = &leaf->n;
  3321      while (node->parent) {
  3322          zbtScoreInner *parent = node->parent;
  3323          for (unsigned int i = 0; i < node->parent_index; i++)
  3324              rank += parent->child_count[i];
  3325          node = &parent->n;
  3326      }
  3327      return rank;
  3328  }
```

Line 3324 is the whole algorithm: at each level, add the counts of the
complete siblings left of us. Where does `(leaf, pos)` come from? From the
**member index** (comment at 72-92): an **open-addressing** hash table — one
whose entries live directly in the bucket array, with collisions resolved by
probing neighbouring buckets instead of chaining — with 8 slots per bucket,
where a slot holds only a one-byte **tag** (the high eight bits of the
member's hash, used to reject non-matches without touching the member) and a
two-byte leaf ID (four bytes once a very large set needs it). A whole bucket
of 8 slots, with a summary word that rejects most absent members, is 32 bytes
(`zbtIndexBucket16`, 248-256). `zbtreeRank` (3332-3343) hashes the member, finds its leaf
through that index, and calls `zbtScoreRank` — no string comparison on the
way up, exactly like `zslGetRankByNode`. The inverse, ZRANGE-by-index, is
Step 4's `zslGetElementByRankFromNode` turned into a descent:

```c
// redis@e87dda2a4 — src/zset_btree.c:3898-3920, zbtreeIteratorSeekRank
  3898  int zbtreeIteratorSeekRank(const zbtreeSet *zs, unsigned long rank,
  3899                             zbtreeIterator *iter)
  3900  {
  3901      if (rank >= zs->length) {
  3902          iter->leaf = NULL;
  3903          iter->pos = 0;
  3904          return 0;
  3905      }
  3906
  3907      zbtScoreNode *node = zs->score_root;
  3908      while (!node->isleaf) {
  3909          zbtScoreInner *inner = (zbtScoreInner *)node;
  3910          unsigned int pos = 0;
  3911          while (rank >= inner->child_count[pos]) {
  3912              rank -= inner->child_count[pos];
  3913              pos++;
  3914          }
  3915          node = inner->child[pos];
  3916      }
  3917      iter->leaf = node;
  3918      iter->pos = rank;
  3919      return 1;
  3920  }
```

Lines 3911-3912 subtract each whole child's count until the wanted rank falls
inside one, then descend into it (3915); at the leaf the remainder *is* the
position (3918).

The maintenance bill is smaller than the skiplist's. An insert or delete
changes one leaf's count and then walks to the root:

```c
// redis@e87dda2a4 — src/zset_btree.c:918-934, zbtScoreUpdatePath
   918  /* Update only the path fields affected by a local leaf edit or by replacing a
   919   * leaf at the same position. length_change is the change in the leaf's
   920   * element count: +1 for insertion, -1 for deletion, zero for a score change. */
   921  static void zbtScoreUpdatePath(zbtScoreNode *node, long long length_change) {
   922      while (node->parent) {
   923          zbtScoreInner *parent = node->parent;
   924          unsigned int pos = node->parent_index;
   925          double max = zbtScoreNodeMaxScore(node);
   926          /* An in place score change that leaves the node maximum alone leaves
   927           * every page above it alone too. */
   928          if (length_change == 0 && parent->max_score[pos] == max) return;
   929          parent->child_count[pos] = node->subtree;
   930          parent->max_score[pos] = max;
   931          parent->n.subtree += length_change;
   932          node = &parent->n;
   933      }
   934  }
```

Line 929 refreshes one `child_count` per level and line 931 the parent's own
total; line 928 stops at once for a score change that leaves a page's maximum
alone. Compare Step 6, where an insert rewrites two spans on each of the new
node's levels and increments one on every level above it.

Price both rank algorithms at n = 1,000,000 with the same arithmetic as
Step 3 (computed, not estimated):

```
leaves, all full:      1,000,000 / 96  = 10,416.7  → 10,417 leaves
inner levels above:    log₃₂(10,417)   = ln(10,417)/ln(32) = 9.251/3.466 = 2.67 → 3
leaves half full (48): 1,000,000 / 48  = 20,833.3; log₃₂ = 2.87 → still 3

SeekRank descent:      3 inner pages + 1 leaf = 4 dependent loads
                       against Step 3's ≈ 30 forward hops for the skiplist
counts summed per page: up to 31 × 8 bytes, contiguous in one array
                       (line 244) — sequential, prefetchable, not dependent
```

The tree is not "free rank" in a stronger sense than the skiplist — both
compute rank from counts carried by the search structure, in O(log n). It is
shallower (4 dependent loads instead of ~30) and far denser: the member index
spends 32 bytes per 8 members, 4 bytes each, where the skiplist design pays an
8-byte bucket pointer plus a node of ≈ 37 bytes before the string (Step 7) for
every member.

**Valkey** reaches the same place with different parameters. Its sorted set
is a hashtable plus an `OrderedIndex` (`src/server.h:1613-1616`), and the
`OrderedIndex` is a B+ tree in `src/fbtree.c`:

```c
// valkey@668d875ca — src/fbtree_internal.h:41-53, the inner node
    41  typedef struct {
    42      node header;
    43      char embedded_prefix[EMBED_PREFIX_LEN]; /* Short prefix inline; long prefix stores char* at aligned offset */
    44      size_t prefix_len;                      /* Common prefix length of this node's anchor values (NOT all subtree keys).
    45                                               * Features are the 4 bytes at offset prefix_len in each anchor, so the
    46                                               * prefix skips identical leading bytes to keep features discriminating.
    47                                               * Note: this can exceed a child's prefix_len — see updateCommonPrefix. */
    48      char features[FEATURE_SIZE][FEATURE_ROW_SIZE];
    49      sds anchors[NODE_SIZE]; /* High keys of children (rightmost value in each child's subtree) */
    50      node *children[NODE_SIZE];
    51      size_t child_sizes[NODE_SIZE];      /* subtree element counts for rank queries */
    52      uint8_t child_num_items[NODE_SIZE]; /* direct item count of each child */
    53  } innerNode;
```

`NODE_SIZE` is 61 (line 18); `static_assert`s at lines 70-71 pin the inner
node at exactly 2048 bytes and the leaf at 512, two jemalloc size classes.
Line 51 is the span again: `child_sizes[i]`, "subtree element counts for rank
queries". The rank of an item found through the hashtable sums the counts to
its left on the way down (`fbtree.c:1187-1188`), and the seek-by-rank loop at
`fbtree.c:1160-1161` subtracts them exactly as redis's 3911-3912 does.

So the chapter's argument holds in all three structures: rank is O(log n) and
needs no auxiliary index because the ordered structure counts what it routes
over — per link in the skiplist, per child slot in the B+ trees.

## Where each step lives in the code

| Lines | What | Step |
|---|---|---|
| `t_zset.c:20-23` | the skiplist is a temporary structure; stored zsets are B+ trees | intro |
| `server.h:693-694` | `ZSKIPLIST_MAXLEVEL` 32, `ZSKIPLIST_P` 0.25 | 2 |
| `server.h:1865-1870` | `zskiplistNodeInfo` — what squats in level[0].span | 4 |
| `server.h:1872-1882` | `zskiplistNode` — no `ele` field; sds embedded after `level[]` | 4 |
| `server.h:1884-1889` | `zskiplist` — header, tail, length, level, `alloc_size` | 2 |
| `t_zset.c:54-65` | `zsetDictType` — `no_value`, `keyFromStoredKey` | 7 |
| `t_zset.c:75-99` | span accessors; level 0 is synthesised, never written | 4 |
| `t_zset.c:101-114` | `zslGetNodeInfo` / `zslSetNodeInfo` + the `static_assert` | 4 |
| `t_zset.c:120-133` | `zslCompareWithNode`, `zslGetNodeElement` (offset → sds) | 3 |
| `t_zset.c:169-205` | `zslCreateNode` — the single allocation | 7 |
| `t_zset.c:250-260` | `zslRandomLevel` — the coin, and the cap | 2 |
| **`t_zset.c:265-321`** | **`zslInsertNode` — descent, `update[]`, `rank[]`, splice** | 6 |
| `t_zset.c:277-285` | the descent, in its canonical form | 3 |
| `t_zset.c:288-296` | new-tallest-level case: header spans = `zsl->length` | 6 |
| `t_zset.c:299-306` | splice + span algebra | 6 |
| `t_zset.c:309-311` | above-height spans += 1 | 6 |
| `t_zset.c:326-339` | `zslInsert` — draw height, allocate, delegate | 6 |
| `t_zset.c:345-366` | `zslUnlinkNode` — the same algebra in reverse | 6 |
| `t_zset.c:396-430` | `zslUpdateScore` — O(1) fast path, node identity preserved | 6 |
| `t_zset.c:645-662` | `zslGetRank` — spans summed during the descent | 4 |
| `t_zset.c:672-685` | `zslGetRankByNode` — forward walk, no string compares | 5 |
| `t_zset.c:688-705` | `zslGetElementByRankFromNode` — the descent, inverted | 4 |
| `t_zset.c:5482` | `zslDebugVerifyStruct` — the invariants, as code | 6 |
| `zset_btree.c:23-138` | the design comment: score tree, member index, leaf IDs | 8 |
| `zset_btree.c:144-147` | leaf ≤ 96 elements / 4095 bytes; inner fanout 32 | 8 |
| `zset_btree.c:212-246` | `zbtScoreNode`, `zbtScoreLeaf`, `zbtScoreInner` (`child_count`, 244) | 8 |
| `zset_btree.c:248-267` | member-index buckets: 8 tags + 8 leaf IDs, 32 bytes | 8 |
| `zset_btree.c:918-934` | `zbtScoreUpdatePath` — one count per level per insert | 8 |
| `zset_btree.c:3316-3343` | `zbtScoreRank` / `zbtreeRank` — leaf-to-root rank | 8 |
| `zset_btree.c:3898-3920` | `zbtreeIteratorSeekRank` — rank-to-leaf descent | 8 |
| valkey `fbtree_internal.h:41-53`, `70-71` | `innerNode` with `child_sizes`; 2048 / 512-byte asserts | 8 |
| valkey `fbtree.c:1174-1201` | `subtreeGetIndexOfItem` — rank as a sum of left counts | 8 |

Read in this order:

1. **`server.h:1863-1889`** (Step 4) — the structs. Ask where the member
   string is before reading line 1881.
2. **`t_zset.c:75-114`** (Step 4) — the accessors. Once you see that level 0's
   span is computed rather than stored, the rest of the file's insistence on
   `zslGetNodeSpanAtLevel` stops looking like ceremony.
3. **`t_zset.c:250-260`** (Step 2) — three lines of randomness, the entire
   balance strategy.
4. **`t_zset.c:265-321`** (Step 6) — `zslInsertNode`. Work the example in Step
   6 by hand and check your spans against lines 304-305.
5. **`t_zset.c:645-685`** (Steps 4-5) — the two rank algorithms side by side.
   The second one is the payoff for the metadata word in Step 4.
6. **Aha: `t_zset.c:5482`** — `zslDebugVerifyStruct`. Every invariant this
   chapter states, written as assertions. Port it into your own
   implementation before you port anything else.
7. **`zset_btree.c:23-138`, then `3316-3328` and `3898-3920`** (Step 8) — the
   design comment, then the two rank algorithms again, over a B+ tree. Put
   them next to `t_zset.c:672-685` and `688-705`.

**Contrast case.** Compare `zslInsertNode`'s span bookkeeping with what
`InlineSkipList` does at the equivalent moment (next chapter): nothing, because
it has no spans to keep. Then ask what ZRANK would cost without them — an O(n)
walk of level 0, or a second index — and you have priced the feature.

## Questions to answer in notes.md

1. Why does the skiplist design need *both* the skiplist and a dict? Look at
   `t_zset.c:54-65` and `:3429` before answering the memory half: the dict
   stores node pointers with `.no_value = 1`, so what exactly is duplicated
   and what is not? State the RUM trade-off in one sentence — then say which
   part of that duplication `zset_btree.c`'s member index (comment at 72-92)
   removes.
2. Derive the expected search cost at p = 0.25 and check the derivation in
   Step 3: log₄(n) levels × (1−p)/p forward steps. Do it for n = 1e6 and
   n = 1e7, price both with topic 0's ~100 ns cold-miss figure, then say why
   the resulting number is an upper bound rather than an estimate.
3. Level 0's span field holds `zskiplistNodeInfo` instead of a span
   (`server.h:1865`, `t_zset.c:75-81`). What does that buy, what does it cost,
   and what would break if someone wrote `x->level[0].span = 1` directly?
4. `zslGetRankByNode` (`t_zset.c:672-685`) walks *forward* to the tail rather
   than descending from the header. Why is that the same O(log n), and which
   property of the height distribution makes it so? `zbtScoreRank`
   (`zset_btree.c:3316-3328`) walks *up*; which property of a B+ tree makes
   that one O(log n)?
5. Your own skiplist has to choose a node layout. Redis makes one allocation
   containing score, levels and the member string (`t_zset.c:169-205`). Write
   down what you will do instead and what it costs you in cache misses per
   comparison — this is the notes.md line "Implementation trade I chose for
   skiplist node layout, and why".

## Takeaway

A skiplist is not competitive with a hash table on point lookups and is not
trying to be — topic 0 measured the gap at 3-5× against ordered structures
generally. It is competitive on *what else the descent can carry*. Redis hangs
three things on a traversal it was doing anyway: exact rank (spans), reverse
iteration (backward pointers) and node metadata (a repurposed dead word).
Every one of them costs multi-pointer updates, which is precisely the currency
a concurrent implementation cannot spend. And the counting idea outlives the
structure: the B+ trees that store sorted sets in redis and Valkey keep a
count per child slot and get the same O(log n) rank from a tree four loads
deep instead of thirty.

## Done when

Answer each before unfolding it.

- [ ] You can explain spans in two sentences, and say what they cost.

  <details><summary>Answer</summary>

  A span is the number of level-0 nodes a given forward link jumps over, so
  summing the spans of the links you traverse during an ordinary descent gives
  you the rank of where you landed — ZRANK and ZRANGE-by-index in O(log n)
  with no auxiliary structure (`zslGetRank`, `t_zset.c:645-662`). The cost is
  one `unsigned long` per forward link, plus the obligation that every insert
  and delete keep every affected span exact: `t_zset.c:304-305` for the
  spliced levels, `:309-311` for the levels above the new node's height, and
  `:288-296` when the list grows taller.

  </details>

- [ ] You can say where the member string lives and why the answer is not "in an `sds ele` field".

  <details><summary>Answer</summary>

  There is no `ele` field. `zslCreateNode` (`t_zset.c:169-205`) computes
  `node_size + sds_buf_size` and makes a single allocation (line 181) holding
  the score, the backward pointer, `level[0..h−1]`, and a copy of the member
  placed in-line with `sdsnewplacement` (line 193). The byte offset from the
  node's start to the string is stored in `zskiplistNodeInfo.sdsoffset`
  (`server.h:1867`) and read back by `zslGetNodeElement` (`t_zset.c:129-133`).

  The payoff is one cache miss instead of two when a comparison finally has to
  look at the key — every `zslCompareWithNode` (`t_zset.c:120-126`) that gets
  past the score check reads a string already on a line the node touched.

  </details>

- [ ] You can explain why level 0's `span` field does not hold a span.

  <details><summary>Answer</summary>

  Because a level-0 link always jumps exactly one node, so the value is
  constant and storing it is waste. Redis puts a `zskiplistNodeInfo`
  (`sdsoffset`, `levels`, `reserved` — `server.h:1866-1870`) in that word
  instead, guarded by a `static_assert` that it fits (`t_zset.c:102`).
  `zslGetNodeSpanAtLevel` therefore *computes* level 0's span — 1, or 0 at the
  tail (`t_zset.c:78-80`) — and the setter, incrementer and decrementer all
  skip level 0 (`:85`, `:91`, `:97`).

  It buys the node's height and its string offset for free, which is what
  makes `zslGetRankByNode` (`t_zset.c:678`) and `zslGetNodeElement` possible.
  It costs one indirection on every span access and one very sharp edge:
  writing `x->level[0].span` directly would silently corrupt the node's height
  and string offset at once.

  </details>

- [ ] You can derive the search cost at p = 0.25 rather than quoting it.

  <details><summary>Answer</summary>

  A level-k lane holds about n·p^k nodes, so the tower is
  log_{1/p}(n) levels tall: at n = 1e6 and p = 1/4 that is
  ln(1e6)/ln(4) = 13.8155/1.3863 = **9.97** levels. Within a level you expect
  to pass (1−p)/p = 0.75/0.25 = **3.0** nodes before the next one is tall
  enough to have appeared on the lane above. Total forward hops:
  9.97 × 3.0 = **29.9 ≈ 30**; at n = 1e7, 11.63 × 3.0 ≈ 35.

  Only forward hops are dependent loads — dropping a level reads
  `level[i−1]` in the node you are already standing on (`server.h:1880`).
  Multiplying 30 × ~100 ns gives ~3.0 µs, which is an *upper* bound assuming
  every hop misses to DRAM; the top of the tower is walked by every search and
  stays cached, so the real figure is much lower. The comparison that matters
  is structural: tens of dependent misses here against two for hashbrown.

  </details>

- [ ] You can name the features single-threading buys, and predict which ones the next chapter loses.

  <details><summary>Answer</summary>

  Redis is single-threaded on the data path, so an operation may touch many
  pointers before anyone else looks. That affords: **backward pointers**
  (`server.h:1874`), which make a reverse range a plain tail walk; **spans**,
  which require updating O(log n) counters per insert (`t_zset.c:299-311`);
  **deletes**, which require the same algebra in reverse
  (`zslUnlinkNode`, `:345-366`); and **in-place score updates** that keep the
  node's address stable so the dict's pointer stays valid (`:396-430`).

  RocksDB's `InlineSkipList` supports concurrent writers, so it drops all
  four: no backward pointers, no spans, no deletes, no repositioning. Its
  insert is a per-level CAS on a single `next` pointer, which is the largest
  update it can make atomically. Concurrency does not make the structure
  faster; it makes it do less.

  </details>

- [ ] You can say where a stored sorted set keeps its rank counts in redis and in Valkey, and why the B+ tree gets rank in fewer dependent loads.

  <details><summary>Answer</summary>

  In redis, a large sorted set is `OBJ_ENCODING_BTREE`; its inner pages hold
  `child_count[ZBT_SCORE_INNER_MAX]` beside the child pointers and max scores
  (`zset_btree.c:244`), and each node's header holds its own `subtree` count
  (line 214). `zbtScoreRank` (3316-3328) adds the counts of complete left
  siblings from the element's leaf to the root; `zbtreeIteratorSeekRank`
  (3898-3920) subtracts them on the way down. An insert refreshes one count
  per level in `zbtScoreUpdatePath` (918-934). In Valkey the same field is
  `child_sizes[NODE_SIZE]` in `innerNode` (`fbtree_internal.h:51`), summed at
  `fbtree.c:1188`.

  The difference in cost is shape, not idea. With 96-element leaves and
  fanout 32, a million elements is 10,417 leaves under
  ⌈log₃₂(10,417)⌉ = ⌈2.67⌉ = 3 inner levels, so a rank seek is 4 dependent
  loads; the skiplist's descent is ≈ 30 forward hops (Step 3). The counts in a
  page sit in one contiguous `uint64_t` array, so summing up to 31 of them is a
  sequential read of at most 248 bytes, not a pointer chase.

  </details>

## References

**Code**
- [redis](https://github.com/redis/redis) — pinned at `unstable` /
  `e87dda2a4` (2026-10-26). The skiplist is `src/t_zset.c`, its structs are
  in `src/server.h`; the stored encoding is `src/zset_btree.c`.

| File | Lines | What |
|---|---|---|
| `src/t_zset.c` | 20-23 | the skiplist is temporary; stored zsets are B+ trees |
| `src/server.h` | 693-694 | `ZSKIPLIST_MAXLEVEL` = 32, `ZSKIPLIST_P` = 0.25 |
| `src/server.h` | 1865-1870 | `zskiplistNodeInfo` — the word stored in level[0].span |
| `src/server.h` | 1872-1882 | `zskiplistNode`; note the absent `ele` field |
| `src/t_zset.c` | 54-65 | `zsetDictType` — the dict indexes node pointers |
| `src/t_zset.c` | 75-99 | span accessors; level 0 synthesised, never written |
| `src/t_zset.c` | 102 | the `static_assert` that makes the repurposing legal |
| `src/t_zset.c` | 169-205 | `zslCreateNode` — one allocation, embedded sds |
| `src/t_zset.c` | 250-260 | `zslRandomLevel` |
| `src/t_zset.c` | 265-321 | `zslInsertNode` — the descent and all three span cases |
| `src/t_zset.c` | 326-339 | `zslInsert` — the wrapper that draws the height |
| `src/t_zset.c` | 396-430 | `zslUpdateScore` — O(1) when order is unchanged |
| `src/t_zset.c` | 645-662 | `zslGetRank` |
| `src/t_zset.c` | 672-685 | `zslGetRankByNode` — no string comparisons |
| `src/t_zset.c` | 5482 | `zslDebugVerifyStruct` — the invariants as assertions |
| `src/dict.h` | 15-23 | why a `no_value` dict needs no `dictEntry` allocation |
| `src/object.h` | 81 | `OBJ_ENCODING_BTREE` |
| `src/zset_btree.c` | 23-138 | design comment: score tree, member index, leaf IDs |
| `src/zset_btree.c` | 212-246 | node header, leaf, inner page with `child_count` |
| `src/zset_btree.c` | 918-934 | `zbtScoreUpdatePath` |
| `src/zset_btree.c` | 3316-3343 | `zbtScoreRank`, `zbtreeRank` |
| `src/zset_btree.c` | 3898-3920 | `zbtreeIteratorSeekRank` |

- [valkey](https://github.com/valkey-io/valkey) — pinned at `unstable` /
  `668d875ca`. `src/server.h:1613-1616` (a zset is a hashtable plus an
  `OrderedIndex`), `src/ordered_index.h` (the interface),
  `src/fbtree_internal.h:41-53` and `70-71` (the inner node and its size
  asserts), `src/fbtree.c:1174-1201` (rank of an item).

**Measured in this repo**
- [FINDINGS.md](../../FINDINGS.md) row 0 — the ~1 / 5 / 100 ns cache ladder
  used to price the descent.
- [topics/00-performance-toolbox/notes.md](../00-performance-toolbox/notes.md)
  — `lookup_shootout` at n = 1e6: HashMap 8.8 ns, BTreeMap 26.6 ns, sorted-vec
  binary search 25.8 ns. The ordered-structure penalty, measured.

**Companion chapters**
- [reading-rocksdb-memtable.md](reading-rocksdb-memtable.md) — the same
  structure with concurrent writers, and the features that removes.
- [reading-hashbrown.md](reading-hashbrown.md) — the two-cache-line point
  lookup this chapter is compared against.
- [reading-redis-dict.md](reading-redis-dict.md) — the other half of the
  skiplist design.
- [Topic 3](../03-btree-internals/README.md) — B+ trees on disk, where fanout
  is set by the page size instead of an allocator size class.
