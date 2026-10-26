# valkey io-threads: parallelize the majority, nothing else

Valkey 8 rewrote redis 6's io-threads and published a 3.3× throughput increase
— while commands still execute on one thread, with zero locks in the data
structures. Read it as a case study in *what you are allowed to parallelize
when you refuse to lock the keyspace*: three queue disciplines chosen per
direction, two priority lanes so control-plane traffic never waits behind
client traffic, a one-word job handoff, a published-watermark protocol that
replaces locking, an adaptive pool that turns itself on, and a prefetcher that
turns serial pointer chases into overlapping ones.

**Which version this chapter is about.** Every anchor below is
`valkey-io/valkey@668d875ca` (the `unstable` branch, 2026-10-24). The 2024 blog
posts that introduced the design describe an earlier shape of the code; where
the blog and the code disagree, this chapter says so. Confirm and read with:

```
$ tools/pinned-source.py ref valkey
valkey-io/valkey@668d875ca  via clone ~/repos/valkey

$ tools/pinned-source.py show valkey src/io_threads.c -r 1:96
$ tools/pinned-source.py check valkey src/io_threads.c:666 --contains 'trySendReadToIOThreads'
```

`src/io_threads.c` is 1,318 lines and `src/memory_prefetch.c` is 415 — read
both in full. You do not need a local clone.

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
enums are the exact list, and they are short:

```c
// valkey src/io_threads.h — the complete set of offloadable jobs and their results, 6-40
     6  /* Tag values for tagged pointers on work/response queues.
     7   * Tags must fit in 3 bits (0-7) due to 8-byte alignment from jemalloc
     8   * with --with-lg-quantum=3. SPSC and SPMC tags are separate enums
     9   * because they occupy independent queues and may reuse values. */
    10
    11  /* Tags for the SPSC private inbox (main thread → specific I/O thread). */
    12  typedef enum {
    13      JOB_SPSC_FREE_ARGV = 0,
    14      JOB_SPSC_POLL = 1,
    15  } JobRequestSPSC;
    16
    17  /* Tags for the SPMC shared inbox (main thread → any I/O thread). */
    18  typedef enum {
    19      JOB_REQ_READ_CLIENT = 0,
    20      JOB_REQ_WRITE_CLIENT,
    21      JOB_REQ_FREE_OBJ,
    22      JOB_REQ_POLL,
    23      JOB_REQ_ACCEPT,
    24      JOB_REQ_CLUSTER_READ,
    25      JOB_REQ_CLUSTER_WRITE,
    26      JOB_REQ_CLUSTER_ACCEPT,
    27      JOB_REQ_COUNT
    28  } JobRequestSPMC;
    29  static_assert(JOB_REQ_COUNT <= 8, "JOB_REQ_COUNT must not exceed 8 for pointer arithmetic");
    30
    31  /* Tags for the MPSC response queue (I/O threads → main thread). */
    32  typedef enum {
    33      JOB_RES_READ_CLIENT = 0,
    34      JOB_RES_WRITE_CLIENT,
    35      JOB_RES_CLUSTER_READ,
    36      JOB_RES_CLUSTER_WRITE,
    37      JOB_RES_CLUSTER_ACCEPT,
    38      JOB_RES_COUNT
    39  } JobResult;
    40  static_assert(JOB_RES_COUNT <= 8, "JOB_RES_COUNT must not exceed 8 for pointer arithmetic");
```

There are three enums because there are three queues (Step 2), and the comment
at lines 8-9 says why the tags may overlap: the enums "occupy independent
queues and may reuse values". Read them as a list of work:

- **`JobRequestSPSC`** (lines 12-15) — the two jobs that must land on one
  *named* thread: free an `argv` array (on the thread that allocated it) and
  run the poll when the pool is large.
- **`JobRequestSPMC`** (lines 18-28) — the eight jobs any thread may take: read
  a client (which includes RESP parsing), write a client, free an object, run
  the poll, accept a connection, and the three cluster-bus transport jobs —
  read, write and accept on a link between cluster nodes.
- **`JobResult`** (lines 32-39) — the five things a worker reports back.

Note what is *not* there: no "execute". The cluster jobs keep the same line.
`clusterReadJob` "read[s] bytes from a cluster link's connection, grow[s] the
receive buffer as needed, frame[s] packets, and post[s] a completion"
(`src/cluster_legacy.c:9289-9297`); the packets themselves are applied on the
main thread, which calls `clusterProcessPacket` for each complete one in
`clusterDrainCompletePackets` (`cluster_legacy.c:4866-4887`). Transport moves;
cluster-state mutation does not.

Note also `JOB_REQ_POLL` — valkey offloads the `epoll_wait` itself, which
redis does not. The maintainers' stated reason is that "when executed solely by
the main thread, `epoll_wait` consumes more than 20 percent of the time"
(*Unlock 1 Million RPS*, part 1, § *High Level Design*), with the discipline
that "at any given time, at most one thread, either an io_thread or the main
thread, executes `epoll_wait`".

Now the Amdahl accounting, because this is the step where people fool
themselves. **Amdahl's law** says speedup is capped by the fraction you *do
not* parallelize: if a share `s` of the work stays serial, the ceiling is `1/s`
no matter how many threads you add. Work it on the maintainers' own published
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

### Step 2 — three queues, three disciplines, two lanes

> **In:** Step 1's job list — work the main thread wants to give away — and N
> workers with results to give back.
> **Out:** a handoff whose cost is a few atomic operations, chosen per direction
> — because "which threads may touch this queue" is a different question in each
> direction, and the right answer is a different queue — split into a
> high-priority and a normal lane wherever the queue is shared.

Three disciplines need names first. A **SPSC** queue has a single producer and
a single consumer; **SPMC** has a single producer and many consumers; **MPSC**
has many producers and a single consumer. The fewer parties may touch an end,
the cheaper that end is, because an end only one thread touches needs no
compare-and-swap.

Most descriptions of valkey's I/O threads say "each thread gets its own SPSC
queue". That is one third of the truth, and it is the least important third.
The declarations are the whole map:

```c
// valkey src/io_threads.c — the queues and who may touch them, 39-49
    39  /* Backlog of responses when io_shared_outbox is full. Should be rare. */
    40  static _Thread_local list *pending_io_responses[JOB_PRIORITY_COUNT] = {NULL, NULL};
    41  static pthread_t io_threads[IO_THREADS_MAX_NUM] = {0};
    42  static pthread_mutex_t io_threads_mutex[IO_THREADS_MAX_NUM];
    43  static int cur_epoll_thread = 0;
    44  // Main -> IO: Shared Queue (Single Producer Multi Consumer) where all IO threads pull jobs from
    45  static spmcQueue io_shared_inbox[JOB_PRIORITY_COUNT] = {0};
    46  // IO -> Main: Response Channel (Multi Producer Single Consumer) used by IO threads to send results back to main-thread
    47  static mpscQueue io_shared_outbox[JOB_PRIORITY_COUNT] = {0};
    48  // Main -> IO (Thread-Specific) for tasks that must run on specific IO thread where IO threads check their private inbox before the shared queue
    49  static spscQueue io_private_inbox[IO_THREADS_MAX_NUM] = {0};
```

Read the three comments as three different answers to "who is allowed to touch
this?":

- **`io_shared_inbox` — SPMC.** One producer (the main thread), many consumers
  (every I/O thread). This is where the *actual work* goes: `JOB_REQ_READ_CLIENT`
  and `JOB_REQ_WRITE_CLIENT` are enqueued here (`io_threads.c:699`, `:762`). It
  is a shared queue on purpose: any thread may take any client's read, so a
  burst of work is load-balanced automatically with no scheduling decision by
  the main thread.
- **`io_shared_outbox` — MPSC.** Many producers (every I/O thread), one consumer
  (the main thread). Results come back here (`sendToMainThread`, `:1110-1128`).
  This one *must* tolerate concurrent producers, so it costs more — and when it
  is full, the producing thread spills into the thread-local
  `pending_io_responses` list (line 40, used at `:1122-1127`) rather than
  blocking.
- **`io_private_inbox[i]` — SPSC, one per thread.** One producer, one consumer,
  the cheapest discipline there is: the producer owns the head index, the
  consumer owns the tail, and no CAS loop is ever needed. These carry only
  Step 1's `JobRequestSPSC` jobs — `JOB_SPSC_FREE_ARGV` (free the memory on the
  thread that allocated it) and `JOB_SPSC_POLL`.

The two shared queues are *arrays*, indexed by a `jobPriority`. The enum's own
comment names what the high lane is for:

```c
// valkey src/io_threads.c — the two QoS lanes, 22-35
    22  /* QoS Swim Lanes for I/O threads
    23   * High priority queues: reserved for critical internal communication such as
    24   * cluster bus messages, slot migration, and replication streams
    25   * Normal priority queues: used for normal client connections
    26   */
    27  typedef enum {
    28      /* Normal priority jobs - used for normal client connections */
    29      JOB_PRIORITY_NORMAL = 0,
    30      /* High priority jobs - used for critical internal communication such as
    31       * cluster bus messages, slot migration, and replication streams */
    32      JOB_PRIORITY_HIGH,
    33      /* Number of priority levels */
    34      JOB_PRIORITY_COUNT
    35  } jobPriority;
```

A client's lane is a property of its connection — `getJobPriority` returns
`JOB_PRIORITY_HIGH` when `connIsPriority(c->conn)` (`io_threads.c:64-66`), a
flag set by `connSetPriority` (`src/connection.c:176`) from the replication,
slot-migration and cluster code — and every cluster-bus job goes to the high lane
unconditionally (`:832`, `:909`, `:941`). The point of a **QoS lane** (quality
of service: a separate path whose traffic is served first) is that a burst of
ten thousand client reads cannot sit in front of a cluster heartbeat.

The consumer loop makes the priority explicit, and the comments name the
disciplines:

```c
// valkey src/io_threads.c — IOThreadMain, 377-421 (private first, then high, then normal)
   377      while (1) {
// ... 378-387: cancellation point; account time spent since the last turn ...
   388          processed = 0;
   389          /* PRIORITY 1: Drain Private SPSC Queue (Batch Processing) */
   390          while ((batch_count = spscDequeueBatch(&io_private_inbox[id], batch_jobs, BATCH_SIZE)) > 0) {
   391              for (size_t i = 0; i < batch_count; i++) {
   392                  void *data;
   393                  int type;
   394                  untagJob(batch_jobs[i], &data, &type);
   395
   396                  switch (type) {
   397                  case JOB_SPSC_FREE_ARGV:
   398                      ioThreadFreeArgv((robj **)data);
   399                      break;
   400                  case JOB_SPSC_POLL:
   401                      ioThreadPoll((aeEventLoop *)data);
   402                      break;
   403                  default:
   404                      serverPanic("Invalid SPSC job type: %d", type);
   405                  }
   406              }
   407              processed += batch_count;
   408          }
   409
   410          /* PRIORITY 2: Shared Global Queue (SPMC)
   411           * Only checked after SPSC is drained. */
   412          void *tagged_job;
   413          if ((tagged_job = spmcDequeue(&io_shared_inbox[JOB_PRIORITY_HIGH])) != NULL) {
   414              processTaggedSPMCJob(tagged_job);
   415              processed++;
   416          }
   417
   418          if ((tagged_job = spmcDequeue(&io_shared_inbox[JOB_PRIORITY_NORMAL])) != NULL) {
   419              processTaggedSPMCJob(tagged_job);
   420              processed++;
   421          }
```

Each turn is: drain the private SPSC inbox completely (lines 390-408), then take
**one** job from the high lane (413-416) and **one** from the normal lane
(418-421). Note the asymmetry: the private queue is drained in **batches of
`BATCH_SIZE` = 32** (`:176`, `:374`, `:390`), the shared queues one job at a time
(`:413`, `:418`). Batch dequeue is exactly the SPSC discipline's payoff — the
consumer owns its index, so it can advance it 32 slots with one publish. The
shared queues cannot be drained that way without starving the other workers of
the same burst.

The return direction is prioritized the same way. The main thread empties the
high outbox before it touches the normal one, and checks for newly arrived
high-priority socket events in between:

```c
// valkey src/io_threads.c — processIOThreadsResponses, 1303-1317 (strict priority on the way back)
  1303      int total_processed = 0;
  1304      /* Loop until we consume all pending jobs */
  1305      while (1) {
  1306          /* 1. Strict Priority: First, drain high-priority events (cluster bus, replication, and slot migration jobs) */
  1307          int processed = processOutboxBatch(&io_shared_outbox[JOB_PRIORITY_HIGH]);
  1308          if (processed == 0) {
  1309              /* 2. Preemptive Poll: When high-priority outbox is empty, check if any new
  1310               * high-priority events arrived on QoS channels before processing normal traffic. */
  1311              aeProcessQoSEventsPreemptively(server.el);
  1312          }
  1313          /* 3. Drain normal client events */
  1314          processed += processOutboxBatch(&io_shared_outbox[JOB_PRIORITY_NORMAL]);
  1315          total_processed += processed;
  1316          if (processed == 0) return total_processed;
  1317      }
```

Line 1307 is the strict priority; line 1311 calls
`aeProcessQoSEventsPreemptively` (`src/ae.c:564`), which services the event
loop's high-priority descriptors if enough time has passed since they were last
polled, so a long run of normal completions cannot delay a cluster message that
arrived meanwhile.

And when a thread finds every queue empty it does not spin. It blocks on a
mutex the main thread holds:

```c
// valkey src/io_threads.c — IOThreadMain, 427-440 (park on a mutex, do not spin)
   427          /* If both queues were empty (no processing done), wait for signal. */
   428          if (processed == 0) {
   429              int has_pending = 0;
   430              for (int p = 0; p < JOB_PRIORITY_COUNT; p++) {
   431                  if (pending_io_responses[p]) has_pending = 1;
   432              }
   433              if (unlikely(has_pending)) {
   434                  flushPendingIOResponses(0);
   435              } else {
   436                  /* If it is locked. We should block until main thread unlocks it. */
   437                  pthread_mutex_lock(&io_threads_mutex[id]);
   438                  pthread_mutex_unlock(&io_threads_mutex[id]);
   439              }
   440          }
```

That is the single most important difference from redis 6's io-threads, which
had worker threads **busy-waiting** on a shared list with a spin fence: they
burned a core each while idle, which is why the feature was widely disabled in
production. Here an inactive thread costs nothing, which is what makes Step 4's
adaptive pool possible at all.

Which queue the *poll* job uses is decided by thread count, and the comment
states the trade-off outright:

```c
// valkey src/io_threads.c — trySendPollJobToIOThreads, 1089-1104 (SPMC or SPSC, by scale)
  1089      /* Use SPMC to minimize polling overhead. At high thread counts, use private SPSC queues for lower latency. */
  1090      if (server.active_io_threads_num <= 9) {
  1091          if (unlikely(spmcEnqueue(&io_shared_inbox[JOB_PRIORITY_NORMAL], tagJob(server.el, JOB_REQ_POLL)) == false)) {
  1092              server.io_poll_state = AE_IO_STATE_NONE;
  1093              aeSetPollProtect(server.el, 0);
  1094              return;
  1095          }
  1096      } else {
  1097          cur_epoll_thread = ((cur_epoll_thread) % (server.active_io_threads_num - 1)) + 1;
  1098          if (unlikely(spscIsFull(&io_private_inbox[cur_epoll_thread]))) {
  1099              server.io_poll_state = AE_IO_STATE_NONE;
  1100              aeSetPollProtect(server.el, 0);
  1101              return;
  1102          }
  1103          spscEnqueue(&io_private_inbox[cur_epoll_thread], tagJob(server.el, JOB_SPSC_POLL), true);
  1104      }
```

Up to nine active threads (line 1090), contention on the shared queue is cheaper
than the bookkeeping of round-robining private ones; from ten up, it is not.
That crossover is a *measured* engineering constant, not a principle — the
lesson to steal is that "SPSC is always better" is false, and which queue wins
depends on N. The poll always uses the normal lane (line 1091): it is the
client-traffic poll, and the high-priority descriptors have their own path
through line 1311 above.

### Step 3 — tagged pointers and batch commit: making the handoff nearly free

> **In:** a job from Step 2's queues, which is a (pointer, type) pair.
> **Out:** a single machine word in a queue slot, and a batch of them published
> with one release-store instead of one per job.

The handoff has to be cheaper than the work it offloads or the whole scheme
loses. Two micro-optimizations get it down to a few instructions.

**Tagged job pointers.** A job is one word — the pointer with its type smuggled
into the low bits that alignment guarantees are zero:

```c
// valkey src/io_threads.c — the tagged-pointer job encoding, 58-75
    58  /* Job Types for Tagged Pointers
    59   * We use the lower 3 bits of the pointer to store the job type.
    60   * Requires data pointers to be 8-byte aligned (standard for zmalloc/ptrs). */
    61  #define JOB_TAG_MASK 0x7
    62  #define JOB_PTR_MASK (~(uintptr_t)JOB_TAG_MASK)
// ... 63-67: getJobPriority (Step 2) ...
    68  static inline void *tagJob(void *ptr, int type) {
    69      return (void *)((uintptr_t)ptr | type);
    70  }
    71
    72  static inline void untagJob(void *tagged_ptr, void **ptr, int *type) {
    73      *type = (int)((uintptr_t)tagged_ptr & JOB_TAG_MASK);
    74      *ptr = (void *)((uintptr_t)tagged_ptr & JOB_PTR_MASK);
    75  }
```

Three bits is a budget of eight job types, and Step 1's
`static_assert(JOB_REQ_COUNT <= 8, ...)` (`io_threads.h:29`) is what stops
someone spending a ninth. Encoding is one `or`; decoding is two `and`s. This is
topic 2's bit-smuggling in its simplest form.

The budget is fully spent. `JobRequestSPMC` has exactly eight members (Step 1),
so `JOB_REQ_COUNT` is 8 and every 3-bit tag value from 0 to 7 is in use on the
shared queue. That is why the private-inbox tags are a separate enum: the
header says SPSC and SPMC tags "occupy independent queues and may reuse values"
(`io_threads.h:8-9`), so `JOB_SPSC_POLL = 1` and `JOB_REQ_WRITE_CLIENT = 1` never
meet. A ninth shared job type would need either another queue or a wider tag —
and a wider tag needs a stronger alignment guarantee than the 8 bytes the
header comment relies on (`io_threads.h:7-8`).

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
and publishes the batch with one commit. The free-argv path shows the deferred
enqueue, with the reason in its comment:

```c
// valkey src/io_threads.c — tryOffloadFreeArgvToIOThreads, 1019-1025 (enqueue now, publish later)
  1019      void *job = tagJob(argv, JOB_SPSC_FREE_ARGV);
  1020      /* We pass false to enqueue the job without committing the queue index immediately.
  1021       * This allows us to batch multiple free jobs together and
  1022       * commit them in a single operation later in the event loop. This reduces the overhead
  1023       * of memory barriers and cache line bouncing associated
  1024       * with updating the queue's write pointer per job. */
  1025      spscEnqueue(&io_private_inbox[target_id], job, false);
```

```c
// valkey src/io_threads.c — commitIOJobs, 92-96 (one publish per batch, per thread)
    92  void commitIOJobs(void) {
    93      for (int i = 1; i < server.active_io_threads_num; i++) {
    94          spscCommit(&io_private_inbox[i]);
    95      }
    96  }
```

Line 1025 passes `false`, so the job is written into the ring but the producer
index is not yet published. `spscCommit` (declared in `src/queues.h:138`,
included at `io_threads.c:14`) is that publish; line 94 is the call site — the
point where the main thread makes a whole batch of private-queue work visible
at once, from `IOThreadsBeforeSleep` (`:146`) just before the event loop sleeps.
This is the same shape as topic 5's group commit: the fence is the expensive
part, so amortize it over as many items as you can bear to delay.

Why it matters, in one line: the handoff must cost less than a syscall. This
repo measured `write()` at **1.17 µs** ([FINDINGS.md](../../FINDINGS.md) row 5).
A tagged-pointer enqueue into an uncontended SPSC ring is a store, an index
bump, and — once per batch — one release fence: tens of nanoseconds, not
microseconds. There is roughly two orders of magnitude of headroom, which is
why the design works at all. (This repo has not measured the enqueue cost
directly; if you want the number, that is an exercise, not a citation.)

### Step 4 — the offload decision: eligibility, a same-thread fallback, and when threads exist at all

> **In:** a client with a readable socket or pending replies, and Step 2's
> queues.
> **Out:** either a job on the shared inbox, or nothing at all — in which case
> the main thread does the work itself, exactly as redis would — plus the rule
> that decides how many threads are awake to take jobs.

The main thread decides per client, per event. `trySendReadToIOThreads`
(`io_threads.c:666`) is a wall of eligibility checks followed by one enqueue:

```c
// valkey src/io_threads.c — trySendReadToIOThreads, 666-710 (eligibility, then SPMC enqueue)
   666  int trySendReadToIOThreads(client *c) {
   667      if (server.active_io_threads_num <= 1) return C_ERR;
   668      /* Fake/teardown clients may have no connection; never offload those. */
   669      if (!c->conn) return C_ERR;
   670      /* If IO thread is still reading, return C_OK so the main thread does not race it. */
   671      if (c->io_read_state == CLIENT_PENDING_IO) return C_OK;
   672      /* A completed read must be finished by processClientIOReadsDone on the main thread
   673       * before we try to offload another read; do not treat it like PENDING_IO. */
   674      if (c->io_read_state == CLIENT_COMPLETED_IO) return C_ERR;
   675      if (c->io_write_state == CLIENT_PENDING_IO) return C_OK;
   676      /* For simplicity, don't offload replica clients reads as read traffic from replica is negligible */
   677      if (getClientType(c) == CLIENT_TYPE_REPLICA) return C_ERR;
   678      /* Keep the primary link on the main thread: readQueryFromClient drains multiple
   679       * full reads per event before serving other clients, and the IO-thread read path
   680       * does not decode the replication stream. */
   681      if (c->flag.primary) return C_ERR;
   682      /* With Lua debug client we may call connWrite directly in the main thread */
   683      if (c->flag.lua_debug) return C_ERR;
   684      /* For simplicity let the main-thread handle the blocked clients */
   685      if (c->flag.blocked || c->flag.unblocked) return C_ERR;
   686      if (c->flag.close_asap) return C_ERR;
   687      /* Avoid offloading reads to IO thread for the slot migration export job while snapshotting.
   688       * During this phase, the main thread writes snapshot data directly via connWrite(). */
   689      if (c->slot_migration_job && !clusterSlotMigrationShouldInstallWriteHandler(c)) return C_ERR;
// ... 690-697: stash parse/auth/replication flags on the client, mark it
//              CLIENT_PENDING_IO, postpone connection state updates ...
   698      jobPriority qidx = getJobPriority(c);
   699      if (unlikely(spmcEnqueue(&io_shared_inbox[qidx], tagJob(c, JOB_REQ_READ_CLIENT)) == false)) {
   700          c->read_flags = 0;
   701          c->io_read_state = CLIENT_IDLE;
   702          connSetPostponeUpdateState(c->conn, 0);
   703          return C_ERR;
   704      }
   705
   706      io_jobs_submitted++;
   707      server.stat_io_reads_pending++;
   708      c->flag.pending_read = 1;
   709      return C_OK;
   710  }
```

Most checks are *simplifications*, not correctness requirements — two say "for
simplicity" outright (lines 676, 684). Blocked clients, replicas and Lua-debug
clients are hard to reason about concurrently, so they are simply not
offloaded. The two that give a concrete reason are just as instructive: the
primary link stays on the main thread because the threaded read path "does not
decode the replication stream" (lines 678-681), and a slot-migration export job
stays there while the main thread is writing snapshot data directly (lines
687-689). That is the correct instinct for a change of this risk: shrink the
concurrent surface until you can hold it in your head.

Note lines 699-704: if the queue is full, the function **undoes its own state
changes** and returns `C_ERR`. Everything is reversible up to the enqueue, and
the enqueue is the commit point. Line 698 is where Step 2's lane is chosen.

The `C_ERR` matters because every call site has a same-thread fallback:

```c
// valkey src/networking.c — sendReplyToClient, 3575-3580 (offload, else do it here)
  3575  /* Write event handler. Just send data to the client. */
  3576  void sendReplyToClient(connection *conn) {
  3577      client *c = connGetPrivateData(conn);
  3578      if (trySendWriteToIOThreads(c) == C_OK) return;
  3579      writeToClient(c);
  3580  }
```

```c
// valkey src/networking.c — handleClientsWithPendingWrites, 3812-3826 (same shape, in the flush loop)
  3812          c->flag.pending_write = 0;
  3813          listUnlinkNode(server.clients_pending_write, ln);
  3814
  3815          if (!clientHasPendingReplies(c)) continue;
  3816
  3817          /* If we can send the client to the I/O thread, let it handle the write. */
  3818          if (trySendWriteToIOThreads(c) == C_OK) continue;
  3819
  3820          /* We can't write to the client while IO operation is in progress. */
  3821          if (c->io_write_state != CLIENT_IDLE) continue;
  3822
  3823          processed++;
  3824
  3825          /* Try to write buffers to the client socket. */
  3826          if (writeToClient(c) == C_ERR) continue;
```

Threads are an **accelerator, not a dependency**. Set `io-threads 1` — which is
the default, "Single threaded by default" (`config.c:3566`) — and every one of
these paths falls through to the redis behaviour the previous chapter of this
topic describes, function for function.

They are also *adaptive*. `active_io_threads_num` starts at 1 even when
`io-threads` is 8 (`io_threads.c:593`, "We start with threads not active"), and
threads ignite only when the main thread is visibly busy. Two definitions
first. **Active time** (`server.stat_active_time`, "Cumulative active time for
main thread in microseconds", `server.h:2078`) is the wall-clock time of every
event-loop iteration that did work (`server.c:2113-2119`) — iterations that
woke up and found nothing do not count. An **instantaneous metric** is valkey's
rolling rate: each sample is `Δvalue × factor / Δbase` (`server.c:958-969`), and
the reading is the mean of the last `STATS_METRIC_SAMPLES` = 16 samples
(`server.c:972-978`, `server.h:211`).

```c
// valkey src/io_threads.c — sampling main-thread active time while the pool is off, 159-165
   159      /* If threads are not active, track main-thread active time for ignition decision */
   160      if (server.active_io_threads_num == 1) {
   161          static long long last_measurement_time = 0;
   162          if (current_time - last_measurement_time < 50000) return; /* Sample once in 50ms */
   163          last_measurement_time = current_time;
   164          trackInstantaneousMetric(STATS_METRIC_MAIN_THREAD_ACTIVE_TIME, server.stat_active_time, current_time, 1000000);
   165      }
```

```c
// valkey src/io_threads.c — the ignition threshold and rule, 171-176 and 195-208
   171  /* Start using I/O threads when the main thread is active for more than the below
   172   * defined percentage of the time. This number is picked somewhat arbitrarily but
   173   * needed to be low enough to make sure we start the next thread quickly while not
   174   * starting too many threads unnecessarily to avoid contention. */
   175  #define IO_IGNITION_MAIN_THREAD_ACTIVE_PERCENT 30
   176  #define BATCH_SIZE 32
// ... 177-194: IOThreadsAfterSleep; the always-active debug policy short-circuits here ...
   195      /* Ignition Policy */
   196      if (server.active_io_threads_num == 1) {
   197          int should_ignite = 0;
   198          float main_thread_active_time = (float)getInstantaneousMetric(STATS_METRIC_MAIN_THREAD_ACTIVE_TIME) / 10000.0;
   199          /* Ignite IO threads when main-thread active time exceeds the threshold (30%) */
   200          should_ignite = (main_thread_active_time > (float)IO_IGNITION_MAIN_THREAD_ACTIVE_PERCENT);
   201          if (should_ignite) {
   202              pthread_mutex_unlock(&io_threads_mutex[1]);
   203              server.active_io_threads_num++;
   204              last_scale_time = now;
   205              serverLog(LL_DEBUG, "IO threads ignition: increased to %d", server.active_io_threads_num);
   206          }
   207          return;
   208      }
```

The line to focus on is 200. Work it once. Line 164 samples at most every 50 ms
(line 162) with `factor` = 1,000,000 and both values in microseconds, so a
50,000 µs window in which the main thread was active for 20,000 µs produces a
sample of `20,000 × 1,000,000 / 50,000 = 400,000`. If all 16 samples read
400,000, the mean is 400,000; line 198 divides by 10,000 to get **40.0**, and
`40.0 > 30` ignites one thread (lines 202-203). A window with 12,000 µs active
gives `240,000 / 10,000 = 24.0` — no ignition. "The main thread is active more
than 30% of the time" is Step 1's premise turned into a runtime test: *if the
one thread is busy, there may be I/O worth stealing* — whether that busy time
is spent in the kernel or in user space.

After ignition the pool scales by queue depth, one thread at a time:

```c
// valkey src/io_threads.c — the scaling decision, 214-236 (queue depth drives the pool)
   214      /* Scaling Up/Down Policy */
   215      if (now - last_sample_time < IO_SAMPLE_RATE_MS) return;
   216      last_sample_time = now;
   217
   218      spmc_size_sum += spmcSize(&io_shared_inbox[JOB_PRIORITY_NORMAL]);
   219      spmc_size_sum += spmcSize(&io_shared_inbox[JOB_PRIORITY_HIGH]);
   220      sample_count++;
   221
   222      trackInstantaneousMetric(STATS_METRIC_IO_WAIT, spmc_size_sum, sample_count, 1);
   223
   224      /* Decision (Every STATS_METRIC_SAMPLES Samples) */
   225      if (sample_count % STATS_METRIC_SAMPLES != 0) return;
   226
   227      size_t avg_q_size = getInstantaneousMetric(STATS_METRIC_IO_WAIT);
   228      size_t active = server.active_io_threads_num;
   229      size_t target = active;
   230
   231      /* Calculate Target */
   232      if (avg_q_size > 1 && active < (size_t)server.io_threads_num) {
   233          target++;
   234      } else if (avg_q_size == 0 && (now - last_scale_time > IO_COOLDOWN_MS)) {
   235          if (target > 1) target--;
   236      }
```

A sample is taken at most every `IO_SAMPLE_RATE_MS` = 10 ms (lines 169, 215)
and sums the depth of *both* inbox lanes (218-219); line 222 turns the running
sums into "average depth per sample since the last track", and a decision is
made every 16th sample (225) — so at least 160 ms apart. Scale up when the
average queue holds more than one job; scale down, after a 1,000 ms cooldown
(`IO_COOLDOWN_MS`, line 168), when it is empty. `io-threads` is a *ceiling*, not
a thread count (max `IO_THREADS_MAX_NUM` = 256, `config.h:383`), and it is
modifiable at runtime via `updateIOThreads` (`io_threads.c:533`), which refuses
while the pending responses would not fit in either outbox lane
(`:545-560`) — a deadlock this design has to actively avoid, and documents in
the comment at lines 545-549.

### Step 5 — the published watermark: how you share buffers without locking them

> **In:** a reply buffer the main thread is appending to and, from Step 4's
> write offload, an I/O thread about to write it to the socket.
> **Out:** a single snapshot value that tells the worker exactly how far it may
> read — and no lock anywhere.

This is the step the design's popular summaries skip, and it is where the real
concurrency reasoning lives. If commands run on the main thread and writes run
on an I/O thread, they are both touching `c->reply` and `c->buf`. What stops
them tearing?

Not a lock. A **watermark** — a bound published by one side and only ever
advanced by that side — snapshotted by the main thread *before* the job is
enqueued:

```c
// valkey src/io_threads.c — trySendWriteToIOThreads, 733-751 (snapshot how far the worker may go)
   733      int is_replica = getClientType(c) == CLIENT_TYPE_REPLICA;
   734      clientReplyBlock *block = NULL;
   735      if (is_replica) {
   736          c->io_last_reply_block = listLast(server.repl_buffer_blocks);
   737          replBufBlock *o = listNodeValue(c->io_last_reply_block);
   738          c->io_last_bufpos = o->used;
   739      } else {
   740          /* Save the last block of the reply list to io_last_reply_block and the used
   741           * position to io_last_bufpos. The I/O thread will write only up to
   742           * io_last_bufpos, regardless of the c->bufpos value. This is to prevent I/O
   743           * threads from reading data that might be invalid in their local CPU cache. */
   744          c->io_last_reply_block = listLast(c->reply);
   745          if (c->io_last_reply_block) {
   746              block = (clientReplyBlock *)listNodeValue(c->io_last_reply_block);
   747              c->io_last_bufpos = block->used;
   748          } else {
   749              c->io_last_bufpos = (size_t)c->bufpos;
   750          }
   751      }
```

Read the comment at lines 740-743 slowly. The worker writes "only up to
`io_last_bufpos`, **regardless of the `c->bufpos` value**". The main thread may
keep appending past that point while the worker is running; the worker will not
look, so it cannot observe a half-written byte range, and it does not need the
main thread's later writes to be visible to it at all. The bound was published
once, before the handoff, and the handoff's release-store is what makes
everything written before it visible. A replica client gets the same treatment
against the shared replication buffer instead of its own reply list (lines
735-738).

The other half of the contract sits right after the enqueue:

```c
// valkey src/io_threads.c — trySendWriteToIOThreads, 770-778 (stop extending what the worker reads)
   770      /* Force new header after successful enqueue so the main thread doesn't
   771       * extend a header the I/O thread is currently reading. */
   772      if (!is_replica) {
   773          if (block) {
   774              if (block->flag.buf_encoded) block->last_header = NULL;
   775          } else {
   776              if (c->flag.buf_encoded) c->last_header = NULL;
   777          }
   778      }
```

An encoded reply block carries a header the main thread may keep extending as
it appends. Lines 772-777 drop the main thread's pointer to that header, so the
next append starts a fresh one instead of rewriting bytes below the watermark
that the worker is reading. The watermark protects "how far"; this protects
"what is below it".

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
`io_threads.c:671-675` and `:719-720`, the asserts where results are collected
at `:1262` and `:1266`, and the assert in `processClientIOReadsDone`,
`networking.c:7105`). A client is owned by exactly one thread at a time, and the
state field is the token of ownership. Again: not a lock — an invariant
maintained by whose turn it is.

### Step 6 — the clever part: prefetching the batch's lookups

> **In:** a batch of parsed commands returned by the I/O threads (Step 2's
> outbox), and a main thread about to execute them one at a time.
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
// valkey src/memory_prefetch.c — hashtablePrefetch, 264-275 (round-robin, one step per key)
   264  static void hashtablePrefetch(hashtable **tables) {
   265      initBatchInfo(tables);
   266      KeyPrefetchInfo *info;
   267      while ((info = getNextPrefetchInfo())) {
   268          switch (info->state) {
   269          case PREFETCH_ENTRY: prefetchEntry(info); break;
   270          case PREFETCH_VALUE: prefetchValue(info); break;
   271          case PREFETCH_VALUE_NESTED: prefetchValueNested(info); break;
   272          default: serverPanic("Unknown prefetch state %d", info->state);
   273          }
   274      }
   275  }
```

`getNextPrefetchInfo` (`:113-121`) advances a cursor modulo the batch size and
returns the next key that is not `PREFETCH_DONE`; `prefetchEntry` (`:149-156`)
performs exactly **one** `hashtableIncrementalFindStep` and then calls
`moveToNextKey` (`:102-104`). So the loop's shape is:

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
`hashtableIncrementalFindInit` / `…Step` / `…GetResult` (`:137`, `:150`, `:161`)
— the hashtable exposes a *resumable* find precisely so a caller can interleave
several. That is the reusable design lesson: to get memory-level parallelism out
of a data structure, its lookup has to be expressible as a state machine you can
step, not a function you must call to completion.

The state machine goes one level deeper than the keyspace. Its states are:

```c
// valkey src/memory_prefetch.c — per-key states and the nested phases, 15-26
    15  typedef enum {
    16      PREFETCH_ENTRY,        /* Initial state, prefetch entries associated with the given key's hash */
    17      PREFETCH_VALUE,        /* prefetch the value object of the entry found in the previous step */
    18      PREFETCH_VALUE_NESTED, /* nested prefetch of inner hashtable for hash/zset types */
    19      PREFETCH_DONE          /* Indicates that prefetching for this key is complete */
    20  } PrefetchState;
    21
    22  typedef enum {
    23      NESTED_PREFETCH_INIT,  /* Init incremental find on inner hashtable */
    24      NESTED_PREFETCH_STEP,  /* Step through incremental find */
    25      NESTED_PREFETCH_VALUE, /* Prefetch the found entry's value (non-embedded only) */
    26  } NestedPrefetchPhase;
```

When the found value is itself a hashtable — a hash or set in its hashtable
encoding, or a sorted set's member index (`canNestedPrefetch`, `:143-146`) —
and the command names a member to look up, `prefetchValue` hands the key to the
nested path instead of finishing it:

```c
// valkey src/memory_prefetch.c — prefetchValue, 159-176 (finish the key, or go one level down)
   159  static void prefetchValue(KeyPrefetchInfo *info) {
   160      void *entry;
   161      if (hashtableIncrementalFindGetResult(&info->hashtab_state, &entry)) {
   162          robj *val = entry;
   163          if (canNestedPrefetch(info, val)) {
   164              valkey_prefetch(objectGetVal(val));
   165              info->state = PREFETCH_VALUE_NESTED;
   166              info->nested_phase = NESTED_PREFETCH_INIT;
   167              moveToNextKey();
   168              return;
   169          }
   170          if (server.io_threads_num < server.min_io_threads_copy_avoid && val->encoding == OBJ_ENCODING_RAW && val->type == OBJ_STRING) {
   171              valkey_prefetch(objectGetVal(val));
   172          }
   173      }
   174
   175      markKeyAsdone(info);
   176  }
```

Line 163 is the branch. `prefetchValueNested` (`:182-252`) then runs the same
three-call incremental find on the *inner* table — `NESTED_PREFETCH_INIT`
starts it (`:191-214`), `NESTED_PREFETCH_STEP` advances it one step per turn
(`:216-239`), and for a hash field whose value is not embedded in the entry,
`NESTED_PREFETCH_VALUE` prefetches the value too (`:241-249`). Every phase ends
in `moveToNextKey`, so the inner chases interleave with every other key's outer
chase. Which argument is "the member" is decided at build time: the command
table generator marks the first argument of type `field` or `member`
(`utils/generate-command-code.py:21`, `:415-421`) as `member_arg_index`, and
`addCommandToBatch` reads it (`memory_prefetch.c:357-358`) — but not for a key
the command will overwrite without reading (`:363-364`), where warming the old
value would be wasted work. One member per key keeps the batch bounded by
`prefetch-batch-max-size`. The nested phases work only because the inner
hashtable exposes the same resumable API — the design lesson again, applied a
second time.

Line 170 is the other branch worth reading: a plain string value is prefetched
only when `io-threads` is below the hidden `min-io-threads-avoid-copy-reply`
setting (default 7, `config.c:3567`). Above it, the reply references the value
instead of copying it, and the worker that writes it is the one that will touch
those bytes — so warming them in the main thread's cache would be wasted.

There are two more prefetch phases before the hashtable walk, and they are not
about keys at all:

```c
// valkey src/memory_prefetch.c — prefetchCommands, 288-321 (argv first, then the tables)
   288  static void prefetchCommands(void) {
   289      /* Prefetch argv's for all clients */
   290      for (size_t i = 0; i < batch->client_count; i++) {
   291          client *c = batch->clients[i];
   292          if (!c || c->argc <= 1) continue;
   293          /* Skip prefetching first argv (cmd name) it was already looked up by the I/O thread. */
   294          for (int j = 1; j < c->argc; j++) {
   295              valkey_prefetch(c->argv[j]);
   296          }
   297      }
   298
// ... 299-309: a second pass prefetching argv[j]->ptr for RAW-encoded objects ...
   310      /* Get the keys ptrs - we do it here after the key obj was prefetched. */
   311      for (size_t i = 0; i < batch->key_count; i++) {
   312          batch->keys[i] = objectGetVal((robj *)batch->keys[i]);
   313      }
   314
   315      /* Prefetch hashtable keys for all commands. Prefetching is beneficial only if there are more than one key. */
   316      if (batch->key_count > 1) {
   317          server.stat_total_prefetch_batches++;
   318          /* Prefetch keys from the main hashtable */
   319          hashtablePrefetch(batch->keys_tables);
   320      }
   321  }
```

The `argv` objects were allocated *on an I/O thread's core*, so they are cold in
the main thread's L1 — part 2 names this as a second problem it had to solve
with the same method. And line 316: with a single key there is nothing to
overlap with, so the whole mechanism is skipped. Prefetching is only ever worth
it in batches.

Finally, where the batch comes from — and this is the answer to the "why span
multiple clients?" question:

```c
// valkey src/memory_prefetch.c — addCommandToBatchAndProcessIfFull, 374-403 (batch across clients AND pipelines)
   374  int addCommandToBatchAndProcessIfFull(client *c) {
   375      if (!batch) return C_ERR;
   376
   377      batch->clients[batch->client_count++] = c;
   378
   379      /* Client's next command */
   380      if (c->parsed_cmd && !(c->read_flags & READ_FLAGS_BAD_ARITY)) {
   381          c->read_flags |= READ_FLAGS_PREFETCHED;
   382          addCommandToBatch(c->parsed_cmd, c->argv, c->argc, c->db, c->slot);
   383      }
   384
   385      /* Commands in the queue. */
   386      for (int j = c->cmd_queue.off; j < c->cmd_queue.len && batch->key_count < batch->max_prefetch_size; j++) {
// ... 387-392: skip errors and bad-arity commands; add each already-parsed pipelined command's keys ...
   393      }
   394
   395      /* If the batch is full, process it.
   396       * We also check the client count to handle cases where
   397       * no keys exist for the clients' commands. */
   398      if (batch->client_count == batch->max_prefetch_size || batch->key_count == batch->max_prefetch_size) {
   399          processClientsCommandsBatch();
   400      }
   401
   402      return C_OK;
   403  }
```

Both sources count: lines 377-383 add the client's next command, and lines
386-393 add everything already parsed in that client's *pipeline*.
`max_prefetch_size` is the `prefetch-batch-max-size` config, **default 16**,
range 0–128 (`config.c:3570`). So the batch fills from many clients *and* from
one client's depth — which is another way of saying that this topic's measured
279× pipelining result ([FINDINGS.md](../../FINDINGS.md) row 7) is not only
about syscalls. Depth also feeds the prefetcher. A client that pipelines gives
the server both fewer syscalls per command *and* a wider batch to overlap
misses across.

If you read the blog first, map its names: part 2 calls this function
`dictPrefetch` and describes a chained hash of `dictEntry`s. The code has an
open-addressed `hashtable`, and the function is `hashtablePrefetch`. The idea is
identical; the names and the data structure are not.

## Where each step lives in the code

Everything is `valkey-io/valkey@668d875ca`. Read `src/io_threads.c` (1,318
lines) and `src/memory_prefetch.c` (415 lines) in full; from `src/networking.c`
(7,377 lines) read only the call sites.

| Anchor | What | Step |
|--------|------|------|
| `JobRequestSPSC` / `JobRequestSPMC` / `JobResult` — `src/io_threads.h:12-39` | the complete list of what may be offloaded; the `<= 8` static asserts at `:29`, `:40` | 1, 3 |
| `clusterReadJob` — `src/cluster_legacy.c:9297`; `clusterDrainCompletePackets` — `:4866` | cluster transport on a worker; `clusterProcessPacket` on main (`:4887`) | 1 |
| `jobPriority` — `src/io_threads.c:27-35`; `getJobPriority` — `:64` | the two QoS lanes; lane chosen per connection | 2 |
| `io_shared_inbox[]` (SPMC) — `src/io_threads.c:45` | main → any thread; carries the *read and write* jobs | 2 |
| `io_shared_outbox[]` (MPSC) — `src/io_threads.c:47` | threads → main; `sendToMainThread` `:1110` | 2 |
| `io_private_inbox[]` (SPSC) — `src/io_threads.c:49` | main → *one specific* thread; free-argv and poll only | 2 |
| `IOThreadMain` — `src/io_threads.c:362` | private batch `:389-408`, high `:413`, normal `:418`, park `:427-440` | 2 |
| `processIOThreadsResponses` — `src/io_threads.c:1297` | high outbox first `:1307`, preemptive QoS poll `:1311` | 2 |
| `trySendPollJobToIOThreads` — `src/io_threads.c:1071` | SPMC up to 9 threads, SPSC above `:1090` | 2 |
| `BATCH_SIZE 32` — `src/io_threads.c:176` | `spscDequeueBatch` at `:390` | 2, 3 |
| `tagJob` / `untagJob` — `src/io_threads.c:68`, `:72` | 3-bit type in the pointer's low bits, `:58-62` | 3 |
| `commitIOJobs` — `src/io_threads.c:92` | `spscCommit` per thread at `:94`; deferred enqueue at `:1025`; the queue code is in `src/queues.h` | 3 |
| `trySendReadToIOThreads` — `src/io_threads.c:666` | eligibility wall, then `spmcEnqueue` at `:699`; full-queue rollback `:699-704` | 4 |
| `trySendWriteToIOThreads` — `src/io_threads.c:715` | same shape; the watermark snapshot at `:733-751` | 4, 5 |
| `sendReplyToClient` — `src/networking.c:3576` | offload, else `writeToClient` — the fallback pattern | 4 |
| `handleClientsWithPendingWrites` — `src/networking.c:3791` | same fallback inside the flush loop, `:3818` | 4 |
| `postponeClientRead` — `src/networking.c:7097` | the read-side entry point | 4 |
| ignition — `src/io_threads.c:159-165`, `:175`, `:195-208` | threads start when main-thread active time > 30% | 4 |
| scaling decision — `src/io_threads.c:214-236` | ±1 thread by average queue depth | 4 |
| `updateIOThreads` — `src/io_threads.c:533` | runtime resize; refuses under load `:545-560` | 4 |
| `io_last_bufpos` — `src/io_threads.c:733-751` | the published watermark that replaces a lock | 5 |
| `io_read_state` / `io_write_state` — `src/io_threads.c:671-675`, `:719-720` | ownership as a state machine | 5 |
| `hashtablePrefetch` — `src/memory_prefetch.c:264` | round-robin one step per key | 6 |
| `getNextPrefetchInfo` / `moveToNextKey` — `:113`, `:102` | the cursor that makes it round-robin | 6 |
| `prefetchEntry` / `prefetchValue` / `prefetchValueNested` — `:149`, `:159`, `:182` | the states; nested phases `:22-26` | 6 |
| `prefetchCommands` — `:288` | argv pass, argv->ptr pass, then the tables at `:316-320` | 6 |
| `addCommandToBatchAndProcessIfFull` — `:374` | batch spans clients `:377` *and* pipelines `:386` | 6 |
| `prefetch-batch-max-size` (default 16) — `src/config.c:3570` | `io-threads` (default 1) at `:3566` | 4, 6 |

Suggested route: `io_threads.h` first — 71 lines, and the enums are the design
brief. Then `io_threads.c` lines 1-96 (the lanes, the queues and the tagging),
then `IOThreadMain`, then the two `trySend*` functions and
`processIOThreadsResponses`. Then `memory_prefetch.c` end to end. Only then the
`networking.c` call sites, to see how little they had to change.

## What to steal

- **Pick the queue discipline per direction, not per project.** SPMC where one
  producer feeds many workers, MPSC coming back, SPSC only where the job must
  land on a named thread — and note that valkey switches between SPMC and SPSC
  for the *same* job type at a measured crossover of 9 threads
  (`io_threads.c:1090`). In tokio terms you get the per-connection shape for
  free; the lesson bites when you add a worker pool for query execution.
- **Give control-plane traffic its own lane, end to end.** A high-priority
  inbox is not enough on its own: valkey also drains the high outbox first and
  polls high-priority sockets between batches of normal completions
  (`io_threads.c:1307-1311`).
- **Batch the handoff and the commit, not per-item signalling** — and let idle
  workers block on a mutex rather than spin. Redis 6's spinning io-threads are
  the counter-example the whole rewrite is arguing against.
- **A published watermark beats a lock** whenever one side only ever advances a
  bound. `io_last_bufpos` is the whole synchronization protocol for the write
  path.
- **Expose lookups as steppable state machines** if you want callers to be able
  to overlap their misses. `hashtableIncrementalFindStep` is what makes
  `hashtablePrefetch` possible — on the keyspace and again on the inner table of
  a hash or sorted set; a lookup that can only be called to completion cannot be
  interleaved. For a graph store the analogue is a batched node/edge-attribute
  fetch that can be advanced one level at a time.
- **Prefetching only pays on predictable pointer chains with a batch to
  amortize over** — `memory_prefetch.c:316` skips it entirely for a single key,
  and matrix kernels are already streaming, so they gain nothing.

## Questions to answer in notes.md

1. Valkey uses three queue disciplines. For each, say what would break (or
   merely get slower) if it were replaced by an MPMC queue, and why the poll job
   switches between two of them above 9 threads (`io_threads.c:1090`).
2. Tagged job pointers: why smuggle the type into the low bits instead of a
   `struct { void *ptr; int type; }`? Do the cache-line arithmetic for
   `BATCH_SIZE = 32` both ways, and find the assertion that keeps the trick
   sound. The shared-queue tag budget is now fully spent — what are your two
   options for a ninth shared job type?
3. `io_last_bufpos`: construct the tearing bug that would exist if the I/O
   thread used `c->bufpos` instead. Then find the other place in this repo's
   topics where the same "publish a bound, don't take a lock" pattern appears.
4. Amdahl accounting for FalkorDB: estimate the parse+I/O share of a
   `GRAPH.QUERY` round trip. At what per-query cost does io-threading stop
   mattering? Cross-check your estimate against the ignition rule at
   `io_threads.c:195-200` — a FalkorDB main thread that is busy, but busy
   executing queries, *would* ignite the threads; what would they then have to
   do?
5. Why must the prefetch batch span multiple clients *and* each client's
   pipeline (`memory_prefetch.c:377`, `:386`)? What actually limits batch depth,
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
socket, freeing `argv`, freeing objects, running the poll (`epoll_wait`),
accepting connections, and the cluster bus's transport — reading, framing,
writing and accepting on links between nodes. The complete list is the three
enums in `src/io_threads.h:12-39`: `JobRequestSPSC` (free-argv and poll, for a
named thread), `JobRequestSPMC` (eight jobs any thread may take) and
`JobResult` (five results). `static_assert(JOB_REQ_COUNT <= 8)` at `:29` caps
the shared list at the 3-bit pointer-tag budget, and it is exactly full.

Refused: **command execution** — and for the cluster bus, applying packets:
`clusterProcessPacket` runs on the main thread from
`clusterDrainCompletePackets` (`cluster_legacy.c:4866-4887`). The keyspace is
still touched by exactly one thread, which is what lets every
hashtable/rax/listpack operation run without a single lock and makes commands
atomic by construction. Offloading the poll is something redis does not do; the
maintainers measured `epoll_wait` at "more than 20 percent of the time" on the
main thread.

</details>

- [ ] You can name valkey's three queues, their disciplines, their directions,
      which one carries the actual read and write jobs, and how priority splits
      them.

<details>
<summary>Answer</summary>

From `io_threads.c:45-49`:

- `io_shared_inbox[2]` — **SPMC**, main → any I/O thread. This is the one that
  carries `JOB_REQ_READ_CLIENT` and `JOB_REQ_WRITE_CLIENT` (`spmcEnqueue` at
  `:699` and `:762`). Shared on purpose: any worker takes any client's job, so
  load balances itself.
- `io_shared_outbox[2]` — **MPSC**, I/O threads → main. Results come back here
  via `sendToMainThread` (`:1110`); when it is full the worker spills into a
  thread-local `pending_io_responses` list rather than blocking.
- `io_private_inbox[i]` — **SPSC**, main → thread `i`. Carries only work that
  must land on a *specific* thread: `JOB_SPSC_FREE_ARGV` and `JOB_SPSC_POLL`
  (`IOThreadMain`, `:396-405`). Drained in batches of `BATCH_SIZE = 32`, because
  single-consumer ownership is what makes batch dequeue cheap.

Both shared queues come in two lanes, `JOB_PRIORITY_HIGH` and
`JOB_PRIORITY_NORMAL` (`:27-35`). Priority connections (replication, slot
migration) and all cluster-bus jobs use the high lane; each worker turn takes
one high job before one normal job (`:413`, `:418`), and the main thread drains
the high outbox before the normal one (`:1307`, `:1314`).

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

The trick works because data pointers are 8-byte aligned (the header comment at
`io_threads.h:7-8` cites jemalloc's 8-byte quantum), so the low 3 bits are
always zero (`io_threads.c:58-62`, `JOB_TAG_MASK 0x7`). Three bits is eight
types, and `static_assert(JOB_REQ_COUNT <= 8, ...)` at `io_threads.h:29` is what
stops a future contributor from silently adding a ninth and corrupting every
pointer in the queue. With eight shared job types the budget is exactly spent;
the private-inbox tags live in a separate enum because they travel on a
different queue and may reuse values.

</details>

- [ ] You can explain how the write path shares `c->reply` between two threads
      with no lock, and construct the bug that would exist without it.

<details>
<summary>Answer</summary>

Before enqueueing the write job, the main thread snapshots how far the worker
may go into `c->io_last_bufpos` (and `c->io_last_reply_block`), at
`io_threads.c:733-751`. The comment is explicit: the I/O thread writes "only up
to `io_last_bufpos`, **regardless of the `c->bufpos` value**". After a
successful enqueue it also drops its pointer to an encoded block's header
(`:770-778`), so later appends start a new header instead of rewriting one the
worker is reading.

Without the watermark, the worker would read `c->bufpos` live. The main thread
keeps executing commands and appending replies while the worker runs, so the
worker could observe a `bufpos` that has advanced past bytes not yet written,
or a reply-list tail being mutated underneath it — a torn read, and on some
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

`hashtablePrefetch` (`memory_prefetch.c:264-275`) runs the same n chases
**round-robin, one step each**: `getNextPrefetchInfo` (`:113`) advances a cursor
modulo the batch, `prefetchEntry` (`:149`) performs exactly one
`hashtableIncrementalFindStep` and calls `moveToNextKey` (`:102`). While key A's
bucket line is in flight, key B's is issued. The chases are not shortened — they
are overlapped, so the batch pays roughly one DRAM latency instead of n. For a
hash, set or sorted set whose command names a member, the same stepping
continues into the value's inner hashtable (`prefetchValueNested`, `:182`).

The new API is `hashtableIncrementalFindInit` / `…Step` / `…GetResult`
(`:137`, `:150`, `:161`). A lookup you can only call to completion cannot be
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

When all its queues are empty, an I/O thread blocks on `pthread_mutex_lock`
(`io_threads.c:437-438`) — a mutex the main thread holds while the thread should
be inactive. It consumes no CPU. Redis 6's io-threads busy-waited on a shared
list behind a spin fence, so an enabled-but-idle thread burned a core; that is
the main reason the feature was widely left off.

Costless idling is what makes the **adaptive pool** possible.
`active_io_threads_num` starts at 1 even when `io-threads` is 8
(`io_threads.c:593`). While only the main thread runs, it samples its own
active time every 50 ms (`:159-165`); the first worker ignites when the 16-sample
mean exceeds 30% (`IO_IGNITION_MAIN_THREAD_ACTIVE_PERCENT`, `:175`, tested at
`:200`) — e.g. 20 ms active in each 50 ms window reads as 40.0 and ignites, 12 ms
reads as 24.0 and does not. After that the pool moves ±1 thread based on the
average depth of both inbox lanes (`:214-236`), at most once per 16 samples of
≥ 10 ms, scaling down only after a 1 s cooldown. `io-threads` is therefore a
ceiling (max 256, `config.h:383`), not a thread count — and its default is 1
(`config.c:3566`, "Single threaded by default").

</details>

- [ ] You can state what happens when the offload cannot happen, and why that
      makes this design safe to adopt.

<details>
<summary>Answer</summary>

Every `trySend*` path has a same-thread fallback. `trySendReadToIOThreads`
returns `C_ERR` for ineligible clients (replicas, the primary link, blocked,
Lua-debug, closing, a snapshotting slot-migration export —
`io_threads.c:676-689`, two of them marked "for simplicity") and, if the queue
is full, **rolls back every state change it made** before returning `C_ERR`
(`:699-704`). The enqueue is the commit point.

Callers then just do the work inline: `sendReplyToClient` falls through to
`writeToClient` (`networking.c:3578-3579`), `handleClientsWithPendingWrites`
does the same inside its loop (`:3818-3826`).

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
  the reproduce section (c7g.4xlarge, 16 aarch64 cores, `--io-threads 9`). The
  blog's `dictPrefetch` over a chained `dictEntry` hash is `hashtablePrefetch`
  over an open-addressed `hashtable` in the code.

**Code** — all `valkey-io/valkey@668d875ca` (`unstable`, 2026-10-24), verified
with `tools/pinned-source.py`:

- `src/io_threads.h` (71 lines) — the three job enums and the two static
  asserts.
- `src/io_threads.c` (1,318 lines) — read in full.
- `src/memory_prefetch.c` (415 lines) — read in full; the file comment at
  `:6-9` states the whole idea.
- `src/networking.c` — the five call sites: `:2656`, `:3578`, `:3781`, `:3818`,
  `:7100`.
- `src/cluster_legacy.c:9289-9297` (`clusterReadJob`, the worker side of the
  cluster bus) and `:4866-4887` (`clusterDrainCompletePackets`, the main-thread
  side).
- `src/ae.c:564` (`aeProcessQoSEventsPreemptively`); `src/connection.c:176`
  (`connSetPriority`).
- `src/config.c:3566` (`io-threads`, default 1), `:3567`
  (`min-io-threads-avoid-copy-reply`, default 7) and `:3570`
  (`prefetch-batch-max-size`, default 16, range 0-128); `src/config.h:383`
  (`IO_THREADS_MAX_NUM` 256).

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
