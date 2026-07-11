# DuckDB's buffer pool: eviction by queue of hints

The interesting contrast with postgres: no fixed frame array, no CLOCK —
blocks are heap-allocated, tracked by `shared_ptr`, and eviction is a
concurrent FIFO queue of *hints* that are allowed to go stale. Re-pinning
never removes a queue entry; it invalidates one, and dead nodes get swept in
bulk. Mark now, collect later — the amortization move again, this time inside
the replacement policy itself.

## 1. BlockHandle — the unit of residency

- block_handle.hpp: `BlockState` (BLOCK_LOADED/BLOCK_UNLOADED, :62–71),
  atomic `readers` pin count (:73–87), `CanUnload` (:208).
- A `BufferHandle` (RAII) holds a pin; destruction decrements readers and the
  block becomes evictable. Rust translation: this is exactly a guard object —
  your buffer pool's `PageGuard` should work the same way.

## 2. The eviction queue — buffer_pool.cpp

- `BufferEvictionNode` — :42: a **weak_ptr** to the block memory + the
  `handle_sequence_number` at enqueue time.
- Unpin ⇒ `BufferPool::AddToEvictionQueue` — :271: bump the handle's
  eviction sequence number, enqueue a fresh node; the OLD node for this block
  (still in the queue!) is now a **dead node** (:284, IncrementDeadNodes).
- Eviction — `EvictBlocks`/`EvictBlocksInternal` (:377+):
  `IterateUnloadableBlocks` pops nodes; a node whose seq_num ≠ the handle's
  current one is dead — skip; whose weak_ptr won't lock — dead; else
  `Unload` (:38 in that loop) frees the memory.
- Cleanup is amortized: `PurgeIteration` (:104 hpp) runs every
  `INSERT_INTERVAL = 4096` insertions (:116) and bulk-removes dead nodes.

```
 re-pin doesn't REMOVE the queue entry (that needs a lock or O(n) search);
 it INVALIDATES it with a seq bump and re-enqueues later.
 → same amortization move as topic 2's incremental rehash and topic 4's
   tombstones: mark now, collect in bulk later.
```

The eviction loop is mostly corpse-skipping:

```rust
fn evict_until(&self, needed: usize) -> bool {
    let mut freed = 0;
    while freed < needed {
        let Some(node) = self.queue.pop() else { return false };
        let Some(block) = node.block.upgrade() else { continue };  // weak_ptr: block
                                                                   // already gone
        if node.seq != block.eviction_seq.load() { continue; }     // DEAD: re-pinned
                                                                   // since enqueue
        if !block.can_unload() { continue; }                       // pinned right now
        freed += block.unload();          // write to temp file if no disk home
    }
    true
}
```

## 3. Memory reservations — standard_buffer_manager.cpp

- `EvictBlocksOrThrow` — :126: every allocation first evicts until the
  reservation fits, else throws "could not allocate block of size…" (:155).
  Memory accounting is a *gate in front of malloc*, not an after-the-fact
  counter — compare redis, which counts after and evicts keys asynchronously.
- `Pin` — :333/:337: loaded ⇒ readers++; unloaded ⇒ reserve memory
  (evicting), reload from disk or temp file.
- Multiple queues by buffer type — buffer_pool.hpp:116–122
  (`EVICTION_QUEUE_TYPES`, priority order): managed buffers vs external files
  don't compete in one queue.

## 4. Spilling — WriteTemporaryBuffer, standard_buffer_manager.cpp:501

Evicted *temporary* data (hash tables, sorts — no disk home) goes to the
temp file manager (:508). This is why DuckDB joins bigger than RAM work: the
buffer pool doubles as the spill mechanism. Postgres spills per-operator
(work_mem) instead — two philosophies of the same fallback.

## Questions to answer in notes.md

1. Why weak_ptr in the queue node? What breaks with shared_ptr? (Queue would
   keep every block alive — the cache becomes a leak.)
2. Dead-node ratio: worst-case queue length for a workload that re-pins the
   same block N times between purges. When is CLOCK's fixed array strictly
   better?
3. DuckDB throws on memory pressure; postgres errors only when all buffers
   are pinned. Trace where each behavior comes from and which your capstone
   pool should adopt (server vs embedded assumptions).

## Done when

You can explain a dead node, the 4096-insert purge cadence, and why re-pin
never touches the queue — and name the postgres structure each replaces.

## References

**Code**
- [duckdb/duckdb](https://github.com/duckdb/duckdb) —
  `src/storage/buffer/buffer_pool.cpp`,
  `src/storage/standard_buffer_manager.cpp`,
  `src/include/duckdb/storage/buffer/buffer_pool.hpp`,
  `src/include/duckdb/storage/buffer/block_handle.hpp`. Local clone at
  `~/repos/duckdb`.
