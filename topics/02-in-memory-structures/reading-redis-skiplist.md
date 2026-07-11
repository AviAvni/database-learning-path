# Reading redis `t_zset.c` — the skiplist with rank queries

Files: `~/repos/redis/src/t_zset.c`, struct defs in `src/server.h`. This is the
skiplist behind ZADD/ZRANGE/ZRANK — the canonical readable implementation.

## 1. The structs — server.h:1699–1716

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

Two things beyond the textbook skiplist:
- **`span`** — each forward link records how many level-0 nodes it skips. Summing
  spans during a descent = the node's **rank**, for free. That's ZRANK/ZRANGE-by-index
  in O(log n) without any extra structure.
- **`backward`** — level-0 only, making reverse range queries (ZREVRANGE) a plain
  list walk from the tail.

## 2. Height selection — t_zset.c:254

`zslRandomLevel()`: geometric with **p = 0.25** (`ZSKIPLIST_P`, server.h:630), max
level 32. Compare: RocksDB uses branching factor 4 (same p) but caps at 12. Question:
expected pointers per node at p=0.25? (1/(1−p) = 1.33 — vs 2 for a binary tree.)

## 3. `zslInsert` — t_zset.c:265–339

The heart. The descent records, per level:
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

Note the span bookkeeping at t_zset.c:304–305: levels *above* the new node's height
don't get a new link, but their spans still grow by one — subtle, and the kind of
invariant your own implementation will get wrong first try.

## 4. What redis does NOT do

No locks, no CAS — redis is single-threaded on the data path, so this skiplist is
free to use backward pointers and spans (both hard to maintain lock-free). Contrast
with RocksDB's `InlineSkipList` (concurrent writers ⇒ no backward pointers, no spans,
no deletes). Concurrency *removes* features — a theme topic 9 makes precise.

## Questions to answer in notes.md

1. Why does the zset need *both* the skiplist and a dict (score lookup by member)?
   What does that cost in memory, and what's the RUM read?
2. Derive the expected search cost at p=0.25: levels × nodes-per-level ≈
   log₄(n) × ~3 compares. At n=1M: ~30 dependent pointer hops — now price it with
   topic 0's ladder (30 × ~100ns if cold). Compare your measured number.

## Done when

You can explain spans to someone in two sentences, and you know which features your
experiment's skiplist can steal (backward/span) vs what RocksDB's concurrency forbids.
