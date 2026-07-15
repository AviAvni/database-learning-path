# Two concurrent skiplists: CAS vs lazy locking

Same structure, two schools of coordination: RocksDB's memtable skiplist
links nodes with per-level CAS and never deletes; memgraph's skiplist — the
spine of its whole graph store — uses per-node spinlocks, state bits, and
real deletion with GC. Before you open either file, this chapter builds
both designs one concept at a time — the skiplist shape, the CAS toolkit,
each school's insert protocol, and the deletion problem only one of them
has to solve — then hands you the line anchors to watch each piece in
production code. Read RocksDB first (you know this file from topic 2 — now
the concurrency), then memgraph as the contrast.

## The problem in one sentence

Keep one sorted in-memory structure correct while 32 writer threads insert
into it and readers traverse it at full speed — a single mutex around it
caps a 32-core machine at the throughput of one core, so both designs
coordinate at the granularity of individual pointers instead.

## The concepts, step by step

### Step 1 — the skiplist: a sorted linked list with express lanes

A **skiplist** is a sorted linked list where each node also gets a random
number of stacked "express lane" links — a **tower**. A node's tower height
is chosen by coin flips at creation (height ≥ h with probability ~2⁻ʰ), so
level 1 skips ~half the nodes, level 2 skips ~three quarters, and so on:

```
 level 3:  head ─────────────────────► 50 ──────────────────────► nil
 level 2:  head ─────────► 20 ───────► 50 ─────────► 80 ────────► nil
 level 1:  head ──► 10 ──► 20 ──► 30 ─► 50 ──► 60 ──► 80 ──► 90 ─► nil
           (level 1 = every node; search: top-left, go right until
            you'd overshoot, drop a level — ~2·log₂ n ≈ 40 hops at 1M keys)
```

Why both engines picked it for concurrency: a B-tree (topic 1) keeps sorted
order by shifting rows inside pages and splitting full pages — bulk moves of
existing data. A skiplist keeps order purely with pointers, so an insert
never moves anything that already exists: it is one pointer swing per level,
and single pointer swings are exactly what atomic hardware instructions can
do. The cost: ~40 *dependent* pointer hops per search — up to 40 potential
cache misses (topic 0) — versus a B-tree's few cache-friendly binary
searches. Step 5 shows RocksDB clawing that back.

### Step 2 — CAS and the publication idiom

**CAS** (compare-and-swap) is the atomic CPU instruction "replace this one
64-bit word with a new value only if it still equals the value I read" — if
another thread changed it in between, the CAS fails and you retry. Three
**memory orderings** appear in every listing below: **Relaxed** (the write
is atomic but promises nothing about *other* writes), **Release** (all my
earlier writes become visible to anyone who reads this value), and
**Acquire** (I see all writes that happened before the Release I just
read).

Together they form the **publication idiom**: build your object privately
with plain/Relaxed writes, then *publish* it with one Release operation;
readers Acquire-load and are guaranteed to see a fully-built object. The
catch that splits the two schools: CAS swings ONE word, but a height-4
tower is four links — a multi-pointer insert cannot be atomic, so each
school must decide what readers are allowed to see in between.

### Step 3 — the CAS school: link one level at a time (RocksDB)

RocksDB's answer: don't make the tower atomic — link it bottom-up, one CAS
per level, and let readers see partial towers. `CASNext` (:393) is the
linking primitive — one `compare_exchange_strong` per level. Per level:
read pred/succ, set `new->next = succ` (Relaxed — unpublished, so a plain
write is fine), CAS `pred->next` from succ to new (Release — the publish);
on failure, re-find just that level and retry:

```rust
fn link_at_level(mut pred: &Node, new: &Node, lvl: usize) {
    loop {
        let succ = pred.next[lvl].load(Acquire);
        new.next[lvl].store(succ, Relaxed);   // unpublished yet: plain write
        if pred.next[lvl]
            .compare_exchange(succ, new, Release, Relaxed) // publish
            .is_ok() { return; }
        pred = refind_pred(new.key, lvl);     // lost the race — re-find
    }                                         // ONLY this level, then retry
}
```

Why bottom-first makes partial towers harmless *for a set*: level 1 (every
node) is the ground truth, and the node is findable the instant its
bottom link lands — upper levels are only shortcuts, so a reader that
doesn't see node 35 at level 2 yet still finds it at level 1:

```
 inserting 35, tower height 3:
 level 3:   20 ─────────────► 50        (not linked yet — readers skip 35 here)
 level 2:   20 ────► 35 ────► 50        (linked)
 level 1:   30 ────► 35 ────► 50        (linked FIRST — 35 is now findable)
```

The cost profile: a lost race costs a re-find of *one level* (a few hops),
never a full restart, and no thread ever waits for another. The contract
comment (:23) states the guarantee: `InsertConcurrently` is safe with
concurrent reads AND writes.

### Step 4 — what the workload let RocksDB not build: deletion

The same contract (:23) hides the enabling assumption: memtable entries
are **never deleted**. In the LSM (topic 4), a delete is a *tombstone
insert*, and the whole memtable dies wholesale at flush — its arena is
freed in one shot. No delete ⇒ no "when may I `free()` a node some reader
still holds?" problem ⇒ no epochs, no GC, nothing. This is why the crate
you'll read next (crossbeam-epoch) is absent here. The discipline to carry
into every code read: always ask **"what did the workload let them NOT
solve?"**

### Step 5 — the splice: amortizing the search across nearby inserts

The search (Step 1's ~40 dependent hops) dwarfs the CASes (one per level),
so RocksDB caches it. A `Splice` (:64) is a cached array of (pred, succ)
per level left over from the previous insert; sequential writers reuse it
(`Insert(key, splice, ...)` :1028, hint variant :113) and
`RecomputeSpliceLevels` (:331/:1016) repairs only the levels the new key
invalidated — nearby keys share most of their path, so most levels survive.
Amortize the O(log n) search across nearby inserts.

One more workload door: `Insert` :908 vs `InsertConcurrently` :913 are the
same template with a `UseCAS` flag — single-writer mode skips the atomics
entirely, a *compile-time* choice. (M9 note: FalkorDB's single writer can
take exactly this door.)

### Step 6 — the locking school: lazy locking (memgraph, Herlihy et al.)

memgraph takes the other road: make the whole tower appear atomically by
briefly locking the neighbors. A **spinlock** is a lock you busy-wait on
instead of sleeping — right for critical sections measured in nanoseconds.
Each Node (:156) carries a per-node `SpinLock` (:163), TWO state bits —
`marked` (:164) and `fully_linked` (:165) — and the flexible-array tower
`nexts[0]` (:169), the same intrusive-tower trick as RocksDB.

Insert (:1335) is **optimistic**: `find_node` (:1285) collects preds/succs
with *no* locks held, then LOCKs the preds bottom-up, **re-validates**
(each pred still points at its succ, nobody got marked in between — the
optimistic read may be stale), links ALL levels while holding the locks,
and finally PUBLISHes with `fully_linked.store(true, release)` (:1398).
Readers ignore half-linked nodes — the publication idiom from Step 2, with
a bit instead of a CAS'd pointer, and the entire tower appears at once.
The cost profile flips Step 3's: a failed validation unlocks everything
and restarts the whole insert (vs the CAS school's one-level re-find), and
lock-order discipline (always bottom-up) is what prevents deadlock.

### Step 7 — real deletion, and the scorecard

memgraph must delete for real, and it does it in two phases. Remove
(:1655): lock, then `marked.store(true, release)` (:1672) — **logical
delete** first (readers skip marked nodes, so the node vanishes from the
set before any pointer moves), THEN physically unlink. Deletion exists
here, so reclamation must too — the problem RocksDB dodged in Step 4:

- **Accessor-id GC** (:244–246, `SkipListGc` :257, `Collect` :367): every
  `Accessor` (:877) gets a monotonically increasing id; a retired node
  records the newest alive accessor id; free when all older accessors
  are gone. Epoch reclamation with transaction-scoped pins — compare
  crossbeam's 3-epoch scheme; same idea, coarser pin.
- `kSkipListGcHeightTrigger` (:69) and `create_chunks` (:817–955 —
  chunked parallel iteration for analytics) show this is the SPINE of
  memgraph: vertices, edges, and indexes all live in these lists.

The comparison table (fill it in notes.md):

| | RocksDB | memgraph |
|---|---|---|
| writers coordinate by | CAS per level | per-node spinlocks |
| readers see partial insert? | yes — per-level linking is independent (fine for a set) | no — fully_linked gate |
| delete | never (tombstones) | marked bit + unlink |
| reclamation | none needed (arena dies at flush) | accessor-id GC |
| failure/retry | re-find level, re-CAS | unlock all, restart |

## Where each step lives in the code

Read RocksDB first, then memgraph as the contrast.

**RocksDB InlineSkipList** —
[`~/repos/rocksdb/memtable/inlineskiplist.h`](https://github.com/facebook/rocksdb)

- **Steps 3–4**: start at the contract comment (:23) — the guarantee AND
  the never-delete assumption in one place; then `CASNext` (:393), the
  one-CAS-per-level linking primitive behind `link_at_level` above.
- **Step 5**: `Splice` (:64); `Insert(key, splice, ...)` (:1028) and the
  hint variant (:113); `RecomputeSpliceLevels` (:331/:1016); the
  `Insert` :908 vs `InsertConcurrently` :913 template pair with the
  `UseCAS` flag.

**memgraph SkipList** —
[`~/repos/memgraph/src/utils/skip_list.hpp`](https://github.com/memgraph/memgraph)
— one header holds the list, the accessors, and the GC.

- **Step 6**: Node (:156), `SpinLock` (:163), `fully_linked` (:165), tower
  `nexts[0]` (:169); insert (:1335) via `find_node` (:1285); the publish
  at (:1398).
- **Step 7**: `marked` (:164); Remove (:1655) with the logical delete at
  (:1672); GC at (:244–246), `SkipListGc` (:257), `Collect` (:367),
  `Accessor` (:877); `kSkipListGcHeightTrigger` (:69); `create_chunks`
  (:817–955).

## Questions for notes.md

1. RocksDB dodged reclamation via arena-per-memtable. What's the graph
   equivalent — arena per matrix version? Does M8's CoW give M9 the same
   dodge (old version dies wholesale when last reader leaves)?
2. Why does the lazy list lock preds BOTTOM-up and validate after
   locking? Construct the lost-insert without validation.
3. A splice cache assumes locality of consecutive inserts. Does a graph
   bulk-load (sorted node ids) hit that path? What about random edges?
4. Which school for YOUR concurrent_set.rs — and what does crossbeam-epoch
   give you that lets you pick CAS *with* deletion (the combination
   neither production list needed)?

## Done when

You can fill the table from memory and explain what each system's
workload allowed it to NOT build.

## References

**Papers**
- Herlihy, Lev, Luchangco, Shavit — "A Simple Optimistic Skiplist
  Algorithm" (SIROCCO 2007) — the lazy-locking design memgraph implements

**Code**
- [rocksdb](https://github.com/facebook/rocksdb)
  `memtable/inlineskiplist.h` — start at the :23 contract comment
- [memgraph](https://github.com/memgraph/memgraph)
  `src/utils/skip_list.hpp` — one header holds the list, the accessors,
  and the GC
