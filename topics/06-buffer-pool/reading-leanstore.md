# LeanStore in code: swips, cooling, hybrid latches

The paper claims a hot page access can cost zero *writes* to shared memory;
this chapter walks the classic ICDE '18 codebase to see how — a `union` that
is either a pointer or a page id, a background thread that samples random
frames, and latches whose readers hold nothing and abort by `longjmp`. Read
the paper guide ([reading-leanstore-paper.md](reading-leanstore-paper.md))
first for the why; this chapter rebuilds the mechanism step by step, then
hands you the file and line anchors to watch each piece work.

Read at [`leanstore/leanstore@90fcf18`](https://github.com/leanstore/leanstore),
the repo's pinned commit (the pin table is at the end of
`resources/codebases.md`; local clone at `~/repos/leanstore`). Everything is
under `backend/leanstore/`. Three places the **code differs from the paper**
are flagged as you reach them — they are the most useful part of reading both.

## The problem in one sentence

Postgres charges every page access a hash probe plus a partition LWLock plus
a CAS pin — two atomics and a likely cache miss even when the page is already
in RAM ([`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md), Step 3) —
and LeanStore's code has to deliver the same page, crash-safe and evictable,
for the cost of a dereference and a version compare.

## The concepts, step by step

### Step 1 — the swip: one u64 that is either a pointer or a page id

> **In:** nothing yet — this is the representation everything else reads.
> **Out:** three states (HOT / COOL / EVICTED) encoded in two bits, which
> Step 2 branches on and Step 3 transitions between.

A **swip** is a reference slot inside a parent node. It holds *either* a raw
in-memory pointer to a **BufferFrame** — the RAM slot holding a page, plus
its header — *or* the page's on-disk **page id**. There is no separate field
saying which; it is a `union`, and two tag bits in the high end of the word
decide how to read it:

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/storage/buffer-manager/Swip.hpp, the tag bits, 20-34
    20     // 1xxxxxxxxxxxx evicted, 01xxxxxxxxxxx cooling, 00xxxxxxxxxxx hot
    21     static const u64 evicted_bit = u64(1) << 63;
    22     static const u64 evicted_mask = ~(u64(1) << 63);
    23     static const u64 cool_bit = u64(1) << 62;
    24     static const u64 cool_mask = ~(u64(1) << 62);
    25     static const u64 hot_mask = ~(u64(3) << 62);
    26     static_assert(evicted_bit == 0x8000000000000000, "");
    27     static_assert(evicted_mask == 0x7FFFFFFFFFFFFFFF, "");
    28     static_assert(hot_mask == 0x3FFFFFFFFFFFFFFF, "");
    29
    30    public:
    31     union {
    32        u64 pid;
    33        BufferFrame* bf;
    34     };
```

Line 31's `union` is the whole trick: the same 8 bytes are a `u64 pid` or a
`BufferFrame*`, and lines 45–47 read the tag to say which.

```
 bit 63 (evicted)  bit 62 (cool)
      0                 0        HOT     — the bytes ARE a BufferFrame*
      0                 1        COOL    — frame exists; mask the bit off (:51)
      1                 -        EVICTED — low 63 bits are the page id (:49)
```

The transitions are three one-line mutators: `warm()` clears the cool bit
(:59–63), `cool()` sets it (:65), `evict(pid)` overwrites the word with a
page id plus bit 63 (:67).

> **Code vs paper #1.** §III-A of the paper uses **one** tag bit and keeps
> cooling pages in a separate hash table; the code uses **two** and encodes
> `COOL` in the swip itself. That single change is why Step 2's COOL arm is a
> bit-clear rather than a hash lookup — the code is strictly cheaper than the
> design it published.

The buffer pool's mapping table is thereby *distributed into the parent
nodes*: no hash lookup and no partition lock on any hot access. The price is
the paper's §IV-B rule — exactly one swip may reference a page, because if
two parents held raw pointers, un-swizzling on eviction could not find them
both. There is no central table left to consult.

### Step 2 — resolveSwip: the three-arm hot path

> **In:** a swip in one of Step 1's three states, plus an optimistic `Guard`
> on the parent (Step 4).
> **Out:** a usable `BufferFrame&` — and, on the third arm, a page read that
> Step 3's provider thread will eventually undo.

`resolveSwip` is the single function every page access goes through. Its
three arms are the swip's three states, and only the third touches the disk:

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/storage/buffer-manager/BufferManager.cpp, resolveSwip's first two arms, 280-299
   280  // Returns a non-latched BufferFrame, called by worker threads
   281  BufferFrame& BufferManager::resolveSwip(Guard& swip_guard, Swip<BufferFrame>& swip_value)
   282  {
   283     if (swip_value.isHOT()) {
   284        BufferFrame& bf = swip_value.asBufferFrame();
   285        swip_guard.recheck();
   286        return bf;
   287     } else if (swip_value.isCOOL()) {
   288        BufferFrame* bf = &swip_value.asBufferFrameMasked();
   289        swip_guard.recheck();
   290        BMOptimisticGuard bf_guard(bf->header.latch);
   291        BMExclusiveUpgradeIfNeeded swip_x_guard(swip_guard);  // parent
   292        BMExclusiveGuard bf_x_guard(bf_guard);                // child
   293        bf->header.state = BufferFrame::STATE::HOT;
   294        swip_value.warm();
   295        return *bf;
   296     }
   297     // -------------------------------------------------------------------------------------
   298     swip_guard.unlock();  // Otherwise we would get a deadlock, P->G, G->P
   299     const PID pid = swip_value.asPageID();
```

Read the three arms as three price tags:

- **HOT (283–286)** — a load, a branch, and `recheck()` (Step 4: re-read the
  parent's version and compare). No atomic read-modify-write, no shared
  write, no lock. This is the case that runs on essentially every access, and
  it is the entire point of the design.
- **COOL (287–295)** — the second chance. The page is in RAM but unswizzled,
  so the accessor rescues it: three guards (child optimistic, parent
  upgraded to exclusive, child upgraded to exclusive), then flip the frame
  state and clear the bit. Note **two** structures change — the frame's
  `header.state` at 293 *and* the parent's swip at 294 — which is why both
  latches are needed and why this arm, unlike HOT, does write shared memory.
- **EVICTED (298 onward)** — the page fault. Line 298's comment is the
  interesting part: the parent guard is *released first* because the lock
  order here (page → global partition) is the reverse of Step 3's (global →
  page), and the resolution is to not hold both.

The miss path is more careful than "read the page". It takes the partition
mutex (:301), looks the pid up in `partition.io_ht` — an **I/O hash table**
of reads currently in flight (`IOFrame`, Partition.hpp:18–33) — and only if
there is no entry does it pop a free frame (:307), publish an `IOFrame` in
state `READING` (:311), drop the global lock, and call `readPageSync`
(:317). A second thread that wants the same page finds the `READING` entry,
blocks on that `IOFrame`'s mutex (:372–376), and retries — one physical read
serves both. On success the swip is swizzled and the frame marked HOT, in
that order, with the comment shouting why:

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/storage/buffer-manager/BufferManager.cpp, publishing the loaded page, 341-351
   341           swip_guard.recheck();
   342           JMUW<std::unique_lock<std::mutex>> g_guard(partition.ht_mutex);
   343           BMExclusiveUpgradeIfNeeded swip_x_guard(swip_guard);
   344           io_frame.mutex.unlock();
   345           swip_value.warm(&bf);
   346           bf.header.state = BufferFrame::STATE::HOT;  // ATTENTION: SET TO HOT AFTER
   347                                                       // IT IS SWIZZLED IN
   348           // -------------------------------------------------------------------------------------
   349           if (io_frame.readers_counter.fetch_add(-1) == 1) {
   350              partition.io_ht.remove(pid);
   351           }
```

If anything in that block fails validation, `jumpmuCatch` (:356) parks the
frame in the `IOFrame` as `READY` and jumps — the read is not wasted, just
handed to whoever retries.

> **Code vs paper #2.** The frame carries an explicit `STATE` enum —
> `FREE / HOT / COOL / LOADED` (BufferFrame.hpp:19) — *in addition to* the
> swip's tag bits. The paper describes one state machine; the code keeps two
> and asserts they agree (e.g. PageProviderThread.cpp:192–193). Order matters
> at 345–346 for exactly this reason.

### Step 3 — the page provider: replacement by random sampling, twice

> **In:** HOT frames from Step 2, and a free list running low.
> **Out:** COOL frames (phase 1) and free frames (phase 2) — the supply Step
> 2's miss path draws on.

Classic policies do bookkeeping on every access: postgres bumps a usage
counter inside the pin CAS, DuckDB enqueues on every unpin. LeanStore does
*nothing* per access. A background `pageProviderThread`
(PageProviderThread.cpp:28, owning a range of partitions) samples frames at
random instead:

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/storage/buffer-manager/PageProviderThread.cpp, candidate selection, 40-49
    40     auto next_bf_range = [&]() {
    41        const u64 BATCH_SIZE = FLAGS_replacement_chunk_size;
    42        cool_candidate_bfs.clear();
    43        for (u64 i = 0; i < BATCH_SIZE; i++) {
    44           BufferFrame* r_bf = &randomBufferFrame();
    45           DO_NOT_OPTIMIZE(r_bf->header.state);
    46           cool_candidate_bfs.push_back(r_bf);
    47        }
    48        return;
    49     };
```

`randomBufferFrame()` at line 44 is the entire replacement policy's input.
There is no LRU list, no usage counter, no access timestamp — none exists to
consult. Line 45's `DO_NOT_OPTIMIZE` prefetches the header the loop is about
to need. The batch is `FLAGS_replacement_chunk_size`, **default 64**
(Config.cpp:75).

The loop only runs when frames are short (:64):
`dram_free_list.counter < free_bfs_limit`, where the limit is
`FLAGS_free_pct` — **default 1** — percent of the pool, divided across
partitions (BufferManager.cpp:55). Then, per candidate:

1. Take an optimistic guard; skip anything pinned in memory, being written
   back, or exclusively latched (:74).
2. **If it is already COOL, it goes on the evict list (:77–79).** This is
   phase 2's entire input.
3. If it is HOT, check its children — `iterateChildrenSwips` (:90) with
   `all_children_evicted &= swip.isEVICTED()` (:91). If a child is still HOT,
   push *the child* onto the candidate list and repick (:92–97): eviction is
   bottom-up, and inner pages therefore drift towards staying resident, which
   is Fig. 5 of the paper implemented.
4. Find the parent (`findParent`, :114) so the swip can be reached at all,
   then upgrade both parent and child to exclusive and cool the page:

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/storage/buffer-manager/PageProviderThread.cpp, the cooling itself, 143-154
   143                       BMExclusiveUpgradeIfNeeded p_x_guard(parent_handler.parent_guard);
   144                       BMExclusiveGuard r_x_guard(r_guard);
   147                       paranoid(r_buffer->header.state == BufferFrame::STATE::HOT);
   152                       r_buffer->header.state = BufferFrame::STATE::COOL;
   153                       parent_handler.swip.cool();  // Cool the pointing swip before unlocking the current bf
   154                    }
```

Phase 2 then evicts what phase 1 found already cool: `evict_bf` (:171) finds
the parent again, asserts the page is clean (`ensure(!bf.isDirty())`, :190 —
dirty pages are written by `AsyncWriteBuffer` in phase 3 first),
`swip.evict(pid)` (:196), and returns the frame to the free list.

> **Code vs paper #3, the big one.** The paper's cooling stage is a **FIFO
> queue plus a hash table** (§IV-C), and the paper explicitly rejects
> background threads in favour of synchronous unswizzling by workers. The
> code has **neither**: `struct Partition` (Partition.hpp:65–104) contains an
> I/O hash table, a free list and page-id allocation — and no cooling queue
> at all — while a dedicated `pageProviderThread` (`FLAGS_pp_threads`,
> default 1, Config.cpp:7) does the work. A page is evicted only if it is
> **randomly sampled twice**: once while HOT (→ COOL) and again while still
> COOL. The FIFO's grace period has become a second sampling draw.

**What that costs, as arithmetic.** Take a 16 GiB pool of 16 KiB pages:
`N = 1,048,576` frames, `FLAGS_free_pct = 1`, chunk 64.

```
 finding a victim: a draw hits a COOL frame with probability c ≈ the cool
 fraction, so
     expected draws per eviction = 1/c
     at c = 1%:  100 random frame-header reads per page evicted
 each draw is a random access into a 16 GiB array: a near-certain cache miss
 (≈219 ns for a random page touch over 128 GB, vmcache Table 2), so
     ≈ 100 × 219 ns ≈ 22 µs of sampling per eviction
 against an NVMe read of ≈100 µs — the sampling is ~20% of the I/O it
 enables, and it runs on a background thread, off the critical path.

 the grace period: with k = 64 draws per round, a given frame waits
     N/k = 1,048,576/64 = 16,384 rounds
 between visits, on average. That interval IS the second chance — any single
 access during it calls resolveSwip's COOL arm and re-swizzles the page.

 what randomness gives up: to visit EVERY frame once, uniform sampling needs
     N·ln N = 1,048,576 × 13.86 ≈ 14.5M draws  (coupon collector)
 where postgres's clock hand needs exactly N = 1,048,576 visits — 14×
 fewer. LeanStore does not care, because it never needs full coverage; it
 needs one victim.
```

Why it matters: the per-access cost of replacement is *zero*, and the price
is paid in sampling draws on a background thread. That is the trade the
paper's §VI-B hit-rate table prices at 0.3 percentage points against LRU.

### Step 4 — hybrid latches: readers that hold nothing

> **In:** every `Guard` the previous three steps constructed.
> **Out:** the safety argument for all of it — and the abort mechanism
> (`jumpmu`) those steps' `jumpmuTry` blocks depend on.

A **latch** is a short-lived lock protecting an in-memory structure (as
opposed to a transactional lock over data). A classic read latch is an atomic
increment, which bounces the cache line between every reading core.
LeanStore's `HybridLatch` supports three modes in one 64-byte object:

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/sync-primitives/Latch.hpp, HybridLatch, 21-43
    21  constexpr static u64 LATCH_EXCLUSIVE_BIT = 1ull;
    22  constexpr static u64 LATCH_VERSION_MASK = ~(0ull);
    23  // -------------------------------------------------------------------------------------
    24  using VersionType = atomic<u64>;
    25  struct alignas(64) HybridLatch {
    26     VersionType version;
    27     std::shared_mutex mutex;
    41     bool isExclusivelyLatched() { return (version & LATCH_EXCLUSIVE_BIT) == LATCH_EXCLUSIVE_BIT; }
    42  };
    43  static_assert(sizeof(HybridLatch) == 64, "");
```

`alignas(64)` plus the `static_assert` at 43 mean one latch is exactly one
cache line — no false sharing between neighbouring frames. The version's low
bit doubles as the exclusive flag, so the word is **odd while held**:
`toExclusive` (:155) takes the mutex and CASes `version → version + 1`, and
`unlock` (:93–104) adds the bit again and release-stores, leaving it even
with a new value.

Optimistic readers write nothing at all. Validation is one function:

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/sync-primitives/Latch.hpp, Guard::recheck, 84-91
    84     void recheck()
    85     {
    86        // maybe only if state == optimistic
    87        assert(state == GUARD_STATE::OPTIMISTIC || version == latch->ref().load());
    88        if (state == GUARD_STATE::OPTIMISTIC && version != latch->ref().load()) {
    89           jumpmu::jump();
    90        }
    91     }
```

Line 89 is the abort: not an error code, a `longjmp` back to the enclosing
`jumpmuTry` block. That is why Steps 2 and 3 are written as `jumpmuTry` /
`jumpmuCatch` pairs — any validation failure anywhere inside unwinds to the
top and retries the whole operation, and *that* is the deadlock-avoidance
strategy for the conflicting lock orders Step 2's line 298 warned about. The
guard's `faced_contention` flag (:52) even records that it happened, which
feeds contention-split heuristics elsewhere.

This is what makes swizzling safe. A reader holding no pin cannot block
eviction — the page may be cooled or evicted underneath it, and the reader
simply fails `recheck()` and starts over. It is also topic 9's subject making
an early appearance.

### Step 5 — the frame header: dirtiness derived, not flagged

> **In:** the `BufferFrame` that Steps 2 and 3 pass around.
> **Out:** the one design detail from this codebase worth copying into the
> capstone's own pool.

`BufferFrame` (BufferFrame.hpp:18) is a header plus a `Page`, and the header
holds everything the previous steps touched:

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/storage/buffer-manager/BufferFrame.hpp, header and isDirty, 18-27
    18  struct BufferFrame {
    19     enum class STATE : u8 { FREE = 0, HOT = 1, COOL = 2, LOADED = 3 };
    20     struct Header {
    21        WORKERID last_writer_worker_id = std::numeric_limits<u8>::max();  // for RFA
    22        LID last_written_plsn = 0;
    23        STATE state = STATE::FREE;  // INIT:
    24        std::atomic<bool> is_being_written_back = false;
    25        bool keep_in_memory = false;
    26        PID pid = 9999;         // INIT:
    27        HybridLatch latch = 0;  // INIT: // ATTENTION: NEVER DECREMENT
```

```cpp
// leanstore/leanstore@90fcf18 — backend/leanstore/storage/buffer-manager/BufferFrame.hpp, dirtiness without a flag, 84-85
    84     inline bool isDirty() const { return page.PLSN != header.last_written_plsn; }
    85     inline bool isFree() const { return header.state == STATE::FREE; }
```

A **dirty page** is one modified in RAM but not yet written back — and line
84 defines it without a flag at all: the page is dirty exactly when its
current page LSN (`PLSN`, the WAL position of its last modification,
BufferFrame.hpp:68) differs from the LSN it was last written back at
(:22). There is no `is_dirty` bit to keep in sync with the log; the WAL
position *is* the flag, and it is set once on load (BufferManager.cpp:332)
and once per write-back. Compare postgres, which carries a `BM_DIRTY` flag
bit in the state word and must clear it in the right order relative to
`XLogFlush`.

Two more details worth stealing: the latch's "NEVER DECREMENT" comment at :27
(versions only ever grow, so a stale reader can never be fooled by
wraparound within a run), and `alignas(512) struct Page` at :67 — page
buffers are sector-aligned because the reads and writes are `O_DIRECT`.

## Where each step lives in the code

Read in this order: `Swip.hpp` (78 lines, all of it), then `Latch.hpp`'s
`HybridLatch` and `Guard::recheck`, then `resolveSwip`, then phase 1 of
`pageProviderThread`.

| File (under `backend/leanstore/`) | What | Steps |
|------|------|-------|
| `storage/buffer-manager/Swip.hpp` | the tagged union | 1 |
| `storage/buffer-manager/BufferManager.cpp` | `resolveSwip`, the miss path | 2 |
| `storage/buffer-manager/Partition.hpp` | `IOFrame`, the I/O hash table, the free list — **no cooling FIFO** | 2, 3 |
| `storage/buffer-manager/PageProviderThread.cpp` | random sampling, cooling, eviction | 3 |
| `sync-primitives/Latch.hpp` | hybrid latches, `recheck`, `jumpmu` | 4 |
| `storage/buffer-manager/BufferFrame.hpp` | frame header, LSN-derived dirtiness | 5 |
| `Config.cpp` | the defaults that make the policy concrete | 3 |

| Step | Symbol | Location |
|---|---|---|
| 1 | `evicted_bit` (1<<63), `cool_bit` (1<<62), the state comment | Swip.hpp:20–25 |
| 1 | the `union { u64 pid; BufferFrame* bf; }` | Swip.hpp:31–34 |
| 1 | `isHOT` / `isCOOL` / `isEVICTED` | Swip.hpp:45, :46, :47 |
| 1 | `asPageID` (mask bit 63), `asBufferFrameMasked` (mask both) | Swip.hpp:49, :51 |
| 1 | `warm()` clears cool, `cool()` sets it, `evict(pid)` overwrites | Swip.hpp:59–63, :65, :67 |
| 2 | `resolveSwip` — HOT / COOL / EVICTED arms | BufferManager.cpp:281, :283, :287, :298 |
| 2 | the reverse-lock-order comment | BufferManager.cpp:298 |
| 2 | `partition.io_ht` lookup, free-frame pop, `readPageSync` | BufferManager.cpp:305, :307, :317 |
| 2 | swizzle-then-mark-HOT, and why the order matters | BufferManager.cpp:345–347 |
| 2 | a second reader waiting on an in-flight read | BufferManager.cpp:372–386 |
| 2 | `IOFrame` states `READING` / `READY`, `readers_counter` | Partition.hpp:18–33 |
| 3 | `pageProviderThread(p_begin, p_end)` | PageProviderThread.cpp:28 |
| 3 | `randomBufferFrame()` in chunks of `replacement_chunk_size` | PageProviderThread.cpp:40–49 |
| 3 | the trigger: free list below `free_bfs_limit` | PageProviderThread.cpp:64; BufferManager.cpp:55 |
| 3 | already-COOL frames become evict candidates | PageProviderThread.cpp:77–79 |
| 3 | `iterateChildrenSwips`; pick a hot child instead | PageProviderThread.cpp:90–97 |
| 3 | `findParent`; the cooling itself | PageProviderThread.cpp:114, :143–153 |
| 3 | phase 2 `evict_bf`; `ensure(!bf.isDirty())`; `swip.evict` | PageProviderThread.cpp:171, :190, :196 |
| 3 | `free_pct` 1, `pp_threads` 1, `replacement_chunk_size` 64 | Config.cpp:5, :7, :75 |
| 4 | `LATCH_EXCLUSIVE_BIT`; `HybridLatch`; one cache line | Latch.hpp:21, :25–42, :43 |
| 4 | `GUARD_STATE`, `LATCH_FALLBACK_MODE` | Latch.hpp:45, :46 |
| 4 | `Guard::recheck` → `jumpmu::jump()` | Latch.hpp:84–91 |
| 4 | `toExclusive` (mutex + version CAS), `unlock` | Latch.hpp:155, :93–104 |
| 5 | `BufferFrame`, `STATE` enum, header fields, the latch | BufferFrame.hpp:18, :19, :20–27 |
| 5 | `OptimisticParentPointer` — how `findParent` gets cheap | BufferFrame.hpp:45–63 |
| 5 | `alignas(512) struct Page`, `PLSN` / `GSN` | BufferFrame.hpp:67, :68–69 |
| 5 | `isDirty()` from LSNs | BufferFrame.hpp:84 |

## Questions to answer in notes.md

1. The one-parent constraint: why exactly does swizzling forbid two swips to
   the same page? Walk the eviction of a doubly-referenced page. Then decide:
   do FalkorDB's tensor/matrix blocks form a tree or a DAG?
2. Bottom-up eviction (children before parents): what breaks top-down?
   (An evicted parent's swip can't hold a hot child's pointer — the child
   would be unreachable.)
3. Random candidate selection: estimate hit-rate loss vs true LRU on a Zipf
   workload (then measure — experiments/benches/eviction.rs has a FIFO
   arm you can extend with random-cooling).
4. vmcache (SIGMOD '23) removes swizzling — pages live at `virt[pid]`, the
   mapping is the MMU's problem, explicit state machine per page. What of
   LeanStore survives in it? (Cooling idea stays; swips go; one-parent
   constraint gone — that's the headline win.)

## Takeaway

Three mechanisms, each visible in about twenty lines. A tagged union puts the
page table inside the parent node, so a hot access is a load and a compare.
A background thread samples random frames instead of maintaining metadata, so
an access writes nothing — and pays for it in sampling draws (≈1/c per
eviction) rather than per-access bookkeeping. A version-word latch lets
readers hold nothing and abort by `longjmp`, which is what makes the other
two safe. The code is not the paper: two tag bits instead of one, a
page-provider thread instead of synchronous cooling, and a second random draw
instead of a FIFO.

## Done when

Answer each before unfolding it.

- [ ] You can draw the swip state machine — HOT / COOL / EVICTED, every transition, and which thread performs each — from the three mutators in `Swip.hpp`.

  <details><summary>Answer</summary>

  ```
    HOT ──cool()──────────────► COOL ──evict(pid)──► EVICTED
     ▲   PageProviderThread      │  PageProviderThread    │
     │   phase 1 (:153)          │  phase 2 (:196)        │
     │                           │                        │
     └──── warm() ───────────────┘                        │
     │     worker thread, resolveSwip's COOL arm (:294)    │
     │                                                     │
     └──── warm(&bf) ──────────────────────────────────────┘
           worker thread, after readPageSync (:345)
  ```

  Two mutators belong to the background provider (`cool()` in phase 1,
  `evict()` in phase 2, both under exclusive guards on parent *and* child),
  and two belong to whichever worker thread happens to touch the page
  (`warm()` on the COOL arm, `warm(&bf)` after a read). The transitions are
  strictly HOT → COOL → EVICTED going down, and both downward states jump
  straight back to HOT going up — there is no COOL → EVICTED shortcut a
  worker can take, and no HOT → EVICTED shortcut at all.

  </details>

- [ ] You can say exactly what a hot page access costs, and why "zero atomics" is not quite the right claim.

  <details><summary>Answer</summary>

  `resolveSwip`'s HOT arm (BufferManager.cpp:283–286) is: test two bits of
  the swip, dereference it, and call `swip_guard.recheck()` — which is an
  atomic **load** of the parent latch's version word and a compare
  (Latch.hpp:88). So it costs two loads and a branch.

  What it does *not* cost is the thing that matters: no atomic
  read-modify-write, no store to shared memory, no lock acquisition. A pure
  load of a shared cache line leaves it in the Shared state on every reading
  core; postgres's `PinBuffer` CAS (bufmgr.c:3351) takes it Exclusive and
  bounces it. "Zero atomics" is loose; "zero *writes* to shared state" is the
  claim, and it is the one that determines scalability.

  </details>

- [ ] You can explain how the code's replacement policy differs from the paper's, and what plays the role of the cooling FIFO.

  <details><summary>Answer</summary>

  The paper (§IV-C) keeps cooling pages in a FIFO queue plus a hash table
  from page id to queue entry, and has *worker* threads do the unswizzling
  synchronously, explicitly rejecting background threads. The code has no
  queue — `struct Partition` (Partition.hpp:65–104) holds an I/O hash table,
  a free list and page-id allocation, and nothing else — and runs a dedicated
  `pageProviderThread` (`FLAGS_pp_threads`, default 1).

  The FIFO's role is played by **a second random draw**. Phase 1 samples 64
  frames (`replacement_chunk_size`, Config.cpp:75); a HOT one gets cooled
  (:153); an already-COOL one is put on the evict list (:77–79). So a page is
  evicted only if it is sampled twice and not accessed in between, and the
  expected gap between two draws on the same frame — `N/k` rounds, 16,384 for
  a 1,048,576-frame pool — *is* the grace period. It is also why the code
  needs the second tag bit the paper did not: COOL has to be visible in the
  swip, since there is no queue to look in.

  </details>

- [ ] You can work out how many random draws an eviction costs, and say why that is affordable.

  <details><summary>Answer</summary>

  A draw finds an evictable frame with probability equal to the cool
  fraction `c`, so the expected number of draws per eviction is `1/c`. With
  `FLAGS_free_pct = 1` (Config.cpp:5) and a cool fraction of the same order,
  that is ~100 draws. Each is a random read of a `BufferFrame` header
  scattered over the whole pool — a cache miss, on the order of 219 ns for a
  random page touch over 128 GB (vmcache Table 2) — so roughly 22 µs of
  sampling per page evicted.

  Affordable for two reasons. It buys an SSD read of ~100 µs, so it is about
  20% overhead on the operation it enables; and it happens on a background
  thread, so it is not on any query's critical path. What it gives up is
  coverage: visiting every frame once takes `N·ln N` ≈ 14.5M draws against
  the clock hand's exactly `N` ≈ 1.05M visits. LeanStore never needs
  coverage — it needs one victim — which is why it can trade 14× thoroughness
  for zero per-access bookkeeping.

  </details>

- [ ] You can explain why `isDirty()` needs no flag, and what postgres has to do instead.

  <details><summary>Answer</summary>

  `isDirty()` is `page.PLSN != header.last_written_plsn` (BufferFrame.hpp:84):
  the page's current log sequence number against the LSN it was last written
  back at. Any modification advances `PLSN` as a side effect of logging, so
  dirtiness is *derived* and cannot drift out of sync with the WAL. The two
  places it is set are load (`last_written_plsn = page.PLSN`,
  BufferManager.cpp:332) and write-back.

  Postgres instead carries a `BM_DIRTY` flag inside the packed state word and
  must order its clearing against `XLogFlush(recptr)` in `FlushBuffer`
  (bufmgr.c:4585) and against concurrent dirtiers — which is why
  `GetVictimBuffer` needs the comment at bufmgr.c:2577–2583 about a backend
  re-dirtying the page between the sweep and the invalidation. A derived
  predicate has no such race: there is nothing to clear.

  </details>

- [ ] You wrote answers to all four questions in notes.md, including the tree-or-DAG verdict.

  <details><summary>Answer</summary>

  Nothing to unfold — that verdict is the exercise, and it decides whether
  this whole design is admissible for the capstone. The bar for question 1:
  walk the eviction of a page reachable from two parents and name the exact
  line that cannot be executed (`parent_handler.swip.evict(evicted_pid)`,
  PageProviderThread.cpp:196 — `findParent` returns *one* handler). For
  question 3, the paper's own answer is §VI-B's table (LeanEvict 92.8% vs LRU
  93.1% at Zipf 1.0); yours should come from `benches/eviction.rs`.

  </details>

## References

**Code** — [leanstore/leanstore](https://github.com/leanstore/leanstore) at
`90fcf18`, the classic ICDE '18 codebase. Local clone at `~/repos/leanstore`;
all paths below are relative to `backend/leanstore/`.

| File | Lines | What |
|---|---|---|
| `storage/buffer-manager/Swip.hpp` | 20–34 | the two tag bits and the `union` |
| `storage/buffer-manager/Swip.hpp` | 45–67 | the three predicates and the three mutators |
| `storage/buffer-manager/BufferManager.cpp` | 55 | `free_bfs_limit` from `FLAGS_free_pct` |
| `storage/buffer-manager/BufferManager.cpp` | 281–400 | `resolveSwip`, all three arms and the I/O-frame protocol |
| `storage/buffer-manager/Partition.hpp` | 18–33 | `IOFrame` — how two threads share one read |
| `storage/buffer-manager/Partition.hpp` | 65–104 | `Partition` — note what is *not* in it |
| `storage/buffer-manager/PageProviderThread.cpp` | 28–49 | the thread, and random candidate batches |
| `storage/buffer-manager/PageProviderThread.cpp` | 64–108 | the trigger, the COOL shortcut, the children check |
| `storage/buffer-manager/PageProviderThread.cpp` | 143–153 | cooling, under two exclusive guards |
| `storage/buffer-manager/PageProviderThread.cpp` | 171–200 | phase 2 eviction |
| `sync-primitives/Latch.hpp` | 21–43 | `HybridLatch`: version word + `shared_mutex`, one cache line |
| `sync-primitives/Latch.hpp` | 84–104 | `recheck` and `unlock` |
| `sync-primitives/Latch.hpp` | 155–165 | `toExclusive` |
| `storage/buffer-manager/BufferFrame.hpp` | 18–27 | `STATE` enum and the header |
| `storage/buffer-manager/BufferFrame.hpp` | 45–69 | optimistic parent pointer; `alignas(512)` page |
| `storage/buffer-manager/BufferFrame.hpp` | 84 | `isDirty()` |
| `Config.cpp` | 5, 7, 75 | `free_pct` 1, `pp_threads` 1, `replacement_chunk_size` 64 |

**Related**
- [`reading-leanstore-paper.md`](reading-leanstore-paper.md) — the design as
  published, and the three places this code departs from it.
- [`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md) — the classic
  pool this is measured against.
- vmcache (SIGMOD '23) Table 2 — the 219 ns random page touch used in
  Step 3's sampling arithmetic.
