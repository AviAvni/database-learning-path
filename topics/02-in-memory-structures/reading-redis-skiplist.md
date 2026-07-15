# The redis skiplist: spans make rank queries free

The canonical readable skiplist — the structure behind ZADD/ZRANGE/ZRANK in
`t_zset.c` — with one addition the textbooks skip: every forward link records
how many level-0 nodes it jumps over, so summing spans during an ordinary
descent yields a node's rank at no extra cost. This chapter builds the
structure from a plain sorted list upward — express lanes, the descent,
spans, the insert bookkeeping — then anchors each piece in the source. Read
it before the RocksDB memtable chapter to see what a skiplist looks like when
concurrency isn't allowed to take features away.

## The problem in one sentence

A sorted set needs insert, lookup, range-by-score, *and* "what is element
#4,217?" — all in O(log n) — and a plain sorted linked list does every one of
them in O(n): at 1M elements that's ~1M dependent pointer hops, milliseconds
per query.

## The concepts, step by step

### Step 1 — a sorted linked list, and why it's too slow

The simplest ordered structure is a linked list kept in key order: each node
holds a key and a pointer to the next. Ordered iteration and range scans are
trivial — but *finding* anything means walking from the head, one node at a
time. Each hop is a dependent load (the next address comes from the current
node — topic 0's pointer chase), so a search at n=1M costs up to 1M
serialized cache misses. Arrays fix search (binary search) but make insert
O(n) memmove. We want list-like inserts with search that skips ahead.

### Step 2 — express lanes: give random nodes extra levels

A **skiplist** keeps the sorted level-0 list and adds sparser "express lanes"
above it: each node is assigned a random **height**, and a node of height h
appears in levels 0..h−1. Heights follow a geometric distribution — flip a
biased coin (redis: p = 0.25, `ZSKIPLIST_P`, server.h:630, max level 32,
`zslRandomLevel()` t_zset.c:254) until it fails. So ~1/4 of nodes reach level
1, ~1/16 level 2, and so on:

```
L3 ──────────────────────────────► 42 ─────────────────────────► ∅
L2 ─────────► 17 ─────────────────► 42 ─────────► 71 ──────────► ∅
L1 ─► 8 ────► 17 ────► 29 ────────► 42 ─► 55 ───► 71 ─► 88 ────► ∅
      search 55: descend when next > target — O(log n) expected
```

Expected pointers per node: 1/(1−p) = 1.33 at p=0.25 — cheaper than a binary
tree's 2, and no rebalancing logic exists at all: balance is probabilistic,
not maintained.

### Step 3 — the descent: the one search algorithm for everything

Every skiplist operation starts the same way: begin at the head's top level,
move right while the next node's key is still less than the target, and when
it isn't, drop down one level. At the bottom you're standing immediately
before the target position. Expected cost at p=0.25: ~log₄(n) levels × ~3
compares per level — at n=1M, ~30 dependent pointer hops. Price it with topic
0's ladder: 30 × ~100 ns if every hop misses to DRAM ≈ 3 µs worst case —
that's why the hashbrown chapter's table beats it 5–10× on point lookups, and
why sortedness (not raw speed) is what a skiplist is for.

### Step 4 — spans: count what you skip, and rank is free

A **rank** query ("what is 55's index?", "give me elements 100–110") needs to
know *how many* level-0 nodes each express-lane jump flew over. Redis stores
exactly that: each forward link carries a **span** — the number of level-0
nodes it skips. The structs (server.h:1699–1716):

```c
typedef struct zskiplistNode {
    sds ele; double score;
    struct zskiplistNode *backward;      // level-0 doubly-linked
    struct zskiplistLevel {
        struct zskiplistNode *forward;
        unsigned long span;              // # of L0 nodes this link jumps over
    } level[];                           // flexible array: height varies per node
} zskiplistNode;
```

Now the ordinary descent computes rank as a side effect — sum the spans of
every link you traverse:

```rust
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

That's ZRANK and ZRANGE-by-index in O(log n) with zero extra structure — the
descent was happening anyway. The cost: every insert and delete must keep
every affected span exact (Step 6).

### Step 5 — backward pointers: reverse ranges as a list walk

Level 0 is doubly linked: each node's `backward` pointer makes ZREVRANGE a
plain walk from the tail — no descent, no cleverness. Only level 0 gets this
(higher levels would double the pointer cost for no query redis runs). Note
for later: a backward pointer is a *second* pointer that must be updated
atomically-with the forward one — trivial single-threaded, poison for
lock-free designs (Step 7).

### Step 6 — insert: remember the splice points on the way down

`zslInsert` (t_zset.c:265–339) is the heart. One descent records, per level:

- `update[i]` — the rightmost node at level i that precedes the insert point
  (the nodes whose forward pointers must be spliced);
- `rank[i]` — cumulative span up to `update[i]` (so new spans can be computed
  without re-walking).

```
insert 55, height 2:                       update[] captured on the way down
L2 ──────► 17 ────────────────► 71        update[2]=17  rank[2]=2
L1 ──────► 17 ────► 42 ─[55]──► 71        update[1]=42  rank[1]=3   splice
L0 ─► 8 ─► 17 ─► 29 ─► 42 ─[55]► 71       update[0]=42  rank[0]=3   splice
                                           levels above height: span += 1 only
```

Then splice the new node in at each level ≤ its height, computing its spans
from the rank differences. Note the span bookkeeping at t_zset.c:304–305:
levels *above* the new node's height don't get a new link, but their spans
still grow by one — a node now exists underneath them. Subtle, and the kind
of invariant your own implementation will get wrong first try.

### Step 7 — what single-threading buys: features

No locks, no CAS (compare-and-swap — the atomic instruction lock-free
structures are built from) — redis is single-threaded on the data path, so
this skiplist is free to use backward pointers and spans, both of which
require multi-pointer updates that are hard to make atomic without locks.
Contrast with RocksDB's `InlineSkipList` (next chapter): concurrent writers
⇒ no backward pointers, no spans, no deletes. Concurrency *removes* features
— a theme topic 9 makes precise.

## Where each step lives in the code

- **Steps 2, 4, 5** — the structs: `zskiplistNode` / `zskiplistLevel` —
  server.h:1699–1716; `span` and `backward` fields.
- **Step 2** — `zslRandomLevel()` — t_zset.c:254; `ZSKIPLIST_P` (0.25) —
  server.h:630, max level 32. Compare: RocksDB uses branching factor 4 (same
  p) but caps at 12.
- **Steps 3–4** — the descent pattern opens every zsl function; rank
  accumulation visible in `zslGetRank` and inside `zslInsert`.
- **Step 6** — `zslInsert` — t_zset.c:265–339; the above-height span
  increment at t_zset.c:304–305.

## Questions to answer in notes.md

1. Why does the zset need *both* the skiplist and a dict (score lookup by member)?
   What does that cost in memory, and what's the RUM read?
2. Derive the expected search cost at p=0.25: levels × nodes-per-level ≈
   log₄(n) × ~3 compares. At n=1M: ~30 dependent pointer hops — now price it with
   topic 0's ladder (30 × ~100ns if cold). Compare your measured number.

## Done when

You can explain spans to someone in two sentences, and you know which features your
experiment's skiplist can steal (backward/span) vs what RocksDB's concurrency forbids.

## References

**Code**
- [redis](https://github.com/redis/redis) `src/t_zset.c` (zslInsert,
  zslRandomLevel) — struct definitions in `src/server.h:1699–1716`
