# DuckDB's buffer pool: eviction by queue of hints

The interesting contrast with postgres: no fixed frame array, no clock sweep
— blocks are heap allocations tracked by `shared_ptr`, and eviction is a
lock-free FIFO of *hints* that are allowed to go stale. Re-pinning never
removes a queue entry; it invalidates one, and the corpses get swept in bulk
later. Mark now, collect later — the amortization move again, this time
inside the replacement policy itself. This chapter builds the design step by
step, works out what the staleness actually costs in queue length, then maps
each piece to the C++.

Read at [`duckdb/duckdb@d54cbbe3df`](https://github.com/duckdb/duckdb) (v2.0.0),
the repo's pinned commit (pin table at the end of `resources/codebases.md`; local
clone at `~/repos/duckdb`). One naming note before you open the files: the
control object was split. **`BlockMemory`** (block_handle.hpp:34) owns the
residency state, the pin count and the eviction sequence number;
**`BlockHandle`** (:262) is the outer handle callers hold. The eviction queue
tracks `BlockMemory`.

## The problem in one sentence

An embedded analytics engine cannot pre-allocate a fixed frame array — it
shares RAM with a host process and juggles buffers from 256 KB row groups to
multi-GB hash tables — so DuckDB must track recency and enforce a memory
budget over *heap allocations of arbitrary size*, without a global lock and
without per-access list surgery.

## The concepts, step by step

### Step 1 — the BlockMemory: residency without frames

> **In:** nothing yet — this is the unit everything else operates on.
> **Out:** a pin count and a residency state, which Step 2 must observe
> without holding anything and Step 4 must re-verify before freeing.

Postgres's unit is the **frame**: a fixed slot in a preallocated array
([`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md), Step 1). DuckDB
has no frames. Each block is a separate heap allocation, and the unit of
residency is a control object whose whole job is to answer two questions:

```cpp
// duckdb/duckdb@d54cbbe3df — src/include/duckdb/storage/buffer/block_handle.hpp, BlockMemory's residency accessors, 71-93
    71  	//! Returns true, if the block state is BLOCK_UNLOADED.
    72  	bool IsUnloaded() const {
    73  		return state == BlockState::BLOCK_UNLOADED;
    74  	}
    75  	//! Returns the number of readers.
    76  	int32_t GetReaders() const {
    77  		return readers.load(std::memory_order_relaxed);
    78  	}
    79  	//! Increments the number of readers prior to returning it.
    80  	int32_t IncrementReaders(BlockLock &l) {
    81  		VerifyMutex(l);
    82  		// Tiny buffers do not decrement on unpin, so their reader count can wrap.
    83  		auto new_readers = static_cast<int32_t>(static_cast<uint32_t>(readers.load(std::memory_order_relaxed)) + 1);
    84  		readers.store(new_readers, std::memory_order_relaxed);
    85  		return new_readers;
    86  	}
    87  	//! Decrements the number of readers prior to returning it.
    88  	int32_t DecrementReaders(BlockLock &l) {
    89  		VerifyMutex(l);
    90  		auto new_readers = static_cast<int32_t>(static_cast<uint32_t>(readers.load(std::memory_order_relaxed)) - 1);
    91  		readers.store(new_readers, std::memory_order_relaxed);
    92  		return new_readers;
    93  	}
```

`readers` is an `atomic<int32_t>` (:235) — the **pin** count, "in use, don't
evict", the same concept as postgres's refcount and LeanStore's absence of
one. The line to focus on is 80: changing the count requires the block's lock
(`BlockLock`, a `unique_lock<mutex>`, :30), and `VerifyMutex` checks the caller
really holds it, so the increment can be a plain load and store; the field is
atomic only so that `GetReaders` can be read *without* the lock — the comment on
the field says exactly that (:234, "Writers hold the block lock; reads may be
unlocked"). `state` is `BLOCK_LOADED` or `BLOCK_UNLOADED`: the bytes are in
memory, or they are not. Eviction's one question is answered by `CanUnload`,
which says not just *whether* but *why not*:

```cpp
// duckdb/duckdb@d54cbbe3df — src/storage/buffer/block_handle.cpp, CanUnload, 105-121
   105  CanUnloadResult BlockMemory::CanUnload() const {
   106  	if (GetState() == BlockState::BLOCK_UNLOADED) {
   107  		// The block has already been unloaded.
   108  		return CanUnloadResult::ALREADY_UNLOADED;
   109  	}
   110  	if (GetReaders() > 0) {
   111  		// There are active readers.
   112  		return CanUnloadResult::PINNED;
   113  	}
   114  	if (BlockId() >= MAXIMUM_BLOCK && MustWriteToTemporaryFile() && !GetBufferManager().HasTemporaryDirectory()) {
   115  		// The block memory cannot be destroyed upon eviction/unpinning.
   116  		// In order to unload this block we need to write it to a temporary buffer.
   117  		// However, no temporary directory is specified, hence, we cannot unload.
   118  		return CanUnloadResult::NO_TEMP_DIRECTORY;
   119  	}
   120  	return CanUnloadResult::CAN_UNLOAD;
   121  }
```

The four answers are the enum `CanUnloadResult` (block_handle.hpp:32). Line 114
is Step 6 arriving early: some blocks have no home on disk to be dropped back
to, and if there is nowhere to spill them, they are not evictable *yet* — the
distinct `NO_TEMP_DIRECTORY` answer lets Step 4 treat that case differently from
a pinned block.

Callers never touch `readers` directly. `Pin` returns a `BufferHandle`, an
RAII guard whose destructor unpins — drop the guard and the block becomes
evictable. That is exactly a Rust guard; the capstone's `PageGuard` should
work the same way.

Why it matters: no fixed array means memory can flow between the pool and the
rest of the process — but it also means eviction has no array to sweep a
clock hand over. Something else has to remember what is cold. That is Step 2.

### Step 2 — the eviction queue: a FIFO of hints, not truths

> **In:** Step 1's `BlockMemory`, at the moment it is unpinned.
> **Out:** a queue entry that may be a lie by the time anyone reads it —
> which Steps 3 and 4 exist to cope with.

The replacement policy is a lock-free FIFO that approximates LRU: every time
a block is unpinned, a node is pushed, so blocks unpinned longest ago surface
first. The queue is `duckdb_moodycamel::ConcurrentQueue<BufferEvictionNode>`
(buffer_pool.cpp:61) — a multi-producer/multi-consumer lock-free queue. The
interesting part is what a node holds:

```cpp
// duckdb/duckdb@d54cbbe3df — src/storage/buffer/buffer_pool.cpp, BufferEvictionNode and its liveness test, 42-59
    42  BufferEvictionNode::BufferEvictionNode(weak_ptr<BlockMemory> block_memory_p, idx_t eviction_seq_num)
    43      : memory_p(std::move(block_memory_p)), handle_sequence_number(eviction_seq_num) {
    44  	D_ASSERT(!memory_p.expired());
    45  }
    46
    47  bool BufferEvictionNode::IsDeadNode(optional_idx debug_sleep_micros) {
    48  	auto shared_memory_p = memory_p.lock();
        // ... 49-51: debug-only sleep ...
    52  	if (!shared_memory_p) {
    53  		return true;
    54  	}
    55  	if (handle_sequence_number != shared_memory_p->GetEvictionSequenceNumber()) {
    56  		return true;
    57  	}
    58  	return false;
    59  }
```

Two fields, two failure modes:

- a **`weak_ptr`** (a non-owning reference that can answer "is this object
  still alive?" without keeping it alive). A `shared_ptr` here would make the
  queue itself pin every block forever — the cache would become a leak, which
  is question 1 below. Line 52: if the upgrade fails, the block is gone and
  the node is dead.
- the block's **eviction sequence number as of enqueue time** — a version
  stamp, `atomic<idx_t> eviction_seq_num` (block_handle.hpp:244), which the
  field's comment calls "Monotonic: it is never reset, so an eviction queue
  entry is stale if and only if its sequence number differs from this one"
  (:242–243). Line 55: if the block's current number has moved on, this node
  has been superseded.

A queue entry is therefore just a *hint*: "this block was cold when I was
pushed." Nothing guarantees it is still true when someone pops it.

### Step 3 — dead nodes: invalidate instead of remove

> **In:** Step 2's hints, and a block that turns out to be hot again.
> **Out:** a queue that grows corpses, plus the bulk-collection policy that
> bounds how many — the arithmetic below is the point of this step.

Here is the concurrency problem the design dodges. When a block is re-pinned,
true LRU would remove its entry from the middle of the queue — but removing
from the middle of a concurrent FIFO needs a lock or an O(n) search. DuckDB
refuses: **re-pin never touches the queue.** Unpinning later bumps the
sequence number and enqueues a *fresh* node, and the old node — still sitting
in the queue — now carries a stale number and has become a **dead node**, a
corpse Step 4 will recognise and skip.

```cpp
// duckdb/duckdb@d54cbbe3df — src/storage/buffer/buffer_pool.cpp, BufferPool::AddToEvictionQueue, 283-300
   283  	if (memory.HasLiveQueueEntry(lock)) {
   284  		// Count the previous live entry before bumping the sequence number. PurgeIteration
   285  		// reads sequence numbers without the block lock; bumping first could let it see the
   286  		// previous entry as stale and decrement dead_nodes before this matching increment.
   287  		queue.IncrementDeadNodes();
   288  	}
   289
   290  	auto ts = memory.NextEvictionSequenceNumber(lock);
        // ... 291-295: optional LRU timestamp ...
   296  	memory.SetHasLiveQueueEntry(lock, true);
   297
   298  	// Get the eviction queue for the block and add it
   299  	BufferEvictionNode node(handle->GetMemoryWeak(), ts);
   300  	return queue.AddToEvictionQueue(std::move(node));
```

The line to focus on is 290: the bump that turns the previous entry into a
corpse. Lines 283–288 count that corpse first, only if the block actually had a
live entry (`has_queue_entry`, block_handle.hpp:245–248), and the comment says
why the order matters: a purging thread reads sequence numbers without the
block's lock, so bumping first could let it discount the corpse before it had
been counted.

```
 re-pin does NOT remove the queue entry (that needs a lock or an O(n) search);
 it INVALIDATES it with a sequence bump and enqueues a fresh node later.
 → same amortization move as topic 2's incremental rehash and topic 4's
   tombstones: mark now, collect in bulk later.
```

Corpses pile up, so collection is amortized and bounded by four constants
that sit together in the source and are worth reading as a policy:

```cpp
// duckdb/duckdb@d54cbbe3df — src/storage/buffer/buffer_pool.cpp, the purge policy constants, 115-124
   115  	//! We trigger a purge of the eviction queue every INSERT_INTERVAL insertions
   116  	constexpr static idx_t INSERT_INTERVAL = 4096;
   117  	//! We multiply the base purge size by this value.
   118  	constexpr static idx_t PURGE_SIZE_MULTIPLIER = 2;
   119  	//! We multiply the purge size by this value to determine early-outs. This is the minimum queue size.
   120  	//! We never purge below this point.
   121  	constexpr static idx_t EARLY_OUT_MULTIPLIER = 4;
   122  	//! We multiply the approximate alive nodes by this value to test whether our total dead nodes
   123  	//! exceed their allowed ratio. Must be greater than 1.
   124  	constexpr static idx_t ALIVE_NODE_MULTIPLIER = 4;
```

`Purge` (:154) is entered by whichever thread's insertion hits the interval;
everyone else `try_lock`s and leaves (:156–159). `PurgeIteration` (:215)
dequeues `purge_size` nodes in bulk (:225), drops the dead ones (:236), and
re-enqueues the survivors in bulk (:252). The loop's two early-outs are at
:198 and :207.

**What that policy costs, as arithmetic.** All four constants are load-bearing:

```
 purge_size          = INSERT_INTERVAL × PURGE_SIZE_MULTIPLIER
                     = 4096 × 2 = 8,192 nodes swept per purge
 amortized per unpin = 8,192 / 4,096 = 2 node inspections
                       (it sweeps twice what it inserts, which is what
                        stops the queue oscillating — comment at :176)

 minimum queue size before ANY purging happens (:169):
     purge_size × EARLY_OUT_MULTIPLIER = 8,192 × 4 = 32,768 nodes

 tolerated corpse ratio — the loop keeps purging while (:207)
     alive × (ALIVE_NODE_MULTIPLIER − 1) ≤ dead,  i.e.  3·alive ≤ dead
 so it stops once dead < 3·alive:
     dead nodes may be up to 75% of the queue
     the queue may be up to 4× the live set

 what that costs in bytes, for a 16 GB pool of 256 KB row groups:
     live blocks              16 GiB / 256 KiB = 65,536
     queue at the 4× ceiling  262,144 nodes
     node ≈ weak_ptr (2 pointers) + idx_t = 24 B
     queue                    ≈ 6.3 MB = 0.04% of the pool
```

Two inspections per unpin, and 0.04% of the pool spent on corpses, to make
re-pin cost *nothing at all*. That is the trade, and it is a good one for
analytics — question 2 asks when it stops being one.

### Step 4 — the eviction loop: mostly corpse-skipping

> **In:** Step 2's queue of hints and Step 3's corpses.
> **Out:** freed bytes — or a failure, which Step 5 turns into a thrown
> query error rather than an OOM kill.

Evicting is a pop-and-verify loop. Every popped node must survive three
liveness checks before anything is freed, and all three are lock-free:

```cpp
// duckdb/duckdb@d54cbbe3df — src/storage/buffer/buffer_pool.cpp, EvictionQueue::IterateUnloadableBlocks, 474-531
   474  	for (;;) {
   475  		// get a block to unpin from the queue
   476  		BufferEvictionNode node;
   477  		if (!q.try_dequeue(node)) {
   478  			// we could not dequeue any eviction node, so we try one more time,
   479  			// but more aggressively
   480  			if (!TryDequeueWithLock(node)) {
   481  				return;
   482  			}
   483  		}
   484
   485  		// get a reference to the underlying block pointer
   486  		auto handle = node.memory_p.lock();
        // ... 487-491: debug-only sleep ...
   492  		if (!handle) {
   493  			DecrementDeadNodes();
   494  			continue;
   495  		}
   496
   497  		// we might be able to free this block: grab the mutex and check if we can free it
   498  		auto lock = handle->GetLock();
   499  		if (node.handle_sequence_number != handle->GetEvictionSequenceNumber()) {
   500  			// A newer entry superseded this node: it was counted as dead when that entry was added.
   501  			DecrementDeadNodes();
   502  			continue;
   503  		}
        // ... 504-505: comment ...
   506  		handle->SetHasLiveQueueEntry(lock, false);
   507  		switch (handle->CanUnload()) {
   508  		case CanUnloadResult::CAN_UNLOAD:
   509  			break;
   510  		case CanUnloadResult::NO_TEMP_DIRECTORY:
   511  			// Unpinned temporary block that cannot be offloaded yet: re-enqueue until temp directory is set.
   512  			handle->SetHasLiveQueueEntry(lock, true);
   513  			q.enqueue(std::move(node));
   514  			return;
   515  		case CanUnloadResult::PINNED:
   516  		case CanUnloadResult::ALREADY_UNLOADED:
   517  			// The block cannot be unloaded right now (e.g. it is pinned). It gets a new queue
   518  			// entry when it is unpinned again.
   519  			continue;
   520  		}
   521
   522  		bool continue_iteration;
   523  		try {
   524  			continue_iteration = fn(node, handle, lock);
   525  		} catch (...) {
   526  			// The unload failed (e.g. the temporary directory is full) and the block is still loaded.
   527  			// Give it its queue entry back, or it stays un-evictable until the next unpin.
   528  			handle->SetHasLiveQueueEntry(lock, true);
   529  			q.enqueue(std::move(node));
   530  			throw;
   531  		}
```

Line 486: the block may already be gone (`weak_ptr` upgrade fails). Line 499:
it may have been re-pinned since (Step 3's corpse). Line 507: it may be
pinned *right now*, in which case the node is simply dropped — lines 517–518
are the design in one sentence: "It gets a new queue entry when it is unpinned
again." A pinned block's position is never preserved.

The two places the node *is* put back are the cases where no future unpin would
re-enqueue the block. An unpinned temporary block with no temporary directory
(lines 510–514) is not pinned, so nothing will ever give it a fresh entry;
dropping the node would make it unevictable forever, so the loop puts it back
and returns rather than continuing — continuing could dequeue the very node it
just re-enqueued. And if
the unload itself throws (lines 525–530 — writing to a full temporary
directory, for instance), the block is still loaded and still unpinned, so it
gets its entry back before the exception propagates. `has_queue_entry` tracks
which blocks currently own a live entry, and these two paths are what keep it
true.

The callback that actually frees is in `EvictBlocksInternal` (:398): it
early-returns if usage is already under the limit (:404), and otherwise
`Unload`s (:421) until it is (:423). There is a nice special case at
:413–417 — if the victim's allocation is exactly the size being requested,
the memory is handed over directly instead of being freed and re-malloc'd.

Why it matters: the cost model inverts postgres's. Hits and re-pins pay
nothing; the *evictor* walks everyone's corpses. Postgres pays a usage-count
CAS on every hit so its clock hand never has to skip anything stale
([`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md), Step 4).

One refinement: there is not one queue but **eight**, in three types —
`BLOCK_AND_EXTERNAL_FILE_QUEUE_SIZE = 1`, `MANAGED_BUFFER_QUEUE_SIZE = 6`,
`TINY_BUFFER_QUEUE_SIZE = 1` (buffer_pool.hpp:123–129), constructed in that
order at buffer_pool.cpp:256–270. Managed buffers are sharded six ways
because they are the contended case; blocks and tiny buffers are not.

### Step 5 — memory reservations: a gate in front of malloc

> **In:** Step 4's eviction loop, as a subroutine.
> **Out:** either a reservation that fits the budget, or a thrown query
> error — never an over-limit allocation.

The budget is enforced *before* allocating, not observed after:

```cpp
// duckdb/duckdb@d54cbbe3df — src/storage/standard_buffer_manager.cpp, EvictBlocksOrThrow, 126-138
   126  template <typename... ARGS>
   127  TempBufferPoolReservation StandardBufferManager::EvictBlocksOrThrow(QueryContext context, MemoryTag tag,
   128                                                                      idx_t memory_delta, unique_ptr<FileBuffer> *buffer,
   129                                                                      const ARGS &...args) {
   130  	auto r = buffer_pool.EvictBlocks(context, tag, memory_delta, buffer_pool.maximum_memory, buffer);
   131  	if (!r.success) {
   132  		string extra_text = StringUtil::Format(" (%s/%s used)", StringUtil::BytesToHumanReadableString(GetUsedMemory()),
   133  		                                       StringUtil::BytesToHumanReadableString(GetMaxMemory()));
   134  		extra_text += InMemoryWarning();
   135  		throw OutOfMemoryException(args..., extra_text);
   136  	}
   137  	return std::move(r.reservation);
   138  }
```

If eviction cannot free enough, line 135 throws — the *query* fails, with the
used/max numbers in the message, rather than the process being OOM-killed.
Callers supply the message: "could not allocate block of size %s%s" at :156
and :177 for the allocation paths, "failed to pin block of size %s%s" at :391
for `Pin`.

`Pin` (:364) composes everything: take the block's lock, and if the state is
`BLOCK_LOADED` just `Load` (increment readers) and return (:376–385). If not,
**drop the lock** — the comment at :365–367 explains that returning a
`BufferHandle` while holding the lock would deadlock on its destructor — run
`EvictBlocksOrThrow` (:389–391), then re-take the lock and *re-check* the state
(:396), because another thread may have loaded the block while eviction was
running. Two checks around one lock gap; the same shape as Step 4's three
re-verifications.

Contrast the two accounting philosophies you now know: DuckDB gates
allocations up front, redis
([`reading-redis-zmalloc.md`](reading-redis-zmalloc.md)) counts after the
fact and evicts keys asynchronously.

### Step 6 — spilling: the buffer pool doubles as the swap file

> **In:** Step 4's decision to unload a block, and Step 1's line 118.
> **Out:** why larger-than-RAM joins work at all, and the design contrast
> with postgres's `work_mem`.

Not every buffer has a home on disk. Hash-join tables and sort runs are
*temporary*: evicting them cannot just drop the bytes, because there is
nowhere to read them back from. `WriteTemporaryBuffer`
(standard_buffer_manager.cpp:533) hands them to the temp-file manager (:540),
and Step 5's reload path brings them back on demand. That is why DuckDB joins
bigger than RAM work at all — eviction and spilling are one mechanism, not
two. It is also why `CanUnload` returns false when there is no temporary
directory (block_handle.cpp:114): with nowhere to spill, an unspillable block
is pinned in effect until a directory is configured.

Postgres spills per operator instead: each sort or hash gets `work_mem` and
manages its own temp files, and the buffer pool never sees them. Two
philosophies of the same fallback — one budget shared and enforced centrally,
versus many budgets enforced locally.

Both, note, are *the database deciding*. The alternative — letting the OS
page the working set out under you — is what this topic's lane measures:
p50 42 ns against a 182 µs maximum, a 4300× spread of stalls the database can
neither see nor schedule ([FINDINGS.md row 6](../../FINDINGS.md);
[`reading-mmap-paper.md`](reading-mmap-paper.md)). DuckDB throwing an
`OutOfMemoryException` is the deliberate opposite: a failure you can attribute
and a query you can retry with a bigger limit.

## Where each step lives in the code

Read `buffer_pool.cpp` top to bottom — it is 668 lines and contains the whole
policy — then dip into `standard_buffer_manager.cpp` for `Pin` and the
reservation path.

| File | What | Steps |
|------|------|-------|
| `src/include/duckdb/storage/buffer/block_handle.hpp` | `BlockMemory` (:34) and `BlockHandle` (:262) | 1 |
| `src/storage/buffer/block_handle.cpp` | `CanUnload`, `Unload` | 1, 4 |
| `src/storage/buffer/buffer_pool.cpp` | node, queue, purge policy, eviction loop | 2–4 |
| `src/include/duckdb/storage/buffer/buffer_pool.hpp` | how many queues, of which types | 4 |
| `src/storage/standard_buffer_manager.cpp` | reservations, `Pin`, spilling | 5–6 |

| Step | Symbol | Location |
|---|---|---|
| 1 | `BlockMemory` / `BlockHandle` class split | block_handle.hpp:34, :262 |
| 1 | `IsUnloaded`, `GetReaders`, `Increment/DecrementReaders` | block_handle.hpp:71–93 |
| 1 | `atomic<int32_t> readers`, `atomic<idx_t> eviction_seq_num`, `has_queue_entry` | block_handle.hpp:235, :244, :248 |
| 1 | `GetEvictionSequenceNumber` / the bump | block_handle.hpp:121, :125–131 |
| 1 | `CanUnloadResult`; `CanUnload` — declaration, then the three conditions | block_handle.hpp:32, :221; block_handle.cpp:105–121 |
| 2 | `BufferEvictionNode` ctor and `IsDeadNode` | buffer_pool.cpp:42, :47–59 |
| 2 | the lock-free queue type | buffer_pool.cpp:61 |
| 3 | `BufferPool::AddToEvictionQueue` — count dead, then re-enqueue | buffer_pool.cpp:274, :287, :290, :299 |
| 3 | `EvictionQueue::AddToEvictionQueue` returns "time to purge" | buffer_pool.cpp:144–147 |
| 3 | the four purge constants | buffer_pool.cpp:115–124 |
| 3 | `Purge` — single-purger `try_lock`, both early-outs | buffer_pool.cpp:154, :156, :169, :198, :207 |
| 3 | `PurgeIteration` — bulk dequeue, drop dead, bulk re-enqueue | buffer_pool.cpp:215, :225, :236, :252 |
| 4 | `IterateUnloadableBlocks` — the three checks and the two put-backs | buffer_pool.cpp:472, :486, :499, :507, :510–514, :525–530 |
| 4 | `EvictBlocksInternal` — the callback that frees | buffer_pool.cpp:398, :404, :413, :421 |
| 4 | queue counts by type | buffer_pool.hpp:123–129; buffer_pool.cpp:256–270 |
| 5 | `EvictBlocksOrThrow` and the throw | standard_buffer_manager.cpp:127, :135 |
| 5 | the caller messages | standard_buffer_manager.cpp:156, :177, :391 |
| 5 | `Pin`, its deadlock comment, and the re-check after eviction | standard_buffer_manager.cpp:364, :365–367, :376, :396 |
| 6 | `WriteTemporaryBuffer` → temp-file manager | standard_buffer_manager.cpp:533, :540 |

## Questions to answer in notes.md

1. Why weak_ptr in the queue node? What breaks with shared_ptr? (Queue would
   keep every block alive — the cache becomes a leak.)
2. Dead-node ratio: worst-case queue length for a workload that re-pins the
   same block N times between purges. When is CLOCK's fixed array strictly
   better?
3. DuckDB throws on memory pressure; postgres errors only when all buffers
   are pinned. Trace where each behavior comes from and which your capstone
   pool should adopt (server vs embedded assumptions).

## Takeaway

Take the frame array away and the clock hand goes with it, so recency has to
live somewhere else: a lock-free FIFO of weak references plus version stamps.
Every entry is a hint that may be stale, and the whole design follows from
refusing to fix stale entries eagerly — re-pin is free, the evictor skips
corpses, and a bulk purge every 4,096 insertions keeps the queue within 4× of
the live set. The budget is then enforced at the only place it can be
enforced without a frame array: in front of `malloc`, with a thrown query
error when eviction cannot make room.

## Done when

Answer each before unfolding it.

- [ ] You can define a dead node, name its two causes, and say which line detects each.

  <details><summary>Answer</summary>

  A dead node is a queue entry that no longer describes reality. Two causes,
  both in `IsDeadNode` (buffer_pool.cpp:47–59) and again inline in
  `IterateUnloadableBlocks`:

  1. **The block is gone.** `memory_p.lock()` on the `weak_ptr` fails — line
     52 in `IsDeadNode`, line 486/492 in the eviction loop. The `BlockMemory`
     was destroyed while the node sat in the queue.
  2. **The block was re-pinned and re-enqueued.** The node's
     `handle_sequence_number` no longer equals the block's current
     `GetEvictionSequenceNumber()` — line 55, and line 499 in the loop. A
     newer node for the same block exists further back in the queue, so this
     one is a corpse.

  Both paths call `DecrementDeadNodes()` when found, because the corpse was
  counted at `IncrementDeadNodes()` (buffer_pool.cpp:287) the moment it was
  superseded.

  </details>

- [ ] You can explain why re-pin does not remove the block's queue entry, and what it does instead.

  <details><summary>Answer</summary>

  Because the queue is a lock-free MPMC FIFO
  (`duckdb_moodycamel::ConcurrentQueue`, :61) and removing from the middle of
  one requires either a lock or an O(n) scan — either of which would put a
  contended operation on the hot path, where re-pins are frequent.

  Instead the entry is *invalidated*: `BufferPool::AddToEvictionQueue` (:274)
  counts the previous live entry as dead (:287) and bumps the block's
  eviction sequence number (:290), then pushes a fresh node carrying the new
  number (:299–300). The stale node stays in the queue until someone pops it, and
  whoever does — either `PurgeIteration` or the eviction loop — throws it
  away in O(1). The cost of correctness has been moved from the frequent
  operation (re-pin, now free) to the rare one (purge, batched).

  </details>

- [ ] You can compute the purge cadence, the amortized work per unpin, and the corpse ratio the policy tolerates.

  <details><summary>Answer</summary>

  From the constants at buffer_pool.cpp:115–124:

  - **Cadence**: a purge is triggered when insertions hit a multiple of
    `INSERT_INTERVAL = 4096` (:146). One thread wins the `try_lock` at :156;
    the rest return immediately.
  - **Work per purge**: `purge_size = INSERT_INTERVAL × PURGE_SIZE_MULTIPLIER`
    = 8,192 nodes, dequeued in bulk (:225). So **2 node inspections per
    unpin**, amortized. It deliberately sweeps twice what it inserted — the
    comment at :176 says this is what stops the queue oscillating around the
    trigger.
  - **Floor**: nothing is purged while the queue is below
    `purge_size × EARLY_OUT_MULTIPLIER` = 32,768 nodes (:169), to keep the
    LRU characteristic (:168).
  - **Corpse ratio**: the aggressive loop stops when
    `alive × (ALIVE_NODE_MULTIPLIER − 1) > dead` (:207), i.e. once
    `dead < 3 × alive`. So up to **75% of the queue may be corpses**, and the
    queue may be up to 4× the live set. For a 16 GB pool of 256 KB row groups
    (65,536 live blocks) that is 262,144 nodes of ~24 bytes ≈ 6.3 MB, or
    0.04% of the pool.

  </details>

- [ ] You can name the postgres structure that each DuckDB piece replaces, and say what postgres pays instead.

  <details><summary>Answer</summary>

  | DuckDB | postgres | who pays |
  |---|---|---|
  | heap allocation + `BlockMemory` | fixed 8 KB frame in the `shared_buffers` array | postgres pays a fixed, pre-committed budget; DuckDB pays per-allocation bookkeeping |
  | `atomic<int32_t> readers` | the 18-bit refcount inside the packed state word | same idea, same cost |
  | eviction queue of hints | `nextVictimBuffer` + 4-bit usage counts | postgres pays a usage bump inside the pin CAS on **every hit**; DuckDB pays nothing on hits and makes the evictor skip corpses |
  | `PurgeIteration` every 4,096 inserts | nothing — the clock array never grows stale | DuckDB's amortized 2 inspections/unpin is the price of not having an array |
  | `EvictBlocksOrThrow` | no equivalent; postgres errors only when *all* buffers are pinned (freelist.c:274) | DuckDB fails the query at the limit; postgres relies on the fixed array making the limit unreachable |
  | `WriteTemporaryBuffer` | per-operator `work_mem` and private temp files | one central budget vs many local ones |

  </details>

- [ ] You can say what DuckDB does when the budget cannot be met, and why that is the right failure for an embedded engine.

  <details><summary>Answer</summary>

  `EvictBlocksOrThrow` (standard_buffer_manager.cpp:127) runs the eviction
  loop first, and if `r.success` is false it throws an `OutOfMemoryException`
  at :135 with the used and maximum figures formatted into the message. The
  allocation never happens; the query dies, the process does not.

  That is right for an embedded engine because DuckDB is a library inside
  someone else's process. Exceeding the budget would not just harm DuckDB —
  it would take the host application down with an OOM kill, at a moment the
  host cannot attribute or handle. A thrown exception is attributable
  (memory tag, used/max in the message), catchable, and retryable with a
  higher `memory_limit`. Postgres can afford the opposite default because it
  owns its processes and its `shared_buffers` array is preallocated, so the
  limit is enforced by construction rather than by checking.

  </details>

- [ ] You wrote answers to all three questions in notes.md.

  <details><summary>Answer</summary>

  Nothing to unfold. Question 2 is the one with real content: the worst case
  is `N` corpses per block re-pinned `N` times between purges, bounded in
  practice by the 3:1 dead:alive rule at :207 — so the honest answer names
  the workload where that bound is reached (many small, repeatedly re-pinned
  blocks) and compares it to a clock array, which has *no* stale state to
  sweep because its metadata lives with the frame instead of in a queue.

  </details>

## References

**Code** — [duckdb/duckdb](https://github.com/duckdb/duckdb) at `d54cbbe3df` (v2.0.0).
Local clone at `~/repos/duckdb`; the pin table is at the end of
`resources/codebases.md`.

| File | Lines | What |
|---|---|---|
| `src/include/duckdb/storage/buffer/block_handle.hpp` | 32–34, 262 | `CanUnloadResult`, and the `BlockMemory` / `BlockHandle` split |
| `src/include/duckdb/storage/buffer/block_handle.hpp` | 71–93 | residency state and the pin count |
| `src/include/duckdb/storage/buffer/block_handle.hpp` | 121–131, 235, 242–248 | the eviction sequence number, `readers`, `has_queue_entry` |
| `src/storage/buffer/block_handle.cpp` | 105–121 | `CanUnload`'s three conditions |
| `src/storage/buffer/buffer_pool.cpp` | 42–61 | the node, `IsDeadNode`, the queue type |
| `src/storage/buffer/buffer_pool.cpp` | 115–124 | the four constants that define the purge policy |
| `src/storage/buffer/buffer_pool.cpp` | 144–254 | `AddToEvictionQueue`, `Purge`, `PurgeIteration` |
| `src/storage/buffer/buffer_pool.cpp` | 256–270 | eight queues in three types |
| `src/storage/buffer/buffer_pool.cpp` | 274–301 | invalidate-then-re-enqueue |
| `src/storage/buffer/buffer_pool.cpp` | 398–439 | `EvictBlocksInternal` |
| `src/storage/buffer/buffer_pool.cpp` | 472–536 | `IterateUnloadableBlocks` — the three checks |
| `src/include/duckdb/storage/buffer/buffer_pool.hpp` | 123–129 | `EVICTION_QUEUE_TYPES` and the per-type counts |
| `src/storage/standard_buffer_manager.cpp` | 126–138 | `EvictBlocksOrThrow` |
| `src/storage/standard_buffer_manager.cpp` | 364–412 | `Pin`: lock, drop, evict, re-check |
| `src/storage/standard_buffer_manager.cpp` | 533–540 | spilling to the temp-file manager |

**Related**
- [`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md) — the fixed-array
  design this one is the inverse of.
- [`reading-redis-zmalloc.md`](reading-redis-zmalloc.md) — the third
  accounting philosophy: count after the fact.
- [FINDINGS.md row 6](../../FINDINGS.md) — what happens when nobody enforces
  a budget and the OS pages for you.
