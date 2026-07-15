# postgres bufmgr: a buffer's life in one atomic word

Postgres packs everything CLOCK needs to know about a buffer — refcount,
usage count, flags — into a single atomic u64, so the hit path is one CAS and
the sweep hand reads victims without locks. This chapter builds the classic
shared-buffers design step by step — frames and pins, the packed state word,
the hit path, CLOCK, the foreground dirty-victim flush, scan admission, and
the background writer that exists to hide the flushes — then maps each step
to the exact lines in `bufmgr.c` and `freelist.c`.

## The problem in one sentence

With 128 backends hammering a shared cache of (say) 16 GB / 4 million
buffers, every page access must find, claim, and protect a buffer using at
most a couple of atomic instructions — any lock on the common path would
serialize the whole server.

## The concepts, step by step

### Step 1 — frames, the mapping table, and pins

Postgres's buffer pool (the fixed-size in-memory cache of disk pages the
engine manages itself, `shared_buffers`) is a big array of fixed 8 KB
**frames** allocated at startup, plus a shared hash table mapping
`(relation, block number) → frame index`. Two operations define everything
else:

- **Lookup**: hash the page's identity, probe the table. Found ⇒ hit;
  not found ⇒ miss, and some frame must be recycled.
- **Pin**: atomically increment the frame's reference count before touching
  its bytes. A pinned frame (refcount > 0) is invisible to eviction — the
  pin is the only thing standing between your pointer and the frame being
  reused for a different page mid-read. Unpin when done.

Why it matters: on a hot workload these two operations run millions of
times per second across 100+ processes. Everything below is about making
them cost one or two atomics.

### Step 2 — the packed state: one atomic u64 per buffer

Instead of separate fields guarded by a spinlock, postgres packs a buffer's
entire hot-path state — refcount, usage count, and flag bits (dirty, valid,
locked) — into ONE atomic u64 (`BufferDesc.state`):

```
 ┌──────────── 64-bit state ────────────┐
 │ lock bits │ flags │ usage(4) │ refcount(18) │
 └───────────────────────────────────────┘
 BUF_REFCOUNT_BITS 18 (:49)   BUF_USAGECOUNT_BITS 4 (:50)
 BM_MAX_USAGE_COUNT 5 (:144)  — CLOCK survives ≤5 sweeps
```

- **refcount (18 bits)** — the pin count from Step 1; 18 bits because at
  most MAX_BACKENDS processes can pin simultaneously (StaticAssert at :130).
- **usage count (4 bits, capped at 5)** — a tiny popularity score for
  eviction (Step 4); it saturates harmlessly at 5.

Why packed: pin/unpin/usage-bump become a single CAS (compare-and-swap — an
atomic "replace this word only if it still holds the value I read") — no
spinlock on the hit path. Same trick as topic-2's SwissTable metadata byte:
cram the hot-path-decidable state into one word.

### Step 3 — the hit path: sharded lookup, then one CAS

A hit costs one hash probe plus one CAS. The probe: `BufTableLookup` runs
under one of **`NUM_BUFFER_PARTITIONS = 128`** partition locks — the hash
table is sharded 128 ways so concurrent lookups almost never contend on the
same lock. The claim: `PinBuffer` runs a CAS loop on the state word —
refcount+1, and usage_count+1 if it's below the cap of 5 (:3338–3352). Two
memory operations total; nothing global is written.

Cost to notice: still ~2 atomics + a probable cache miss on the hash bucket
per access — this is exactly the tax LeanStore's swizzling eliminates
(reading-leanstore-paper.md, Step 1).

### Step 4 — CLOCK: eviction as a sweeping second-chance hand

On a miss, some unpinned frame must be recycled — but maintaining a true
LRU list (move-to-front on every hit) would mean list surgery on the hot
path. **CLOCK** approximates LRU with the usage count: one shared atomic
counter, `nextVictimBuffer`, ticks around the frame array like a clock
hand. At each frame it inspects the state word:

- pinned (refcount ≠ 0) ⇒ skip — invisible to CLOCK;
- usage_count > 0 ⇒ decrement it and move on — the buffer "spends a life";
- both zero ⇒ victim.

A hit bumps usage (max 5), so a frequently-used buffer survives up to 5
full sweeps untouched. The sweep, distilled:

```rust
// One shared clock hand; a buffer survives ≤5 sweeps untouched.
fn get_victim(&self) -> BufId {
    loop {
        let id = self.clock_tick();                 // fetch_add(1) % NBuffers
        let s = self.desc[id].state.load();
        if s.refcount() != 0 { continue; }          // pinned: invisible to CLOCK
        if s.usage_count() > 0 {                    // recently used: spend a life
            let _ = self.desc[id].state.cas(s, s.dec_usage());
            continue;
        }
        if self.desc[id].state.cas(s, s.pinned()) { // both zero ⇒ victim; pin it
            return id;                              // caller flushes it if dirty —
        }                                           // in the FOREGROUND
    }
}
```

Why it matters: a hit costs a saturating increment; only misses pay the
sweep. That trade — no per-hit bookkeeping beyond one CAS — is why nobody
ships strict LRU (your `benches/eviction.rs` measures exactly this).

### Step 5 — the miss path: the dirty victim is YOUR problem

`BufferAlloc` (bufmgr.c:2197) looks up, misses, and calls `GetVictimBuffer`
(:2548), which runs Step 4's sweep. Now the ugly part: **if the victim is
dirty** (modified in RAM but not yet written to disk), *the backend that
wants a new page writes the old one out itself* — `FlushBuffer`, right
there in the foreground (:2584 onward). Your innocent `SELECT` eats a full
disk write before its read can even start: every dirty eviction is a
user-visible latency spike.

Note the WAL-rule cameo: before flushing, `XLogNeedsFlush(BufferGetLSN(...))`
(~:2633) — a page may not be written until the log covering its changes is
durable (topic 5's invariant; the same one mmap can't enforce,
reading-mmap-paper.md Step 3).

Also read how reads got faster: `PinBufferForBlock` (:1223) →
`ReadBuffer_common` (:1276) → `StartReadBuffersImpl` (:1371) — v17+ turned
the miss into a vectored/async `ReadBuffersOperation`; the miss path now
streams.

### Step 6 — buffer rings: eviction policy as admission policy

One `SELECT count(*)` on a 100 GB table would, naively, march through the
pool evicting everything — a sequential scan touches each page once and
never again, the worst possible tenant. Postgres's defense is at
*admission*: `GetAccessStrategy` (freelist.c:426) gives bulk scans a private
**ring** of ~256 KB (BAS_BULKREAD, :442–459) — the scan recycles its own 32
buffers instead of claiming fresh ones, so the other 4 million buffers never
see it. Compare LeanStore, which defends at *eviction* (unlucky pages get a
second chance in the cooling FIFO); question 3 below asks what each misses.

### Step 7 — the background writer: hide the foreground flush

Step 5's foreground flush is the latency killer, so a dedicated process,
`BgBufferSync` (bufmgr.c:3854), runs the *same clock* slightly ahead of the
sweep hand, writing dirty buffers preemptively so that when
`GetVictimBuffer` arrives, victims are already clean. Its pace is an
estimate: `bgwriter_lru_maxpages` (default 100 pages/round) scaled by a
moving average of recent buffer-allocation rate (:3877–3911). It's an
*estimator* — read the long comment; when it guesses low, backends pay
Step 5's spike again.

## Where each step lives in the code

| File | What | Steps |
|------|------|-------|
| `src/include/storage/buf_internals.h` | packed state word :49–147, `BufferDesc` :344, partitions :244–250 | 1–2 |
| `src/backend/storage/buffer/bufmgr.c` | pin, miss path, bgwriter | 3, 5, 7 |
| `src/backend/storage/buffer/freelist.c` | CLOCK + strategies/rings | 4, 6 |
| `src/include/storage/lwlock.h` | `NUM_BUFFER_PARTITIONS = 128` :83 | 3 |

- **Step 2**: `BUF_REFCOUNT_BITS`/`BUF_USAGECOUNT_BITS` —
  buf_internals.h:49–50; `BM_MAX_USAGE_COUNT` :144; the
  refcount-vs-MAX_BACKENDS StaticAssert :130.
- **Step 3**: `PinBuffer` — bufmgr.c:3295 (CAS loop :3338–3352);
  `BufTableLookup` under partition locks — buf_internals.h:244–250,
  lwlock.h:83. Async reads: `PinBufferForBlock` :1223 → `ReadBuffer_common`
  :1276 → `StartReadBuffersImpl` :1371.
- **Step 4**: `StrategyControl->nextVictimBuffer` — freelist.c:42;
  `ClockSweepTick` — :104–160 (`fetch_add(1) % NBuffers` with a CAS-based
  modular wraparound so the counter never overflows mid-flight);
  `StrategyGetBuffer` — :184, the sweep loop :246–290, the
  "no unpinned buffers available" error when everything's pinned ~:274.
- **Step 5**: `BufferAlloc` — bufmgr.c:2197 (lookup :2224);
  `GetVictimBuffer` — :2548 (foreground `FlushBuffer` :2584 onward; WAL
  check `XLogNeedsFlush` ~:2633; `ReservePrivateRefCountEntry` :2559 —
  question 2's resource-owner machinery).
- **Step 6**: `GetAccessStrategy` — freelist.c:426; BAS_BULKREAD ring
  sizing :442–459.
- **Step 7**: `BgBufferSync` — bufmgr.c:3854; pacing
  `bgwriter_lru_maxpages` :190 + the moving-average estimator :3877–3911
  (read the long comment).

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

## Done when

You can narrate a miss on a dirty victim end-to-end — every lock, atomic,
and I/O in order — and say which step BgBufferSync moves off the hot path.

## References

**Code**
- [postgres/postgres](https://github.com/postgres/postgres) —
  `src/backend/storage/buffer/bufmgr.c`,
  `src/backend/storage/buffer/freelist.c`,
  `src/include/storage/buf_internals.h`,
  `src/include/storage/lwlock.h`. Local clone at `~/repos/postgres`.
