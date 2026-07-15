# zmalloc: memory management when there are no pages

Redis has no buffer pool — no pages, no frames, no eviction hand. What it has
instead is an allocation *ledger*: every malloc accounted on per-thread
padded counters, `maxmemory` enforced against an allocator statistic, and
key-level eviction after the fact. This chapter builds that ledger step by
step — why accounting replaces caching, how you learn a pointer's size, why
the counters are padded, how eviction hangs off a statistic, and what
fragmentation forces redis to do — plus a bonus: turso's CLOCK page cache in
Rust, the closest existing code to your experiment.

## The problem in one sentence

An in-memory store with a `maxmemory` limit of, say, 4 GB must know — on
every one of millions of mallocs per second, from multiple threads — how
much memory it is using, accurately enough to start evicting keys before the
kernel's OOM killer does it for them.

## The concepts, step by step

### Step 1 — no pages: accounting instead of caching

A buffer pool exists to decide which disk pages live in RAM; redis keeps
*everything* in RAM, so there is nothing to cache and nothing to page. Its
memory-management problem is different: track how much the process has
allocated (the ledger), compare against a budget (`maxmemory`), and shed
load (evict whole keys) when over. `zmalloc` is a thin wrapper around the
allocator (jemalloc in practice) whose whole job is to maintain that ledger
on every allocate and free.

Why it matters: the mechanisms look superficially like topic 6's — a limit,
an eviction policy — but the *unit* is a variable-size key-value pair, not
a fixed page, and the trigger is an allocator statistic, not a miss.

### Step 2 — the prerequisite: how big is this pointer?

To subtract on `free(p)`, the ledger must answer "how many bytes was `p`?" —
and plain libc historically had no portable way to ask. Two paths
(`PREFIX_SIZE`, zmalloc.c:39–46):

- With jemalloc (`HAVE_MALLOC_SIZE`), the allocator itself reports any
  pointer's *usable size* (`malloc_usable_size`) ⇒ prefix is 0 bytes.
- Without it, redis prepends an 8-byte size header to every allocation and
  reads it back on free — 8 bytes of overhead on *every* allocation, which
  for a store full of 40-byte keys is real money.

Note that the ledger counts *usable* size (what the allocator actually
reserved, including bucket rounding), not requested size — ask for 100
bytes, jemalloc hands you a 112-byte bin, the ledger records 112. That gap,
multiplied by millions of allocations, is fragmentation (Step 5).

### Step 3 — per-thread padded counters: the ledger without coherence traffic

A single global `used_memory` counter updated with an atomic `fetch_add`
would put the same cache line in play on every malloc on every core — the
line ping-pongs between cores at ~100 cycles a bounce (topic 0's false
sharing), taxing every allocation in the process. Redis instead keeps one
counter *per thread*, each padded out to its own cache line
(`aligned(CACHE_LINE_SIZE)`, a MAX_THREADS array — zmalloc.c:86–92): each
thread bumps its own line, uncontended, and the true total is computed by
summing all counters *when someone reads it* — and reads are rare.

```rust
// One counter per thread, each on its own cache line — a single global
// fetch_add would put coherence traffic on EVERY malloc on EVERY core.
#[repr(align(64))]
struct Padded(AtomicI64);
static USED: [Padded; MAX_THREADS] = /* … */;

fn zmalloc(size: usize) -> *mut u8 {
    let p = unsafe { malloc(size) };
    let real = malloc_usable_size(p);          // jemalloc answers "how big is p?"
    USED[thread_id()].0.fetch_add(real as i64, Relaxed);  // uncontended bump
    p
}
fn used_memory() -> i64 {
    USED.iter().map(|c| c.0.load(Relaxed)).sum()  // the SUM is paid on read,
}                                                 // and reads are rare
```

Even the occasional full sum is throttled: `update_zmalloc_stat_alloc`
(:105–145) bumps the local counter always, but pays for the cross-thread
sum only occasionally (the peak-check throttle, :109–118).

### Step 4 — maxmemory: eviction hangs off a statistic

The ledger's sum is what `maxmemory` compares against. When
`used_memory() > maxmemory`, redis evicts — but the unit of eviction is a
**key** (a complete value: a whole hash, a whole list), chosen by
approximate LRU or LFU over sampled keys, freed *after* the limit is
already breached. Compare the two accounting philosophies in this topic:
DuckDB gates every allocation up front (reserve-or-throw, before malloc);
redis counts after and evicts asynchronously. A key can't be partially
resident — there's no analogue of "page out half the value" — which is
question 2 below.

### Step 5 — active defrag: moving memory that the allocator can't

**Fragmentation** here means: jemalloc serves allocations from size-class
bins backed by pages; free objects leave holes, and a bin holding 3 live
objects out of 128 slots still pins its pages — RSS (what the OS charges
you) stays high while `used_memory` (live bytes) is low. A normal allocator
can't fix this: it handed out raw pointers and may never move the memory
they target.

Redis fixes it *cooperatively*: `activeDefragAlloc` (defrag.c:177, and the
:142 comment) asks jemalloc which allocations sit in sparse bins,
re-allocates each one (new pointer, same bytes), frees the old, and — the
expensive part — **rewrites every reference** to it, which redis can do
because it owns all the data structures that hold the pointers.
Defragmentation in userspace, because the allocator can't move memory it
handed out.

FalkorDB angle: GraphBLAS matrices are big opaque zmalloc blobs — redis can
count them but not defrag them (their internal pointers are GraphBLAS's,
not redis's), and one matrix can blow the maxmemory budget in a single GrB
call. Your capstone owns its allocations; decide what "maxmemory" should
even mean for a graph store.

### Step 6 — bonus: turso's CLOCK page cache, the Rust reference

Back in buffer-pool land: turso (topic 1's B-tree) carries a real Rust
CLOCK implementation — the closest existing code to your
`src/buffer_pool.rs` experiment. Compare with yours *after* you build it
(don't copy first):

- `PageCache` — page_cache.rs:99–116: an intrusive circular list with a
  `clock_hand` raw pointer (:107); the comment at :95–98 states the
  discipline (insert behind the hand).
- `advance_clock_hand` — :174; `insert` — :204.
- Note what's unsafe (`Send`/`Sync` impls :115–116, raw pointers) and what
  your Rust version can do differently with indices into a `Vec<Frame>`
  instead of pointers (safe, and the array is exactly postgres's layout).

## Where each step lives in the code

Local clones at `~/repos/redis` and `~/repos/turso`:

| File | What | Steps |
|------|------|-------|
| `redis/src/zmalloc.c` | the ledger | 2–4 |
| `redis/src/defrag.c` | cooperative defrag | 5 |
| `turso/core/storage/page_cache.rs` | Rust CLOCK | 6 |

- **Step 2**: `PREFIX_SIZE` — zmalloc.c:39–46; `zmalloc` — :161–193
  (`malloc_usable_size` path vs prefix path).
- **Step 3**: `used_memory` per-thread padded counters — :86–92;
  `update_zmalloc_stat_alloc` + peak-check throttle — :105–145.
- **Step 5**: `activeDefragAlloc` — defrag.c:177 (+ the :142 comment).
- **Step 6**: `PageCache` — page_cache.rs:99–116; `advance_clock_hand`
  :174; `insert` :204.

## Questions to answer in notes.md

1. Why per-thread counters instead of one atomic? Estimate the cost of a
   shared `fetch_add` on every malloc at 8 threads (topic-0 numbers).
2. Redis evicts keys; a buffer pool evicts pages. Which gets better hit
   rates for the same RAM and why is the comparison unfair? (Keys are
   variable-size and *complete* — no partial residency of a value.)
3. After building your CLOCK pool: diff your design against turso's — hand
   placement on insert, where usage bits live, pin representation.

## Done when

You can explain PREFIX_SIZE, why the counters are padded, and what active
defrag can't touch — and you've compared your finished pool to turso's.

## References

**Code**
- [redis](https://github.com/redis/redis) — `src/zmalloc.c` (the ledger)
  and `src/defrag.c` (cooperative userspace defragmentation). Local clone
  at `~/repos/redis`.
- [tursodatabase/turso](https://github.com/tursodatabase/turso) —
  `core/storage/page_cache.rs`, a real Rust CLOCK to diff against your
  experiment *after* you build it (don't copy first). Local clone at
  `~/repos/turso`.
