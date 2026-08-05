# DuckDB's buffer pool: eviction by queue of hints

The interesting contrast with postgres: no fixed frame array, no clock sweep
— blocks are heap allocations tracked by `shared_ptr`, and eviction is a
lock-free FIFO of *hints* that are allowed to go stale. Re-pinning never
removes a queue entry; it invalidates one, and the corpses get swept in bulk
later. Mark now, collect later — the amortization move again, this time
inside the replacement policy itself. This chapter builds the design step by
step, works out what the staleness actually costs in queue length, then maps
each piece to the C++.

Read at [`duckdb/duckdb@6c0c1a68`](https://github.com/duckdb/duckdb), the
repo's pinned commit (pin table at the end of `resources/codebases.md`; local
clone at `~/repos/duckdb`). One naming note before you open the files: the
control object was split. **`BlockMemory`** (block_handle.hpp:32) owns the
residency state, the pin count and the eviction sequence number;
**`BlockHandle`** (:251) is the outer handle callers hold. The eviction queue
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
// duckdb/duckdb@6c0c1a68 — src/include/duckdb/storage/buffer/block_handle.hpp, BlockMemory's residency accessors, 69-84
    69  	//! Returns true, if the block state is BLOCK_UNLOADED.
    70  	bool IsUnloaded() const {
    71  		return state == BlockState::BLOCK_UNLOADED;
    72  	}
    73  	//! Returns the number of readers.
    74  	int32_t GetReaders() const {
    75  		return readers;
    76  	}
    77  	//! Increments the number of readers prior to returning it.
    78  	int32_t IncrementReaders() {
    79  		return ++readers;
    80  	}
    81  	//! Decrements the number of readers prior to returning it.
    82  	int32_t DecrementReaders() {
    83  		return --readers;
    84  	}
```

`readers` is an `atomic<int32_t>` (:222) — the **pin** count, "in use, don't
evict", the same concept as postgres's refcount and LeanStore's absence of
one. `state` is `BLOCK_LOADED` or `BLOCK_UNLOADED`: the bytes are in memory,
or they are not. Eviction's one question is answered by `CanUnload`:

```cpp
// duckdb/duckdb@6c0c1a68 — src/storage/buffer/block_handle.cpp, CanUnload, 109-125
   109  bool BlockMemory::CanUnload() const {
   110  	if (GetState() == BlockState::BLOCK_UNLOADED) {
   111  		// The block has already been unloaded.
   112  		return false;
   113  	}
   114  	if (GetReaders() > 0) {
   115  		// There are active readers.
   116  		return false;
   117  	}
   118  	if (BlockId() >= MAXIMUM_BLOCK && MustWriteToTemporaryFile() && !GetBufferManager().HasTemporaryDirectory()) {
   119  		// The block memory cannot be destroyed upon eviction/unpinning.
   120  		// In order to unload this block we need to write it to a temporary buffer.
   121  		// However, no temporary directory is specified, hence, we cannot unload.
   122  		return false;
   123  	}
   124  	return true;
   125  }
```

Line 118 is Step 6 arriving early: some blocks have no home on disk to be
dropped back to, and if there is nowhere to spill them, they are simply not
evictable.

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
// duckdb/duckdb@6c0c1a68 — src/storage/buffer/buffer_pool.cpp, BufferEvictionNode and its liveness test, 42-59
    42  BufferEvictionNode::BufferEvictionNode(weak_ptr<BlockMemory> block_memory_p, idx_t eviction_seq_num)
    43      : memory_p(std::move(block_memory_p)), handle_sequence_number(eviction_seq_num) {
    44  	D_ASSERT(!memory_p.expired());
    45  }
    46
    47  bool BufferEvictionNode::IsDeadNode(optional_idx debug_sleep_micros) {
    48  	auto shared_memory_p = memory_p.lock();
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
  stamp, `atomic<idx_t> eviction_seq_num` (block_handle.hpp:231). Line 55: if
  the block's current number has moved on, this node has been superseded.

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
// duckdb/duckdb@6c0c1a68 — src/storage/buffer/buffer_pool.cpp, BufferPool::AddToEvictionQueue, 281-297
   281  		// Count the previous live entry before bumping the sequence number. PurgeIteration
   284  		queue.IncrementDeadNodes();
   296  	BufferEvictionNode node(handle->GetMemoryWeak(), ts);
   297  	return queue.AddToEvictionQueue(std::move(node));
```

```
 re-pin does NOT remove the queue entry (that needs a lock or an O(n) search);
 it INVALIDATES it with a sequence bump and enqueues a fresh node later.
 → same amortization move as topic 2's incremental rehash and topic 4's
   tombstones: mark now, collect in bulk later.
```

Corpses pile up, so collection is amortized and bounded by four constants
that sit together in the source and are worth reading as a policy:

```cpp
// duckdb/duckdb@6c0c1a68 — src/storage/buffer/buffer_pool.cpp, the purge policy constants, 115-124
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
dequeues `purge_size` nodes in bulk (:225), drops the dead ones, and
re-enqueues the survivors in bulk (:249). The loop's two early-outs are at
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
// duckdb/duckdb@6c0c1a68 — src/storage/buffer/buffer_pool.cpp, EvictionQueue::IterateUnloadableBlocks, 467-506
   467  	for (;;) {
   468  		// get a block to unpin from the queue
   469  		BufferEvictionNode node;
   470  		if (!q.try_dequeue(node)) {
   471  			// we could not dequeue any eviction node, so we try one more time,
   472  			// but more aggressively
   473  			if (!TryDequeueWithLock(node)) {
   474  				return;
   475  			}
   476  		}
   477
   478  		// get a reference to the underlying block pointer
   479  		auto handle = node.memory_p.lock();
   485  		if (!handle) {
   486  			DecrementDeadNodes();
   487  			continue;
   488  		}
   489
   490  		// we might be able to free this block: grab the mutex and check if we can free it
   491  		auto lock = handle->GetLock();
   492  		if (node.handle_sequence_number != handle->GetEvictionSequenceNumber()) {
   493  			// A newer entry superseded this node: it was counted as dead when that entry was added.
   494  			DecrementDeadNodes();
   495  			continue;
   496  		}
   499  		handle->SetHasLiveQueueEntry(lock, false);
   500  		if (!handle->CanUnload()) {
   501  			// The block cannot be unloaded right now (e.g. it is pinned). It gets a new queue
   502  			// entry when it is unpinned again.
   503  			continue;
   504  		}
   505
   506  		if (!fn(node, handle, lock)) {
```

Line 479: the block may already be gone (`weak_ptr` upgrade fails). Line 492:
it may have been re-pinned since (Step 3's corpse). Line 500: it may be
pinned *right now*, in which case the node is simply dropped — line 502 is
the design in one sentence: "It gets a new queue entry when it is unpinned
again." Nothing is ever put back to preserve ordering.

The callback that actually frees is in `EvictBlocksInternal` (:391): it
early-returns if usage is already under the limit (:397), and otherwise
`Unload`s (:414) until it is (:416). There is a nice special case at
:406–408 — if the victim's allocation is exactly the size being requested,
the memory is handed over directly instead of being freed and re-malloc'd.

Why it matters: the cost model inverts postgres's. Hits and re-pins pay
nothing; the *evictor* walks everyone's corpses. Postgres pays a usage-count
CAS on every hit so its clock hand never has to skip anything stale
([`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md), Step 4).

One refinement: there is not one queue but **eight**, in three types —
`BLOCK_AND_EXTERNAL_FILE_QUEUE_SIZE = 1`, `MANAGED_BUFFER_QUEUE_SIZE = 6`,
`TINY_BUFFER_QUEUE_SIZE = 1` (buffer_pool.hpp:116–122), constructed in that
order at buffer_pool.cpp:255–266. Managed buffers are sharded six ways
because they are the contended case; blocks and tiny buffers are not.

### Step 5 — memory reservations: a gate in front of malloc

> **In:** Step 4's eviction loop, as a subroutine.
> **Out:** either a reservation that fits the budget, or a thrown query
> error — never an over-limit allocation.

The budget is enforced *before* allocating, not observed after:

```cpp
// duckdb/duckdb@6c0c1a68 — src/storage/standard_buffer_manager.cpp, EvictBlocksOrThrow, 126-137
   126  TempBufferPoolReservation StandardBufferManager::EvictBlocksOrThrow(QueryContext context, MemoryTag tag,
   127                                                                      idx_t memory_delta, unique_ptr<FileBuffer> *buffer,
   128                                                                      ARGS... args) {
   129  	auto r = buffer_pool.EvictBlocks(context, tag, memory_delta, buffer_pool.maximum_memory, buffer);
   130  	if (!r.success) {
   131  		string extra_text = StringUtil::Format(" (%s/%s used)", StringUtil::BytesToHumanReadableString(GetUsedMemory()),
   132  		                                       StringUtil::BytesToHumanReadableString(GetMaxMemory()));
   133  		extra_text += InMemoryWarning();
   134  		throw OutOfMemoryException(args..., extra_text);
   135  	}
   136  	return std::move(r.reservation);
   137  }
```

If eviction cannot free enough, line 134 throws — the *query* fails, with the
used/max numbers in the message, rather than the process being OOM-killed.
Callers supply the message: "could not allocate block of size %s" at :155 for
`Allocate`, "failed to pin block of size %s" at :364 for `Pin`.

`Pin` (:337) composes everything: take the block's lock, and if the state is
`BLOCK_LOADED` just `Load` (increment readers) and return (:349–351). If not,
**drop the lock** — the comment at :338–340 explains that returning a
`BufferHandle` while holding the lock would deadlock on its destructor — run
`EvictBlocksOrThrow` (:362), then re-take the lock and *re-check* the state
(:369), because another thread may have loaded the block while eviction was
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
(standard_buffer_manager.cpp:501) hands them to the temp-file manager (:508),
and Step 5's reload path brings them back on demand. That is why DuckDB joins
bigger than RAM work at all — eviction and spilling are one mechanism, not
two. It is also why `CanUnload` returns false when there is no temporary
directory (block_handle.cpp:118): with nowhere to spill, an unspillable block
is pinned in effect.

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

Read `buffer_pool.cpp` top to bottom — it is 612 lines and contains the whole
policy — then dip into `standard_buffer_manager.cpp` for `Pin` and the
reservation path.

| File | What | Steps |
|------|------|-------|
| `src/include/duckdb/storage/buffer/block_handle.hpp` | `BlockMemory` (:32) and `BlockHandle` (:251) | 1 |
| `src/storage/buffer/block_handle.cpp` | `CanUnload`, `Unload` | 1, 4 |
| `src/storage/buffer/buffer_pool.cpp` | node, queue, purge policy, eviction loop | 2–4 |
| `src/include/duckdb/storage/buffer/buffer_pool.hpp` | how many queues, of which types | 4 |
| `src/storage/standard_buffer_manager.cpp` | reservations, `Pin`, spilling | 5–6 |

| Step | Symbol | Location |
|---|---|---|
| 1 | `BlockMemory` / `BlockHandle` class split | block_handle.hpp:32, :251 |
| 1 | `IsUnloaded`, `GetReaders`, `Increment/DecrementReaders` | block_handle.hpp:69–84 |
| 1 | `atomic<int32_t> readers`, `atomic<idx_t> eviction_seq_num` | block_handle.hpp:222, :231 |
| 1 | `GetEvictionSequenceNumber` / the bump | block_handle.hpp:111, :116 |
| 1 | `CanUnload` — declaration, then the three conditions | block_handle.hpp:208; block_handle.cpp:109–125 |
| 2 | `BufferEvictionNode` ctor and `IsDeadNode` | buffer_pool.cpp:42, :47–59 |
| 2 | the lock-free queue type | buffer_pool.cpp:61 |
| 3 | `BufferPool::AddToEvictionQueue` — count dead, then re-enqueue | buffer_pool.cpp:271, :284, :296 |
| 3 | `EvictionQueue::AddToEvictionQueue` returns "time to purge" | buffer_pool.cpp:144–147 |
| 3 | the four purge constants | buffer_pool.cpp:115–124 |
| 3 | `Purge` — single-purger `try_lock`, both early-outs | buffer_pool.cpp:154, :156, :169, :198, :207 |
| 3 | `PurgeIteration` — bulk dequeue, drop dead, bulk re-enqueue | buffer_pool.cpp:215, :225, :236, :249 |
| 4 | `IterateUnloadableBlocks` — the three checks | buffer_pool.cpp:465, :479, :492, :500 |
| 4 | `EvictBlocksInternal` — the callback that frees | buffer_pool.cpp:391, :397, :406, :414 |
| 4 | queue counts by type | buffer_pool.hpp:116–122; buffer_pool.cpp:255–266 |
| 5 | `EvictBlocksOrThrow` and the throw | standard_buffer_manager.cpp:126, :134 |
| 5 | the two caller messages | standard_buffer_manager.cpp:155, :364 |
| 5 | `Pin`, its deadlock comment, and the re-check after eviction | standard_buffer_manager.cpp:337, :338–340, :349, :369 |
| 6 | `WriteTemporaryBuffer` → temp-file manager | standard_buffer_manager.cpp:501, :508 |

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
     52 in `IsDeadNode`, line 479/485 in the eviction loop. The `BlockMemory`
     was destroyed while the node sat in the queue.
  2. **The block was re-pinned and re-enqueued.** The node's
     `handle_sequence_number` no longer equals the block's current
     `GetEvictionSequenceNumber()` — line 55, and line 492 in the loop. A
     newer node for the same block exists further back in the queue, so this
     one is a corpse.

  Both paths call `DecrementDeadNodes()` when found, because the corpse was
  counted at `IncrementDeadNodes()` (buffer_pool.cpp:284) the moment it was
  superseded.

  </details>

- [ ] You can explain why re-pin does not remove the block's queue entry, and what it does instead.

  <details><summary>Answer</summary>

  Because the queue is a lock-free MPMC FIFO
  (`duckdb_moodycamel::ConcurrentQueue`, :61) and removing from the middle of
  one requires either a lock or an O(n) scan — either of which would put a
  contended operation on the hot path, where re-pins are frequent.

  Instead the entry is *invalidated*: `BufferPool::AddToEvictionQueue` (:271)
  counts the previous live entry as dead (:284) and bumps the block's
  eviction sequence number, then pushes a fresh node carrying the new number
  (:296–297). The stale node stays in the queue until someone pops it, and
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

  `EvictBlocksOrThrow` (standard_buffer_manager.cpp:126) runs the eviction
  loop first, and if `r.success` is false it throws an `OutOfMemoryException`
  at :134 with the used and maximum figures formatted into the message. The
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

**Code** — [duckdb/duckdb](https://github.com/duckdb/duckdb) at `6c0c1a68`.
Local clone at `~/repos/duckdb`; the pin table is at the end of
`resources/codebases.md`.

| File | Lines | What |
|---|---|---|
| `src/include/duckdb/storage/buffer/block_handle.hpp` | 32, 251 | the `BlockMemory` / `BlockHandle` split |
| `src/include/duckdb/storage/buffer/block_handle.hpp` | 69–87 | residency state and the pin count |
| `src/include/duckdb/storage/buffer/block_handle.hpp` | 111–116, 222, 231 | the eviction sequence number and `readers` |
| `src/storage/buffer/block_handle.cpp` | 109–125 | `CanUnload`'s three conditions |
| `src/storage/buffer/buffer_pool.cpp` | 42–61 | the node, `IsDeadNode`, the queue type |
| `src/storage/buffer/buffer_pool.cpp` | 115–124 | the four constants that define the purge policy |
| `src/storage/buffer/buffer_pool.cpp` | 144–251 | `AddToEvictionQueue`, `Purge`, `PurgeIteration` |
| `src/storage/buffer/buffer_pool.cpp` | 255–266 | eight queues in three types |
| `src/storage/buffer/buffer_pool.cpp` | 271–297 | invalidate-then-re-enqueue |
| `src/storage/buffer/buffer_pool.cpp` | 391–432 | `EvictBlocksInternal` |
| `src/storage/buffer/buffer_pool.cpp` | 465–510 | `IterateUnloadableBlocks` — the three checks |
| `src/include/duckdb/storage/buffer/buffer_pool.hpp` | 116–122 | `EVICTION_QUEUE_TYPES` and the per-type counts |
| `src/storage/standard_buffer_manager.cpp` | 126–137 | `EvictBlocksOrThrow` |
| `src/storage/standard_buffer_manager.cpp` | 337–375 | `Pin`: lock, drop, evict, re-check |
| `src/storage/standard_buffer_manager.cpp` | 501–508 | spilling to the temp-file manager |

**Related**
- [`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md) — the fixed-array
  design this one is the inverse of.
- [`reading-redis-zmalloc.md`](reading-redis-zmalloc.md) — the third
  accounting philosophy: count after the fact.
- [FINDINGS.md row 6](../../FINDINGS.md) — what happens when nobody enforces
  a budget and the OS pages for you.
