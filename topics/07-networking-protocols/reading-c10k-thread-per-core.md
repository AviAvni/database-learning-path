# C10K to thread-per-core: what is a server thread for?

Three readings spanning 1999→2024, one question: *what should a server thread be
responsible for?* Dan Kegel's C10K page is the catalogue of answers that existed
when the question was new; valkey's 8.0 blog posts are a modern answer arrived at
by profiling; Glauber Costa's thread-per-core writing is the radical endpoint —
share nothing between cores. This chapter builds the concepts in the order the
industry found them, then checks each against code you can read: the event loop
FalkorDB and redis actually run, and the numbers this repo actually measured.

**Which versions this chapter is about.** Kegel's page is a living document with
a stale heartbeat: its RCS log ends at *Revision 1.212, 2006/09/02*, and the
hand-written changelog has one later entry, *2011/07/21 — Added nginx.org*. So it
is a 1999 essay revised through 2006, and anything it says about "current" kernels
is twenty years old. The valkey claims come from two dated posts, 2024-08-05 and
2024-09-13. The redis code is the repo's pin:

```sh
tools/pinned-source.py ref redis          # a176d1225
tools/pinned-source.py show redis src/ae.c -r 30:44
tools/pinned-source.py check redis src/config.h:86 --contains 'HAVE_EPOLL'
```

Every `file:line` below was re-checked against that pin. Where this chapter used
to state a number with no source, it now either cites one or says the number was
removed.

## The problem in one sentence

Kegel's arithmetic: a $1,200 machine of the day — 1000 MHz, 2 GB RAM, 1000
Mbit/s — divided by 20,000 clients leaves 50 kHz, 100 KB and 50 kbit/s each,
"so hardware is no longer the bottleneck"; the bottleneck is the *unit of work
you hand a thread*, and every design since 1999 is a different answer to "what
do we give a thread to do, if not one connection?"

(Check his division as you read — 1000 MHz / 20,000 = 50 kHz ✓, 2 GB / 20,000 =
100 KB ✓, 1000 Mbit/s / 20,000 = 50 kbit/s ✓, but $1,200 / 20,000 = **$0.06**,
not the $0.08 he prints. The argument survives the slip; the habit of checking
does not survive not checking.)

## The concepts, step by step

### Step 1 — what a thread costs when it owns a connection

> **In:** nothing but the naive design — one thread per connection, blocking
> `read()`.
> **Out:** the three costs that design pays per connection, one of them with
> Kegel's own arithmetic and one of them measured in this repo.

Three words first, because everything below is built from them.

A **syscall** is a call from your process into the kernel — `read`, `write`,
`epoll_wait`. It is not a function call: it traps into the kernel, which
validates arguments, may copy buffers across the user/kernel boundary, and may
put your thread to sleep. A **context switch** is the kernel taking one thread
off a core and putting another on: registers saved and restored, and — the part
that usually costs more — the cache and TLB state the incoming thread finds
cold. A **round trip** is one complete send-and-receive exchange between two
processes: request out, reply back, including both sides' syscalls and both
sides' wakeups.

Kegel's fourth strategy, *"Serve one client with each server thread"*, is the
naive design, and he prices it in virtual memory rather than in RAM:

> "Has the disadvantage of using a whole stack frame for each client, which
> costs memory. Many OS's also have trouble handling more than a few hundred
> threads. If each thread gets a 2MB stack (not an uncommon default value), you
> run out of *virtual memory* at (2^30 / 2^21) = 512 threads on a 32 bit machine
> with 1GB user-accessible VM."
> — Kegel, *I/O Strategies* § 4

That is the real 1999 constraint, and it is worth being precise about it: the
wall was **address space**, not physical memory. 2^30 bytes of user-accessible
virtual address space divided by a 2^21-byte stack is 512 threads, and the
stacks are mostly untouched — an idle connection's thread does not fault in 2 MB.
(An earlier version of this chapter said "10,000 stacks, ~80 MB at 8 KB minimum
each". That number had no source, and it also argues the wrong quantity. It is
gone; Kegel's own division replaces it.)

The second cost is the context switch, and this chapter no longer quotes a
per-switch figure — the "~1–10 µs" it used to print was unsourced. What this
repo *can* say is measured. Topic 7's own lane does nothing at all: a 32-byte
request, an 8-byte reply, no parsing, no store, over loopback. At pipeline depth
1 it runs at **44,088 ops/s, 22.68 µs per request**
([FINDINGS.md](../../FINDINGS.md) row 7; full table in [notes.md](notes.md)).
Every microsecond of that 22.68 is syscalls, wakeups and scheduling on a machine
with no network in it. That is the honest scale for "what attention costs when
it has to move between two runnable things".

The third cost is the one Kegel's page cannot show you because it was not yet a
problem: a thread that blocks in `read()` is *also* a thread whose work cannot be
batched with anyone else's. Hold that thought until Step 5.

### Step 2 — Kegel's five strategies, in his order and his words

> **In:** the per-connection costs from Step 1.
> **Out:** the actual 1999 menu — five strategies, not four, and in an order
> that is not the one this chapter used to print.

Kegel lists exactly five, under the heading *I/O Strategies*, introduced as "The
following five combinations seem to be popular":

1. **Serve many clients with each thread, and use nonblocking I/O and
   level-triggered readiness notification.** His sub-list: `select()`, `poll()`,
   `/dev/poll` (Solaris 2.7+), and **kqueue** (FreeBSD, NetBSD).
2. **Serve many clients with each thread, and use nonblocking I/O and readiness
   *change* notification.** His sub-list: **epoll** (Linux 2.6+), Polyakov's
   kevent, Drepper's proposal, realtime signals, signal-per-fd, and **kqueue
   again**.
3. **Serve many clients with each server thread, and use asynchronous I/O.**
4. **Serve one client with each server thread** (and use blocking I/O).
5. **Build the server code into the kernel.**

Two things about that list are worth more than the list itself.

**First, this chapter used to get the order wrong.** It said "the fourth entry in
Kegel's menu was asynchronous I/O". Async I/O is the *third*; the fourth is
thread-per-client. The old ordering — thread-per-connection, select/poll,
readiness notification, async I/O — was a retelling in historical order, not
Kegel's taxonomy, and presenting it as his was simply wrong. It is corrected
above.

**Second, kqueue appears in both 1 and 2, and that is the real lesson.**
Level-triggered against edge-triggered is a *mode* you select, not a property an
API has. Kegel defines both:

> "With this scheme, the kernel tells you whether a file descriptor is ready,
> whether or not you've done anything with that file descriptor since the last
> time the kernel told you about it."
> — § 1, on level-triggered
>
> "Readiness change notification (or edge-triggered readiness notification) means
> you give the kernel a file descriptor, and later, when that descriptor
> transitions from not ready to ready, the kernel notifies you somehow. It then
> assumes you know the file descriptor is ready, and will not send any more
> readiness notifications of that type for that file descriptor until you do
> something that causes the file descriptor to no longer be ready."
> — § 2

And the sentence that makes the whole family make sense, from § 1:

> "readiness notification from the kernel is only a hint; the file descriptor
> might not be ready anymore when you try to read from it."

Two vocabulary items fall out. A **file descriptor** (fd) is the small integer
the kernel gives you to name an open socket or file. **Readiness notification**
means the kernel tells you *when you may call `read`* and you then call it;
**completion notification** — Step 4 — means you hand the kernel the read itself
and it tells you when the bytes have landed. The first still costs you a syscall
per operation; the second does not.

What strategies 1 and 2 share, and what strategy 1's own `select`/`poll` do not,
is a kernel-side **interest list that survives between calls**. `select` and
`poll` take the whole fd array on every call, so each wakeup is O(registered):
10,000 idle connections cost 10,000 array entries copied and scanned to find the
3 that are ready. `epoll` and `kqueue` register once (`epoll_ctl`, `EV_SET` +
`kevent`) and each wait returns only the ready ones — O(ready). That is the line
that won, and every mainstream event loop is on it.

An **event loop** is the shape those APIs imply: a single thread that blocks in
one "which of my fds are ready?" call, dispatches a callback per ready fd, and
goes round again.

### Step 3 — which strategy the pinned redis code is (and on which machine)

> **In:** Kegel's five strategies from Step 2.
> **Out:** the exact strategy and the exact mode redis chose, read out of
> `src/ae.c` at the pin — including which backend file compiles on the machine
> that produced this topic's numbers.

`ae.c` picks its backend at compile time, in a nested `#ifdef` ladder with a
comment that states the intended ranking:

```c
// redis src/ae.c — the backend ladder, 30-44
    30  /* Include the best multiplexing layer supported by this system.
    31   * The following should be ordered by performances, descending. */
    32  #ifdef HAVE_EVPORT
    33  #include "ae_evport.c"
    34  #else
    35      #ifdef HAVE_EPOLL
    36      #include "ae_epoll.c"
    37      #else
    38          #ifdef HAVE_KQUEUE
    39          #include "ae_kqueue.c"
    40          #else
    41          #include "ae_select.c"
    42          #endif
    43      #endif
    44  #endif
```

Those three macros are set by platform, not by configuration:

```c
// redis src/config.h — "Test for polling API", 84-99 (accept4 test elided)
    84  /* Test for polling API */
    85  #ifdef __linux__
    86  #define HAVE_EPOLL 1
    87  #endif
   // ... 89-95: HAVE_ACCEPT4 ...
    97  #if (defined(__APPLE__) && defined(MAC_OS_10_6_DETECTED)) || defined(__FreeBSD__) || defined(__OpenBSD__) || defined (__NetBSD__)
    98  #define HAVE_KQUEUE 1
    99  #endif
```

Line 85 is the one to internalize before writing anything about `epoll` in this
repo: **`ae_epoll.c` is compiled only on Linux.** The measurements in
[notes.md](notes.md) were taken on an Apple M3 Pro, so the loop that produced
them was `ae_kqueue.c`. Anything this repo says about "how the event loop
behaves" is, on the reader's machine, a statement about kqueue.

Now the mode. Redis registers interest like this:

```c
// redis src/ae_kqueue.c — aeApiAddEvent, 102-111
   102  static int aeApiAddEvent(aeEventLoop *eventLoop, int fd, int mask) {
   103      aeApiState *state = eventLoop->apidata;
   104      struct kevent evs[2];
   105      int nch = 0;
   106
   107      if (mask & AE_READABLE) EV_SET(evs + nch++, fd, EVFILT_READ, EV_ADD, 0, 0, NULL);
   108      if (mask & AE_WRITABLE) EV_SET(evs + nch++, fd, EVFILT_WRITE, EV_ADD, 0, 0, NULL);
   109
   110      return kevent(state->kqfd, evs, nch, NULL, 0, NULL);
   111  }
```

The flag on lines 107–108 is `EV_ADD` and **not** `EV_CLEAR`. `EV_CLEAR` is
kqueue's edge-triggered switch; without it, kqueue is level-triggered. The Linux
file makes the same choice — `ae_epoll.c:62-67` builds `ee.events` out of
`EPOLLIN`/`EPOLLOUT` and never sets `EPOLLET`; grep the file for `EPOLLET` and
you get nothing. So:

> **Redis is Kegel's strategy 1 on both platforms — many clients per thread,
> nonblocking I/O, *level-triggered* readiness notification — not strategy 2.**

This chapter previously implied the opposite by calling Step 3 "readiness
notification: epoll/kqueue, the line that won" and describing strategy 2's
semantics. The family is right; the mode was wrong. Level-triggered is the
forgiving mode — Kegel again: edge-triggered "is a bit less forgiving of
programming mistakes, since if you miss just one event, the connection that event
was for gets stuck forever" — and redis, which reads a bounded 16 KB per wakeup
and comes back later for the rest, *depends* on being told again.

There is one more platform difference visible in the pinned code, and it is a
nice example of an abstraction leaking. `epoll` merges a fd's interests into one
registration (`mask |= eventLoop->events[fd].mask` at `ae_epoll.c:63`, one
`epoll_ctl` at `:67`) and returns one event per fd. kqueue registers up to two
kevents per fd (lines 104–108 above) and returns them *separately*, so `ae` has
to re-merge them:

```c
// redis src/ae_kqueue.c — aeApiPoll's merge pass, 142-157
   142          /* Normally we execute the read event first and then the write event.
   143           * When the barrier is set, we will do it reverse.
   144           *
   145           * However, under kqueue, read and write events would be separate
   146           * events, which would make it impossible to control the order of
   147           * reads and writes. So we store the event's mask we've got and merge
   148           * the same fd events later. */
   149          for (j = 0; j < retval; j++) {
   150              struct kevent *e = state->events+j;
   151              int fd = e->ident;
   152              int mask = 0;
   153
   154              if (e->filter == EVFILT_READ) mask = AE_READABLE;
   155              else if (e->filter == EVFILT_WRITE) mask = AE_WRITABLE;
   156              addEventMask(state->eventsMask, fd, mask);
   157          }
```

A second pass at `:162-170` walks the same array again, reads the merged mask
back out and clears it. Two O(ready) passes instead of one, on macOS only,
because redis wants to control read-before-write ordering (the `AE_BARRIER`
feature) and kqueue will not give it that for free.

### Step 4 — the async-I/O line: stillborn, then resurrected as io_uring

> **In:** readiness notification from Steps 2–3, which still costs one `read`
> syscall per ready socket.
> **Out:** the other half of the taxonomy — completion notification — why it
> failed in 1999, and what it does to an `ae.c`-shaped design when it returns.

Kegel's **third** strategy is the one that lost:

> "This has not yet become popular in Unix, probably because few operating
> systems support asynchronous I/O, also possibly because it (like nonblocking
> I/O) requires rethinking your application. Under standard Unix, asynchronous
> I/O is provided by the `aio_` interface […] AIO is normally used with
> edge-triggered completion notification, i.e. a signal is queued when the
> operation is complete."
> — § 3

Note his phrasing: **completion** notification, and it is orthogonal to
level/edge. You submit "read 16 KB from fd 7 into this buffer" and are told when
the bytes are *there*, rather than being told the fd is readable and then doing
the read yourself. POSIX aio never worked well for sockets on Linux, and the line
went quiet for twenty years.

**io_uring** is that line resurrected. Two shared ring buffers, mapped into both
the process and the kernel: a submission queue you push operations onto and a
completion queue the kernel pushes results onto. Because the rings are shared
memory, N operations can be submitted and N results collected with as few as one
syscall — or zero, in polled mode. The arithmetic from Step 1 is why anyone
cares: at P=1 the topic's lane spends 22.68 µs to move 40 bytes, and essentially
all of it is the crossing, not the copying.

What that does to an `ae.c`-shaped loop is question 4, and it is a real design
question rather than a rhetorical one: `aeApiPoll` returns *fds*, and every
handler then calls `read`/`write` itself. Under io_uring the loop would return
*finished operations*, so `readQueryFromClient` would stop being "the thing that
reads" and start being "the thing that runs after the read", and the buffer it
reads into would have to be pinned and owned by the kernel between submission and
completion. That is not a backend swap; it inverts who owns the buffer.

### Step 5 — the assumptions that expired, and the arithmetic of one loop

> **In:** the single-threaded event loop of Steps 2–4.
> **Out:** why one loop stopped being enough, worked two ways — a queueing
> calculation on stated assumptions, and valkey's profile-first answer with the
> numbers its authors published.

Three of Kegel's premises expired.

**Threads got cheaper.** Kegel saw it coming himself: "Perhaps in the
not-too-distant future, those who prefer using one thread per client will be able
to use that paradigm even for 10000 clients." His *Limits on threads* section
already puts Linux 2.6 + NPTL at "32000 or so threads" subject to
`/proc/sys/vm/max_map_count`, with the caveat that you need very small stacks
unless you are on a 64-bit processor — which everyone now is, which dissolves the
2^30-of-address-space wall from Step 1 entirely.

**Cores multiplied**, and this is the one that matters. A single event loop is
one thread, so it can use exactly one core, and the question inverted: not "how
does one thread serve 10,000 sockets" but "how do 64 cores share one server".

Work the saturation arithmetic before reading anyone's blog post. Model the loop
as one server in a queue. Suppose the per-request service time on the loop thread
— parse, execute, encode, the `read` and the `write` — is s = 1 µs. Then:

```
  service rate      µ  = 1/s          = 1 000 000 req/s   ← hard ceiling, one thread
  utilisation       ρ  = λ/µ
  M/M/1 wait        W  = s / (1 - ρ)

  λ = 500 000  ρ = 0.50   W = 1 µs / 0.50 =  2 µs
  λ = 800 000  ρ = 0.80   W = 1 µs / 0.20 =  5 µs
  λ = 950 000  ρ = 0.95   W = 1 µs / 0.05 = 20 µs
  λ = 990 000  ρ = 0.99   W = 1 µs / 0.01 = 100 µs
```

(M/M/1 — Poisson arrivals, exponential service, one server — is the wrong model
for a loop that batches, and it flatters nothing: it is a *lower* bound on how
badly the last 5% of capacity behaves. The shape is the point. Doubling the
arrival rate from 500k to 990k, still under the ceiling, multiplies waiting time
by fifty.)

Now put the measured numbers next to it. Topic 7's lane at P=1 spends 22.68 µs
per request, which by the same formula is a ceiling of 44,088 req/s — and it is
exactly the measured figure, because the lane's server does no work at all. That
is the whole content of the P=1 row: **when s is dominated by round trips, the
"service time" you are saturating a core with is not your code**. At P=256 the
same server does 12,321,414 ops/s (row 7 again), because 256 requests now share
one round trip. Batching moved the ceiling by 279× without making a single line
of the server faster.

**Valkey 8's answer** is the conservative one: keep one execution thread — no
locks in the data structures, commands atomic by construction — and parallelize
only I/O. The discipline worth copying is that they profiled first, and their
published profile is not what this chapter used to claim:

- "Socket polling system calls, such as `epoll_wait`, are expensive procedures.
  When executed solely by the main thread, `epoll_wait` consumes more than 20
  percent of the time." — part 1, § *High Level Design*
- After the I/O-thread rework alone, "we observed an increase in the number of
  requests per second, reaching up to 780K SET commands per second. Profiling the
  execution revealed that Valkey's main thread was spending more than 40% of its
  time in a single function: `lookupKey`." — part 2, § *Back to Valkey*
- Prefetching the dictionary chains for a whole batch "reduces the time spent on
  `lookupKey` by more than 80%"; the total impact of memory-access amortization
  is "almost 50%", taking it "to more than 1.19M rps". — part 2, § *Batching and
  interleaving*
- The headline: "Throughput increased by approximately 230%, rising from 360K to
  1.19M requests per second compared to **Valkey 7.2**. […] average latency
  decreasing by 69.8% from 1.792 ms to 0.542 ms." Measured with "8 I/O threads,
  3M keys DB size, 512 bytes value size, and 650 clients running sequential SET
  commands using AWS EC2 C7g.16xlarge". — part 1, § *Major Upgrade to Valkey
  Performance*

Three corrections come out of that list, and they are the reason to read sources
rather than summaries. This chapter used to say "commands themselves ~30%" — that
figure appears in neither post; the profiled figures are `epoll_wait` > 20% and
`lookupKey` > 40%, and both are *I/O and memory*, not command logic. It used to
say "~2–3× redis 7"; the comparison is against **valkey 7.2**, and it is
approximately 3.3× (1.19M / 360K), stated by its authors as ~230% *increase*. And
it used to attribute the whole gain to threading: 780K of it is threading, and the
step from 780K to 1.19M is the prefetcher, which is a memory-latency trick
(topic 0's territory) that only became possible *because* the I/O threads deliver
commands in batches.

Amdahl's law is the frame: speedup is capped by the fraction you do not
parallelize. Valkey measured the fraction first and parallelized exactly it.

### Step 6 — thread-per-core: the shared-nothing endpoint

> **In:** valkey's shared-keyspace answer from Step 5.
> **Out:** the opposite answer — shard the data itself by core — its two named
> implementations, and the specific costs it accepts.

**Thread-per-core** means one thread per CPU, usually pinned, with no thread pool
and no migration. Glauber Costa's definition, from the Glommio announcement:

> "Each core, or CPU, runs a single thread, and often (although not necessarily),
> each of these threads is pinned to a specific CPU. As the Operating System
> Scheduler cannot move these threads around, and there is never another thread
> in that same CPU, there are no context switches."
> — *Introducing Glommio, a thread-per-core crate for Rust and Linux*,
>   § *What is thread-per-core?*

That alone is not the win; the win needs **sharding**, and with it,
**shared-nothing** — an architecture in which no two threads touch the same
mutable state, so there is nothing to lock:

> "each of the threads in the thread-per-core application becomes responsible for
> a subset of the data […] Anything is possible, so long as two threads never
> share the responsibility of handling a particular request."
> — same article, § *Using Sharding*

Costa's own account of why locks disappear (same section) is worth reading twice:
sharding alone still needs a lock, because the OS can preempt a thread mid-update
and schedule another thread that touches the same shard. It is *thread-per-core
plus sharding* that removes the lock, because updates to two keys in one shard are
serialized by construction — they run on the same thread, one at a time.

The two named implementations both state it plainly. Seastar, the C++ framework
under ScyllaDB, leads its home page with "Shared-nothing design: Seastar uses a
shared-nothing model that shards all requests onto individual cores" and
"Message passing: A design for sharing information between CPU cores without
time-consuming locking". DragonflyDB's README says the same in redis terms: "we
use shared-nothing architecture, which allows us to partition the keyspace of the
memory store between threads so that each thread can manage its own slice of
dictionary data. We call these slices 'shards'" — and, for multi-key commands, it
cites the VLL paper: "The choice of shared-nothing architecture and VLL allowed us
to compose atomic multi-key operations without using mutexes or spinlocks."

The trade is real and this chapter should not soften it. No locks means no lock
contention and excellent cache locality; it also means **no work stealing**, so a
hot shard is a hot core and your tail latency is now a property of your
partitioning function. The contrast in Rust is exact: Glommio is "a Cooperative
Thread-per-Core crate for Rust & Linux based on `io_uring`" that "doesn't use
helper threads anywhere" (its README), while tokio's multi-thread runtime steals:

> "Each processor maintains its own run queue. Tasks that become runnable are
> pushed onto the current processor's run queue and processors drain their local
> run queue. However, when a processor becomes idle, it checks sibling processor
> run queues and attempts to steal from them. […] Under load, processors operate
> independently, avoiding synchronization overhead. In cases where the load is
> not evenly distributed across processors, the scheduler is able to
> redistribute."
> — tokio blog, *Making the Tokio Scheduler 10x Faster*, § *Work-stealing
>   scheduler*

Evens the load, pays cross-core synchronization. Costa's model never pays the
synchronization and never evens the load. Neither is free.

```
        shared keyspace ◄──────────────────────► sharded keyspace
 redis / valkey 8:               DragonflyDB / ScyllaDB:
 ONE execution thread,           N execution threads, keyspace
 N I/O threads,                  partitioned per core; multi-key
 zero data locks                 ops = VLL transactions, no mutexes
        ▲                                          ▲
   ae.c + io_threads.c                    Seastar / helio + io_uring
```

This is the plane M7 has to position itself in: shared↔sharded on one axis,
one-loop↔many-threads on the other. FalkorDB sits at the top-left — redis's model,
one execution thread, a graph as one keyspace entry — which is why its concurrency
story is module-level locking rather than partitioning.

## How to read the three resources (with the concepts in hand)

- **Kegel, "The C10K problem"** (kegel.com/c10k.html) — read the opening
  arithmetic and the five *I/O Strategies* sections, and nothing else. Check each
  strategy against Step 2's list, then skip *LinuxThreads*, *NGPT*, *NPTL*, the
  per-OS notes and the driver patches entirely: they are 2006 kernel trivia.
  *Limits on threads* is worth two minutes for the `max_map_count` note. Read it
  as an artefact — its "current" is twenty years stale, and noticing which of its
  premises expired (Step 5) is most of its value.
- **Valkey's 8.0 posts** — "Unlock 1 Million RPS: Experience Triple the Speed
  with Valkey" (2024-08-05) and its part 2 (2024-09-13), on valkey.io/blog. Read
  after [reading-valkey-iothreads.md](reading-valkey-iothreads.md), because the
  posts describe the code that guide reads. Copy down every number *with its
  configuration attached* — the 1.19M is 8 I/O threads on a c7g.16xlarge with
  512-byte values and 650 clients running sequential SET, and quoting it without
  that is how "~2–3× redis 7" got into this file.
- **Glauber Costa on thread-per-core** — the Glommio announcement on the Datadog
  engineering blog is the readable entry point; seastar.io's home page and
  DragonflyDB's README give the same design in two other codebases' words. Read
  for Step 6's position *and* its costs, and keep asking M7's question: what is
  the sharding unit for a *graph*?

## Questions to answer in notes.md

1. Which C10K strategy is tokio's multi-thread runtime? Careful — it is two
   layers, and they are different answers: name what mio does underneath and what
   the scheduler does on top.
2. Redis is level-triggered (Step 3). Sketch what would have to change in
   `readQueryFromClient` if `ae` registered with `EV_CLEAR`/`EPOLLET` instead.
   Which of redis's existing behaviours — the 16 KB bounded read especially —
   becomes a bug?
3. A graph database's unit of work is a *query* (ms-scale), not a GET (µs-scale).
   Redo valkey's Amdahl analysis for M7: if execution is 1 ms and parse+I/O is
   20 µs, what is the ceiling on any amount of network threading? Where do the
   threads belong instead (M9)?
4. Thread-per-core for a graph: matrices do not hash-partition like a keyspace.
   What is the sharding unit — graph, subgraph, matrix tile? What does one BFS
   frontier step crossing shards cost in messages?
5. io_uring (Step 4): what changes in `ae.c`'s design if poll+read+write become
   submission-queue entries? Who owns the read buffer between submission and
   completion, and what does that do to `querybuf` reallocation? (Topic 6's
   `O_DIRECT` thread rejoins here.)

## Done when

Answer each before unfolding it.

- [ ] You can list Kegel's five I/O strategies in his order, and say which one
      the pinned redis code implements.

  <details><summary>Answer</summary>

  (1) many clients per thread + nonblocking I/O + level-triggered readiness;
  (2) many clients per thread + nonblocking I/O + readiness *change*
  (edge-triggered) notification; (3) many clients per server thread +
  asynchronous I/O; (4) one client per server thread with blocking I/O;
  (5) server code in the kernel.

  Redis is **strategy 1** — level-triggered readiness — on both platforms.
  `ae_kqueue.c:107-108` registers with `EV_ADD` and no `EV_CLEAR`;
  `ae_epoll.c:62-67` builds `ee.events` from `EPOLLIN`/`EPOLLOUT` and never sets
  `EPOLLET`. Being in strategy 1 alongside `select` does not make it O(n): the
  interest list persists across calls, which is the property that matters, and
  which `select`/`poll` lack.

  </details>

- [ ] You can say which multiplexing backend compiled on the machine that
      produced this topic's measurements, and prove it from the source.

  <details><summary>Answer</summary>

  `ae_kqueue.c`. `config.h:85` guards `HAVE_EPOLL` with `#ifdef __linux__`, and
  `config.h:97` guards `HAVE_KQUEUE` with `__APPLE__ && MAC_OS_10_6_DETECTED` (or
  a BSD). The ladder at `ae.c:32-44` therefore falls through `HAVE_EVPORT` and
  `HAVE_EPOLL` to `#include "ae_kqueue.c"` on the Apple M3 Pro of
  [notes.md](notes.md). Consequence: on that machine `aeApiPoll` runs the
  two-pass read/write merge at `ae_kqueue.c:149-170`, which has no counterpart in
  the epoll backend, because kqueue delivers `EVFILT_READ` and `EVFILT_WRITE` as
  separate events.

  </details>

- [ ] You can explain readiness notification against completion notification
      without using the word "async", and say which syscalls each costs you.

  <details><summary>Answer</summary>

  Readiness: you tell the kernel which descriptors you care about; it tells you
  which are *ready*; you then perform the `read`/`write` yourself. Cost: one wait
  syscall per wakeup plus one I/O syscall per ready descriptor — and Kegel's
  warning applies, the readiness is "only a hint", the fd may not be ready by the
  time you act, so the fd must be nonblocking.

  Completion: you hand the kernel the operation *and its buffer*; it tells you
  when the bytes have moved. Cost: with io_uring's shared submission/completion
  rings, N operations can cost one syscall, or zero in polled mode. The price is
  ownership — the buffer belongs to the kernel until the completion arrives, so
  you cannot resize or free it in between.

  </details>

- [ ] Given a per-request service time, you can compute the arrival rate that
      saturates one event-loop thread, and say what happens just below it.

  <details><summary>Answer</summary>

  µ = 1/s. At s = 1 µs the ceiling is 1,000,000 req/s. Below it, waiting time
  grows as W = s/(1−ρ): 2 µs at λ=500k, 5 µs at λ=800k, 20 µs at λ=950k, 100 µs
  at λ=990k. The ceiling is not where the trouble starts — a 2× rise in arrivals
  from 500k to 990k is a 50× rise in queueing delay.

  Then the humbling version: this topic's lane at P=1 has s ≈ 22.68 µs, so its
  ceiling is 1/22.68 µs = 44,088 req/s — the measured number
  ([FINDINGS.md](../../FINDINGS.md) row 7) — for a server that executes *nothing*.
  Its service time is round trips. Batching at P=256 amortizes one round trip over
  256 requests and the same server reaches 12,321,414 ops/s.

  </details>

- [ ] You can state valkey 8.0's published numbers with the configuration
      attached, and say which part of the gain is not threading.

  <details><summary>Answer</summary>

  360K → 1.19M rps, ~230% increase, versus **valkey 7.2** (not redis 7): 8 I/O
  threads, 3M keys, 512-byte values, 650 clients, sequential SET, AWS EC2
  c7g.16xlarge; average latency 1.792 ms → 0.542 ms (−69.8%). Part 1,
  § *Major Upgrade to Valkey Performance*.

  The I/O-thread rework alone reached 780K (part 2, § *Back to Valkey*). The step
  from 780K to 1.19M is the **prefetcher**: `lookupKey` was over 40% of main-thread
  time, `dictPrefetch` interleaves the hash-chain walks for a whole batch and cuts
  it by more than 80%, worth "almost 50%" overall. That is a memory-latency win,
  not a concurrency win — and it is only available because the I/O threads hand
  over *batches*.

  </details>

- [ ] You can place redis, valkey 8, tokio and DragonflyDB on the
      shared↔sharded / one-loop↔many-threads plane, and argue M7's position.

  <details><summary>Answer</summary>

  Redis ≤5 and M7 v1: shared keyspace, one loop — everything serialized, no locks
  needed because there is one thread. Valkey 8: shared keyspace, one *execution*
  thread plus N I/O threads — commands still serialized, so still no data locks;
  I/O and parsing parallel. Tokio's multi-thread runtime: shared state, N
  work-stealing workers — so anything shared needs a lock or a channel, and tasks
  migrate between cores. DragonflyDB / ScyllaDB: sharded keyspace,
  thread-per-core, no cross-core locks at all; multi-key atomicity comes from a
  transaction protocol (VLL) instead of mutexes.

  M7's honest position is valkey's, one step back: one execution thread, because a
  graph is one keyspace entry and the executor is the expensive part. The
  thread-per-core question is deferred to a real answer for question 4 — what a
  graph shard *is* — and until that exists, sharding buys contention, not
  throughput.

  </details>

## References

**Primary sources (web documents, cited by section)**
- Dan Kegel — "The C10K problem", kegel.com/c10k.html. Written 1999; RCS log ends
  at *Revision 1.212, 2006/09/02*; last changelog entry *2011/07/21*. Sections
  used here: the opening cost arithmetic; *I/O Strategies* §§ 1–4; *Limits on
  threads*.
- Dan Touitou & Uri Yagelnik — "Unlock 1 Million RPS: Experience Triple the Speed
  with Valkey", valkey.io/blog, 2024-08-05. Sections used: *Major Upgrade to
  Valkey Performance* (360K → 1.19M, −69.8% latency, test configuration);
  *High Level Design* (`epoll_wait` > 20%, one poller at a time).
- Dan Touitou & Uri Yagelnik — "Unlock 1 Million RPS … part 2", valkey.io/blog,
  2024-09-13. Sections used: *Speculative execution and linked lists* (20.8 s →
  <2 s → 1.8 s on Graviton 3; external memory ≈ 50× L1); *Back to Valkey* (780K,
  `lookupKey` > 40%); *Batching and interleaving* (>80%, ~50%, 1.19M).
- Glauber Costa — "Introducing Glommio, a thread-per-core crate for Rust and
  Linux", Datadog engineering blog. Sections used: *What is thread-per-core?*,
  *Using Sharding*. (An earlier version of this chapter cited an essay titled
  "The reactor pattern is dead, long live the reactor". That title could not be
  verified against any primary source and has been removed.)
- Glommio README, github.com/DataDog/glommio — § *What is Glommio?*
- seastar.io home page — § *Shared-nothing design*, § *Message passing*.
- DragonflyDB README, github.com/dragonflydb/dragonfly — the shared-nothing and
  VLL paragraphs.
- "VLL: a lock manager redesign for main memory database systems" (VLDB Journal;
  DragonflyDB links `cs.umd.edu/~abadi/papers/vldbj-vll.pdf`) — the paper cited
  for multi-key atomicity without mutexes. Not read for this chapter; listed
  because Dragonfly's claim rests on it.
- tokio blog — "Making the Tokio Scheduler 10x Faster", § *Work-stealing
  scheduler*.

**Code, at this repo's pins** (`tools/pinned-source.py ref redis` → `a176d1225`)
- `src/ae.c:30-44` — the backend ladder.
- `src/config.h:84-99` — `HAVE_EPOLL` is `__linux__`; `HAVE_KQUEUE` is Apple/BSD.
- `src/ae_kqueue.c:102-111` — `EV_ADD`, no `EV_CLEAR`: level-triggered.
- `src/ae_kqueue.c:142-170` — the two-pass read/write merge kqueue forces.
- `src/ae_epoll.c:54-69` — one merged registration per fd; no `EPOLLET`.
- Read next: [reading-redis-ae-networking.md](reading-redis-ae-networking.md) for
  the loop itself, [reading-valkey-iothreads.md](reading-valkey-iothreads.md) for
  the code behind Step 5's numbers.

**Measured in this repo**
- [FINDINGS.md](../../FINDINGS.md) row 7 — 44,088 ops/s at P=1, 12,321,414 at
  P=256, 279×. Full table and method in [notes.md](notes.md); the lane is
  `experiments/src/bin/loopback_bench.rs`.
