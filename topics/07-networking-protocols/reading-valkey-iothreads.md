# valkey io-threads: parallelize the majority, nothing else

Valkey 8 rewrote redis 6's io-threads and roughly doubled throughput — while
commands still execute on one thread with zero locks in the data structures.
Read it as a case study in *what to parallelize when you refuse to lock the
data structures*: SPSC handoff, batch commit, and a prefetcher that turns
pointer chases into a pipeline. This chapter builds those three ideas step
by step, then maps them to `io_threads.c` and `memory_prefetch.c`. This is
the "great perf PRs to study" item.

## The problem in one sentence

At 1M small ops/s, profiling shows the single redis thread spends the
*majority* of its CPU on `read()`/`write()` syscalls and RESP parsing —
command execution itself is only ~30% — so the ceiling can be roughly
doubled by parallelizing only the I/O layer, if (and only if) the handoff to
worker threads costs less than the work being handed off.

## The concepts, step by step

### Step 1 — the contract: what may move to threads, what must not

The single-threaded command model is redis's core invariant: because
exactly one thread ever touches the keyspace, every dict/rax/listpack
operation runs with zero locks and commands are atomic by construction.
Valkey keeps that contract absolutely — commands still execute ONLY on the
main thread. What moves to io-threads: `read()`, RESP parsing, `write()`,
and (new in valkey 8) memory prefetching.

Amdahl's law (speedup is capped by the fraction you *don't* parallelize)
says this is worth it exactly when parse+I/O dominates — i.e., small
commands, many clients. GRAPH.QUERY with 50 ms of matrix math? io-threads
buy ~nothing. GET/SET at 1M ops/s? 2×.

### Step 2 — SPSC queues: handoff without contention

An **SPSC queue** (single-producer single-consumer: exactly one thread ever
pushes, exactly one ever pops) needs no CAS loops at all — the producer
owns the head index, the consumer owns the tail, and one release-store
publishes each batch. Valkey gives each io-thread its own private SPSC
inbox (`io_private_inbox[IO_THREADS_MAX_NUM]`, io_threads.c:23) fed only by
the main thread: N threads, N uncontended queues, zero shared-queue
contention.

Compare redis 6's design — worker threads spinning on one shared list with
a busy-wait fence, the main thread coordinating every batch — which burned
CPU for modest gains. The queue discipline alone is a large part of the
rewrite's win.

Why it matters: the handoff has to be cheaper than the ~1–2 µs syscall it
offloads, or the whole scheme loses. Uncontended SPSC push is ~10 ns.

### Step 3 — tagged pointers and batch commit: shrinking the handoff further

Two micro-optimizations make each handoff nearly free:

- **Tagged job pointers**: a job is one word — a pointer with the job
  *type* smuggled into its unused low bits (`untagJob`, :333; pointers to
  aligned objects always have zero low bits to spare). One word per job
  means a batch of 8 jobs moves in a single cache line — topic 2's
  bit-smuggling again.
- **Batch commit**: the producer doesn't publish each enqueue; it buffers
  them and `spscCommit` (:61) publishes the whole batch with one
  release-store — amortizing the fence, the same group-commit shape as
  topic 5's WAL.

On the consumer side, `IOThreadMain` (:293) drains its inbox in batches of
`BATCH_SIZE` (:320–321, `spscDequeueBatch`).

### Step 4 — the offload decision: eligible clients, same-thread fallback

The main thread decides per client, per event, whether to offload:
`trySendReadToIOThreads` (:514) and `trySendWriteToIOThreads` (:550)
offload only if threads are enabled and the client is eligible — and every
call site in networking.c (:2313, :3043, :6408) has a **same-thread
fallback**: if the offload can't happen, the main thread just does the work
itself, redis-style. Threads are an accelerator, not a dependency — they
can even be resized at runtime (`initIOThreads` :489, resize :476).

### Step 5 — the clever part: prefetching the batch's dict entries

By the time a batch of parsed commands reaches the main thread, valkey
knows every key the batch will touch — so before executing, it warms the
cache. A dict lookup is a **pointer chase** (hash → bucket → entry →
value: each load's address depends on the previous load's result, so the
~100 ns DRAM misses serialize — topic 0, Step 5). But *across* commands
the chains are independent, so `PrefetchCommandsBatch`
(memory_prefetch.c:26–33) walks all the chains **level by level**, issuing
`__builtin_prefetch` for every batch member at each level — while key A's
bucket line is in flight, it computes key B's hash:

```
 without: exec(A): miss…wait 100ns… exec(B): miss…wait…      serial misses
 with:    prefetch A.bucket, B.bucket, C.bucket  (overlap!)
          exec(A) hit, exec(B) hit, exec(C) hit               misses paid once
```

```rust
// Walk every key's lookup path LEVEL BY LEVEL across the batch:
// while A's bucket line is in flight, compute B's hash — the pointer
// chase becomes a pipeline of overlapping misses, not a chain.
fn prefetch_batch(dict: &Dict, batch: &[Command]) {
    let hashes: Vec<u64> = batch.iter().map(|c| hash(c.key())).collect();
    for &h in &hashes {
        prefetch(dict.bucket_addr(h));          // level 1: all bucket lines
    }
    for &h in &hashes {
        prefetch(dict.entry_addr(h));           // level 2: entries (buckets now warm)
    }
    // main thread then executes the batch: every lookup hits warm lines
}
```

This is software memory-level parallelism: topic 0's MLP finding (10
independent misses in flight ≈ 10× cheaper per miss) engineered
deliberately. The file comment at memory_prefetch.c:7 states the whole
idea; keys come from up to `max_prefetch_size` commands *across multiple
clients* (question 4 asks why that matters).

## Where each step lives in the code

Local clone at `~/repos/valkey`:

| Anchor | What | Step |
|--------|------|------|
| `io_private_inbox[IO_THREADS_MAX_NUM]` — io_threads.c:23 | one SPSC per thread | 2 |
| `spscCommit` — io_threads.c:61 | batched producer commit | 3 |
| `IOThreadMain` — io_threads.c:293 | consumer loop, `spscDequeueBatch` :320–321 | 2–3 |
| `untagJob` — io_threads.c:333 | job type in pointer low bits | 3 |
| `initIOThreads` — io_threads.c:489 (resize :476) | setup, runtime resize | 4 |
| `trySendReadToIOThreads` — :514 / `trySendWriteToIOThreads` — :550 | offload decision | 4 |
| networking.c :2313, :3043, :6408 | call sites, each with same-thread fallback | 4 |
| `PrefetchCommandsBatch` — memory_prefetch.c:26–33 (file comment :7) | batch prefetch | 5 |

## What to steal for M7

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

## References

**Code**
- [valkey-io/valkey](https://github.com/valkey-io/valkey) —
  `src/io_threads.c`, `src/memory_prefetch.c` (the file comment at :7
  states the whole idea), plus the grep points in `src/networking.c`.
  Local clone at `~/repos/valkey`.
