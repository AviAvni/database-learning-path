# Reading RocksDB `InlineSkipList` — the lock-free memtable

File: `~/repos/rocksdb/memtable/inlineskiplist.h`. This is where LSM write throughput
lives: every `Put` in half the industry lands in this structure. Budget: 1–2 h.

## 1. The node layout trick — read this first

Lines 358–421. A node is **one allocation, three regions, and the struct points at
the middle**:

```
 raw allocation (from concurrent arena, line 860-869):

 ┌──────────────────────────┬───────────────┬─────────────────┐
 │ tower: next_[h-1]…next_[1]│ Node: next_[0]│ key bytes inline│
 └──────────────────────────┴───────────────┴─────────────────┘
                              ▲ Node* points HERE
   levels accessed by NEGATIVE indexing: (&next_[0] - n)      line 383
   key accessed as (&next_[1]):                               line 374
```

Why: the common case touches `next_[0]` and the key — **adjacent, same cache
line(s)**. Taller levels (rare) sit before the node. No separate key allocation, no
pointer to the key. This is README §4's dense-filter/inline-payload pattern again,
by an author who priced the cache lines.

## 2. The concurrency contract

- `Next()`/`SetNext()` — acquire/release (lines 383, 390); `CASNext` — line 395.
- `InsertConcurrently` — line 913; the CAS loop lines 1135–1171: compute splice
  (prev/next per level), CAS level 0 first? No — read carefully: which level is
  linked first, and why does a *partially linked* node never break readers?
  (A node visible at level i but not i+1 is just... slower to find. Correctness
  needs only level 0.)
- **No deletes, no unlink** (comment lines 31–33): memtables are frozen then dropped
  wholesale. This one workload restriction is what keeps the lock-free code ~200
  lines instead of a research paper (no hazard pointers to unlink, nothing is ever
  freed while readers run). Constraint-driven simplicity — the design lesson of the
  whole file.

## 3. Supporting cast

- `RandomHeight` — lines 559–573, branching factor 4, max 12 levels.
- Arena: `memory/concurrent_arena.h:57–68` — per-core shards so concurrent inserts
  don't contend on the allocator either.
- The plug into the engine: `memtable/skiplistrep.cc:17–397` implements
  `MemTableRep`; siblings in `memtable/`: `hash_skiplist_rep` (hash → per-bucket
  skiplists, for point-heavy), `hash_linklist_rep`, `vectorrep` (bulk-load: append
  then sort-on-flush). The memtable is *pluggable* because RUM positions differ per
  workload — RocksDB ships four answers.

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
