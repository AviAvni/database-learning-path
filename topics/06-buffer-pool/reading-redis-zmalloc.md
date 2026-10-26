# zmalloc: memory management when there are no pages

Redis has no buffer pool — no pages, no frames, no eviction hand. What it has
instead is an allocation *ledger*: every malloc accounted on per-thread padded
counters, `maxmemory` enforced against that ledger, and key-level eviction
after the fact. This chapter builds the ledger step by step — why accounting
replaces caching, how you learn a pointer's size, why the counters are padded
(and to *what*, which this repo has measured), how eviction hangs off a
statistic, and what fragmentation forces redis to do — plus a bonus: turso's
Rust page cache, the closest existing code to your experiment.

Read at [`redis/redis@e87dda2a4`](https://github.com/redis/redis) (`unstable`,
2026-10-26) and
[`tursodatabase/turso@a820079e8`](https://github.com/tursodatabase/turso)
(v0.9.0-pre.1), the
repo's pinned commits (pin table at the end of `resources/codebases.md`; local
clones at `~/repos/redis` and `~/repos/turso`).

## The problem in one sentence

An in-memory store with a `maxmemory` limit of, say, 4 GB must know — on
every one of millions of mallocs per second, from multiple threads — how
much memory it is using, accurately enough to start evicting keys before the
kernel's OOM killer does it for them.

## The concepts, step by step

### Step 1 — no pages: accounting instead of caching

> **In:** the vocabulary of this topic — buffer pool, page, frame, eviction.
> **Out:** which of those words still mean anything when the dataset is
> already in RAM, which is the frame Steps 2–5 are built in.

Three definitions, since this guide is where they stop applying. A **buffer
pool** is a fixed region of RAM holding copies of disk **pages** (fixed-size
blocks, 8 KB in postgres) in **frames** (the slots that hold them); an
**eviction policy** picks which resident page to drop when a new one must
come in. All three assume the data has a home on disk and RAM is the scarce
copy of it.

Redis keeps *everything* in RAM. There is no disk copy to fall back to, so
there is nothing to cache and nothing to page. Its memory problem is a
different one: track how much the process has allocated (the ledger), compare
against a budget (`maxmemory`), and shed load — evict whole keys, losing the
data — when over. `zmalloc` is a thin wrapper around the allocator (jemalloc
in practice) whose entire job is to maintain that ledger on every allocate and
free.

Why it matters: the mechanisms *look* like a buffer pool's — a limit, an
eviction policy, a sampling scan — but the unit is a variable-size key-value
pair rather than a fixed page, and the trigger is an allocator statistic
rather than a miss. Everything that follows is downstream of that one
substitution.

### Step 2 — the prerequisite: how big is this pointer?

> **In:** Step 1's ledger, which must add on alloc and *subtract* on free.
> **Out:** a size for any pointer — and a per-allocation overhead that is
> either 0 or 8 bytes, which Step 5's fragmentation story then builds on.

To subtract on `free(p)` the ledger must answer "how many bytes was `p`?",
and portable libc has no way to ask. Redis compiles one of two answers:

```c
// redis/redis@e87dda2a4 — src/zmalloc.c, PREFIX_SIZE, 42-51
    42  #ifdef HAVE_MALLOC_SIZE
    43  #define PREFIX_SIZE (0)
    44  #else
    45  /* Use at least 8 bytes alignment on all systems. */
    46  #if SIZE_MAX < 0xffffffffffffffffull
    47  #define PREFIX_SIZE 8
    48  #else
    49  #define PREFIX_SIZE (sizeof(size_t))
    50  #endif
    51  #endif
```

The line to focus on is 43: with an allocator that can size a pointer, the
header costs nothing.

- **With `HAVE_MALLOC_SIZE`** (jemalloc, and macOS's libc): the allocator
  itself reports any pointer's *usable size*, so the prefix is 0 bytes. The
  alloc path calls `zmalloc_size(ptr)` and hands the answer straight to the
  ledger (zmalloc.c:259–263). Redis's bundled jemalloc goes one better: built
  with its `malloc_with_usize` extension (`HAVE_ALLOC_WITH_USIZE`,
  zmalloc.h:89–91), the allocation call *returns* the usable size alongside the
  pointer (:250, :255–258), and `free_with_usize` returns it on free (:588–591),
  so the ledger never pays for a separate size lookup at all.
- **Without either**: redis prepends an 8-byte header holding the size to every
  allocation, returns `ptr + PREFIX_SIZE` to the caller (:264–269), and reads
  the header back on free (:575–579, :596–600). Eight bytes on *every*
  allocation — for a store full of 40-byte keys that is a 20% tax on the data
  itself.

Note *which* size the ledger records: the **usable** size, what the allocator
actually reserved including bucket rounding, not what you asked for. Ask
jemalloc for 100 bytes, get a 112-byte bin, and the ledger records 112. That
gap, times millions of allocations, is Step 5.

### Step 3 — per-thread padded counters: a ledger without coherence traffic

> **In:** Step 2's byte count, produced on every alloc and free.
> **Out:** a total that Step 4 can read, at a per-allocation cost small
> enough to be invisible — the arithmetic for "small enough" is the point of
> this step.

One global `used_memory` updated with an atomic `fetch_add` would put the
same cache line in play on every malloc on every core. The line ping-pongs
between cores, and this repo has measured what that costs: topic 9 found
padding "independent" counters apart to be worth **17.8×**
([FINDINGS.md row 9](../../FINDINGS.md)). Redis pays none of it — one counter
struct per thread, each padded to its own cache line:

```c
// redis/redis@e87dda2a4 — src/zmalloc.c, the per-thread ledger, 95-116
    95  /* Per-thread memory accounting slots. The first DEDICATED_ENTRIES threads
    96   * (typically the main thread plus io threads) each get a private slot and can
    97   * use the cheap single-writer atomic operation (plain load+store).
    98   * Threads beyond that share a pool hashed by thread index and pay the cost of
    99   * a full atomic RMW. */
   100  #define DEDICATED_ENTRIES 8
   101  #define SHARED_ENTRIES 8 /* Must be a power of 2 for modulo */
   102  #define SHARED_ENTRIES_MASK (SHARED_ENTRIES - 1)
   103  #define MAX_ENTRIES (DEDICATED_ENTRIES + SHARED_ENTRIES)
   104  #define PEAK_CHECK_THRESHOLD (1024 * 100) /* 100KB */
   105
   106  typedef struct used_memory_entry {
   107      redisAtomic long long used_memory;
   108      redisAtomic long long last_peak_check;
   109      char padding[CACHE_LINE_SIZE - sizeof(long long) - sizeof(long long)];
   110  } used_memory_entry;
   111
   112  static __attribute__((aligned(CACHE_LINE_SIZE))) used_memory_entry used_memory[MAX_ENTRIES];
   113  static redisAtomic size_t num_active_threads = 0;
   114  static redisAtomic size_t zmalloc_peak = 0;
   115  static redisAtomic time_t zmalloc_peak_time = 0;
   116  static __thread long my_thread_index = -1;
```

Read line 109 and 112 together: explicit tail padding *and* alignment, so no
two slots can share a line. And `CACHE_LINE_SIZE` is not 64 everywhere —

```c
// redis/redis@e87dda2a4 — src/config.h, CACHE_LINE_SIZE, 38-44
    38  #ifndef CACHE_LINE_SIZE
    39  #if defined(__aarch64__) && defined(__APPLE__)
    40  #define CACHE_LINE_SIZE 128
    41  #else
    42  #define CACHE_LINE_SIZE 64
    43  #endif
    44  #endif
```

— which is the same conclusion this repo reached from the other direction:
topic 9's lane found that on M-series, 64-byte padding only *half*-fixes the
sharing and 128 is what actually works ([FINDINGS.md row 9](../../FINDINGS.md)).
Redis's `#ifdef` and our measurement agree, and neither knew about the other.

The slots come in two kinds, and lines 95–99 say why. A slot owned by exactly
one thread needs no atomic read-modify-write — the **RMW**, an instruction that
reads, changes and writes a location as one indivisible step, which on x86 means
a `lock`-prefixed instruction. A single writer can use a plain load, add and
store, and readers on other threads still see either the old or the new value
(`atomicIncrGetSingleWriter`, atomicvar.h:205–221; its comment puts the
difference at "~20-40 cycles vs ~2-3"). Slots shared by several threads must
pay for the RMW, or concurrent updates would be lost:

```c
// redis/redis@e87dda2a4 — src/zmalloc.c, slot assignment and update, 118-179
   118  static inline void init_my_thread_index(void) {
   119      if (unlikely(my_thread_index == -1)) {
   120          long idx;
   121          atomicGetIncr(num_active_threads, idx, 1);
   122          if (idx < DEDICATED_ENTRIES) {
   123              my_thread_index = idx;
   124          } else {
   125              /* Overflow threads share the shared pool entries (atomic RMW). */
   126              my_thread_index = DEDICATED_ENTRIES + (idx & SHARED_ENTRIES_MASK);
   127          }
   128      }
   129  }
        // ... 130-167: zmalloc_reserve_thread_slots / zmalloc_register_reserved_slot ...
   168  static inline long long update_used_memory_entry(used_memory_entry *entry, long long bytes_delta) {
   169      long long thread_used;
   170
   171      if (my_thread_index < DEDICATED_ENTRIES) {
   172          /* Dedicated slot: single writer, plain load+store (no lock prefix). */
   173          atomicIncrGetSingleWriter(entry->used_memory, bytes_delta, thread_used);
   174      } else {
   175          /* Shared pool slots: multiple writers, atomic RMW required. */
   176          atomicIncrGet(entry->used_memory, thread_used, bytes_delta);
   177      }
   178      return thread_used;
   179  }
```

The line to focus on is 126: the ninth thread and beyond hash into eight shared
slots. The elided functions make sure the threads that allocate most get the
private slots: `main()` reserves a block of dedicated indices before any other
thread runs (`zmalloc_reserve_thread_slots`, :136–146), and each I/O thread
claims one of them at startup (`zmalloc_register_reserved_slot`, :151–166), so
a background thread that happens to allocate first cannot take a dedicated slot
from them.

The total is computed on read, by summing the live counters:

```c
// redis/redis@e87dda2a4 — src/zmalloc.c, zmalloc_used_memory, 658-671
   658  size_t zmalloc_used_memory(void) {
   659      size_t local_num_active_threads;
   660      long long total_mem = 0;
   661      atomicGet(num_active_threads,local_num_active_threads);
   662      if (local_num_active_threads > MAX_ENTRIES) {
   663          local_num_active_threads = MAX_ENTRIES;
   664      }
   665      for (size_t i = 0; i < local_num_active_threads; ++i) {
   666          long long thread_used_mem;
   667          atomicGet(used_memory[i].used_memory, thread_used_mem);
   668          total_mem += thread_used_mem;
   669      }
   670      return total_mem;
   671  }
```

The alloc path itself needs that sum, to maintain the peak — so the sum is
*throttled* by the second counter in the struct. `update_zmalloc_stat_alloc`
always bumps the thread's own counter (:185), but only runs the cross-thread
sum once the thread has allocated `PEAK_CHECK_THRESHOLD` more bytes than at
its last check (:192), then records the new watermark (:219). Frees skip the
check entirely — `update_zmalloc_stat_free` (:223–226) is a bare update of the
thread's own slot.

**What the throttle buys, as arithmetic**:

```
 threshold                100 KB of new allocation per thread (:104)
 average redis allocation ~64 B (small key + value + object header)
 ⇒ one global sum every   100 KB / 64 B ≈ 1,600 allocations
 cost of one global sum   ≤ MAX_ENTRIES = 16 atomic loads
 ⇒ amortized             16 / 1,600 = 0.01 loads per malloc

 without the throttle: 16 loads on every malloc, of lines other cores
 are actively writing — i.e. the exact coherence storm the padding
 was there to avoid, reintroduced by the reader.

 the same allocation on the shared-counter design:
   uncontended L1 hit          ~1 ns   (FINDINGS row 0's latency ladder)
   contended line, 8 cores     the 17.8× that topic 9 measured
   jemalloc tcache malloc      tens of ns
 ⇒ accounting would cost more than the allocation it accounts for.
```

One sharp edge worth seeing: past eight threads the slots are shared. Thread
indices are assigned once (:121), and an index of 8 or more is folded into the
shared pool with `& SHARED_ENTRIES_MASK` (:126) — so index 16 lands on the same
slot as index 8 (8 + (16 & 7) = 8 + 0 = 8), and the two threads share a counter
*and* a cache line. The ledger stays correct, because shared slots use the
atomic RMW (:176), but the coherence traffic the padding exists to prevent comes
back for exactly those threads.

### Step 4 — maxmemory: eviction hangs off a statistic

> **In:** Step 3's `zmalloc_used_memory()` sum.
> **Out:** a decision to free whole keys — sampled, approximate, and taken
> *after* the limit is already breached.

`getMaxmemoryState` reads the ledger and works out the shortfall:

```c
// redis/redis@e87dda2a4 — src/evict.c, getMaxmemoryState, 395-431
   395  int getMaxmemoryState(size_t *total, size_t *logical, size_t *tofree, float *level) {
   396      size_t mem_reported, mem_used, mem_tofree;
        // ... 397-399: blank line and comment ...
   400      mem_reported = zmalloc_used_memory();
        // ... 401-407: report the total; no limit configured → return ...
   408      if (mem_reported <= server.maxmemory && !level) return C_OK;
   409
   410      /* Remove the size of slaves output buffers and AOF buffer from the
   411       * count of used memory. */
   412      mem_used = mem_reported;
   413      size_t overhead = freeMemoryGetNotCountedMemory();
   414      mem_used = (mem_used > overhead) ? mem_used-overhead : 0;
        // ... 415-421: compute the level; still under the limit → return ...
   422      if (mem_used <= server.maxmemory) return C_OK;
   423
   424      /* Compute how much memory we need to free. */
   425      mem_tofree = mem_used - server.maxmemory;
        // ... 426-427: blank line; report the logical total ...
   428      if (tofree) *tofree = mem_tofree;
   429
   430      return C_ERR;
   431  }
```

Lines 410–414 are the honest part: replica output buffers and the AOF buffer
are *not* the dataset, so they are subtracted before the comparison. The
ledger measures the process; the policy is about the data.

Then `performEvictions` (:543) frees `mem_tofree` bytes' worth of **keys** —
complete values, a whole hash, a whole list. Two things about that policy are
easy to get wrong:

- **The default is not to evict.** `maxmemory-policy` defaults to
  `MAXMEMORY_NO_EVICTION` (config.c:3597): over the limit, writes get an
  error and nothing is discarded. Eviction is opt-in, because for many redis
  deployments silently losing data is worse than failing a write.
- **The LRU is sampled, not tracked.** With an eviction policy set, each pass
  samples `maxmemory-samples` keys (default **5**, config.c:3629) via
  `evictionPoolPopulate` (evict.c:139, called at :617) and merges them into a
  16-entry pool (`EVPOOL_SIZE`, evict.c:36) that persists across calls; the
  loop at :637 then walks the pool from best to worst. There is no LRU list —
  keeping one would mean a list write on every access, which is precisely the
  hot-path cost Step 3 spent all that padding to avoid.

**How good is 5 samples?** For a key uniformly in the coldest fraction *f*,
the chance that at least one of *k* samples lands there is `1 − (1−f)^k`:

```
 f = 10% coldest, k = 5   1 − 0.9^5  = 41%
 f = 20% coldest, k = 5   1 − 0.8^5  = 67%
 f = 20% coldest, k = 10  1 − 0.8^10 = 89%   (maxmemory-samples 10)
```

A single pass is a poor approximation of LRU. What rescues it is the
16-entry pool surviving across passes, plus the fact that evicting *many*
keys means many independent draws: the probability that a given hot key is
picked before some colder one, repeatedly, falls off fast. Approximation is
affordable here for the same reason it is in DuckDB's queue of hints
([`reading-duckdb-buffer.md`](reading-duckdb-buffer.md)) — being wrong costs
one extra miss, and being exact costs a write on every access.

Compare the three accounting philosophies now on the table:

| System | When the budget is checked | What happens at the limit |
|---|---|---|
| postgres | never — the frame array *is* the budget | a new page evicts an old one; error only if all frames are pinned |
| DuckDB | before every allocation | evict, then throw `OutOfMemoryException` if that was not enough |
| redis | after the fact, from a statistic | error (default), or sample keys and evict whole values |

### Step 5 — active defrag: moving memory that the allocator cannot

> **In:** Step 2's usable-size rounding and a long-running write workload.
> **Out:** why the ledger and the OS disagree about how much memory redis is
> using, and the one mechanism that can close the gap.

**Fragmentation** here means: jemalloc serves allocations from size-class
bins backed by whole pages; freed objects leave holes, and a bin with 3 live
objects in 128 slots still pins all its pages. RSS — what the OS charges you,
and what the OOM killer looks at — stays high while `used_memory` (live
bytes) is low. A normal allocator cannot fix this: it handed out raw
pointers and may never move what they point at.

Redis measures the gap explicitly, and *only* over the bins it could actually
do something about:

```c
// redis/redis@e87dda2a4 — src/defrag.c, getAllocatorFragmentation, 1323-1331
  1323      /* Calculate the fragmentation ratio as the proportion of wasted memory in small
  1324       * bins (which are defraggable) relative to the total allocated memory (including large bins).
  1325       * This is because otherwise, if most of the memory usage is large bins, we may show high percentage,
  1326       * despite the fact it's not a lot of memory for the user. */
  1327      float frag_pct = (float)frag_smallbins_bytes / allocated * 100;
  1328      float rss_pct = ((float)resident / allocated)*100 - 100;
  1329      size_t rss_bytes = resident - allocated;
  1330      if(out_frag_bytes)
  1331          *out_frag_bytes = frag_smallbins_bytes;
```

The line to focus on is 1327: the numerator is waste in small bins only.

It then fixes it *cooperatively*, one allocation at a time:

```c
// redis/redis@e87dda2a4 — src/defrag.c, activeDefragAllocWithoutFree, 145-169
   145  /* this method was added to jemalloc in order to help us understand which
   146   * pointers are worthwhile moving and which aren't */
   147  int je_get_defrag_hint(void* ptr);
        // ... 148-153: #if !defined(DEBUG_DEFRAG_FORCE) and the function's comment ...
   154  void* activeDefragAllocWithoutFree(void *ptr) {
   155      size_t size;
   156      void *newptr;
   157      if(!je_get_defrag_hint(ptr)) {
   158          server.stat_active_defrag_misses++;
   159          return NULL;
   160      }
   161      /* move this allocation to a new allocation.
   162       * make sure not to use the thread cache. so that we don't get back the same
   163       * pointers we try to free */
   164      size = zmalloc_usable_size(ptr);
   165      newptr = zmalloc_no_tcache(size);
   166      memcpy(newptr, ptr, size);
   167      server.stat_active_defrag_hits++;
   168      return newptr;
   169  }
```

Line 147 is a function jemalloc grew *for redis*: "is this pointer sitting in
a sparse run worth moving?" Line 165 is the subtle one — allocate bypassing
the thread cache, or you get handed back the very pointer you are trying to
vacate. The caller (`activeDefragAlloc`, :180) frees the old pointer, and
then — the expensive part this code does not show — every *reference* to it
must be rewritten, which redis can do only because it owns every data
structure holding those pointers. Defragmentation in userspace, because the
allocator cannot move memory it handed out.

**When it runs, as arithmetic.** Two gates and a linear ramp
(`computeDefragCycles`, defrag.c:1512–1549, defaults from config.c:3621–3624
and :3711):

```
 gate 1  frag_pct   ≥ active-defrag-threshold-lower = 10%
 gate 2  frag_bytes ≥ active-defrag-ignore-bytes    = 100 MB
 ramp    cpu_pct = INTERPOLATE(frag_pct, 10, 100, 1, 25)   (:1540)

 a 4 GB instance at 15% fragmentation:
   frag_bytes ≈ 0.15 × 4 GB = 600 MB  ≥ 100 MB      ✓ both gates
   cpu_pct = 1 + (15−10) × (25−1) / (100−10) = 2.33 → int 2% of CPU

 a 500 MB instance at the same 15%:
   frag_bytes ≈ 75 MB < 100 MB                      ✗ gate 2
   → no defrag at all: 75 MB is not worth any CPU

 at 100% fragmentation the ramp saturates at 25% of one core —
 a quarter of redis's single-threaded budget spent moving bytes
 that are already in RAM.
```

The ramp's result is then multiplied by a decay factor (`cpu_pct *=
defrag.decay_rate`, :1546) that starts at 1.0 and shrinks by 0.9 after each
full cycle that barely changed the fragmentation (:1905–1911), and clamped back
into [1, 25] (:1547–1549) — so the figures above are the effort of a defrag that
is still paying off.

FalkorDB angle: GraphBLAS matrices are big opaque zmalloc blobs — redis can
count them but cannot defrag them (their internal pointers are GraphBLAS's,
not redis's), and one matrix can blow the `maxmemory` budget inside a single
`GrB` call, between two checks of the ledger. Your capstone owns its
allocations; decide what "maxmemory" should even mean for a graph store.

### Step 6 — bonus: turso's page cache, the Rust reference

> **In:** everything above, and the buffer-pool vocabulary from Step 1 that
> redis had no use for.
> **Out:** a working Rust implementation to diff your capstone against —
> after you have built yours.

Back in buffer-pool land: turso (topic 1's B-tree) carries a real Rust page
cache, the closest existing code to your `src/buffer_pool.rs` experiment.
Read its header comment before anything else, because it is not the plain
CLOCK the name suggests:

```rust
// tursodatabase/turso@a820079e8 — core/storage/page_cache.rs, PageCache, 90-119
    90  /// PageCache implements a variation of the SIEVE algorithm that maintains an intrusive linked list queue of
    91  /// pages which keep a 'reference_bit' to determine how recently/frequently the page has been accessed.
    92  /// The bit is set to `Clear` on initial insertion and then bumped on each access and decremented
    93  /// during eviction scans.
    94  ///
    95  /// The ring is circular. `clock_hand` points at the tail (LRU).
    96  /// Sweep order follows next: tail (LRU) -> head (MRU) -> .. -> tail
    97  /// New pages are inserted after the clock hand in the `next` direction,
    98  /// which places them at head (MRU) (i.e. `tail.next` is the head).
        // ... 99: #[cfg_attr(feature = "aristo-instr", ...)] ...
   100  pub struct PageCache {
   101      /// Capacity in pages
   102      capacity: usize,
   103      /// Map of Key -> pointer to entry in the queue
   104      map: HashMap<PageCacheKey, *mut PageCacheEntry>,
   105      /// The eviction queue (intrusive doubly-linked list)
   106      queue: LinkedList<EntryAdapter>,
   107      /// Clock hand cursor for SIEVE eviction (pointer to an entry in the queue, or null)
   108      clock_hand: *mut PageCacheEntry,
        // ... 109-112: spill_threshold, spill_enabled ...
   113      /// Conservative estimation of pages that are evictable based on dirty/spilled state.
        // ... 114: #[cfg_attr(...)] ...
   115      evictable_count: usize,
   116  }
   117
   118  unsafe impl Send for PageCache {}
   119  unsafe impl Sync for PageCache {}
```

Three things to take from it:

1. **New pages arrive cold.** `PageCacheEntry::new` sets `ref_bit: CLEAR`
   (:58) and `_insert` splices the entry in *after* the hand (:311–314). In
   classic **clock sweep** — a circular scan that gives each frame a *second
   chance* before evicting it — a new page usually arrives with its reference
   bit *set*, so it survives one full revolution. SIEVE's point is that most
   new pages are one-hit wonders: make them prove themselves instead.
2. **The "bit" is a counter, exactly like postgres's.** `REF_MAX = 3` (:34);
   `bump_ref` saturates at it on every `get` (:65, :426) and `decrement_ref`
   walks it down on the sweep (:717). Postgres's usage count is the same idea
   with a cap of 5
   ([`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md)).
3. **The sweep is bounded, for the same reason postgres's is.**
   `max_examinations = len × (REF_MAX + 1)` (:670) — enough revolutions to
   drive any counter to zero, and then `CacheError::Full` rather than an
   infinite loop. `evictable()` (:653–659) is the pin check: not dirty (or
   already spilled), not locked, not pinned, not the header page, and
   `Arc::strong_count == 1` — Rust's refcount standing in for an explicit
   **pin** (a "someone is using this, don't evict" marker).

Note what is `unsafe` — the `Send`/`Sync` impls at :118–119, the raw
`*mut PageCacheEntry` in the map, `cursor_mut_from_ptr` throughout — and note
that your version does not have to be. Indices into a `Vec<Frame>` are safe,
and give you exactly postgres's layout for free. Diff your design against
this one *after* you have built it; do not copy it first.

## Where each step lives in the code

| File | What | Steps |
|------|------|-------|
| `redis/src/zmalloc.c` | the ledger | 2–3 |
| `redis/src/config.h` | the padding width | 3 |
| `redis/src/evict.c` | `maxmemory` and key eviction | 4 |
| `redis/src/defrag.c` | cooperative userspace defrag | 5 |
| `turso/core/storage/page_cache.rs` | a real Rust SIEVE/clock cache | 6 |

| Step | Symbol | Location |
|---|---|---|
| 2 | `PREFIX_SIZE` — the two builds | zmalloc.c:42–51 |
| 2 | `HAVE_ALLOC_WITH_USIZE` — redis's jemalloc returns the usable size | zmalloc.h:86–91; zmalloc.c:73–81 |
| 2 | alloc path: usize, usable-size and prefix branches | zmalloc.c:246–271 (:255, :259, :264) |
| 2 | `zmalloc_size` reads the header back; `zfree` | zmalloc.c:574–583, :585–602 |
| 3 | `DEDICATED_ENTRIES`, `SHARED_ENTRIES`, `MAX_ENTRIES`, `PEAK_CHECK_THRESHOLD` | zmalloc.c:95–104 |
| 3 | `used_memory_entry` — two counters plus padding | zmalloc.c:106–112 |
| 3 | `CACHE_LINE_SIZE` = 128 on Apple aarch64, else 64 | config.h:38–44 |
| 3 | slot assignment: dedicated, or folded into the shared pool | zmalloc.c:118–129 (:126) |
| 3 | reserved slots for main and I/O threads | zmalloc.c:136–146, :151–166 |
| 3 | single-writer vs RMW update | zmalloc.c:168–179; atomicvar.h:205–221 |
| 3 | `update_zmalloc_stat_alloc` — local bump, throttled sum | zmalloc.c:181–221 (:185, :192, :219) |
| 3 | `update_zmalloc_stat_free` — no check at all | zmalloc.c:223–226 |
| 3 | `zmalloc_used_memory` — the sum over active slots | zmalloc.c:658–671 |
| 4 | `getMaxmemoryState` — read ledger, subtract non-data | evict.c:395–431 (:400, :413, :425) |
| 4 | `performEvictions` | evict.c:543 |
| 4 | `EVPOOL_SIZE` = 16, `evictionPoolPopulate`, the pool walk | evict.c:36, :139, :617, :637 |
| 4 | `maxmemory-policy` default, `maxmemory-samples` default 5 | config.c:3597, :3629 |
| 5 | `getAllocatorFragmentation` — small bins only | defrag.c:1310–1336 (:1327) |
| 5 | `je_get_defrag_hint`, `activeDefragAllocWithoutFree` | defrag.c:145–169 (:157, :165) |
| 5 | `activeDefragAlloc` — move then free | defrag.c:180–185 |
| 5 | `computeDefragCycles` — two gates, the ramp, the decay | defrag.c:1512–1549 (:1526, :1540, :1546) |
| 5 | defrag defaults: 1/25% CPU, 10/100% thresholds, 100 MB floor | config.c:3621–3624, :3711 |
| 6 | `PageCache` and the insertion discipline | page_cache.rs:90–119 |
| 6 | `CLEAR`, `REF_MAX`, `bump_ref`, `decrement_ref` | page_cache.rs:33–34, :64–66, :70–74 |
| 6 | `get` bumps the counter | page_cache.rs:409–428 (:426) |
| 6 | `_insert` splices after the hand | page_cache.rs:256–330 (:302, :311–314) |
| 6 | `advance_clock_hand`, `make_room_for` | page_cache.rs:177, :637 |
| 6 | `evictable`, `evict_one` — the bounded sweep | page_cache.rs:653–659, :662–728 (:670, :687, :717) |

## Questions to answer in notes.md

1. Why per-thread counters instead of one atomic? Estimate the cost of a
   shared `fetch_add` on every malloc at 8 threads (topic-0 numbers).
2. Redis evicts keys; a buffer pool evicts pages. Which gets better hit
   rates for the same RAM and why is the comparison unfair? (Keys are
   variable-size and *complete* — no partial residency of a value.)
3. After building your CLOCK pool: diff your design against turso's — hand
   placement on insert, where usage bits live, pin representation.

## Takeaway

Take pages away and a buffer pool becomes a ledger. The hot path has to be
free, so the counter is sharded per thread and padded to a cache line — 128
bytes on Apple silicon, which is exactly what this repo's topic-9 lane had to
measure the hard way. Reading the total is expensive, so it is throttled to
roughly once per 1,600 allocations. Knowing the total exactly is impossible
anyway, so eviction samples 5 keys and keeps a 16-entry pool. And because the
allocator will not move memory it handed out, redis moves it itself, gated on
a fragmentation percentage and a floor of 100 MB. Every one of those is the
same trade in a different costume: pay approximately and often, or exactly
and rarely.

## Done when

Answer each before unfolding it.

- [ ] You can explain what `PREFIX_SIZE` is for, when it is 0, and what it costs when it is not.

  <details><summary>Answer</summary>

  The ledger has to *subtract* on free, so it must recover an allocation's
  size from a bare pointer. `PREFIX_SIZE` (zmalloc.c:42–51) is how:

  - With `HAVE_MALLOC_SIZE` — jemalloc, or macOS libc — the allocator answers
    the question itself, so `PREFIX_SIZE` is **0** and the alloc path just
    calls `zmalloc_size(ptr)` (:260) — or, with redis's bundled jemalloc,
    gets the size back from the allocation call itself (:250).
  - Without it, redis prepends an 8-byte size header, hands the caller
    `ptr + PREFIX_SIZE` (:264–269), and reads the header back in
    `zmalloc_size` (:575–579) and `zfree` (:596–600).

  The cost is 8 bytes on **every** allocation, not per key — a 40-byte key
  pays 20%. That is a large part of why a jemalloc build is the recommended
  one, quite apart from jemalloc's own behaviour.

  </details>

- [ ] You can say why the counters are padded, to what width, and what happens past 16 threads.

  <details><summary>Answer</summary>

  Padded so that no two threads' counters share a cache line: a shared line
  written by several cores ping-pongs between them, and topic 9 measured
  padding as worth **17.8×** ([FINDINGS.md row 9](../../FINDINGS.md)). The
  struct carries explicit tail padding (zmalloc.c:109) *and* the array is
  aligned (:112).

  To `CACHE_LINE_SIZE`, which redis defines as **128 on Apple aarch64** and 64
  elsewhere (config.h:38–44) — the same platform split topic 9's lane found
  empirically, where 64 only half-fixed the sharing on M-series.

  Past eight threads: the first `DEDICATED_ENTRIES` = 8 thread indices get
  private slots updated with a plain load and store; every later index is
  folded into eight shared slots with `& SHARED_ENTRIES_MASK` (zmalloc.c:126),
  so index 16 shares slot 8 — counter and cache line — with index 8. The
  ledger stays correct because shared slots are updated with an atomic
  read-modify-write (:176), but the false sharing returns for those threads.
  Main and I/O threads reserve dedicated slots at startup (:136–166), so the
  threads that allocate most are not the ones that end up sharing.

  </details>

- [ ] You can compute how often the expensive global sum actually runs, and why that matters.

  <details><summary>Answer</summary>

  `update_zmalloc_stat_alloc` bumps the thread-local counter unconditionally
  (:185) but calls `zmalloc_used_memory()` only when this thread's counter
  has advanced past `PEAK_CHECK_THRESHOLD` = 100 KB since its last check
  (:104, :192), after which it records the new watermark (:219). Frees never
  check at all (:223–226).

  At a ~64-byte average allocation that is one sum per ≈1,600 allocations.
  The sum itself is at most 16 atomic loads (:665–669), so amortized it is
  **0.01 loads per malloc**.

  It matters because the sum reads *every other thread's* counter — the
  exact lines the padding exists to keep private. An unthrottled reader would
  reintroduce, from the read side, the coherence storm the write side was
  designed to avoid.

  </details>

- [ ] You can state what redis does by default when `maxmemory` is exceeded, and how good its LRU actually is.

  <details><summary>Answer</summary>

  By default it **does not evict**: `maxmemory-policy` defaults to
  `MAXMEMORY_NO_EVICTION` (config.c:3597), so writes fail with an error and
  no data is lost. Eviction is an opt-in choice.

  With a policy set, the LRU is sampled, not tracked. Each pass draws
  `maxmemory-samples` keys (default 5, config.c:3629) in
  `evictionPoolPopulate` (evict.c:139) and merges them into a 16-entry pool
  (`EVPOOL_SIZE`, evict.c:36) that persists across passes; the loop at :637
  takes the best entry. A single draw of 5 finds a key from the coldest 10%
  only `1 − 0.9^5 = 41%` of the time, and from the coldest 20% `67%` of the
  time. The persistent pool and the sheer number of evictions are what make
  that acceptable — and the reason to accept it is that a true LRU list
  would need a list write on every access, on the hot path Step 3 worked so
  hard to keep clean.

  </details>

- [ ] You can explain why an ordinary allocator cannot defragment, what redis does instead, and when it bothers.

  <details><summary>Answer</summary>

  An allocator cannot move an allocation because it gave out a raw pointer
  and has no idea who holds copies of it. Redis can, because it owns every
  structure that stores those pointers: `activeDefragAllocWithoutFree`
  (defrag.c:154) asks `je_get_defrag_hint` (:147, a jemalloc entry point
  added for redis) whether a pointer sits in a sparse run, copies it to a
  fresh allocation taken *outside the thread cache* (:165 — otherwise
  jemalloc hands back the pointer you are vacating), and the caller frees
  the old one (:180–185) after every reference has been rewritten.

  It bothers only when both gates in `computeDefragCycles` (:1526) pass:
  fragmentation ≥ 10% **and** wasted bytes ≥ 100 MB. CPU is then interpolated
  linearly from 1% at 10% fragmentation to 25% at 100% (:1540). So a 4 GB
  instance at 15% fragmentation (≈600 MB wasted) spends about 2% of a core;
  a 500 MB instance at the same 15% (≈75 MB) spends nothing, because 75 MB is
  not worth any CPU at all.

  </details>

- [ ] You can name three ways turso's cache differs from a textbook clock sweep, and what your Rust version can do differently.

  <details><summary>Answer</summary>

  It is a SIEVE variation (page_cache.rs:90–93), and:

  1. **New pages start cold.** `ref_bit: CLEAR` on insert (:58), spliced in
     after the hand (:311–314). Textbook clock inserts with the bit *set*, so
     a new page survives a full revolution; SIEVE makes it earn that.
  2. **The bit is a saturating counter**, `REF_MAX = 3` (:34), bumped on
     every `get` (:426, :65) and decremented on the sweep (:717) — postgres's
     usage count with a smaller cap.
  3. **The sweep is explicitly bounded**: `len × (REF_MAX + 1)` examinations
     (:670), then `CacheError::Full` instead of spinning — and `evictable()`
     (:653–659) treats `Arc::strong_count == 1` as the pin check rather than
     keeping a separate pin count.

  Yours can be safe. Turso needs `unsafe impl Send`/`Sync` (:118–119) and raw
  `*mut PageCacheEntry` because the intrusive list holds self-references; a
  `Vec<Frame>` with `u32` indices for the hand and the links has none of that
  and gives you postgres's array layout for free.

  </details>

- [ ] You wrote answers to all three questions in notes.md.

  <details><summary>Answer</summary>

  Nothing to unfold. Question 2 is the one worth care: the comparison is
  unfair because a buffer pool can keep *part* of a value resident — one page
  of a large row — while redis can only keep a key whole or not at all, so
  the same RAM buys a different kind of hit. Say which unit your capstone
  uses before you compare its hit rate to anything.

  </details>

## References

**Code** — [redis/redis](https://github.com/redis/redis) at `e87dda2a4`,
[tursodatabase/turso](https://github.com/tursodatabase/turso) at `a820079e8`.
Local clones at `~/repos/redis` and `~/repos/turso`; the pin table is at the
end of `resources/codebases.md`.

| File | Lines | What |
|---|---|---|
| `redis/src/zmalloc.c` | 42–51 | `PREFIX_SIZE`: the two ways to size a pointer |
| `redis/src/zmalloc.c` | 95–179 | dedicated and shared slots, the padded counter array, slot assignment |
| `redis/src/zmalloc.c` | 181–226 | the throttled alloc stat, and the bare free stat |
| `redis/src/zmalloc.c` | 246–271 | the alloc path's three compile-time branches |
| `redis/src/zmalloc.c` | 574–602 | `zmalloc_size` and `zfree` |
| `redis/src/zmalloc.c` | 658–671 | `zmalloc_used_memory` |
| `redis/src/zmalloc.h` | 86–91 | `HAVE_ALLOC_WITH_USIZE` |
| `redis/src/atomicvar.h` | 205–221 | `atomicIncrGetSingleWriter` |
| `redis/src/config.h` | 38–44 | 128-byte lines on Apple aarch64 |
| `redis/src/evict.c` | 36, 139, 395–431, 543, 617–637 | the pool, the state check, the eviction loop |
| `redis/src/config.c` | 3597, 3629, 3621–3624, 3711 | eviction and defrag defaults |
| `redis/src/defrag.c` | 145–185 | the jemalloc hint and the cooperative move |
| `redis/src/defrag.c` | 1310–1336, 1512–1549 | measuring fragmentation, and the CPU ramp |
| `turso/core/storage/page_cache.rs` | 33–34, 58–74 | `CLEAR`, `REF_MAX`, the counter |
| `turso/core/storage/page_cache.rs` | 90–119 | the SIEVE comment and the struct |
| `turso/core/storage/page_cache.rs` | 177, 256–330 | hand movement and insert-after-hand |
| `turso/core/storage/page_cache.rs` | 637–728 | `make_room_for`, `evictable`, `evict_one` |

**Related**
- [`reading-duckdb-buffer.md`](reading-duckdb-buffer.md) — the other
  approximate policy in this topic, and the opposite budget discipline.
- [`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md) — the usage
  count turso's `ref_bit` is a smaller copy of.
- [FINDINGS.md row 9](../../FINDINGS.md) — the 17.8× that justifies line 109.
