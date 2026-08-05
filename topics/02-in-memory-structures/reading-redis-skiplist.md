# The redis skiplist: spans make rank queries free

The canonical readable skiplist — the structure behind ZADD/ZRANGE/ZRANK in
`t_zset.c` — with one addition the textbooks skip: every forward link records
how many level-0 nodes it jumps over, so summing spans during an ordinary
descent yields a node's rank at no extra cost. This chapter builds the
structure from a plain sorted list upward — express lanes, the descent, spans,
the insert bookkeeping — then anchors each piece in the source. Read it before
the RocksDB memtable chapter to see what a skiplist looks like when
concurrency is not allowed to take features away.

Every anchor below is Redis **8.6.2** (`src/version.h:1`), the commit
`a176d1225` this repo pins. That matters more here than in most chapters,
because this file is not the 2009 skiplist most write-ups describe. Three
things have changed and all three are load-bearing: the node no longer holds
an `sds ele` pointer (the string is *embedded* in the node allocation), the
`span` field at level 0 has been **repurposed** to hold node metadata, and the
zset's dict now stores skiplist *node pointers* rather than member/score
copies. Where this chapter contradicts an older account, check it yourself:

```
tools/pinned-source.py show redis src/server.h -r 1690:1716
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
// redis@a176d1225 — src/t_zset.c:250-260, zslRandomLevel
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
`ZSKIPLIST_P = 0.25` (`server.h:630`); line 259 is the cap,
`ZSKIPLIST_MAXLEVEL = 32` (`server.h:629`, commented "Should be enough for
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
is a flexible array inside the node's own allocation (`server.h:1707`), so
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
// redis@a176d1225 — src/server.h:1690-1709, the node and its info word
  1690  /* ZSETs use a specialized version of Skiplists */
  1691
  1692  /* Node info placed in level[0].span since it's unused at level 0 (static assert verified) */
  1693  typedef struct zskiplistNodeInfo {
  1694      uint16_t sdsoffset;  /* Offset from node start to sds data (after sds header) */
  1695      uint8_t levels;      /* Number of levels in this node (1-32) */
  1696      uint8_t reserved;
  1697  } zskiplistNodeInfo;
  1698
  1699  typedef struct zskiplistNode {
  1700      double score;
  1701      struct zskiplistNode *backward;
  1702      struct zskiplistLevel {
  1703          struct zskiplistNode *forward;
  // ... 1704-1705: comment reproduced in the prose below ...
  1706          unsigned long span;
  1707      } level[];
  1708      /* sds ele is embedded after level[] array (assist zslGetNodeElement(node) to access it) */
  1709  } zskiplistNode;
```

Note what is *not* there: no `sds ele` field. The member string lives inside
the same allocation, past the end of `level[]` (line 1708), reached through
the byte offset stored in `zskiplistNodeInfo.sdsoffset`. One `zmalloc` per
node holds score, backward pointer, the whole level array and the string —
which is the difference between one cache miss and two when you finally
compare the key.

And the trick that pays for that offset: **level 0's span is always 1**, so
the field is dead weight, so redis puts the node metadata there instead.

```c
// redis@a176d1225 — src/t_zset.c:75-81 and 101-104, span synthesised at level 0
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

Level 0 is doubly linked: `backward` (`server.h:1701`) makes ZREVRANGE a plain
walk from `zsl->tail`, with no descent and no cleverness. Only level 0 gets
it; higher levels would double the pointer cost for no query redis runs.

The backward pointer plus spans also enable a rank algorithm the textbooks do
not have — one that avoids string comparison entirely:

```c
// redis@a176d1225 — src/t_zset.c:672-685, zslGetRankByNode
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
lane, sums the spans it crosses, and subtracts from `zsl->length`. Same
O(log n), but zero `sdscmp` calls — the doc comment at 664-671 says so
outright. It is only possible because `levels` is stored in the node
(Step 4's repurposed word) and because spans are exact.

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
node (336) and delegates — an earlier version of this chapter cited
"`zslInsert`, t_zset.c:265-339", which merges the two functions; 265 is
`zslInsertNode` and 326 is `zslInsert`.

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
// redis@a176d1225 — src/t_zset.c:298-311, splice and span fix-up
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
at `t_zset.c:4817`, worth reading before you debug your own.

Lines 309-311 are the case people get wrong first: levels *above* the new
node's height get no new link, but a node now exists underneath them, so
their spans still grow by one. (An earlier version of this chapter cited
304-305 for this; that pair is the splice arithmetic, and the above-height
increment is 309-311.) There is a third case at `t_zset.c:288-296`: when the
new node is taller than the list has ever been, the header's links at the new
levels are created with span `zsl->length` — they leap the entire existing
list.

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

And the second structure in a zset is now cheaper than the folklore says. The
dict does not store a copy of the member and a copy of the score; it stores
the **node pointer**:

```c
// redis@a176d1225 — src/t_zset.c:53-64, the zset's dict is a set of node pointers
    53  /* dictType for zset's dict (maps sds to zskiplistNode*) */
    54  dictType zsetDictType = {
    55      dictSdsHash,        /* hash function */
    // ... 56-57: key dup / val dup, both NULL ...
    58      dictSdsKeyCompare,  /* compares embedded sds by keyFromStoredKey */
    59      NULL,               /* key destructor - skiplist owns the node memory */
    // ... 60-61: val destructor, allow-to-expand ...
    62      .no_value = 1,      /* no values stored (only nodes) */
    63      .keyFromStoredKey = zslGetNodeElementForDict,  /* extract embedded sds from node */
    64  };
```

`dictAdd(zs->dict, node, NULL)` at `t_zset.c:1486` inserts the *node* as the
key; `keyFromStoredKey` (line 63) tells the dict how to find the sds inside it
(`zslGetNodeElement`, `t_zset.c:129-133`); line 59 records that the skiplist
owns the memory. Because `.no_value = 1`, the dict can store that pointer
directly in the bucket with no `dictEntry` allocation whenever the bucket
holds one key or the key sits at a chain tail (`dict.h:17-25`). So the index
costs roughly the bucket array plus tag bits — not a second copy of every
member — and a ZSCORE returns the node, from which the score is one field
away.

## Where each step lives in the code

| Lines | What | Step |
|---|---|---|
| `server.h:629-630` | `ZSKIPLIST_MAXLEVEL` 32, `ZSKIPLIST_P` 0.25 | 2 |
| `server.h:1692-1697` | `zskiplistNodeInfo` — what squats in level[0].span | 4 |
| `server.h:1699-1709` | `zskiplistNode` — no `ele` field; sds embedded after `level[]` | 4 |
| `server.h:1711-1716` | `zskiplist` — header, tail, length, level, `alloc_size` | 2 |
| `t_zset.c:53-64` | `zsetDictType` — `no_value`, `keyFromStoredKey` | 7 |
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
| `t_zset.c:4817` | `zslDebugVerifyStruct` — the invariants, as code | 6 |

Read in this order:

1. **`server.h:1690-1716`** (Step 4) — the structs. Ask where the member
   string is before reading line 1708.
2. **`t_zset.c:75-114`** (Step 4) — the accessors. Once you see that level 0's
   span is computed rather than stored, the rest of the file's insistence on
   `zslGetNodeSpanAtLevel` stops looking like ceremony.
3. **`t_zset.c:250-260`** (Step 2) — three lines of randomness, the entire
   balance strategy.
4. **`t_zset.c:265-321`** (Step 6) — `zslInsertNode`. Work the example in Step
   6 by hand and check your spans against lines 304-305.
5. **`t_zset.c:645-685`** (Steps 4-5) — the two rank algorithms side by side.
   The second one is the payoff for the metadata word in Step 4.
6. **Aha: `t_zset.c:4817`** — `zslDebugVerifyStruct`. Every invariant this
   chapter states, written as assertions. Port it into your own
   implementation before you port anything else.

**Contrast case.** Compare `zslInsertNode`'s span bookkeeping with what
`InlineSkipList` does at the equivalent moment (next chapter): nothing, because
it has no spans to keep. Then ask what ZRANK would cost without them — an O(n)
walk of level 0, or a second index — and you have priced the feature.

## Questions to answer in notes.md

1. Why does a zset need *both* the skiplist and a dict? Look at
   `t_zset.c:53-64` and `:1486` before answering the memory half: the dict
   stores node pointers with `.no_value = 1`, so what exactly is duplicated
   and what is not? State the RUM trade-off in one sentence.
2. Derive the expected search cost at p = 0.25 and check the derivation in
   Step 3: log₄(n) levels × (1−p)/p forward steps. Do it for n = 1e6 and
   n = 1e7, price both with topic 0's ~100 ns cold-miss figure, then say why
   the resulting number is an upper bound rather than an estimate.
3. Level 0's span field holds `zskiplistNodeInfo` instead of a span
   (`server.h:1692`, `t_zset.c:75-81`). What does that buy, what does it cost,
   and what would break if someone wrote `x->level[0].span = 1` directly?
4. `zslGetRankByNode` (`t_zset.c:672-685`) walks *forward* to the tail rather
   than descending from the header. Why is that the same O(log n), and which
   property of the height distribution makes it so?
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
a concurrent implementation cannot spend.

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
  (`server.h:1694`) and read back by `zslGetNodeElement` (`t_zset.c:129-133`).

  The payoff is one cache miss instead of two when a comparison finally has to
  look at the key — every `zslCompareWithNode` (`t_zset.c:120-126`) that gets
  past the score check reads a string already on a line the node touched.

  </details>

- [ ] You can explain why level 0's `span` field does not hold a span.

  <details><summary>Answer</summary>

  Because a level-0 link always jumps exactly one node, so the value is
  constant and storing it is waste. Redis puts a `zskiplistNodeInfo`
  (`sdsoffset`, `levels`, `reserved` — `server.h:1693-1697`) in that word
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
  `level[i−1]` in the node you are already standing on (`server.h:1707`).
  Multiplying 30 × ~100 ns gives ~3.0 µs, which is an *upper* bound assuming
  every hop misses to DRAM; the top of the tower is walked by every search and
  stays cached, so the real figure is much lower. The comparison that matters
  is structural: tens of dependent misses here against two for hashbrown.

  </details>

- [ ] You can name the features single-threading buys, and predict which ones the next chapter loses.

  <details><summary>Answer</summary>

  Redis is single-threaded on the data path, so an operation may touch many
  pointers before anyone else looks. That affords: **backward pointers**
  (`server.h:1701`), which make ZREVRANGE a plain tail walk; **spans**, which
  require updating O(log n) counters per insert (`t_zset.c:299-311`);
  **deletes**, which require the same algebra in reverse
  (`zslUnlinkNode`, `:345-366`); and **in-place score updates** that keep the
  node's address stable so the dict's pointer stays valid (`:396-430`).

  RocksDB's `InlineSkipList` supports concurrent writers, so it drops all
  four: no backward pointers, no spans, no deletes, no repositioning. Its
  insert is a per-level CAS on a single `next` pointer, which is the largest
  update it can make atomically. Concurrency does not make the structure
  faster; it makes it do less.

  </details>

## References

**Code**
- [redis](https://github.com/redis/redis) — pinned at **8.6.2** /
  `a176d1225` (`src/version.h:1`). The skiplist is `src/t_zset.c`, its structs
  are in `src/server.h`.

| File | Lines | What |
|---|---|---|
| `src/server.h` | 629-630 | `ZSKIPLIST_MAXLEVEL` = 32, `ZSKIPLIST_P` = 0.25 |
| `src/server.h` | 1692-1697 | `zskiplistNodeInfo` — the word stored in level[0].span |
| `src/server.h` | 1699-1709 | `zskiplistNode`; note the absent `ele` field |
| `src/t_zset.c` | 53-64 | `zsetDictType` — the dict indexes node pointers |
| `src/t_zset.c` | 75-99 | span accessors; level 0 synthesised, never written |
| `src/t_zset.c` | 102 | the `static_assert` that makes the repurposing legal |
| `src/t_zset.c` | 169-205 | `zslCreateNode` — one allocation, embedded sds |
| `src/t_zset.c` | 250-260 | `zslRandomLevel` |
| `src/t_zset.c` | 265-321 | `zslInsertNode` — the descent and all three span cases |
| `src/t_zset.c` | 326-339 | `zslInsert` — the wrapper that draws the height |
| `src/t_zset.c` | 396-430 | `zslUpdateScore` — O(1) when order is unchanged |
| `src/t_zset.c` | 645-662 | `zslGetRank` |
| `src/t_zset.c` | 672-685 | `zslGetRankByNode` — no string comparisons |
| `src/t_zset.c` | 4817 | `zslDebugVerifyStruct` — the invariants as assertions |
| `src/dict.h` | 17-25 | why a `no_value` dict needs no `dictEntry` allocation |

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
- [reading-redis-dict.md](reading-redis-dict.md) — the other half of a zset.
