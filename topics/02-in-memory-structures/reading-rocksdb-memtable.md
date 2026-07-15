# InlineSkipList: lock-free by refusing to delete

This is where LSM write throughput lives: every `Put` in half the industry
lands in this one header. Two ideas are the whole file — a node layout that
puts the hot pointer and the key on the same cache line by indexing the tower
*negatively*, and a concurrency contract kept simple by one workload
restriction: memtables never delete, they freeze and drop wholesale. This
chapter builds up to both — what a memtable is, why it's a skiplist, the
layout trick, then the lock-free insert — before pointing you at the lines.
Budget: 1–2 h.

## The problem in one sentence

Eight writer threads must insert into one sorted in-memory structure at
millions of ops/s without a lock serializing them — a single mutex around a
sorted map caps the whole LSM engine at one core.

## The concepts, step by step

### Step 1 — the memtable: the sorted buffer every LSM write hits first

In an LSM engine (topic 1), every write goes to an in-memory buffer — the
**memtable** — which, when full (RocksDB default 64 MB), is **frozen** (made
immutable), flushed to disk as a sorted file, and then dropped wholesale.
Two requirements follow: the structure must support **sorted iteration**
(the flush writes a sorted file), and it must absorb **concurrent inserts**
from many writer threads. One non-requirement matters just as much: it never
needs to *delete* a node — even a user's Delete is an insert (a tombstone
entry); physical removal happens only when the whole frozen memtable is
dropped at once.

### Step 2 — why a skiplist, not a hash table or B-tree

A hash table has no ordered iteration — flushing to a sorted file would
require an O(n log n) sort of 64 MB on every flush. A B-tree keeps order but
inserts trigger node splits — multi-node rewrites that need complex latching
under concurrency. A skiplist (previous chapter) keeps order, and an insert
touches only a handful of *independent* forward pointers — each one a single
word that can be swapped atomically with **CAS** (compare-and-swap: an atomic
CPU instruction that writes a new value only if the location still holds the
expected old value, and reports failure otherwise). Independent single-word
updates are exactly what lock-free programming can handle. That's the whole
case: sortedness + CAS-able inserts.

### Step 3 — the node layout: one allocation, tower indexed negatively

A textbook skiplist node holds a key pointer and an array of forward
pointers — so a lookup touches the node, then the key, two dependent misses.
InlineSkipList (lines 358–421) makes a node **one allocation, three regions,
with the struct pointing at the middle**:

```
 raw allocation (from concurrent arena, line 860-869):

 ┌──────────────────────────┬───────────────┬─────────────────┐
 │ tower: next_[h-1]…next_[1]│ Node: next_[0]│ key bytes inline│
 └──────────────────────────┴───────────────┴─────────────────┘
                              ▲ Node* points HERE
   levels accessed by NEGATIVE indexing: (&next_[0] - n)      line 383
   key accessed as (&next_[1]):                               line 374
```

Why: the common case (level-0 traversal + key compare) touches `next_[0]` and
the key — **adjacent, same cache line(s)**. Taller levels (rare: ~1/4 of
nodes at branching factor 4) sit *before* the node, out of the hot path's
way. No separate key allocation, no pointer to the key. This is README §4's
dense-filter/inline-payload pattern again, by an author who priced the cache
lines.

### Step 4 — the concurrency contract: publish with acquire/release

Lock-free readers and writers share the list through the forward pointers,
so every link is an atomic with ordering semantics: `Next()`/`SetNext()` use
**acquire/release** (lines 383, 390 — release on the writer side guarantees
everything written before the pointer store, i.e. the node's key bytes, is
visible to any reader that acquires the pointer), and `CASNext` (line 395)
does the compare-and-swap. This is the classic publish pattern (topic 9):
fully construct the node, *then* make it reachable with one release-store —
readers can never see a half-built node.

### Step 5 — the lock-free insert: level 0 is the only truth

`InsertConcurrently` (line 913, CAS loop lines 1135–1171) computes a
**splice** (the prev/next pair per level — same role as redis's `update[]`),
then links the node in level by level with CAS, retrying any level where a
concurrent insert changed the neighborhood:

```rust
fn insert_concurrently(list: &SkipList, node: &Node, height: usize) {
    let mut splice = list.find_splice(node.key());       // prev/next per level
    for lvl in 0..height {                               // bottom-up: correctness
        loop {                                           //   needs only level 0
            node.set_next(lvl, splice.next[lvl]);        // prepare BEFORE publish
            if splice.prev[lvl]
                .cas_next(lvl, splice.next[lvl], node)   // release: key bytes are
            {                                            //   visible before the link
                break;
            }
            splice.recompute(lvl, node.key());           // lost the race — re-find
        }                                                //   neighbors, retry
    }
}
// a node linked at level 0 but not yet above is merely slower to find —
// never incorrect. That asymmetry is what makes the lock-free version small.
```

The load-bearing asymmetry: level 0 contains *every* node, so a search that
reaches level 0 finds everything; upper levels are only shortcuts. A node
visible at level i but not i+1 is just slower to find — so partial linking
never breaks readers, and levels can be CAS'd independently, without any
multi-word atomicity.

### Step 6 — no deletes: the restriction that keeps it ~200 lines

The header comment (lines 31–33) states the contract: **no deletes, no
unlink**. General lock-free deletion is a research problem — an unlinked node
may still be held by a concurrent reader, so you need hazard pointers or
epochs to know when freeing is safe. InlineSkipList sidesteps all of it with
the Step 1 workload fact: memtables are frozen then dropped wholesale, so
nothing is ever freed while readers run. One workload restriction deletes an
entire class of machinery — constraint-driven simplicity, the design lesson
of the whole file. It's also why the redis skiplist's spans and backward
pointers are absent: both require multi-pointer updates no single CAS can do.

### Step 7 — the supporting cast: arena and pluggable memtables

Nodes come from a **concurrent arena** (`memory/concurrent_arena.h:57–68`) —
a bump allocator with per-core shards, so concurrent inserts don't contend on
malloc either (the allocator would otherwise be the next lock). Heights come
from `RandomHeight` (lines 559–573): branching factor 4, max 12 levels. And
the skiplist is just one implementation of the `MemTableRep` interface
(`memtable/skiplistrep.cc:17–397`); siblings in `memtable/`:
`hash_skiplist_rep` (hash → per-bucket skiplists, for point-heavy),
`hash_linklist_rep`, `vectorrep` (bulk-load: append then sort-on-flush). The
memtable is *pluggable* because RUM positions differ per workload — RocksDB
ships four answers.

## Where each step lives in the code

- **Step 3** — node layout: lines 358–421; negative tower indexing at line
  383, inline key at line 374; arena allocation at lines 860–869.
- **Step 4** — `Next()`/`SetNext()` acquire/release: lines 383, 390;
  `CASNext`: line 395.
- **Step 5** — `InsertConcurrently`: line 913; the CAS loop: lines
  1135–1171. Read it against the Rust skeleton above and answer: which level
  is linked first, and why does a *partially linked* node never break
  readers?
- **Step 6** — the no-delete contract: header comment lines 31–33.
- **Step 7** — `RandomHeight`: lines 559–573; sharded arena:
  `memory/concurrent_arena.h:57–68`; the `MemTableRep` plug-in point and its
  three siblings: `memtable/skiplistrep.cc:17–397` and neighbors in
  `memtable/`.

## Questions to answer in notes.md

1. Redis's skiplist has spans + backward pointers; this one has neither. For each,
   say exactly what breaks under concurrent CAS inserts.
2. Why acquire/release on the links rather than SeqCst? What reorder is actually
   being prevented at line 383? (Reader must see the node's key bytes written
   *before* the pointer that publishes it — classic publish pattern, topic 9.)
3. Estimate: at branching 4 and 1M entries, how many dependent misses per lookup,
   and why does your hashbrown number from topic 0 beat it? Where does the skiplist
   still win? (Sorted iteration for flush; concurrent writers.)

## Done when

You can explain the negative-index tower AND why insert-only makes lock-free easy —
these two ideas are the file.

## References

**Code**
- [rocksdb](https://github.com/facebook/rocksdb)
  `memtable/inlineskiplist.h` — the header comment (lines 31–33) states
  the no-delete contract; also `memory/concurrent_arena.h:57–68`
  (sharded arena) and `memtable/skiplistrep.cc` (the `MemTableRep`
  plug-in point and its three siblings)
