# valkey io-threads: parallelize the majority, nothing else

Valkey 8 rewrote redis 6's io-threads and published a 3.3× throughput increase
— while commands still execute on one thread, with zero locks in the data
structures. Read it as a case study in *what you are allowed to parallelize
when you refuse to lock the keyspace*: three queues with three different
disciplines, a one-word job handoff, a published-watermark protocol that
replaces locking, an adaptive pool that turns itself on, and a prefetcher that
turns serial pointer chases into overlapping ones.

**Which version this chapter is about.** Every anchor below is
`valkey-io/valkey@8891441ab`. The design changed substantially between the
2024 blog posts and this commit, and this chapter flags every place they
disagree. Confirm and read with:

```
$ tools/pinned-source.py ref valkey
valkey  valkey-io/valkey  8891441ab

$ tools/pinned-source.py show valkey src/io_threads.c -r 1:75
$ tools/pinned-source.py check valkey src/io_threads.c:514 --contains 'trySendReadToIOThreads'
```

`src/io_threads.c` is 918 lines and `src/memory_prefetch.c` is 302 — read both
in full. You do not need a local clone.

## The problem in one sentence

At a million small operations per second, the single valkey thread spends the
majority of its CPU not executing commands but on socket syscalls, RESP parsing
and — measured by the maintainers — `epoll_wait` alone taking "more than 20
percent of the time", plus over 40% of main-thread time inside `lookupKey`; so
the ceiling can be raised several-fold by parallelizing *only* the I/O layer and
*only* the memory stalls, provided the handoff to worker threads costs
meaningfully less than the work being handed off.

## The concepts, step by step

### Step 1 — the contract: what may move to threads, what must not

> **In:** a server whose correctness rests on "exactly one thread touches the
> keyspace", and a profile saying that thread is mostly not touching the
> keyspace.
> **Out:** a partition of the work into a threadable majority and a
> non-negotiable single-threaded core — plus the Amdahl arithmetic that says
> when the split is worth anything.

The single-threaded command model is redis's and valkey's core invariant.
Because exactly one thread ever touches the keyspace, every hashtable, rax and
listpack operation runs with **zero locks**, and every command is atomic by
construction — no transaction manager, no latch ordering, no deadlock detector.
That invariant is worth more than any throughput number, and valkey keeps it
absolutely: **commands still execute only on the main thread.**

What moves to I/O threads is everything that is not command execution. The job
enum is the exact list, and it is short:

```c
// valkey src/io_threads.h — the complete set of offloadable jobs, 6-22
     6  typedef enum {
     7      JOB_REQ_READ_CLIENT = 0,
     8      JOB_REQ_WRITE_CLIENT,
     9      JOB_REQ_FREE_ARGV,
    10      JOB_REQ_FREE_OBJ,
    11      JOB_REQ_POLL,
    12      JOB_REQ_ACCEPT,
    13      JOB_REQ_COUNT
    14  } JobRequest;
    15  _Static_assert(JOB_REQ_COUNT <= 8, "JOB_REQ_COUNT must not exceed 7 for pointer arithmetic");
    16
    17  typedef enum {
    18      JOB_RES_READ_CLIENT = 0,
    19      JOB_RES_WRITE_CLIENT,
    20      JOB_RES_COUNT
    21  } JobResult;
    22  _Static_assert(JOB_RES_COUNT <= 8, "JOB_RES_COUNT must not exceed 7 for pointer arithmetic");
```

Six request types: read a client (which includes RESP parsing), write a client,
free an `argv`, free an object, run the poll, and accept a connection. Note what
is *not* there: no "execute". Note also `JOB_REQ_POLL` — valkey offloads the
`epoll_wait` itself, which redis does not. The maintainers' stated reason is
that "when executed solely by the main thread, `epoll_wait` consumes more than
20 percent of the time" (*Unlock 1 Million RPS*, part 1, § *High Level Design*),
with the discipline that "at any given time, at most one thread, either an
io_thread or the main thread, executes `epoll_wait`".

Now the Amdahl accounting, because this is the step where people fool
themselves. Amdahl's law says speedup is capped by the fraction you *do not*
parallelize: if a share `s` of the work stays serial, the ceiling is `1/s` no
matter how many threads you add. Work it on the maintainers' own published
numbers, which come in two stages:

```
Stage 1 — I/O threads alone (part 2, § "Back to Valkey"):
    "reaching up to 780K SET commands per second"
    and then: "Valkey's main thread was spending more than 40% of its
    time in a single function: lookupKey"

    The I/O work was parallelized away, and what surfaced underneath was
    not command logic — it was memory stalls. That 40% is the new serial
    share, so the new ceiling is 1/0.40 = 2.5× over the 780K.

Stage 2 — memory-access amortization (part 2, § "Batching and interleaving"):
    "reduces the time spent on lookupKey by more than 80%"
    "In total the impact of the memory access amortization on Valkey
     performance is almost 50% and it increased the requests per second
     to more than 1.19M rps"

    780K × 1.5 ≈ 1.17M.   Published figure: 1.19M.   The arithmetic closes.

Headline (part 1, § "Major Upgrade to Valkey Performance"):
    360K → 1.19M rps, "approximately 230%" increase, against Valkey 7.2
    average latency 1.792 ms → 0.542 ms
    on AWS EC2 c7g.16xlarge, 8 I/O threads, 3M keys, 512-byte values,
    650 clients, sequential SET
```

Two things to take from that arithmetic. First, **the comparison base is valkey
7.2, not redis**, and the workload is one specific sequential-SET run on a
64-vCPU Graviton instance; quoting "valkey is 3.3× faster" without those
conditions is exactly the sin this topic's measured lane exists to punish. This
repo's own headline ([FINDINGS.md](../../FINDINGS.md) row 7) makes the same
point from the other direction: identical zero-work requests span **279×**
(44,088 ops/s at P=1 to 12,321,414 at P=256) purely by changing pipeline depth,
so a throughput number without its depth, its client count and its value size
is not a measurement of a server at all.

Second, the two stages are the whole design. Stage 1 says *parallelize the I/O*.
Stage 2 says *once you have, the bottleneck is DRAM, not CPU* — and Step 6 is
what they did about it.

So: `GRAPH.QUERY` with 50 ms of matrix math per call? I/O threads buy
approximately nothing — the serial share is ~100%. `GET`/`SET` at a million
ops/s with 650 concurrent clients? That is the case the numbers above describe.
Before you copy this design, measure `s` for *your* workload.

### Step 2 — three queues, three disciplines

> **In:** a main thread with jobs to give away and N workers with results to
> give back.
> **Out:** a handoff whose cost is a few atomic operations, chosen per direction
> — because "which threads may touch this queue" is a different question in each
> direction, and the right answer is a different queue.

Most descriptions of valkey's I/O threads say "each thread gets its own SPSC
queue". That is one third of the truth, and it is the least important third.
The declarations are the whole map:

```c
// valkey src/io_threads.c — the three queues and who may touch them, 17-27
    17  static int cur_epoll_thread = 0;
    18  // Main -> IO: Shared Queue (Single Producer Multi Consumer) where all IO threads pull jobs from
    19  static spmcQueue io_shared_inbox = {0};
    20  // IO -> Main: Response Channel (Multi Producer Single Consumer) used by IO threads to send results back to main-thread
    21  static mpscQueue io_shared_outbox = {0};
    22  // Main -> IO (Thread-Specific) for tasks that must run on specific IO thread where IO threads check their private inbox before the shared queue
    23  static spscQueue io_private_inbox[IO_THREADS_MAX_NUM] = {0};
    24  static size_t io_jobs_submitted;
    25  static _Atomic(size_t) io_jobs_finished;
    26  static int io_threads_initialized = 0;
    27  _Atomic long long used_active_time_io_thread[IO_THREADS_MAX_NUM] = {0};
```

Read the three comments as three different answers to "who is allowed to touch
this?":

- **`io_shared_inbox` — SPMC.** One producer (the main thread), many consumers
  (every I/O thread). This is where the *actual work* goes: `JOB_REQ_READ_CLIENT`
  and `JOB_REQ_WRITE_CLIENT` are enqueued here (`io_threads.c:534`). It is a
  shared queue on purpose: any thread may take any client's read, so a burst of
  work is load-balanced automatically with no scheduling decision by the main
  thread.
- **`io_shared_outbox` — MPSC.** Many producers (every I/O thread), one consumer
  (the main thread). Results come back here (`sendToMainThread`, `:769-775`).
  This one *must* tolerate concurrent producers, so it costs more — and when it
  is full, the producing thread spills into a thread-local
  `pending_io_responses` list (`:14`, `:774-775`) rather than blocking.
- **`io_private_inbox[i]` — SPSC, one per thread.** One producer, one consumer,
  the cheapest discipline there is: the producer owns the head index, the
  consumer owns the tail, and no CAS loop is ever needed. These carry only jobs
  that must land on a *specific* thread — `JOB_REQ_FREE_ARGV` (free the memory
  on the thread that allocated it) and `JOB_REQ_POLL`.

The consumer loop makes the priority explicit, and the comments name the
disciplines:

```c
// valkey src/io_threads.c — IOThreadMain, 308-345 (private first, then shared)
   308      while (1) {
// ... 309-318: cancellation point; account time spent since the last turn ...
   319          processed = 0;
   320          /* PRIORITY 1: Drain Private SPSC Queue (Batch Processing) */
   321          while ((batch_count = spscDequeueBatch(&io_private_inbox[id], batch_jobs, BATCH_SIZE)) > 0) {
   322              for (size_t i = 0; i < batch_count; i++) {
   323                  void *data;
   324                  int type;
   325                  untagJob(batch_jobs[i], &data, &type);
   326
   327                  switch (type) {
   328                  case JOB_REQ_FREE_ARGV:
   329                      IOThreadFreeArgv((robj **)data);
   330                      break;
   331                  case JOB_REQ_POLL:
   332                      IOThreadPoll((aeEventLoop *)data);
   333                      break;
   334                  default:
   335                      serverPanic("Invalid SPSC job type: %d", type);
   336                  }
   337              }
   338              processed += batch_count;
   339          }
   340
   341          /*
   342           * PRIORITY 2: Shared Global Queue (SPMC)
   343           * Only checked after SPSC is drained.
   344           */
   345          void *tagged_job = spmcDequeue(&io_shared_inbox);
```

Note the asymmetry: the private queue is drained in **batches of
`BATCH_SIZE` = 32** (`:152`, `:305`, `:321`), the shared queue one job at a time
(`:345`). Batch dequeue is exactly the SPSC discipline's payoff — the consumer
owns its index, so it can advance it 32 slots with one publish.

And when a thread finds both queues empty it does not spin. It blocks on a
mutex the main thread holds:

```c
// valkey src/io_threads.c — IOThreadMain, 377-386 (park on a mutex, do not spin)
   377          /* If both queues were empty (no processing done), wait for signal. */
   378          if (processed == 0) {
   379              if (unlikely(pending_io_responses)) {
   380                  flushPendingIOResponses(0);
   381              } else {
   382                  /* If it is locked. We should block until main thread unlocks it. */
   383                  pthread_mutex_lock(&io_threads_mutex[id]);
   384                  pthread_mutex_unlock(&io_threads_mutex[id]);
   385              }
   386          }
```

That is the single most important difference from redis 6's io-threads, which
had worker threads **busy-waiting** on a shared list with a spin fence: they
burned a core each while idle, which is why the feature was widely disabled in
production. Here an inactive thread costs nothing, which is what makes Step 5's
adaptive pool possible at all.

Which queue the *poll* job uses is decided by thread count, and the comment
states the trade-off outright:

```c
// valkey src/io_threads.c — trySendPollJobToIOThreads, 748-763 (SPMC or SPSC, by scale)
   748      /* Use SPMC to minimize polling overhead. At high thread counts, use private SPSC queues for lower latency. */
   749      if (server.active_io_threads_num <= 9) {
   750          if (unlikely(spmcEnqueue(&io_shared_inbox, job) == false)) {
// ... 751-754: on a full queue, abandon the offload and poll on the main thread ...
   755      } else {
   756          cur_epoll_thread = ((cur_epoll_thread) % (server.active_io_threads_num - 1)) + 1;
   757          if (unlikely(spscIsFull(&io_private_inbox[cur_epoll_thread]))) {
// ... 758-760: same abandon-and-fall-back path ...
   761          }
   762          spscEnqueue(&io_private_inbox[cur_epoll_thread], job, true);
   763      }
```

Below ten active threads, contention on the shared queue is cheaper than the
bookkeeping of round-robining private ones; above ten, it is not. That crossover
is a *measured* engineering constant, not a principle — the lesson to steal is
that "SPSC is always better" is false, and which queue wins depends on N.

### Step 3 — tagged pointers and batch commit: making the handoff nearly free

> **In:** a job that is a (pointer, type) pair.
> **Out:** a single machine word in a queue slot, and a batch of them published
> with one release-store instead of one per job.

The handoff has to be cheaper than the work it offloads or the whole scheme
loses. Two micro-optimizations get it down to a few instructions.

**Tagged job pointers.** A job is one word — the pointer with its type smuggled
into the low bits that alignment guarantees are zero:

```c
// valkey src/io_threads.c — the tagged-pointer job encoding, 29-42
    29  /* Job Types for Tagged Pointers
    30   * We use the lower 3 bits of the pointer to store the job type.
    31   * Requires data pointers to be 8-byte aligned (standard for zmalloc/ptrs). */
    32  #define JOB_TAG_MASK 0x7
    33  #define JOB_PTR_MASK (~(uintptr_t)JOB_TAG_MASK)
    34
    35  static inline void *tagJob(void *ptr, int type) {
    36      return (void *)((uintptr_t)ptr | type);
    37  }
    38
    39  static inline void untagJob(void *tagged_ptr, void **ptr, int *type) {
    40      *type = (int)((uintptr_t)tagged_ptr & JOB_TAG_MASK);
    41      *ptr = (void *)((uintptr_t)tagged_ptr & JOB_PTR_MASK);
    42  }
```

Three bits is a budget of eight job types, and Step 1's
`_Static_assert(JOB_REQ_COUNT <= 8, ...)` (`io_threads.h:15`) is what stops
someone spending a ninth. Encoding is one `or`; decoding is two `and`s. This is
topic 2's bit-smuggling in its simplest form.

Work the cache arithmetic, because it is the actual point. A queue slot is one
8-byte word, so a 64-byte cache line holds **8 jobs**, and `BATCH_SIZE = 32`
jobs occupy exactly **4 cache lines**:

```
struct { void *ptr; int type; }   →  16 B per slot (12 + padding)
                                     4 slots per cache line
                                     32 jobs = 8 cache lines

tagged single word                →   8 B per slot
                                     8 slots per cache line
                                     32 jobs = 4 cache lines   ← half the traffic
```

Every one of those lines is contended — it crosses from the producer's core to
the consumer's. Halving them halves the coherence traffic on the hottest
structure in the design.

**Batch commit.** The producer does not publish each enqueue. It buffers them
and publishes the batch with one commit:

```c
// valkey src/io_threads.c — commitIOJobs, 59-63 (one publish per batch, per thread)
    59  void commitIOJobs(void) {
    60      for (int i = 1; i < server.active_io_threads_num; i++) {
    61          spscCommit(&io_private_inbox[i]);
    62      }
    63  }
```

`spscCommit` itself lives in `src/queues.h` (included at `io_threads.c:8`);
`:61` is the call site — the point where the main thread makes a whole batch of
private-queue work visible at once. This is the same shape as topic 5's group
commit: the fence is the expensive part, so amortize it over as many items as
you can bear to delay.

Why it matters, in one line: the handoff must cost less than a syscall. This
repo measured `write()` at **1.17 µs** ([FINDINGS.md](../../FINDINGS.md) row 5).
A tagged-pointer enqueue into an uncontended SPSC ring is a store, an index
bump, and — once per batch — one release fence: tens of nanoseconds, not
microseconds. There is roughly two orders of magnitude of headroom, which is
why the design works at all. (This repo has not measured the enqueue cost
directly; if you want the number, that is an exercise, not a citation.)

### Step 4 — the offload decision: eligibility, and always a same-thread fallback

> **In:** a client with a readable socket or pending replies.
> **Out:** either a job on the shared inbox, or nothing at all — in which case
> the main thread does the work itself, exactly as redis would.

The main thread decides per client, per event. `trySendReadToIOThreads`
(`io_threads.c:514`) is a wall of eligibility checks followed by one enqueue:

```c
// valkey src/io_threads.c — trySendReadToIOThreads, 514-544 (eligibility, then SPMC enqueue)
   514  int trySendReadToIOThreads(client *c) {
   515      if (server.active_io_threads_num <= 1) return C_ERR;
   516      /* If IO thread is already reading, return C_OK to make sure the main thread will not handle it. */
   517      if (c->io_read_state != CLIENT_IDLE) return C_OK;
   518      if (c->io_write_state == CLIENT_PENDING_IO) return C_OK;
   519      /* For simplicity, don't offload replica clients reads as read traffic from replica is negligible */
   520      if (getClientType(c) == CLIENT_TYPE_REPLICA) return C_ERR;
   521      /* With Lua debug client we may call connWrite directly in the main thread */
   522      if (c->flag.lua_debug) return C_ERR;
   523      /* For simplicity let the main-thread handle the blocked clients */
   524      if (c->flag.blocked || c->flag.unblocked) return C_ERR;
   525      if (c->flag.close_asap) return C_ERR;
// ... 526-533: stash parse/auth/replication flags on the client, mark it
//              CLIENT_PENDING_IO, postpone connection state updates ...
   534      if (unlikely(spmcEnqueue(&io_shared_inbox, tagJob(c, JOB_REQ_READ_CLIENT)) == false)) {
   535          c->read_flags = 0;
   536          c->io_read_state = CLIENT_IDLE;
   537          connSetPostponeUpdateState(c->conn, 0);
   538          return C_ERR;
   539      }
   540
   541      io_jobs_submitted++;
   542      server.stat_io_reads_pending++;
   543      c->flag.pending_read = 1;
   544      return C_OK;
   545  }
```

Every check is a *simplification*, not a correctness requirement — read the
comments: "for simplicity", "for simplicity", "for simplicity". Blocked clients,
replicas and Lua-debug clients are hard to reason about concurrently, so they
are simply not offloaded. That is the correct instinct for a change of this
risk: shrink the concurrent surface until you can hold it in your head.

Note `:534-539`: if the queue is full, the function **undoes its own state
changes** and returns `C_ERR`. Everything is reversible up to the enqueue, and
the enqueue is the commit point.

The `C_ERR` matters because every call site has a same-thread fallback:

```c
// valkey src/networking.c — sendReplyToClient, 3040-3045 (offload, else do it here)
  3040  /* Write event handler. Just send data to the client. */
  3041  void sendReplyToClient(connection *conn) {
  3042      client *c = connGetPrivateData(conn);
  3043      if (trySendWriteToIOThreads(c) == C_OK) return;
  3044      writeToClient(c);
  3045  }
```

```c
// valkey src/networking.c — handleClientsWithPendingWrites, 3258-3272 (same shape, in the flush loop)
  3258          c->flag.pending_write = 0;
  3259          listUnlinkNode(server.clients_pending_write, ln);
  3260
  3261          if (!clientHasPendingReplies(c)) continue;
  3262
  3263          /* If we can send the client to the I/O thread, let it handle the write. */
  3264          if (trySendWriteToIOThreads(c) == C_OK) continue;
  3265
  3266          /* We can't write to the client while IO operation is in progress. */
  3267          if (c->io_write_state != CLIENT_IDLE) continue;
  3268
  3269          processed++;
  3270
  3271          /* Try to write buffers to the client socket. */
  3272          if (writeToClient(c) == C_ERR) continue;
```

Threads are an **accelerator, not a dependency**. Set `io-threads 1` — which is
the default, "Single threaded by default" (`config.c:3375`) — and every one of
these paths falls through to the redis behaviour the previous chapter of this
topic describes, function for function.

They are also *adaptive*. `active_io_threads_num` starts at 1 even when
`io-threads` is 8 (`io_threads.c:497`), and threads ignite only when the main
thread is visibly drowning:

```c
// valkey src/io_threads.c — the ignition thresholds, 148-152 and 171-179
   148  #define IO_IGNITION_EVENTS 4
   149  #define IO_IGNITION_CPU_SYS 30.0
   150  #define IO_IGNITION_CPU_SYS_LOW 5.0
   151  #define IO_IGNITION_CPU_USER 50.0
   152  #define BATCH_SIZE 32
// ... 153-170: IOThreadsAfterSleep; the always-active policy short-circuits here ...
   171      /* Ignition Policy */
   172      if (server.active_io_threads_num == 1) {
   173          int should_ignite = 0;
   174  #ifdef RUSAGE_THREAD
   175          float cpu_sys = (float)getInstantaneousMetric(STATS_METRIC_MAIN_THREAD_CPU_SYS) / 10000.0;
   176          float cpu_user = (float)getInstantaneousMetric(STATS_METRIC_MAIN_THREAD_CPU_USER) / 10000.0;
   177          /* Ignite IO threads if sys CPU > 30%, or if sys CPU > 5% and user CPU > 50% */
   178          should_ignite = (cpu_sys > IO_IGNITION_CPU_SYS) ||
   179                          (cpu_sys > IO_IGNITION_CPU_SYS_LOW && cpu_user > IO_IGNITION_CPU_USER);
```

"System CPU above 30%" is Step 1's premise turned into a runtime test: *if the
main thread is spending a third of its life in the kernel, there is I/O worth
stealing.* After ignition the pool scales by queue depth, one thread at a time:

```c
// valkey src/io_threads.c — the scaling decision, 206-218 (queue depth drives the pool)
   206      /* Decision (Every STATS_METRIC_SAMPLES Samples) */
   207      if (sample_count % STATS_METRIC_SAMPLES != 0) return;
   208
   209      size_t avg_q_size = getInstantaneousMetric(STATS_METRIC_IO_WAIT);
   210      size_t active = server.active_io_threads_num;
   211      size_t target = active;
   212
   213      /* Calculate Target */
   214      if (avg_q_size > 1 && active < (size_t)server.io_threads_num) {
   215          target++;
   216      } else if (avg_q_size == 0 && (now - last_scale_time > IO_COOLDOWN_MS)) {
   217          if (target > 1) target--;
   218      }
```

Scale up when the average queue is non-trivially occupied; scale down, after a
cooldown, when it is empty. `io-threads` is a *ceiling*, not a thread count (max
`IO_THREADS_MAX_NUM` = 256, `config.h:361`), and it is modifiable at runtime via
`updateIOThreads` (`io_threads.c:442`), which refuses while the response queue is
too full to drain safely (`:455-464`) — a deadlock this design has to actively
avoid, and documents.

### Step 5 — the published watermark: how you share buffers without locking them

> **In:** a reply buffer the main thread is appending to and an I/O thread is
> about to write to the socket.
> **Out:** a single snapshot value that tells the worker exactly how far it may
> read — and no lock anywhere.

This is the step the design's popular summaries skip, and it is where the real
concurrency reasoning lives. If commands run on the main thread and writes run
on an I/O thread, they are both touching `c->reply` and `c->buf`. What stops
them tearing?

Not a lock. A watermark, snapshotted by the main thread *before* the job is
enqueued:

```c
// valkey src/io_threads.c — trySendWriteToIOThreads, 567-583 (snapshot how far the worker may go)
   567      } else {
   568          /* Save the last block of the reply list to io_last_reply_block and the used
   569           * position to io_last_bufpos. The I/O thread will write only up to
   570           * io_last_bufpos, regardless of the c->bufpos value. This is to prevent I/O
   571           * threads from reading data that might be invalid in their local CPU cache. */
   572          c->io_last_reply_block = listLast(c->reply);
   573          if (c->io_last_reply_block) {
   574              clientReplyBlock *block = (clientReplyBlock *)listNodeValue(c->io_last_reply_block);
   575              c->io_last_bufpos = block->used;
// ... 576-577: force a fresh header if the block is encoded ...
   578          } else {
   579              c->io_last_bufpos = (size_t)c->bufpos;
// ... 580-582: same, for the static buffer ...
   583      }
```

Read the comment at `:569-571` slowly. The worker writes "only up to
`io_last_bufpos`, **regardless of the `c->bufpos` value**". The main thread may
keep appending past that point while the worker is running; the worker will not
look, so it cannot observe a half-written byte range, and it does not need the
main thread's writes to be visible to it at all. The bound was published once,
before the handoff, and the handoff's release-store is what makes everything
written before it visible.

This is the general pattern, and it is worth naming because it recurs
everywhere in this repo's topics: **you do not need mutual exclusion if you can
partition the data by a value that only one side ever advances.** The main
thread owns "how much exists"; the worker owns "how much has been sent"; the
watermark is the fence between them. Compare topic 5's WAL, where the durable
LSN plays exactly this role, and topic 8's MVCC, where a snapshot timestamp
does.

The read direction is guarded by a state machine on the client instead:
`io_read_state` and `io_write_state` move between `CLIENT_IDLE`,
`CLIENT_PENDING_IO` and `CLIENT_COMPLETED_IO` (see the guards at
`io_threads.c:517-518` and `:553-554`, and the assert in
`processClientIOReadsDone`, `networking.c:6412`). A client is owned by exactly
one thread at a time, and the state field is the token of ownership. Again: not
a lock — an invariant maintained by whose turn it is.

### Step 6 — the clever part: prefetching the batch's lookups

> **In:** a batch of parsed commands from the I/O threads, and a main thread
> about to execute them one at a time.
> **Out:** every one of those commands' hashtable lookups issued *concurrently*
> as cache misses, so the batch pays one DRAM latency instead of `n`.

Once Step 1's stage 1 removed the I/O, the maintainers found their main thread
spending "more than 40% of its time in a single function: `lookupKey`" (part 2,
§ *Back to Valkey*). A hashtable lookup is a **pointer chase**: hash → bucket →
entry → value, where each load's *address* comes from the previous load's
*result*. The CPU cannot start load `n+1` before load `n` returns, so the misses
serialize. Part 2 puts the scale of the penalty plainly — external memory access
is roughly 50× L1 latency — and demonstrates it on a toy: scanning 16 linked
lists of 10 million elements each takes **20.8 seconds** sequentially on a
Graviton 3, but interleaving the 16 traversals takes **under 2 seconds** — "a 10x
speedup" — and adding `__builtin_prefetch` brings it to **1.8 s**.

Nothing about the memory got faster. The misses simply overlapped. This repo
measured the same effect from the other end: topic 0's `lookup_shootout` finds a
HashMap probe costing **9.3 ns at n = 1e7** in a ~160 MB table where a single
*dependent* random probe "should" cost a ~100 ns DRAM miss
([FINDINGS.md](../../FINDINGS.md) row 0; the table is in
[topic 0's notes.md](../00-performance-toolbox/notes.md)) — roughly a tenfold
gap, produced by nothing but the probes being independent enough for the
out-of-order window to overlap them.

Valkey's contribution is to *engineer* that overlap deliberately. Because the
I/O threads hand over a batch, the main thread knows every key the next `n`
commands will touch before it touches any of them. `hashtablePrefetch` then
walks all the lookups **round-robin, one step each**, rather than one lookup to
completion:

```c
// valkey src/memory_prefetch.c — hashtablePrefetch, 158-168 (round-robin, one step per key)
   158  static void hashtablePrefetch(hashtable **tables) {
   159      initBatchInfo(tables);
   160      KeyPrefetchInfo *info;
   161      while ((info = getNextPrefetchInfo())) {
   162          switch (info->state) {
   163          case PREFETCH_ENTRY: prefetchEntry(info); break;
   164          case PREFETCH_VALUE: prefetchValue(info); break;
   165          default: serverPanic("Unknown prefetch state %d", info->state);
   166          }
   167      }
   168  }
```

`getNextPrefetchInfo` (`:98-106`) advances a cursor modulo the batch size and
returns the next key that is not `PREFETCH_DONE`; `prefetchEntry` (`:122-133`)
performs exactly **one** `hashtableIncrementalFindStep` and then calls
`moveToNextKey` (`:87-89`). So the loop's shape is:

```
key A: step 1  (issue A's bucket load, do not wait)
key B: step 1  (issue B's bucket load — A's is still in flight)
key C: step 1  ...
key A: step 2  (A's line has arrived by now; issue A's entry load)
key B: step 2
...
```

The chase is not shortened. It is *turned sideways*, so `n` independent chains
progress in lockstep and their misses overlap. Note the API this rests on:
`hashtableIncrementalFindInit` / `…Step` / `…GetResult` (`:118`, `:123`, `:138`)
— the hashtable exposes a *resumable* find precisely so a caller can interleave
several. That is the reusable design lesson: to get memory-level parallelism out
of a data structure, its lookup has to be expressible as a state machine you can
step, not a function you must call to completion.

There are two more prefetch phases before the hashtable walk, and they are not
about keys at all:

```c
// valkey src/memory_prefetch.c — prefetchCommands, 181-213 (argv first, then the tables)
   181  static void prefetchCommands(void) {
   182      /* Prefetch argv's for all clients */
   183      for (size_t i = 0; i < batch->client_count; i++) {
   184          client *c = batch->clients[i];
   185          if (!c || c->argc <= 1) continue;
   186          /* Skip prefetching first argv (cmd name) it was already looked up by the I/O thread. */
   187          for (int j = 1; j < c->argc; j++) {
   188              valkey_prefetch(c->argv[j]);
   189          }
   190      }
// ... 191-202: a second pass prefetching argv[j]->ptr for RAW-encoded objects ...
   203      /* Get the keys ptrs - we do it here after the key obj was prefetched. */
   204      for (size_t i = 0; i < batch->key_count; i++) {
   205          batch->keys[i] = objectGetVal((robj *)batch->keys[i]);
   206      }
   207
   208      /* Prefetch hashtable keys for all commands. Prefetching is beneficial only if there are more than one key. */
   209      if (batch->key_count > 1) {
   210          server.stat_total_prefetch_batches++;
   211          /* Prefetch keys from the main hashtable */
   212          hashtablePrefetch(batch->keys_tables);
   213      }
   214  }
```

The `argv` objects were allocated *on an I/O thread's core*, so they are cold in
the main thread's L1 — part 2 names this as a second problem it had to solve
with the same method. And `:209`: with a single key there is nothing to overlap
with, so the whole mechanism is skipped. Prefetching is only ever worth it in
batches.

Finally, where the batch comes from — and this is the answer to the "why span
multiple clients?" question:

```c
// valkey src/memory_prefetch.c — addCommandToBatchAndProcessIfFull, 263-289 (batch across clients AND pipelines)
   263  int addCommandToBatchAndProcessIfFull(client *c) {
   264      if (!batch) return C_ERR;
   265
   266      batch->clients[batch->client_count++] = c;
   267
   268      /* Client's next command */
   269      if (c->parsed_cmd && !(c->read_flags & READ_FLAGS_BAD_ARITY)) {
   270          c->read_flags |= READ_FLAGS_PREFETCHED;
   271          addCommandToBatch(c->parsed_cmd, c->argv, c->argc, c->db, c->slot);
   272      }
   273
   274      /* Commands in the queue. */
   275      for (int j = c->cmd_queue.off; j < c->cmd_queue.len && batch->key_count < batch->max_prefetch_size; j++) {
// ... 276-279: add each already-parsed pipelined command's keys to the batch ...
   280      }
   281
   282      /* If the batch is full, process it.
   283       * We also check the client count to handle cases where
   284       * no keys exist for the clients' commands. */
   285      if (batch->client_count == batch->max_prefetch_size || batch->key_count == batch->max_prefetch_size) {
   286          processClientsCommandsBatch();
   287      }
   288
   289      return C_OK;
   290  }
```

Both sources count: `:266-272` adds the client's next command, and `:275-280`
adds everything already parsed in that client's *pipeline*. `max_prefetch_size`
is the `prefetch-batch-max-size` config, **default 16**, range 0–128
(`config.c:3379`). So the batch fills from many clients *and* from one client's
depth — which is another way of saying that this topic's measured 279× pipelining
result ([FINDINGS.md](../../FINDINGS.md) row 7) is not only about syscalls.
Depth also feeds the prefetcher. A client that pipelines gives the server both
fewer syscalls per command *and* a wider batch to overlap misses across.

One drift to note if you read the blog first: part 2 calls this function
`dictPrefetch` and describes a chained hash of `dictEntry`s. At this pin the
dict has been replaced by an open-addressed `hashtable`, and the function is
`hashtablePrefetch`. The idea is identical; the names and the data structure are
not.

## Where each step lives in the code

Everything is `valkey-io/valkey@8891441ab`. Read `src/io_threads.c` (918 lines)
and `src/memory_prefetch.c` (302 lines) in full; from `src/networking.c` (6,665
lines) read only the call sites.

| Anchor | What | Step |
|--------|------|------|
| `JobRequest` / `JobResult` — `src/io_threads.h:6-22` | the complete list of what may be offloaded; the `<= 8` static asserts | 1, 3 |
| `io_shared_inbox` (SPMC) — `src/io_threads.c:19` | main → any thread; carries the *read and write* jobs | 2 |
| `io_shared_outbox` (MPSC) — `src/io_threads.c:21` | threads → main; `sendToMainThread` `:769` | 2 |
| `io_private_inbox[]` (SPSC) — `src/io_threads.c:23` | main → *one specific* thread; free-argv and poll only | 2 |
| `IOThreadMain` — `src/io_threads.c:293` | private-first priority `:320-339`, shared `:345`, park on mutex `:377-386` | 2 |
| `BATCH_SIZE 32` — `src/io_threads.c:152` | `spscDequeueBatch` at `:321` | 2, 3 |
| `tagJob` / `untagJob` — `src/io_threads.c:35`, `:39` | 3-bit type in the pointer's low bits, `:29-33` | 3 |
| `commitIOJobs` — `src/io_threads.c:59` | `spscCommit` per thread at `:61`; the queue code is in `src/queues.h` | 3 |
| `trySendReadToIOThreads` — `src/io_threads.c:514` | eligibility wall, then `spmcEnqueue` at `:534`; full-queue rollback `:534-539` | 4 |
| `trySendWriteToIOThreads` — `src/io_threads.c:550` | same shape; the watermark snapshot at `:567-583` | 4, 5 |
| `sendReplyToClient` — `src/networking.c:3043` | offload, else `writeToClient` — the fallback pattern | 4 |
| `handleClientsWithPendingWrites` — `src/networking.c:3264` | same fallback inside the flush loop | 4 |
| `postponeClientRead` — `src/networking.c:6408` | the read-side entry point | 4 |
| ignition thresholds — `src/io_threads.c:148-151`, `:171-179` | threads start when main-thread sys CPU > 30% | 4 |
| scaling decision — `src/io_threads.c:206-218` | ±1 thread by average queue depth | 4 |
| `updateIOThreads` — `src/io_threads.c:442` | runtime resize; refuses under load `:455-464` | 4 |
| `io_last_bufpos` — `src/io_threads.c:567-583` | the published watermark that replaces a lock | 5 |
| `io_read_state` / `io_write_state` — `src/io_threads.c:517`, `:553` | ownership as a state machine | 5 |
| `hashtablePrefetch` — `src/memory_prefetch.c:158` | round-robin one step per key | 6 |
| `getNextPrefetchInfo` / `moveToNextKey` — `:98`, `:87` | the cursor that makes it round-robin | 6 |
| `prefetchEntry` / `prefetchValue` — `:122`, `:136` | the two states; `valkey_prefetch` at `:141` | 6 |
| `prefetchCommands` — `:181` | argv pass, argv->ptr pass, then the tables at `:209-213` | 6 |
| `addCommandToBatchAndProcessIfFull` — `:263` | batch spans clients `:266` *and* pipelines `:275` | 6 |
| `prefetch-batch-max-size` (default 16) — `src/config.c:3379` | `io-threads` (default 1) at `:3375` | 4, 6 |

Suggested route: `io_threads.h` first — 45 lines, and the enums are the design
brief. Then `io_threads.c` lines 1-63 (the three queues and the tagging), then
`IOThreadMain`, then the two `trySend*` functions. Then `memory_prefetch.c` end
to end. Only then the `networking.c` call sites, to see how little they had to
change.

## What to steal

- **Pick the queue discipline per direction, not per project.** SPMC where one
  producer feeds many workers, MPSC coming back, SPSC only where the job must
  land on a named thread — and note that valkey switches between SPMC and SPSC
  for the *same* job type at a measured crossover of 9 threads
  (`io_threads.c:749`). In tokio terms you get the per-connection shape for
  free; the lesson bites when you add a worker pool for query execution.
- **Batch the handoff and the commit, not per-item signalling** — and let idle
  workers block on a mutex rather than spin. Redis 6's spinning io-threads are
  the counter-example the whole rewrite is arguing against.
- **A published watermark beats a lock** whenever one side only ever advances a
  bound. `io_last_bufpos` is the whole synchronization protocol for the write
  path.
- **Expose lookups as steppable state machines** if you want callers to be able
  to overlap their misses. `hashtableIncrementalFindStep` is what makes
  `hashtablePrefetch` possible; a lookup that can only be called to completion
  cannot be interleaved. For a graph store the analogue is a batched
  node/edge-attribute fetch that can be advanced one level at a time.
- **Prefetching only pays on predictable pointer chains with a batch to
  amortize over** — `:209` skips it entirely for a single key, and matrix
  kernels are already streaming, so they gain nothing.

## Questions to answer in notes.md

1. Valkey uses three queue disciplines. For each, say what would break (or
   merely get slower) if it were replaced by an MPMC queue, and why the poll job
   switches between two of them at 9 threads (`io_threads.c:749`).
2. Tagged job pointers: why smuggle the type into the low bits instead of a
   `struct { void *ptr; int type; }`? Do the cache-line arithmetic for
   `BATCH_SIZE = 32` both ways, and find the assertion that keeps the trick
   sound.
3. `io_last_bufpos`: construct the tearing bug that would exist if the I/O
   thread used `c->bufpos` instead. Then find the other place in this repo's
   topics where the same "publish a bound, don't take a lock" pattern appears.
4. Amdahl accounting for FalkorDB: estimate the parse+I/O share of a
   `GRAPH.QUERY` round trip. At what per-query cost does io-threading stop
   mattering? Cross-check your estimate against the ignition rule at
   `io_threads.c:177-179` — would your workload ever ignite the threads?
5. Why must the prefetch batch span multiple clients *and* each client's
   pipeline (`memory_prefetch.c:266`, `:275`)? What actually limits batch depth,
   and what does `prefetch-batch-max-size = 16` cost you if you set it to 128?
6. This chapter quotes 360K → 1.19M rps. Write down every condition under which
   that was measured, then state the number you would need to see before
   believing io-threads would help *your* server.

## Done when

Answer each before unfolding it.

- [ ] You can say exactly which work valkey moved off the main thread and which
      it refused to move, and name the file that enumerates it.

<details>
<summary>Answer</summary>

Moved: reading a client socket (including RESP parsing), writing a client
socket, freeing `argv`, freeing objects, running the poll (`epoll_wait`), and
accepting connections. That is the complete `JobRequest` enum at
`src/io_threads.h:6-14` — six types, and `_Static_assert(JOB_REQ_COUNT <= 8)` at
`:15` caps it at the 3-bit pointer tag budget.

Refused: **command execution**. The keyspace is still touched by exactly one
thread, which is what lets every hashtable/rax/listpack operation run without a
single lock and makes commands atomic by construction. Offloading the poll is
the one thing on this list redis does not do; the maintainers measured
`epoll_wait` at "more than 20 percent of the time" on the main thread.

</details>

- [ ] You can name valkey's three queues, their disciplines, their directions,
      and which one carries the actual read and write jobs.

<details>
<summary>Answer</summary>

From `io_threads.c:19-23`:

- `io_shared_inbox` — **SPMC**, main → any I/O thread. This is the one that
  carries `JOB_REQ_READ_CLIENT` and `JOB_REQ_WRITE_CLIENT` (`spmcEnqueue` at
  `:534`). Shared on purpose: any worker takes any client's job, so load
  balances itself.
- `io_shared_outbox` — **MPSC**, I/O threads → main. Results come back here via
  `sendToMainThread` (`:769`); when it is full the worker spills into a
  thread-local `pending_io_responses` list rather than blocking.
- `io_private_inbox[i]` — **SPSC**, main → thread `i`. Carries only work that
  must land on a *specific* thread: `JOB_REQ_FREE_ARGV` and `JOB_REQ_POLL`
  (`IOThreadMain:327-336`). Drained in batches of `BATCH_SIZE = 32`, because
  single-consumer ownership is what makes batch dequeue cheap.

The common summary "each thread gets its own SPSC queue" describes the *least*
used of the three.

</details>

- [ ] You can do the cache arithmetic for tagged job pointers and find the line
      that keeps the trick sound.

<details>
<summary>Answer</summary>

A `struct { void *ptr; int type; }` is 16 bytes after padding → 4 slots per
64-byte line → `BATCH_SIZE = 32` jobs span 8 lines. A tagged pointer is 8 bytes
→ 8 slots per line → 32 jobs span **4 lines**. Half the coherence traffic on the
most contended structure in the design, since every one of those lines migrates
from producer core to consumer core.

The trick works because `zmalloc` returns 8-byte-aligned pointers, so the low 3
bits are always zero (`io_threads.c:29-33`, `JOB_TAG_MASK 0x7`). Three bits is
eight types, and `_Static_assert(JOB_REQ_COUNT <= 8, ...)` at `io_threads.h:15`
is what stops a future contributor from silently adding a ninth and corrupting
every pointer in the queue.

</details>

- [ ] You can explain how the write path shares `c->reply` between two threads
      with no lock, and construct the bug that would exist without it.

<details>
<summary>Answer</summary>

Before enqueueing the write job, the main thread snapshots how far the worker
may go into `c->io_last_bufpos` (and `c->io_last_reply_block`), at
`io_threads.c:567-583`. The comment is explicit: the I/O thread writes "only up
to `io_last_bufpos`, **regardless of the `c->bufpos` value**".

Without it, the worker would read `c->bufpos` live. The main thread keeps
executing commands and appending replies while the worker runs, so the worker
could observe a `bufpos` that has advanced past bytes not yet written, or a
reply-list tail being mutated underneath it — a torn read, and on some
architectures a read of a stale cached line. The watermark makes the two threads
disjoint by *value*: the main thread owns "how much exists", the worker owns
"how much has been sent", and the release-store of the enqueue publishes
everything below the bound.

Same shape as topic 5's durable LSN and topic 8's snapshot timestamp: publish a
bound only one side advances, and you never need mutual exclusion.

</details>

- [ ] You can explain what `hashtablePrefetch` does differently from calling
      `lookupKey` n times, and why the hashtable needed a new API for it.

<details>
<summary>Answer</summary>

`lookupKey` n times runs n pointer chases *serially*: hash → bucket → entry →
value, where each load's address depends on the previous load's result, so the
CPU cannot start chase `n+1` before chase `n` finishes. n dependent DRAM misses,
paid one after another.

`hashtablePrefetch` (`memory_prefetch.c:158-168`) runs the same n chases
**round-robin, one step each**: `getNextPrefetchInfo` (`:98`) advances a cursor
modulo the batch, `prefetchEntry` (`:122`) performs exactly one
`hashtableIncrementalFindStep` and calls `moveToNextKey` (`:87`). While key A's
bucket line is in flight, key B's is issued. The chases are not shortened — they
are overlapped, so the batch pays roughly one DRAM latency instead of n.

The new API is `hashtableIncrementalFindInit` / `…Step` / `…GetResult`
(`:118`, `:123`, `:138`). A lookup you can only call to completion cannot be
interleaved; making the find a *resumable state machine* is the enabling change,
and that is the transferable lesson.

</details>

- [ ] You can quote the two published stages of the speedup with their
      conditions, and say what surfaced as the bottleneck after stage 1.

<details>
<summary>Answer</summary>

Stage 1, I/O threads alone: "reaching up to 780K SET commands per second"
(*Unlock 1 Million RPS*, part 2, § *Back to Valkey*). What surfaced underneath
was **not** command logic — profiling showed the main thread "spending more than
40% of its time in a single function: `lookupKey`". The bottleneck moved from
syscalls to DRAM.

Stage 2, memory-access amortization: prefetching "reduces the time spent on
`lookupKey` by more than 80%"; total impact "almost 50%", taking it "to more
than 1.19M rps". Check: 780K × 1.5 ≈ 1.17M.

Conditions on the headline (part 1, § *Major Upgrade to Valkey Performance*):
360K → 1.19M rps, "approximately 230%" increase, **against Valkey 7.2** — not
redis — on an AWS EC2 c7g.16xlarge, 8 I/O threads, 3M keys, 512-byte values, 650
clients, sequential SET, with average latency 1.792 ms → 0.542 ms. Part 2
reproduces on a c7g.4xlarge with `--io-threads 9`. Every one of those conditions
changes the number.

</details>

- [ ] You can say why an idle I/O thread costs nothing here and did not in redis
      6, and what that enables.

<details>
<summary>Answer</summary>

When both its queues are empty, an I/O thread blocks on `pthread_mutex_lock`
(`io_threads.c:378-385`) — a mutex the main thread holds while the thread should
be inactive. It consumes no CPU. Redis 6's io-threads busy-waited on a shared
list behind a spin fence, so an enabled-but-idle thread burned a core; that is
the main reason the feature was widely left off.

Costless idling is what makes the **adaptive pool** possible.
`active_io_threads_num` starts at 1 even when `io-threads` is 8
(`io_threads.c:497`); threads ignite only when the main thread's system CPU
exceeds 30%, or exceeds 5% while user CPU exceeds 50% (`:148-151`, `:177-179`);
after that the pool moves ±1 thread based on average queue depth (`:206-218`),
scaling down only after a cooldown. `io-threads` is therefore a ceiling (max 256,
`config.h:361`), not a thread count — and its default is 1
(`config.c:3375`, "Single threaded by default").

</details>

- [ ] You can state what happens when the offload cannot happen, and why that
      makes this design safe to adopt.

<details>
<summary>Answer</summary>

Every `trySend*` path has a same-thread fallback. `trySendReadToIOThreads`
returns `C_ERR` for ineligible clients (replicas, blocked, Lua-debug,
closing — each marked "for simplicity" in the source, `io_threads.c:519-525`)
and, if the queue is full, **rolls back every state change it made** before
returning `C_ERR` (`:534-539`). The enqueue is the commit point.

Callers then just do the work inline: `sendReplyToClient` falls through to
`writeToClient` (`networking.c:3043-3044`), `handleClientsWithPendingWrites`
does the same inside its loop (`:3264-3272`).

So with `io-threads 1` — the default — every path degenerates to the redis
behaviour, function for function, and no client is ever *dependent* on a worker
existing. Threads are an accelerator. That is what makes a change of this
blast radius shippable.

</details>

## References

**Primary sources** — the maintainers' own write-up, quoted by section heading:

- Uri Touitou and Alon Yagelnik, *Unlock 1 Million RPS: Experience Sharing with
  Amazon ElastiCache and Valkey* (valkey.io blog, 2024-08-05) — § *Major Upgrade
  to Valkey Performance* (360K → 1.19M rps, ~230%, vs Valkey 7.2, c7g.16xlarge,
  8 I/O threads, 3M keys, 512-byte values, 650 clients, sequential SET; latency
  1.792 → 0.542 ms) and § *High Level Design* (`epoll_wait` > 20% of main-thread
  time; at most one thread runs it at a time; I/O threads never sleep on epoll).
- *Unlock 1 Million RPS — Part 2* (valkey.io blog, 2024-09-13) — § *Speculative
  execution and linked lists* (16 × 10M-element lists: 20.8 s → under 2 s
  interleaved, "a 10x speedup", → 1.8 s with `__builtin_prefetch`; external
  memory ≈ 50× L1), § *Back to Valkey* (780K SET/s from I/O threads alone;
  `lookupKey` > 40% of main-thread time), § *Batching and interleaving*
  (prefetch cuts `lookupKey` time by > 80%; total impact "almost 50%", to
  > 1.19M rps; "All the relevant code can be found in `memory_prefetch.c`"), and
  the reproduce section (c7g.4xlarge, 16 aarch64 cores, `--io-threads 9`).

**Code at this repo's pin** — all `valkey-io/valkey@8891441ab`, verified with
`tools/pinned-source.py`:

- `src/io_threads.h` (45 lines) — the job enums and the two static asserts.
- `src/io_threads.c` (918 lines) — read in full.
- `src/memory_prefetch.c` (302 lines) — read in full; the file comment at `:6-9`
  states the whole idea.
- `src/networking.c` — the five call sites: `:2313`, `:3043`, `:3227`, `:3264`,
  `:6408`.
- `src/config.c:3375` (`io-threads`, default 1) and `:3379`
  (`prefetch-batch-max-size`, default 16, range 0-128);
  `src/config.h:361` (`IO_THREADS_MAX_NUM` 256).

**Measured in this repo:**

- [FINDINGS.md](../../FINDINGS.md) row 7 — 44k ops/s at P=1, 12.3M at P=256,
  **279×**, on identical zero-work requests. Full table in [notes.md](notes.md).
- [FINDINGS.md](../../FINDINGS.md) row 5 — `write()` at **857k/s** (1.17 µs), the
  syscall cost the handoff has to beat.
- [FINDINGS.md](../../FINDINGS.md) row 0 and
  [topic 0's notes.md](../00-performance-toolbox/notes.md) — `lookup_shootout`:
  9.3 ns per *independent* HashMap probe at n = 1e7 over ~160 MB, against a
  ~100 ns dependent-miss expectation. That gap is what Step 6 engineers on
  purpose.

**Corrections made to the previous version of this chapter:**

- "Valkey gives each io-thread its own private SPSC inbox … fed only by the main
  thread: N threads, N uncontended queues" described the *least* used of three
  queues. Reads and writes go through the **shared SPMC** `io_shared_inbox`
  (`:19`, enqueued at `:534`); results return through the **MPSC**
  `io_shared_outbox` (`:21`); the private SPSC queues (`:23`) carry only
  free-argv and poll jobs.
- `untagJob` was cited as `io_threads.c:333`. That is a *call site*; the
  definition is at **`:39-42`**, next to `tagJob` at `:35-37`.
- `spscDequeueBatch` was cited as `:320-321`; it is at **`:321`**, and
  `BATCH_SIZE` is **32** (`:152`).
- `PrefetchCommandsBatch` was described as a function at "`memory_prefetch.c:26-33`"
  that "walks all the chains level by level". It is a **struct** (`:26-39`); the
  walk is `hashtablePrefetch` (`:158-168`), and it is **round-robin one step per
  key**, not level-by-level, driven by `hashtableIncrementalFindStep` rather than
  by hand-rolled `__builtin_prefetch` on bucket addresses. There are two prefetch
  states, `PREFETCH_ENTRY` and `PREFETCH_VALUE` (`:15-19`), not four levels.
- "roughly doubled throughput", "command execution itself is only ~30%",
  "~1M+ ops/s/node, ~2-3× redis 7" and "uncontended SPSC push is ~10 ns" were
  unsourced. Replaced with the maintainers' published figures and their
  conditions (360K → 1.19M vs **Valkey 7.2**; 780K from I/O threads alone;
  `lookupKey` > 40%; prefetch > 80% of that; `epoll_wait` > 20%). The SPSC push
  cost is *not* replaced with a number, because neither the blog nor this repo
  has measured it — the guide now states only the two-orders-of-magnitude
  headroom against the 1.17 µs `write()` this repo did measure.
- "the ~1-2 µs syscall it offloads" — the only syscall cost this repo has
  measured is `write()` at 1.17 µs ([FINDINGS.md](../../FINDINGS.md) row 5).
- "topic 0's MLP finding (10 independent misses in flight ≈ 10× cheaper per
  miss)" was a paraphrase with an invented figure. The measured result is 9.3 ns
  per independent probe at n = 1e7 against a ~100 ns dependent expectation.
- Added, because the previous version omitted them entirely: the `io_last_bufpos`
  published-watermark protocol (Step 5), the adaptive ignition and scaling policy
  (Step 4), the SPMC-vs-SPSC crossover at 9 threads (`:749`), and the fact that
  idle threads park on a mutex rather than spin (`:377-386`).
- The unanchored Rust pseudocode has been removed in favour of the real
  `hashtablePrefetch` and `prefetchCommands`, quoted with line gutters.
- Removed: "Local clone at `~/repos/valkey`". There is no clone; use
  `tools/pinned-source.py`, which pins the commit these line numbers are true at.
- Note for readers of the blog: part 2's `dictPrefetch` over a chained
  `dictEntry` hash is `hashtablePrefetch` over an open-addressed `hashtable` at
  this pin. Same idea, different names and data structure.
