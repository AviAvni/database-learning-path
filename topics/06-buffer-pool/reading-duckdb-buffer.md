# DuckDB's buffer pool: eviction by queue of hints

The interesting contrast with postgres: no fixed frame array, no CLOCK —
blocks are heap-allocated, tracked by `shared_ptr`, and eviction is a
concurrent FIFO queue of *hints* that are allowed to go stale. Re-pinning
never removes a queue entry; it invalidates one, and dead nodes get swept in
bulk. Mark now, collect later — the amortization move again, this time inside
the replacement policy itself. This chapter builds the design step by step,
then maps each piece to the C++ files.

## The problem in one sentence

An embedded analytics engine can't pre-allocate a fixed frame array (it
shares RAM with a host process and juggles buffers from 256 KB row groups to
multi-GB hash tables), so DuckDB must track recency and enforce a memory
budget over *heap allocations of arbitrary size* — without a global lock and
without per-access list surgery.

## The concepts, step by step

### Step 1 — the BlockHandle: residency without frames

Postgres's unit is the frame — a fixed slot in a preallocated array. DuckDB
has no frames: each block of data is a separate heap allocation, and the
unit of residency is the **BlockHandle** — a small control object that says
whether the block's bytes are currently in memory (`BLOCK_LOADED`) or not
(`BLOCK_UNLOADED`), and how many users are reading it right now (an atomic
`readers` pin count — a pin being "in use, don't evict"). `CanUnload`
answers eviction's one question: loaded, unpinned, and not otherwise
protected.

Callers never touch the counter directly: pinning returns a `BufferHandle`,
an RAII guard object whose destructor decrements `readers` — drop the guard
and the block becomes evictable. Rust translation: this is exactly a guard;
your buffer pool's `PageGuard` should work the same way.

Why it matters: no fixed array means memory can flow between the pool and
the rest of the process — but it also means eviction has no array to sweep
a CLOCK hand over. Something else must remember what's cold. That's Step 2.

### Step 2 — the eviction queue: a FIFO of hints, not truths

DuckDB's replacement policy is a concurrent FIFO queue that approximates
LRU: every time a block is *unpinned*, a `BufferEvictionNode` is pushed —
so blocks unpinned longest ago surface first. The trick is what a node
contains:

- a **weak_ptr** to the block (a non-owning reference that can answer "is
  this object still alive?" without keeping it alive — a `shared_ptr` here
  would make the queue itself pin every block forever: the cache becomes a
  leak), and
- the handle's `handle_sequence_number` *as of enqueue time* — a version
  stamp.

A queue entry is therefore just a *hint*: "this block was cold when I was
pushed." Nothing guarantees it's still true by the time eviction pops it.

### Step 3 — dead nodes: invalidate instead of remove

Here's the concurrency problem the design dodges: when a block is re-pinned
(it turned out to be hot), true LRU would remove its entry from the middle
of the queue — but removing from the middle of a concurrent FIFO needs a
lock or an O(n) search. DuckDB refuses: re-pin **never touches the queue**.
Instead, unpinning again later bumps the handle's sequence number and
enqueues a *fresh* node; the OLD node — still sitting in the queue! — now
has a stale sequence number and has become a **dead node**, a corpse that
eviction will recognize and skip.

```
 re-pin doesn't REMOVE the queue entry (that needs a lock or O(n) search);
 it INVALIDATES it with a seq bump and re-enqueues later.
 → same amortization move as topic 2's incremental rehash and topic 4's
   tombstones: mark now, collect in bulk later.
```

Corpses do pile up, so cleanup is amortized: `PurgeIteration` runs once per
`INSERT_INTERVAL = 4096` insertions and bulk-removes dead nodes — O(1)
amortized per operation instead of O(n) per re-pin.

### Step 4 — the eviction loop: mostly corpse-skipping

With Steps 2–3 in place, evicting to free N bytes is a pop-and-verify loop —
each pop must survive three liveness checks before it frees anything:

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

Why it matters: every check is lock-free — a weak_ptr upgrade, an atomic
load, a state check. The cost model inverts postgres's: hits and re-pins pay
nothing; the *evictor* pays for everyone's corpses. Question 2 below asks
when that trade loses.

One refinement: there isn't one queue but several, by buffer type
(`EVICTION_QUEUE_TYPES` in buffer_pool.hpp:116–122, in priority order) —
managed buffers and external file caches don't compete for survival in a
single FIFO.

### Step 5 — memory reservations: a gate in front of malloc

The memory budget is enforced *before* allocating, not observed after:
`EvictBlocksOrThrow` runs Step 4's loop until the requested reservation
fits under the limit, and if eviction can't free enough, the allocation
**throws** ("could not allocate block of size…") — the query fails rather
than the process OOMing. `Pin` composes the pieces: block loaded ⇒
`readers++`; unloaded ⇒ reserve memory (evicting as needed), then reload
from disk or temp file.

Contrast the two accounting philosophies you now know: DuckDB gates
allocations up front; redis (reading-redis-zmalloc.md) counts after the
fact and evicts keys asynchronously.

### Step 6 — spilling: the buffer pool doubles as the swap file

Not every buffer has a home on disk: hash-join tables and sort runs are
*temporary* — evicting them can't just drop the bytes. `WriteTemporaryBuffer`
sends evicted temporary data to the temp-file manager, and Step 5's reload
path brings it back on demand. This is why DuckDB joins bigger than RAM
work: eviction and spilling are one mechanism. Postgres spills per-operator
instead (each sort/hash gets `work_mem` and manages its own temp files) —
two philosophies of the same fallback.

## Where each step lives in the code

Local clone at `~/repos/duckdb`:

| File | What | Steps |
|------|------|-------|
| `src/include/duckdb/storage/buffer/block_handle.hpp` | BlockHandle | 1 |
| `src/storage/buffer/buffer_pool.cpp` | queue, purge, eviction | 2–4 |
| `src/include/duckdb/storage/buffer/buffer_pool.hpp` | purge cadence, queue types | 3–4 |
| `src/storage/standard_buffer_manager.cpp` | reservations, pin, spill | 5–6 |

- **Step 1**: block_handle.hpp — `BlockState`
  (BLOCK_LOADED/BLOCK_UNLOADED, :62–71), atomic `readers` pin count
  (:73–87), `CanUnload` (:208).
- **Step 2**: `BufferEvictionNode` — buffer_pool.cpp:42 (weak_ptr +
  `handle_sequence_number`).
- **Step 3**: `BufferPool::AddToEvictionQueue` — buffer_pool.cpp:271 (seq
  bump + fresh node; old node goes dead — :284, IncrementDeadNodes);
  `PurgeIteration` — buffer_pool.hpp:104, `INSERT_INTERVAL = 4096` :116.
- **Step 4**: `EvictBlocks`/`EvictBlocksInternal` — buffer_pool.cpp:377+
  (`IterateUnloadableBlocks` pops; dead-seq skip; weak_ptr-fail skip;
  `Unload` at :38 in that loop); queue-per-type —
  buffer_pool.hpp:116–122.
- **Step 5**: `EvictBlocksOrThrow` — standard_buffer_manager.cpp:126
  (throw at :155); `Pin` — :333/:337.
- **Step 6**: `WriteTemporaryBuffer` — standard_buffer_manager.cpp:501
  (temp-file manager handoff :508).

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
