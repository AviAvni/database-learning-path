# C10K to thread-per-core: what is a server thread for?

Three readings spanning 1999→2024, one thread: *what should a server thread
be responsible for?* Kegel's C10K catalog explains why event loops became the
default, valkey's 8.0 posts show disciplined Amdahl analysis parallelizing
exactly the profiled majority, and Glauber Costa's thread-per-core essays
take the radical endpoint — share nothing between cores. This chapter builds
the concepts in historical order — what a connection-thread costs, the
polling problem, readiness notification, the async-I/O detour, and the two
modern answers — then tells you how to read each source. Together they span
the shared↔sharded plane M7 must position itself in.

## The problem in one sentence

In 1999, serving 10,000 concurrent connections with one thread each meant
10,000 stacks (~80 MB at 8 KB minimum each, far more at defaults) and a
scheduler drowning in context switches (~1–10 µs each) — and every design
since is a different answer to "what do we give a thread to do, if not one
connection?"

## The concepts, step by step

### Step 1 — the cost of thread-per-connection

The naive server design gives each connection its own thread, which blocks
on `read()` until that client sends something. Simple, and the costs are
per-connection whether the connection is active or idle: a stack (memory
reserved per thread — 10K mostly-idle connections still hold 10K stacks), a
kernel scheduling slot, and a **context switch** (the ~1–10 µs of saving one
thread's registers and loading another's, plus the cache/TLB state the new
thread finds cold) every time attention moves between clients. In 1999 this
died at ~10K connections; Kegel's page is the catalog of escape routes.

### Step 2 — select/poll: one thread, but O(n) per wakeup

The first escape: one thread watches *all* the sockets by handing the
kernel the full list of fds (file descriptors — the small integers naming
open sockets) via `select` or `poll`, sleeping until any is ready. That
fixes the 10K-stacks problem but adds a new tax: the fd list is passed and
scanned *on every call* — O(n) in registered connections, even if only 3
are ready. At 10K mostly-idle connections you burn the CPU scanning 10K
entries to find 3 events, thousands of times per second.

### Step 3 — readiness notification: epoll/kqueue, the line that won

**Readiness notification** means the kernel keeps the interest list
*between* calls: you register each fd once (`epoll_ctl`/`kevent`), then each
wait returns *only the ready fds* — O(ready), not O(registered). 10K idle
connections cost nothing per wakeup; 3 ready ones cost 3 dispatches. This
is the line that won: `ae.c` (reading-redis-ae-networking.md) is exactly
this plus a dispatch table, and every mainstream event loop (libuv, mio
under tokio) is the same shape.

### Step 4 — async I/O: stillborn, then resurrected as io_uring

The fourth entry in Kegel's menu was **asynchronous I/O**: instead of "tell
me when the fd is ready, then I'll call read()", submit the *operation
itself* ("read 16 KB from fd 7 into this buffer") and get completion
notification. POSIX aio was stillborn on Linux for sockets — but the idea
returned twenty years later as **io_uring**: two lock-free rings shared
with the kernel (submission queue in, completion queue out), so batches of
reads/writes/accepts cost near-zero syscalls. Question 4 asks what ae.c's
design becomes when poll+read+write turn into submission entries.

The full 1999 menu, with hindsight:

1. thread per connection — dies at ~10K in 1999 (stacks, context switches)
2. select/poll — O(n) scans of the fd set per wakeup
3. **readiness notification** (epoll/kqueue) — O(ready) not O(registered):
   this line wins; ae.c is exactly this + a dispatch table
4. async I/O (POSIX aio) — stillborn on Linux for sockets; the idea returns
   as io_uring twenty years later

### Step 5 — what changed since 1999, and the io-threads answer

Three assumptions expired: threads got cheaper (10K threads is fine now),
cores multiplied (one loop can't fill a 64-core box — the single-threaded
event loop went from solution to bottleneck), and NICs got faster than a
single core's syscall budget. So the question inverted: not "how does one
thread serve 10K sockets" but "how do 64 cores share one server".

Valkey's 8.0 answer is the conservative one: keep ONE execution thread
(zero locks in the data structures, commands atomic by construction) and
parallelize only I/O. The blog posts give the measured story: redis-6-style
io-threads (spin-waiting threads, main thread coordinating every batch)
gained modestly at high CPU burn; valkey 8's rework — SPSC handoff, threads
owning the whole read→parse and write path, main-thread prefetching for
batches — claims ~1M+ ops/s/node, ~2–3× redis 7 on the same box. The
reasoning to internalize: they **profiled first** — parse+syscall was the
majority of CPU at high op rates, commands themselves ~30% — and
parallelized exactly the majority and nothing else. Amdahl's law (speedup
is capped by the serial fraction), applied with discipline.

### Step 6 — thread-per-core: the shared-nothing endpoint

Glauber Costa's essays (Seastar/ScyllaDB, later Glommio) take the radical
position: don't share ANYTHING between cores. One reactor (event loop) per
core, connections pinned to cores, and the *data itself* **sharded by
core** — the keyspace is hash-partitioned, and a request for shard 7
arriving on core 2 is forwarded to core 7 as a message (cross-core SPSC
again), never accessed under a lock.

The trade: no locks ⇒ no lock contention and perfect cache locality — but
also no work stealing, so a hot shard is a hot core, and tail latency now
depends on your partitioning function. The Rust incarnation of the split:
Glommio (io_uring + thread-per-core executors) never moves a task between
cores (locality, pays imbalance); tokio's work-stealing runtime moves tasks
to idle workers (evens load, pays cross-core cache traffic).

```
        shared keyspace ◄──────────────────────► sharded keyspace
 redis/valkey: 1 exec thread     DragonflyDB/Scylla: N exec threads,
 + io threads, zero data locks   keyspace hash-partitioned per core,
                                 cross-shard ops = messages/transactions
```

This is the plane M7 positions itself in: shared↔sharded on one axis,
loop↔threads on the other.

## How to read the three resources (with the concepts in hand)

- **Kegel, "The C10K problem"** (kegel.com/c10k.html) — read as history
  that explains present defaults. Skim the I/O-strategy section against
  Steps 1–4's menu; skip the dated driver patches entirely.
- **Valkey's 8.0 blog series** (valkey.io/blog) — read *after*
  reading-valkey-iothreads.md; the posts supply the measurements behind
  Step 5. Watch for the profile-first discipline: which numbers justified
  parallelizing parse+I/O and nothing else.
- **Glauber Costa's thread-per-core essays** ("The reactor pattern is
  dead, long live the reactor"; the shard-per-core posts; later Glommio
  writing) — read for Step 6's position and its honest costs. Keep asking
  M7's question: what is the sharding unit for a *graph*?

## Questions to answer in notes.md

1. Which C10K strategy is tokio's multi-thread runtime? (Careful: it's
   readiness-based mio underneath + work-stealing tasks on top — two
   layers, name both.)
2. A graph database's unit of work is a *query* (ms-scale), not a GET
   (µs-scale). Redo valkey's Amdahl analysis for M7: what fraction of a
   GRAPH.QUERY round-trip is parse+I/O, and does ANY threading of the
   network layer matter? Where do the threads belong instead (M9)?
3. Thread-per-core for a graph: matrices don't hash-partition like a
   keyspace. What's the sharding unit — graph? subgraph? matrix tile? What
   does a BFS crossing shards cost in messages?
4. io_uring (the C10K "async I/O" line resurrected): what changes in ae.c's
   design if poll+read+write become submission-queue entries? (Topic 6's
   O_DIRECT thread rejoins here.)

## Done when

You can place redis, valkey 8, tokio, and DragonflyDB in the
shared↔sharded / loop↔threads plane and argue M7's position in it.

## References

**Papers**
- Dan Kegel — "The C10K problem" (kegel.com/c10k.html, 1999–2003) — skim
  the I/O-strategy section, skip the dated driver patches
- Valkey blog — the 8.0 performance/multithreading series
  (valkey.io/blog) — read after
  [reading-valkey-iothreads.md](reading-valkey-iothreads.md) for the
  measured story
- Glauber Costa — thread-per-core essays ("The reactor pattern is dead,
  long live the reactor"; the Seastar/ScyllaDB shard-per-core posts and
  later Glommio writing)
