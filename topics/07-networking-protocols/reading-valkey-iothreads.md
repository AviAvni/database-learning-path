# Reading valkey's io-threads rework (1.5 h)

Repo: `~/repos/valkey`. Files: `src/io_threads.c`, `src/memory_prefetch.c`,
plus grep points in `networking.c`. This is the "great perf PRs to study"
item — valkey 8 rewrote redis 6's io-threads and roughly doubled throughput.
Read it as a case study in *what to parallelize when you refuse to lock the
data structures*.

## 0. The contract

Commands still execute ONLY on the main thread — single-threaded semantics,
zero locks in dict/rax/etc. What moves to threads: read(), RESP parsing,
write(), and (new) memory prefetching. Amdahl says: that's worth it exactly
when parse+I/O dominates — i.e., small commands, many clients. GRAPH.QUERY
with 50ms of matrix math? io-threads buy ~nothing. GET/SET at 1M ops/s? 2×.

## 1. The plumbing — io_threads.c

- `IOThreadMain` — :293: each thread's loop. Priority 1: drain its **private
  SPSC queue** in batches of `BATCH_SIZE` (:320–321, `spscDequeueBatch`),
  jobs are tagged pointers (`untagJob` :333 — job type in the pointer's low
  bits, topic-2's bit-smuggling again).
- `io_private_inbox[IO_THREADS_MAX_NUM]` — :23: one **SPSC** queue per
  thread. Single-producer (main thread), single-consumer (that io thread) ⇒
  no CAS contention at all; compare redis 6's design where threads spun on a
  shared list with a busy-wait fence.
- `spscCommit` (:61) — the producer batches enqueues and commits once:
  amortizing the release-store, same group-commit shape as topic 5.
- `trySendReadToIOThreads` — :514 / `trySendWriteToIOThreads` — :550: main
  thread offloads a client if threads are enabled + client is eligible;
  networking.c calls these at :2313, :3043, :6408 — note every call site
  has a same-thread fallback.
- `initIOThreads` — :489; threads can be resized at runtime (:476).

## 2. The clever part — memory_prefetch.c

Commands parsed by io threads sit in a batch; before the main thread
executes them, it **prefetches the dict entries** the batch will touch:

- `PrefetchCommandsBatch` — :26–33: keys of up to `max_prefetch_size`
  commands from multiple clients.
- The file comment (:7) states the idea: walk each key's lookup path
  (hash → bucket → entry → value) issuing `__builtin_prefetch` at each
  level, *interleaved across keys* — while key A's bucket line is in
  flight, compute key B's hash. This is software memory-level parallelism:
  topic 0's MLP finding (hash lookups are flat because loads overlap)
  engineered deliberately.

```
 without: exec(A): miss…wait 100ns… exec(B): miss…wait…      serial misses
 with:    prefetch A.bucket, B.bucket, C.bucket  (overlap!)
          exec(A) hit, exec(B) hit, exec(C) hit               misses paid once
```

## 3. What to steal for M7

- SPSC per worker beats MPMC when you can dedicate pairs — in tokio terms:
  per-connection tasks already give you this shape for free; the lesson
  applies when you add a worker pool for query execution (M9).
- Batch handoff + commit, not per-item signaling.
- Prefetch only helps when execution is memory-bound on *predictable*
  pointer chains — matrix kernels are already streaming; the graph-store
  analogue is prefetching node/edge attribute blocks for a batch of lookups.

## Questions to answer in notes.md

1. Why SPSC queues instead of one MPMC queue? What does the redis-6 design
   (shared list + busy spin) pay per job that SPSC doesn't?
2. Tagged job pointers: why smuggle the type in low bits instead of a
   struct { type, ptr }? (Queue slot stays one word ⇒ one cache line moves
   per batch of 8.)
3. Amdahl accounting for FalkorDB: measure (or estimate) parse+I/O share of
   a GRAPH.QUERY round-trip; at what query cost does io-threading stop
   mattering?
4. Why must prefetch batches span *multiple clients* to work? (One client's
   pipeline is sequential in the buffer, but its keys are independent —
   what actually limits batch depth?)

## Done when

You can explain what valkey parallelized, what it deliberately didn't, and
why the prefetcher is the same insight as topic 0's MLP experiment.
