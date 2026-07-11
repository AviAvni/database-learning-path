# Reading postgres `bufmgr.c` + `freelist.c` — guided skim (2 h)

Repo: `~/repos/postgres`. Files: `src/backend/storage/buffer/bufmgr.c`,
`freelist.c`, `src/include/storage/buf_internals.h`,
`src/include/storage/lwlock.h`. You're here for four mechanisms.

## 1. The packed state — buf_internals.h:49–147

Everything about a buffer fits in ONE atomic u64 (`BufferDesc.state`, :344):

```
 ┌──────────── 64-bit state ────────────┐
 │ lock bits │ flags │ usage(4) │ refcount(18) │
 └───────────────────────────────────────┘
 BUF_REFCOUNT_BITS 18 (:49)   BUF_USAGECOUNT_BITS 4 (:50)
 BM_MAX_USAGE_COUNT 5 (:144)  — CLOCK survives ≤5 sweeps
```

Why packed: pin/unpin/usage-bump are single CAS ops — no spinlock on the hit
path. Same trick as topic-2's SwissTable metadata byte: cram the
hot-path-decidable state into one word.

## 2. The hit path — PinBuffer, bufmgr.c:3295

CAS loop on `state`: refcount+1, usage_count+1 if `< BM_MAX_USAGE_COUNT`
(:3338–3352). Lookup before that: `BufTableLookup` under one of
**`NUM_BUFFER_PARTITIONS = 128`** partition locks (lwlock.h:83,
buf_internals.h:244–250) — the map is sharded so lookups don't serialize.

Read `PinBufferForBlock` (:1223) → `ReadBuffer_common` (:1276) →
`StartReadBuffersImpl` (:1371) for how reads became a vectored/async
`ReadBuffersOperation` (v17+) — the miss path now streams.

## 3. The miss path — BufferAlloc + GetVictimBuffer

- `BufferAlloc` — bufmgr.c:2197: lookup (:2224), and on miss call
  `GetVictimBuffer` (:2548).
- `GetVictimBuffer`: `StrategyGetBuffer` picks a candidate; **if it's dirty,
  the backend writes it out itself** — `FlushBuffer` right there in the
  foreground (:2584 onward). Every eviction of a dirty page is a user-visible
  latency spike; this is what BgBufferSync exists to prevent.
- Note the WAL-rule cameo: `XLogNeedsFlush(BufferGetLSN(...))` before a
  ring buffer flush (~:2633) — can't write a page whose log isn't durable.

## 4. CLOCK — freelist.c

- `StrategyControl->nextVictimBuffer` — :42, one atomic for the whole pool.
- `ClockSweepTick` — :104–160: `fetch_add(1) % NBuffers`, with a CAS-based
  modular wraparound so the counter never overflows mid-flight.
- `StrategyGetBuffer` — :184: loop at :246–290 — pinned (refcount ≠ 0) ⇒
  skip; usage_count > 0 ⇒ decrement, keep sweeping; both zero ⇒ victim.
  "no unpinned buffers available" error if everything's pinned (~:274).
- **Buffer rings** — `GetAccessStrategy` :426: a seqscan gets a 256KB
  private ring (BAS_BULKREAD, :442–459) so one `SELECT count(*)` on a huge
  table can't flush the whole pool. Eviction policy as *admission* policy.

## 5. Background writer — BgBufferSync, bufmgr.c:3854

Runs the same clock ahead of the sweep hand, writing dirty buffers so
GetVictimBuffer finds clean ones. Pace: `bgwriter_lru_maxpages` (:190,
default 100 pages/round) + a moving average of recent allocation rate
(:3877–3911). It's an *estimator* — read the long comment.

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
