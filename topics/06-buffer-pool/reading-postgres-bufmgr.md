# postgres bufmgr: a buffer's life in one atomic word

Postgres packs everything the clock sweep needs to know about a buffer —
refcount, usage count, flags, and (as of this tree) the content lock itself —
into a single atomic 64-bit word, so the hit path is one CAS and the sweep
hand reads victims without ever locking a header. This chapter builds the
classic `shared_buffers` design step by step — frames and pins, the packed
state word, the hit path, the clock sweep and its arithmetic, the foreground
dirty-victim flush, scan admission, and the background writer that exists to
hide those flushes — then maps each step to the exact lines in `bufmgr.c` and
`freelist.c`.

Everything below is read at the repo's pinned postgres commit,
[`postgres/postgres@701f021`](https://github.com/postgres/postgres), which is
`20devel` — a development tree, not a release. That matters twice: the buffer
**free list is gone** (only the clock sweep remains, in a file still named
`freelist.c`), and the per-buffer content lock has moved *into* the state
word. Descriptions written against PG ≤ 17 will disagree with the line
numbers here, and in those two places with the design.

## The problem in one sentence

With 128 backends hammering a shared cache of (say) 16 GB — 2,097,152 frames
of 8 KB — every page access must find, claim, and protect a frame using at
most a couple of atomic instructions, because any lock held on the common
path would serialize the whole server.

## The concepts, step by step

### Step 1 — frames, the mapping table, and pins

> **In:** nothing yet — this step fixes the two operations every later step
> is trying to make cheap.
> **Out:** the lookup (Step 3 shards it) and the pin (Step 2 packs it into
> one word, Step 4 reads it to skip victims).

A **buffer pool** is the fixed-size in-memory cache of disk pages that the
engine manages itself; in postgres it is `shared_buffers`, a big array of
fixed 8 KB **frames** — RAM slots, each currently holding one **page**, the
unit the storage layer reads and writes — allocated once at startup. Beside
it sits a shared hash table mapping `(relation, fork, block number) → frame
index`. Two operations define everything else:

- **Lookup**: hash the page's identity, probe the table. Found ⇒ *hit*;
  not found ⇒ *miss*, and some frame must be recycled.
- **Pin**: atomically increment the frame's reference count before touching
  its bytes. A pinned frame (refcount > 0) is invisible to eviction — the pin
  is the only thing standing between your pointer and the frame being reused
  for a different page mid-read. **Unpin** when done. (The database sense of
  "pin" throughout; nothing to do with `mlock`.)

A page whose in-RAM copy has been modified but not yet written back is
**dirty**. An **eviction policy** is whatever decides which unpinned frame to
recycle on a miss; Step 4 is postgres's.

Why it matters: on a hot workload these two operations run millions of times
per second across 100+ processes. Everything below is about making them cost
one or two atomics.

### Step 2 — the packed state: one atomic u64 per buffer

> **In:** the pin from Step 1, which naively wants a lock.
> **Out:** a single `pg_atomic_uint64` that Steps 3, 4 and 5 all mutate with
> plain CAS loops instead of taking the header spinlock.

Instead of separate fields guarded by a spinlock, postgres packs a buffer's
entire hot-path state into ONE atomic 64-bit word, `BufferDesc.state`
(buf_internals.h:344). The header comment spells out the division at
:34–42 — and note how little is left over:

```
 ┌───────────────── 64-bit BufferDesc.state ─────────────────┐
 │ 1 excl │ 1 sh-excl │ 18 share-lock │ 12 flags │ 4 usage │ 18 refcount │
 └───────────────────────────────────────────────────────────┘
   bit 53                                bit 21   bit 17         bit 0

 BUF_REFCOUNT_BITS   18   (:49)   BUF_USAGECOUNT_BITS 4   (:50)
 BUF_FLAG_BITS       12   (:51)   BUF_LOCK_BITS      18+2 (:52)
 BM_MAX_USAGE_COUNT   5   (:144)  — the sweep gives ≤5 second chances

 18 + 4 + 12 + 20 = 54 bits used, 10 spare — and there is a
 StaticAssertDecl at :54 that keeps the sum ≤ 64.
```

- **refcount (18 bits)** — the pin count from Step 1. 18 bits because at most
  `MAX_BACKENDS` processes can pin simultaneously, and the compiler is made
  to check it: `StaticAssertDecl(MAX_BACKENDS_BITS <= BUF_REFCOUNT_BITS)`
  at :130.
- **usage count (4 bits, capped at 5)** — a tiny popularity score for
  eviction (Step 4). It saturates harmlessly; the comment at :136–143
  explains the tradeoff, and Step 4 does its arithmetic.
- **lock bits (20)** — in this tree the *content lock* lives here too, with
  a `proclist_head lock_waiters` (:358) for the sleepers. Older postgres kept
  a separate `LWLock content_lock` in the descriptor; it is gone.

Why packed: pin, unpin, and usage-bump each become a single **CAS**
(compare-and-swap — an atomic "replace this word only if it still holds the
value I read"), so nothing on the hit path acquires the buffer header
spinlock. The struct comment says it outright at :272–275. Same trick as
topic 2's SwissTable metadata byte: cram the hot-path-decidable state into
one word.

### Step 3 — the hit path: sharded lookup, then one CAS

> **In:** the mapping table (Step 1) and the packed word (Step 2).
> **Out:** the measured per-hit tax — one partition lock, one probe, one CAS
> — which is exactly what Step 4 must not add to, and what LeanStore's
> swizzling deletes.

A hit is a probe plus a CAS. `BufferAlloc` (bufmgr.c:2197) hashes the tag,
picks a partition lock, and takes it **shared**:

```c
// postgres/postgres@701f021 — src/backend/storage/buffer/bufmgr.c, BufferAlloc, 2218-2240
  2218  	/* determine its hash code and partition lock ID */
  2219  	newHash = BufTableHashCode(&newTag);
  2220  	newPartitionLock = BufMappingPartitionLock(newHash);
  2221
  2222  	/* see if the block is in the buffer pool already */
  2223  	LWLockAcquire(newPartitionLock, LW_SHARED);
  2224  	existing_buf_id = BufTableLookup(&newTag, newHash);
  2225  	if (existing_buf_id >= 0)
  2226  	{
  2227  		BufferDesc *buf;
  2228  		bool		valid;
  2235  		buf = GetBufferDescriptor(existing_buf_id);
  2236
  2237  		valid = PinBuffer(buf, strategy, false);
  2238
  2239  		/* Can release the mapping lock as soon as we've pinned it */
  2240  		LWLockRelease(newPartitionLock);
```

The mapping table is sharded `NUM_BUFFER_PARTITIONS = 128` ways
(lwlock.h:83), with `BufTableHashPartition` = `hashcode % 128`
(buf_internals.h:248–250), so concurrent lookups of different pages almost
never contend on the same lock. Line 2240 is the point of the packing: the
partition lock is released the instant the pin is in, so it is held for one
hash probe and one CAS, never across I/O.

The pin itself, inside `PinBuffer` (:3295), is one CAS loop over the state
word — refcount+1, and usage_count+1 if below the cap:

```c
// postgres/postgres@701f021 — src/backend/storage/buffer/bufmgr.c, PinBuffer's CAS loop, 3330-3352
  3330  			buf_state = old_buf_state;
  3331
  3332  			/* increase refcount */
  3333  			buf_state += BUF_REFCOUNT_ONE;
  3334
  3335  			if (strategy == NULL)
  3336  			{
  3337  				/* Default case: increase usagecount unless already max. */
  3338  				if (BUF_STATE_GET_USAGECOUNT(buf_state) < BM_MAX_USAGE_COUNT)
  3339  					buf_state += BUF_USAGECOUNT_ONE;
  3340  			}
  3341  			else
  3342  			{
  3343  				/*
  3344  				 * Ring buffers shouldn't evict others from pool.  Thus we
  3345  				 * don't make usagecount more than 1.
  3346  				 */
  3347  				if (BUF_STATE_GET_USAGECOUNT(buf_state) == 0)
  3348  					buf_state += BUF_USAGECOUNT_ONE;
  3349  			}
  3350
  3351  			if (pg_atomic_compare_exchange_u64(&buf->state, &old_buf_state,
  3352  											   buf_state))
```

Lines 3341–3348 are Step 6 arriving early: a page pinned through a bulk-read
ring never gets a usage count above 1, so it cannot out-compete a real
working-set page in Step 4's sweep.

Cost to notice: a hit still costs an LWLock acquire/release, a probable cache
miss on the hash bucket, and a CAS on a shared line. vmcache's Table 2
measures the shape of this — a random page access through a hash table takes
**336 ns and 27.9 instructions** against 219 ns and 3.3 instructions for a
plain memory read, and that is for an *unsynchronized* table, i.e. a lower
bound. This is precisely the tax LeanStore's pointer swizzling deletes
([`reading-leanstore-paper.md`](reading-leanstore-paper.md), Step 1).

### Step 4 — the clock sweep: eviction as a second-chance hand

> **In:** the usage count and refcount from Step 2, which the sweep is the
> only consumer of.
> **Out:** a victim frame, pinned and owned by the caller — which Step 5
> then discovers may be dirty.

On a miss, some unpinned frame must be recycled. True **LRU** — a list with
move-to-front on every hit — would mean shared list surgery on the hot path,
which is the one thing Step 3 refuses. **Clock sweep** (also called *second
chance*) approximates it: a single shared counter, `nextVictimBuffer`
(freelist.c:42), ticks around the frame array like the hand of a clock. At
each frame it reads the state word and applies three rules:

```
  refcount ≠ 0        ⇒ skip           (pinned: invisible to the hand)
  usage_count > 0     ⇒ decrement it   (the buffer spends one of its lives)
  both zero           ⇒ VICTIM         (pin it and return it)
```

That is the whole policy, and it is these lines:

```c
// postgres/postgres@701f021 — src/backend/storage/buffer/freelist.c, StrategyGetBuffer's sweep, 239-246
   239  	/* Use the "clock sweep" algorithm to find a free buffer */
   240  	trycounter = NBuffers;
   241  	for (;;)
   242  	{
   243  		uint64		old_buf_state;
   244  		uint64		local_buf_state;
   245
   246  		buf = GetBufferDescriptor(ClockSweepTick());
```

```c
// postgres/postgres@701f021 — src/backend/storage/buffer/freelist.c, the three rules, 263-303
   263  			if (BUF_STATE_GET_REFCOUNT(local_buf_state) != 0)
   264  			{
   265  				if (--trycounter == 0)
   266  				{
   274  					elog(ERROR, "no unpinned buffers available");
   275  				}
   276  				break;
   277  			}
   286  			if (BUF_STATE_GET_USAGECOUNT(local_buf_state) != 0)
   287  			{
   288  				local_buf_state -= BUF_USAGECOUNT_ONE;
   289
   290  				if (pg_atomic_compare_exchange_u64(&buf->state, &old_buf_state,
   291  												   local_buf_state))
   292  				{
   293  					trycounter = NBuffers;
   294  					break;
   295  				}
   296  			}
   297  			else
   298  			{
   299  				/* pin the buffer if the CAS succeeds */
   300  				local_buf_state += BUF_REFCOUNT_ONE;
   301
   302  				if (pg_atomic_compare_exchange_u64(&buf->state, &old_buf_state,
   303  												   local_buf_state))
```

Note what is *not* there: no list, no lock, no per-buffer metadata beyond the
4 bits. `ClockSweepTick` (:110) is a `pg_atomic_fetch_add_u32` (:120) with a
CAS-based modular wraparound so the counter cannot overflow mid-flight while
`completePasses` (:48) is kept consistent with it. And line 274 is the only
failure mode: if the hand makes a whole lap (`trycounter` counts down from
`NBuffers`) finding every frame pinned, postgres errors out rather than spin
forever.

**The arithmetic, part 1: does the policy even matter?** Take
`shared_buffers = 16 GB`, so `NBuffers = 16 GiB / 8 KiB = 2,097,152` frames.

```
 working set 12 GB = 1,572,864 pages  <  pool
   after warm-up every page is resident; over 100M accesses the only
   misses are the compulsory first touches:
     hit rate = 1 − 1,572,864/100,000,000 = 98.4%,  → 100% as the run grows
   the eviction policy is never consulted. LRU, CLOCK, random: identical.

 working set 32 GB = 4,194,304 pages  >  pool, accesses uniform
     hit rate = pool / working set = 2,097,152 / 4,194,304 = 50%
   and that is the answer for EVERY policy, including OPT: under uniform
   access no ordering of victims is better than any other.
```

So the policy only earns its keep when the working set exceeds the pool *and*
the accesses are skewed — which is what makes LeanStore's §VI-B measurement
(random 92.5% vs LRU 93.1% vs OPT 96.3%, at Zipf 1.0) the interesting
comparison rather than a uniform one.

**The arithmetic, part 2: what the sweep costs per miss.** In steady state
the hand must destroy usage counts as fast as hits create them. Per access,
hits create at most `h` increments (hit rate `h`); the hand creates one
decrement per frame it visits. If it visits `S` frames per miss and the miss
rate is `m = 1 − h`, then finding a victim needs `S·m ≥ h + m = 1`:

```
  m = 50%  ⇒ S ≥ 2 frames visited per miss
  m = 5%   ⇒ S ≥ 20
  m = 1%   ⇒ S ≥ 100
  in every case  S·m ≈ 1 frame visit per buffer ACCESS — a constant,
  which is why the hand is affordable at all
```

The better the hit rate, the longer the hand must walk per miss — but the
total work per access stays at about one state-word read. And the cap on
usage explains itself: buf_internals.h:140 warns "it can take as many as
`BM_MAX_USAGE_COUNT`+1 complete cycles of the clock-sweep hand to find a free
buffer", which at 5 is `6 × 2,097,152 = 12.6M` frame visits in the worst case
— tolerable. A cap "comparable to NBuffers" would approximate true LRU
(:139) and make that worst case unbounded.

Why it matters: a hit costs one saturating increment inside a CAS it was
doing anyway; only misses pay the walk. That trade is why nobody ships strict
LRU — and your `benches/eviction.rs` lane measures exactly this.

### Step 5 — the miss path: the dirty victim is YOUR problem

> **In:** Step 4's victim frame.
> **Out:** a user-visible latency spike whose size Step 7's background writer
> is built to hide — and the WAL rule that makes it worse than one write.

`GetVictimBuffer` (bufmgr.c:2548) reserves pin bookkeeping
(`ReservePrivateRefCountEntry` :2559 — question 2's machinery), calls Step 4's
`StrategyGetBuffer` (:2569), and then hits the ugly part: **if the victim is
dirty, the backend that wants a new page writes the old one out itself**,
right there in the foreground, at `FlushBuffer` (:2634). Your innocent
`SELECT` eats a disk write before its read can even start.

And it is not one write. Inside `FlushBuffer` (:4526) comes the WAL rule:

```c
// postgres/postgres@701f021 — src/backend/storage/buffer/bufmgr.c, FlushBuffer's WAL rule, 4565-4585
  4565  	recptr = BufferGetLSN(buf);
  4566
  4567  	/*
  4568  	 * Force XLOG flush up to buffer's LSN.  This implements the basic WAL
  4569  	 * rule that log updates must hit disk before any of the data-file changes
  4570  	 * they describe do.
  4584  	if (pg_atomic_read_u64(&buf->state) & BM_PERMANENT)
  4585  		XLogFlush(recptr);
```

So a dirty eviction can cost a **log flush (an fsync) plus an 8 KB write**,
serially, on a query that only wanted to read. This is topic 5's invariant —
and the same one mmap cannot enforce, because the kernel may write a dirty
page out whenever it likes ([`reading-mmap-paper.md`](reading-mmap-paper.md),
Step 4).

Put a number on the stall. This topic's own lane measures page-access
latency under mmap at **p50 42 ns and max 182 µs**
([FINDINGS.md row 6](../../FINDINGS.md), `notes.md` baseline) — a 4300×
spread caused by minor page faults the database cannot see. A foreground
dirty-victim flush is a stall of comparable magnitude or worse. The
difference is the entire argument for owning the buffer pool: postgres
*knows* it is about to pay it, can attribute it (`pg_stat_io`'s `IOOP_EVICT`,
counted at :2660), and can arrange for someone else to have paid it already
— which is Step 7. Under mmap the same-sized stall arrives unannounced.

There is one more branch worth seeing, at :2624–2631: if the victim came from
a strategy ring and reusing it *would* require a WAL flush,
`StrategyRejectBuffer` can hand it back and the sweep starts over. Postgres
would rather evict a stranger than make a bulk scan wait on the log.

Also read how the read side got faster: `PinBufferForBlock` (:1223) →
`ReadBuffer_common` (:1276) → `StartReadBuffersImpl` (:1371). Recent versions
turned the miss into a vectored/async `ReadBuffersOperation` (note
`PgAioWaitRef io_wref` in the descriptor, buf_internals.h:352); the miss path
now streams instead of blocking one block at a time.

### Step 6 — buffer rings: eviction policy as admission policy

> **In:** the sweep from Step 4, which a sequential scan would otherwise
> feed with the entire pool.
> **Out:** the second of the two places a system can defend itself — and the
> comparison question 3 asks you to settle.

One `SELECT count(*)` on a 100 GB table would, naively, march through the
pool evicting everything: a sequential scan touches each page once and never
again, the worst possible tenant. Postgres's defence is not in the policy but
at *admission*. `GetAccessStrategy` (freelist.c:426) hands bulk operations a
private **ring** of buffers that they recycle among themselves:

```
  BAS_BULKREAD    256 KB base  (:459)  =  32 frames of 8 KB
                  + BLCKSZ × io_combine_limit × effective_io_concurrency
                    for in-flight AIO (:480-481), capped by the pin limit
  BAS_BULKWRITE    16 MB       (:488)  =  2,048 frames
  BAS_VACUUM        2 MB       (:491)  =  256 frames

  a bulk read's blast radius, in a 16 GB pool:
      32 / 2,097,152 = 0.0015% of the frames
```

The scan reuses its own 32 frames instead of claiming fresh ones, so the
other two million never see it — and per Step 3's lines 3341–3348, even the
pages it does touch never accumulate a usage count above 1.

Compare LeanStore, which defends at *eviction* instead: an unlucky page gets
a second chance in the cooling FIFO before it is thrown out
([`reading-leanstore-paper.md`](reading-leanstore-paper.md), Step 4).
Question 3 below asks what each approach misses.

### Step 7 — the background writer: hide the foreground flush

> **In:** Step 5's foreground flush and Step 4's clock hand.
> **Out:** an estimator, its default ceiling in MB/s, and the conditions
> under which it fails and Step 5's spike comes back.

Step 5's foreground flush is the latency killer, so a dedicated process runs
the *same clock* slightly ahead of the sweep hand, writing dirty buffers
preemptively so that when `GetVictimBuffer` arrives, the victims are already
clean. That is `BgBufferSync` (bufmgr.c:3854), and `StrategySyncStart`
(freelist.c:326–348) is how it asks where the hand currently is.

Its pace is a guess, and worth reading as an example of a self-tuning
control loop that can be wrong:

```
  smoothed_alloc      fast-attack, slow-decline EMA of recent allocations,
                      smoothing_samples = 16       (:3876, :4021-4025)
  upcoming_alloc_est  = smoothed_alloc × bgwriter_lru_multiplier (2.0, :191)
  hard cap            bgwriter_lru_maxpages = 100 pages per round (:190)
  round length        BgWriterDelay = 200 ms  (postmaster/bgwriter.c:59)

  default writeback ceiling:
      100 pages × 8 KB / 0.2 s = 800 KB / 0.2 s = 4 MB/s
      = 500 dirty pages per second
```

Four megabytes a second. Dirty more than 500 pages/s in steady state and the
overflow lands on backends as Step 5's spike, no matter how well the
estimator tracks. (For scale, LeanStore's Fig. 9 reports ~500 MB/s written
back in the background while staying near in-memory throughput — 125× the
postgres default. The defaults are old; the mechanism is not the limit.)

Why it matters: this is the shape of every "hide the cost" subsystem — an
estimator plus a cap. Read the long comment above :3877; when it guesses low,
or when the cap binds, the work does not disappear, it simply moves back onto
the query that triggered it.

## Where each step lives in the code

Read in this order: `buf_internals.h`'s header comment (:33–147) for the
state word, then `freelist.c` end-to-end (it is only 770 lines and contains
the entire policy), then the three `bufmgr.c` functions.

| File | What | Steps |
|------|------|-------|
| `src/include/storage/buf_internals.h` | state-word layout :33–147, `BufferDesc` :326 (`state` :344), partition hashing :244–258 | 1–2 |
| `src/backend/storage/buffer/freelist.c` | the whole replacement policy: clock sweep + rings. **No free list** despite the name | 4, 6, 7 |
| `src/backend/storage/buffer/bufmgr.c` | pin, miss path, flush, bgwriter | 3, 5, 7 |
| `src/include/storage/lwlock.h` | `NUM_BUFFER_PARTITIONS 128` :83 | 3 |

| Step | Symbol | Location |
|---|---|---|
| 2 | `BUF_REFCOUNT_BITS` 18 / `BUF_USAGECOUNT_BITS` 4 / `BUF_FLAG_BITS` 12 / `BUF_LOCK_BITS` 20 | buf_internals.h:49–52 |
| 2 | size assertion; refcount-vs-`MAX_BACKENDS` assertion | buf_internals.h:54, :130 |
| 2 | `BM_MAX_USAGE_COUNT 5` and the tradeoff comment | buf_internals.h:136–147 |
| 2 | `BufferDesc` with `state`, `io_wref`, `lock_waiters` | buf_internals.h:326–359 |
| 3 | `BufferAlloc` — hash, partition lock, lookup, pin, release | bufmgr.c:2197, :2218–2240 |
| 3 | `PinBuffer` and its CAS loop (ring cap at :3347) | bufmgr.c:3295, :3330–3352 |
| 3 | `BufTableHashPartition` = `hashcode % NUM_BUFFER_PARTITIONS` | buf_internals.h:248–250, lwlock.h:83 |
| 3 | async reads: `PinBufferForBlock` → `ReadBuffer_common` → `StartReadBuffersImpl` | bufmgr.c:1223, :1276, :1371 |
| 4 | `nextVictimBuffer`, `completePasses` | freelist.c:42, :48 |
| 4 | `ClockSweepTick` — `fetch_add(1)` plus CAS wraparound | freelist.c:110–166 (add at :120) |
| 4 | `StrategyGetBuffer`; the sweep; `"no unpinned buffers available"` | freelist.c:184, :239–316, :274 |
| 5 | `GetVictimBuffer`; `ReservePrivateRefCountEntry`; dirty check | bufmgr.c:2548, :2559, :2584 |
| 5 | ring rejection when a WAL flush would be needed | bufmgr.c:2624–2631 |
| 5 | foreground `FlushBuffer` call, then the function | bufmgr.c:2634, :4526 |
| 5 | the WAL rule: `XLogFlush(recptr)` | bufmgr.c:4565–4585 |
| 6 | `GetAccessStrategy`; ring sizes 256 KB / 16 MB / 2 MB | freelist.c:426, :459, :488, :491 |
| 7 | `BgBufferSync`; `StrategySyncStart` | bufmgr.c:3854; freelist.c:326–348 |
| 7 | `bgwriter_lru_maxpages` 100, `bgwriter_lru_multiplier` 2.0, `smoothing_samples` 16 | bufmgr.c:190, :191, :3876 |
| 7 | the EMA itself | bufmgr.c:4021–4028 |

## Questions to answer in notes.md

1. Why 18 bits of refcount but only 4 of usage count? What failure does each
   cap produce and which is graceful? (usage saturates harmlessly; refcount
   overflow would be corruption — hence StaticAssert vs MAX_BACKENDS :130.)
2. A client pins a page and crashes mid-query — who unpins? (Resource owner
   machinery: ReservePrivateRefCountEntry in GetVictimBuffer :2559.)
3. Buffer rings vs LeanStore's cooling stage: both defend against scans.
   Which defends at admission and which at eviction? What does each miss?
4. Postgres double-buffers (shared_buffers + OS page cache). What does
   `O_DIRECT` (topic 6's io story, debug_io_direct) buy and cost here?

## Takeaway

The hit path is one shared LWLock acquire on a 1-in-128 shard plus one CAS on
a word that holds refcount, usage, flags and the content lock together. The
policy that word feeds — clock sweep with 5 second chances — costs about one
state-word read per access no matter what the hit rate is, and matters at all
only when the working set exceeds the pool *and* the accesses are skewed.
Everything expensive has been pushed onto the miss, where the real hazard is
not the read but the dirty victim: a WAL fsync plus an 8 KB write, in the
foreground, unless a background writer capped at 4 MB/s got there first.

## Done when

Answer each before unfolding it.

- [ ] You can narrate a miss on a dirty victim end-to-end — every lock, atomic and I/O in order — and say which of them `BgBufferSync` moves off the hot path.

  <details><summary>Answer</summary>

  `BufferAlloc` (:2197) hashes the tag, takes the partition LWLock shared
  (:2223), probes (`BufTableLookup` :2224), and misses. It calls
  `GetVictimBuffer` (:2548), which reserves a refcount entry and resource
  owner slot (:2559–2560) and enters `StrategyGetBuffer` (freelist.c:184).
  The clock hand (`ClockSweepTick` :110, an atomic fetch-add) walks frames,
  CAS-decrementing usage counts (:288–291) and skipping pinned ones (:263),
  until it finds refcount 0 and usage 0 and CAS-pins it (:300–303) — no lock
  taken anywhere in the walk.

  Back in `GetVictimBuffer`, `buf_state & BM_DIRTY` (:2584) is true. It takes
  the content lock conditionally (:2603 — conditionally, to avoid deadlock
  with a concurrent page split), then calls `FlushBuffer` (:2634), which
  first does `XLogFlush(recptr)` (:4585) — a **WAL fsync** — and only then
  `smgrwrite`s the 8 KB page. Then the mapping table is updated under the
  *exclusive* partition lock, the buffer is pinned into its new identity, and
  the read is issued (`StartReadBuffersImpl` :1371).

  `BgBufferSync` (:3854) removes exactly one of these: the `XLogFlush` +
  `smgrwrite` pair, by having written the page already so `BM_DIRTY` is clear
  when the hand arrives. It removes neither the sweep, nor the partition
  locks, nor the read.

  </details>

- [ ] You can compute the hit rate for a 16 GB pool against a 12 GB and a 32 GB uniform working set, and say what the eviction policy is worth in each case.

  <details><summary>Answer</summary>

  16 GB / 8 KB = **2,097,152 frames**. A 12 GB working set is 1,572,864
  pages, which fits, so after warm-up the hit rate tends to 100% (over a
  100M-access run, `1 − 1,572,864/100,000,000` = 98.4%) and the policy is
  never consulted — every page is resident and nothing is ever evicted.

  A 32 GB working set is 4,194,304 pages, twice the pool. Under *uniform*
  access the hit rate is `pool / working set` = **50%** for every policy,
  including the unimplementable optimum: with no skew there is no information
  to exploit, so choosing victims well is choosing among equals.

  The policy therefore earns its keep only in the third case — working set
  larger than the pool *and* skewed. That is why LeanStore evaluates at
  Zipf 1.0 rather than uniform (§VI-B: random 92.5%, LRU 93.1%, OPT 96.3%),
  and why the honest summary of clock sweep is not "it approximates LRU" but
  "it approximates LRU in the only regime where either one matters".

  </details>

- [ ] You can explain why a *higher* hit rate makes the clock hand walk *further* per miss, and why that does not make the sweep more expensive overall.

  <details><summary>Answer</summary>

  Conservation of usage counts. Each hit adds at most one increment (capped
  at 5); each frame the hand visits removes at most one. In steady state the
  two must balance, and a victim additionally has to be reached: with hit
  rate `h`, miss rate `m` and `S` frames visited per miss, `S·m ≥ h + m = 1`,
  so `S ≥ 1/m`. At `m = 50%` the hand walks 2 frames per miss; at `m = 5%`,
  20; at `m = 1%`, 100.

  Overall cost is `S·m ≈ 1` state-word read **per access**, independent of
  the hit rate — the walk grows exactly as fast as misses become rare. What
  the cap bounds is the worst case, not the average: buf_internals.h:140 says
  a victim can take `BM_MAX_USAGE_COUNT + 1` = 6 complete cycles, which is
  12.6M frame visits in a 16 GB pool. A cap "comparable to NBuffers" (:139)
  would approximate true LRU and make that unbounded, which is the tradeoff
  the comment describes.

  </details>

- [ ] You can say what a bulk sequential scan can and cannot do to the pool, and name the two independent mechanisms that limit it.

  <details><summary>Answer</summary>

  It can consume at most its ring: `BAS_BULKREAD` starts at 256 KB
  (freelist.c:459) = 32 frames of 8 KB, grown only by
  `BLCKSZ × io_combine_limit × effective_io_concurrency` to keep in-flight
  AIO from stalling on its own pins (:480–481), and capped by the pin limit.
  In a 16 GB pool that is `32 / 2,097,152` = 0.0015% of the frames — the scan
  recycles its own buffers instead of claiming fresh ones.

  Two mechanisms, not one. (1) Admission: `GetAccessStrategy` (:426) hands
  out the ring at all, and `GetBufferFromRing` (:198) is consulted before the
  clock sweep. (2) Usage suppression: `PinBuffer` caps a strategy-pinned
  buffer's usage count at 1 (bufmgr.c:3341–3348) with the comment "Ring
  buffers shouldn't evict others from pool", so even the pages a scan does
  touch cannot out-survive working-set pages in the sweep. A third, smaller
  one: if reusing a ring buffer would force a WAL flush,
  `StrategyRejectBuffer` gives it back and takes a stranger instead (:2624).

  </details>

- [ ] You can compute the background writer's default writeback ceiling and say what happens above it.

  <details><summary>Answer</summary>

  `bgwriter_lru_maxpages = 100` pages per round (bufmgr.c:190) and
  `BgWriterDelay = 200` ms (postmaster/bgwriter.c:59), so the ceiling is
  `100 × 8 KB / 0.2 s` = **4 MB/s**, or 500 dirty pages per second. Below
  that, the estimator — a fast-attack/slow-decline EMA over
  `smoothing_samples = 16` (:3876, :4021–4025) scaled by
  `bgwriter_lru_multiplier = 2.0` (:191) — decides how much of the ceiling to
  use.

  Above it, the work does not disappear: every dirty page beyond 500/s is
  still dirty when the clock hand reaches it, so a backend pays Step 5's
  `XLogFlush` + write in the foreground. The estimator can also simply guess
  low after a workload shift, with the same result. For scale, LeanStore's
  Fig. 9 sustains ~500 MB/s of background writeback while staying near
  in-memory throughput — 125× the postgres default, which says the ceiling is
  a conservative default rather than a property of the design.

  </details>

- [ ] You wrote answers to all four questions in notes.md.

  <details><summary>Answer</summary>

  Nothing to unfold — the answers are the exercise. Two of them have hints
  in the text: question 1's asymmetry is settled by the two `StaticAssertDecl`
  lines (buf_internals.h:130 and :146), and question 3's contrast is Step 6's
  last paragraph. Question 2 wants you to follow `ReservePrivateRefCountEntry`
  into `resowner.h`; question 4 wants `debug_io_direct` and an argument about
  where the second copy of every page is currently living.

  </details>

## References

**Code** — [postgres/postgres](https://github.com/postgres/postgres) at
`701f021` (`20devel`). Local clone at `~/repos/postgres`; the pin table is at
the end of `resources/codebases.md`.

| File | Lines | What |
|---|---|---|
| `src/include/storage/buf_internals.h` | 33–52 | the state word's division, in the header comment |
| `src/include/storage/buf_internals.h` | 130, 146 | the two static assertions that keep the caps honest |
| `src/include/storage/buf_internals.h` | 136–144 | why `BM_MAX_USAGE_COUNT` is 5 and not larger |
| `src/include/storage/buf_internals.h` | 248–258 | `BufTableHashPartition`, `BufMappingPartitionLock` |
| `src/include/storage/buf_internals.h` | 326–359 | `BufferDesc`: tag, `state`, `io_wref`, `lock_waiters` |
| `src/include/storage/lwlock.h` | 77–96 | `NUM_BUFFER_PARTITIONS 128` and its offset in the LWLock array |
| `src/backend/storage/buffer/freelist.c` | 42–48 | `nextVictimBuffer`, `completePasses` |
| `src/backend/storage/buffer/freelist.c` | 110–166 | `ClockSweepTick` |
| `src/backend/storage/buffer/freelist.c` | 184–316 | `StrategyGetBuffer`: ring first, then the sweep |
| `src/backend/storage/buffer/freelist.c` | 426–500 | `GetAccessStrategy` and the three ring sizes |
| `src/backend/storage/buffer/bufmgr.c` | 190–191 | `bgwriter_lru_maxpages`, `bgwriter_lru_multiplier` |
| `src/backend/storage/buffer/bufmgr.c` | 2197–2245 | `BufferAlloc`'s hit path |
| `src/backend/storage/buffer/bufmgr.c` | 2548–2660 | `GetVictimBuffer`, including the foreground flush |
| `src/backend/storage/buffer/bufmgr.c` | 3295–3360 | `PinBuffer` |
| `src/backend/storage/buffer/bufmgr.c` | 3854–4130 | `BgBufferSync` and its estimator |
| `src/backend/storage/buffer/bufmgr.c` | 4526–4600 | `FlushBuffer` and the WAL rule |
| `src/backend/postmaster/bgwriter.c` | 59 | `BgWriterDelay = 200` ms |

**Measurements cited**
- [FINDINGS.md row 6](../../FINDINGS.md) and this topic's `notes.md` — mmap
  page reads p50 42 ns, max 182 µs on this machine; the scale of an
  unscheduled stall.
- vmcache (SIGMOD '23) Table 2 — 336 ns / 27.9 instructions for a hash-table
  page access vs 219 ns / 3.3 for a plain read.
- LeanStore (ICDE '18) §VI-B and Fig. 9 — the hit-rate comparison at Zipf
  1.0, and ~500 MB/s of background writeback.

**Next**
- [`reading-leanstore-paper.md`](reading-leanstore-paper.md) — the design
  that deletes Step 3 entirely.
- [`reading-mmap-paper.md`](reading-mmap-paper.md) — what happens when you
  let the kernel make Steps 4–7's decisions for you.
