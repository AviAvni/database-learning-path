# zmalloc: memory management when there are no pages

Redis has no buffer pool — no pages, no frames, no eviction hand. What it has
instead is an allocation *ledger*: every malloc accounted on per-thread
padded counters, `maxmemory` enforced against an allocator statistic, and
key-level eviction after the fact. This chapter reads that ledger, plus a
bonus: turso's CLOCK page cache in Rust, the closest existing code to your
experiment.

## 1. zmalloc — allocation accounting, not caching

- `PREFIX_SIZE` — zmalloc.c:39–46: with jemalloc (`HAVE_MALLOC_SIZE`) the
  allocator can report a pointer's size ⇒ prefix is 0; with libc malloc,
  redis prepends an 8-byte size header to every allocation. The entire
  used-memory ledger depends on being able to answer "how big is this ptr?"
- `used_memory` — :86–92: **per-thread, cache-line-aligned** counters
  (`aligned(CACHE_LINE_SIZE)`, MAX_THREADS array) — summed on read.
  False sharing on a global counter would tax every malloc on every thread;
  same diagnosis as topic 0's cache-line experiments.
- `update_zmalloc_stat_alloc` — :105–145: bump my thread's counter, and only
  *occasionally* (peak-check throttle :109–118) pay for the full sum.
- `zmalloc` — :161–193: `malloc_usable_size` path vs prefix path.

The ledger, in miniature:

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

This ledger is what `maxmemory` compares against — eviction (LRU/LFU over
*keys*, not pages) triggers on an allocator statistic. The buffer-pool
analogue: DuckDB gates allocations up front; redis counts and evicts after.

## 2. Active defrag — defrag.c

- `activeDefragAlloc` — defrag.c:177 (+ :142 comment): jemalloc tells redis
  which allocations sit in sparse bins; redis re-allocates them (new ptr,
  same bytes) and rewrites every reference. Defragmentation *in userspace,
  cooperatively*, because the allocator can't move memory it handed out.
- FalkorDB angle: GraphBLAS matrices are big opaque zmalloc blobs — redis
  can count them but not defrag them, and one matrix can blow the maxmemory
  budget in a single GrB call. Your capstone owns its allocations; decide
  what "maxmemory" should even mean for a graph store.

## 3. Bonus: turso's page cache — [~/repos/turso](https://github.com/tursodatabase/turso) core/storage/page_cache.rs

A real Rust CLOCK implementation to compare with your experiment *after* you
build it (don't copy first):

- `PageCache` — :99–116: intrusive circular list + `clock_hand` raw pointer
  (:107); comment :95–98 states the discipline (insert behind the hand).
- `advance_clock_hand` — :174; `insert` — :204.
- Note what's unsafe (`Send`/`Sync` impls :115–116, raw pointers) and what
  your Rust version can do differently with indices into a `Vec<Frame>`
  instead of pointers (safe, and the array is exactly postgres's layout).

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
