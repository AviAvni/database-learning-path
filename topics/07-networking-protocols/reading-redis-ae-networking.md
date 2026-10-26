# The redis event loop: pipelining for free

One thread, one poll syscall per iteration, and two buffering decisions —
parse a batch of commands out of whatever the read buffer holds, and write
nothing until the top of the next loop turn — give redis pipelining and reply
batching without any dedicated machinery. This chapter builds the loop step by
step: what an event loop even is, why the handler table is an array, how the
read path turns one syscall into a batch of command executions, the arithmetic
that turns 44k ops/s into 12.3M, why the parser is resumable, why replies are
hoarded, and where the whole thing kills a client.

**Which version this chapter is about.** Every anchor below is
`redis/redis@e87dda2a4` (`unstable`, 2026-10-26), which is what
`tools/pinned-source.py` will hand you:

```
$ tools/pinned-source.py ref redis
redis/redis@e87dda2a4

$ tools/pinned-source.py show redis src/ae.c -r 365:425
$ tools/pinned-source.py check redis src/networking.c:3060 --contains 'handleClientsWithPendingWrites'
```

You do **not** need a local clone. If you have one and its numbers disagree,
your clone is at a different commit — this file's line numbers are only true at
`e87dda2a4`. `src/ae.c` is 516 lines, so read it end to end (a rare luxury in
this repo). `src/networking.c` is 6,146 lines, so read only the functions this
chapter walks.

## The problem in one sentence

One redis thread must serve ten thousand concurrent connections at a million
operations per second, which leaves roughly one microsecond of CPU per
command — and this repo's own topic-5 lane measured a single `write()` syscall
at 857k/s, i.e. **1.17 µs each** ([FINDINGS.md](../../FINDINGS.md) row 5) — so
the naive two-syscalls-per-command design is already 2.3× over budget before it
parses a byte, and the entire architecture is an argument about how to get the
syscall count per command *below one*.

## The concepts, step by step

### Step 1 — the event loop: one thread, one poll syscall, many sockets

> **In:** a set of open sockets, most of them idle, and one thread.
> **Out:** the subset that has data waiting right now, plus a call to the
> handler registered for each — in O(ready) work, not O(registered).

A **syscall** is a call into the kernel: the CPU switches privilege level,
saves and restores register state, and runs kernel code on your thread's
behalf. It is not a function call; it costs on the order of a microsecond
(measured: 1.17 µs for `write()`, [FINDINGS.md](../../FINDINGS.md) row 5).
Every syscall in the hot path is a tax you pay per command unless you can
amortize it over several.

A **file descriptor** (fd) is the small non-negative integer the kernel hands
you to name an open socket, file or pipe. They are allocated from the lowest
free slot, so a process with 10,000 open sockets has fds roughly in the range
0..10,050 — dense, not sparse. That fact drives Step 2.

An **event loop** is a single thread that, instead of dedicating itself to one
connection and blocking on it, repeatedly asks the kernel "which of my
descriptors are ready?" and services exactly those. The asking is one syscall.
This is **readiness notification**: the kernel tells you a socket *can* be read
without blocking, and you then do the read yourself. (The alternative,
**completion notification**, has you submit the read up front and the kernel
tells you when the bytes have landed — that is `io_uring` and IOCP, and the
c10k chapter of this topic works through why the difference matters.)

`aeProcessEvents` is one turn of that loop:

```c
// redis src/ae.c — aeProcessEvents, 365-418 (the shape of one loop turn)
   365  int aeProcessEvents(aeEventLoop *eventLoop, int flags)
   366  {
   367      int processed = 0, numevents;
// ... 368-381: early-out when neither file nor time events were requested;
//              declare the poll timeout ...
   382          if (eventLoop->beforesleep != NULL && (flags & AE_CALL_BEFORE_SLEEP))
   383              eventLoop->beforesleep(eventLoop);
// ... 384-400: tvp = 0 if AE_DONT_WAIT, else time until the earliest timer ...
   401          /* Call the multiplexing API, will return only on timeout or when
   402           * some event fires. */
   403          numevents = aeApiPoll(eventLoop, tvp);
// ... 404-413: zero numevents if file events were not requested; aftersleep ...
   414          for (j = 0; j < numevents; j++) {
   415              int fd = eventLoop->fired[j].fd;
   416              aeFileEvent *fe = &eventLoop->events[fd];
   417              int mask = eventLoop->fired[j].mask;
   418              int fired = 0; /* Number of events fired for current fd. */
```

Three things to notice, in order of how much they will surprise you:

1. **`beforesleep` runs *before* the poll, not after the dispatch** (`:382-383`).
   The name is accurate — it is the last thing that happens before the thread
   goes to sleep in `aeApiPoll`. This is where the entire write path lives
   (Step 6). It is wired up once, at `src/server.c:3274`
   (`aeSetBeforeSleepProc(server.el,beforeSleep)`), and `aeMain(server.el)` at
   `src/server.c:8747` is the whole server's main loop.
2. **One `aeApiPoll` collects *all* ready fds** (`:403`). Not one syscall per
   ready socket — one syscall per *batch* of ready sockets. On your Mac that is
   a single `kevent()`:

```c
// redis src/ae_kqueue.c — aeApiPoll, 124-137 (one kevent() for the whole set)
   124  static int aeApiPoll(aeEventLoop *eventLoop, struct timeval *tvp) {
   125      aeApiState *state = eventLoop->apidata;
   126      int retval, numevents = 0;
   127
   128      if (tvp != NULL) {
   129          struct timespec timeout;
   130          timeout.tv_sec = tvp->tv_sec;
   131          timeout.tv_nsec = tvp->tv_usec * 1000;
   132          retval = kevent(state->kqfd, NULL, 0, state->events, eventLoop->setsize,
   133                          &timeout);
   134      } else {
   135          retval = kevent(state->kqfd, NULL, 0, state->events, eventLoop->setsize,
   136                          NULL);
   137      }
```

3. **Timers ride the same poll.** The timeout handed to `kevent` is the time
   until the nearest timer (`ae.c:393-400`), so redis needs no timer thread and
   no separate `sleep`. One syscall serves both "wake me when a socket is
   ready" and "wake me in 100 ms".

The backend is chosen at compile time behind an abstraction of four functions
(`aeApiCreate` / `aeApiAddEvent` / `aeApiDelEvent` / `aeApiPoll`, plus
`aeApiName`). On macOS you compile `ae_kqueue.c`; `ae_epoll.c` is *not*
compiled on your machine. The c10k chapter of this topic works through the
selection ladder at `ae.c:30-44` and what each backend actually guarantees;
the one fact you need here is that redis registers events **level-triggered**
(`ae_kqueue.c:102-111` uses `EV_ADD` without `EV_CLEAR`), so a socket with
unread bytes reports ready again on the next poll — which is what makes it safe
for Step 3 to stop reading whenever it likes.

Why it matters: 10,000 threads blocked in `read()` would cost 10,000 stacks and
a context switch per message. One loop thread costs one poll syscall per
*batch* of ready events, and that batch can be large.

### Step 2 — `events[fd]`: the dispatch table is an array, not a hash map

> **In:** a bare integer fd that just became readable.
> **Out:** the function to call and the `client *` to call it with, in one
> load — and a dispatch table that costs kilobytes for 10,000 connections.

Step 1's dispatch loop does `&eventLoop->events[fd]` (`ae.c:416`). That is a
plain array indexed by the raw file descriptor. Two questions: why is that fast,
and why is it *correct*?

Fast, because fds are exactly the keys arrays love — small, dense integers
handed out by the OS from the lowest free slot. A hash map would compute a hash
and chase a pointer to reach data an array reaches with one shift-and-add. Here
is what a slot costs:

```c
// redis src/ae.h — the two arrays' element types, 52-57 and 73-76
    52  typedef struct aeFileEvent {
    53      int mask; /* one of AE_(READABLE|WRITABLE|BARRIER) */
    54      aeFileProc *rfileProc;
    55      aeFileProc *wfileProc;
    56      void *clientData;
    57  } aeFileEvent;
// ... 58-72: aeTimeEvent ...
    73  typedef struct aeFiredEvent {
    74      int fd;
    75      int mask;
    76  } aeFiredEvent;
```

Work the memory. On a 64-bit machine `aeFileEvent` is one `int` padded to 8
plus three pointers at 8 = **32 bytes**; `aeFiredEvent` is two `int`s = **8
bytes**. The loop is created with `setsize = maxclients + CONFIG_FDSET_INCR`
(`src/server.c:3138`), and `CONFIG_FDSET_INCR` is `32 + 96 = 128`
(`src/server.h:162`, `:226`). So at `maxclients 10000`:

```
setsize            = 10000 + 128            = 10,128 slots
events array       = 10,128 × 32 bytes      =   324,096 B ≈ 317 KiB
fired array        = 10,128 ×  8 bytes      =    81,024 B ≈  79 KiB
                                              ---------------------
whole dispatch table for 10,000 connections ≈   405,120 B ≈ 396 KiB
```

Compare the thread-per-connection design the c10k chapter takes apart: 10,000
threads at the usual 2 MiB stack reservation is **19.5 GiB** of virtual address
space. The event loop's *entire* connection-dispatch structure is 396 KiB —
about 1/51,700th of it. (Both figures are address space, not resident memory;
idle thread stacks are mostly never faulted in. The point is not that threads
use 19.5 GiB of RAM, it is that the loop's bookkeeping fits in L2.)

And it is not even eagerly allocated. Redis grows the arrays on demand:

```c
// redis src/ae.c — aeCreateFileEvent, 145-168 (grow-on-demand, capped at setsize)
   145  int aeCreateFileEvent(aeEventLoop *eventLoop, int fd, int mask,
   146          aeFileProc *proc, void *clientData)
   147  {
   148      if (fd >= eventLoop->setsize) {
   149          errno = ERANGE;
   150          return AE_ERR;
   151      }
   152
   153      /* Resize the events and fired arrays if the file
   154       * descriptor exceeds the current number of events. */
   155      if (unlikely(fd >= eventLoop->nevents)) {
   156          int newnevents = eventLoop->nevents;
   157          newnevents = (newnevents * 2 > fd + 1) ? newnevents * 2 : fd + 1;
   158          newnevents = (newnevents > eventLoop->setsize) ? eventLoop->setsize : newnevents;
   159          eventLoop->events = zrealloc(eventLoop->events, sizeof(aeFileEvent) * newnevents);
   160          eventLoop->fired = zrealloc(eventLoop->fired, sizeof(aeFiredEvent) * newnevents);
   161
   162          /* Initialize new slots with an AE_NONE mask */
   163          for (int i = eventLoop->nevents; i < newnevents; i++)
   164              eventLoop->events[i].mask = AE_NONE;
   165          eventLoop->nevents = newnevents;
   166      }
   167
   168      aeFileEvent *fe = &eventLoop->events[fd];
```

`nevents` starts at `min(setsize, INITIAL_EVENT)` with `INITIAL_EVENT 1024`
(`ae.c:46`, `:54-56`), so a server configured for 10,000 clients but serving 40
of them holds a 40 KiB table, and doubles from there — `setsize` is a *cap*,
not an allocation.

Correctness is the more interesting half. When a connection closes, its fd
number goes straight back on the free list and the very next `accept()` can
return it. With an array, `events[fd]` is simply overwritten by the new
connection's registration — there is no stale entry to find. With a
`HashMap<fd, handler>` you would have to guarantee the delete happens before
the reinsert, or a handler belonging to a dead client fires on a live one that
happens to have inherited its number. The array makes the aliasing that fd
reuse creates *unrepresentable*: there is exactly one slot per fd, always.

### Step 3 — the read path: one `read()` becomes a batch of commands

> **In:** an fd the poll reported readable.
> **Out:** up to `lookahead` fully parsed commands executed back to back, all
> paid for with a single `read()` syscall.

When a client's fd is readable, the registered handler is `readQueryFromClient`
(`networking.c:4004`, registered at `:133` inside `createClient`). It sizes a
read, does exactly one, and hands the bytes to the parse-and-execute loop.

The sizing is not just "16 KB":

```c
// redis src/networking.c — readQueryFromClient, 4029 and 4077-4100 (one read, sized up)
  4029      readlen = PROTO_IOBUF_LEN;
// ... 4030-4076: if the next thing on the wire is a >= 32 KB bulk argument,
//                set readlen to exactly the remaining bytes of that argument
//                (Step 5's zero-copy depends on this); otherwise borrow the
//                per-thread reusable query buffer ...
  4077      qblen = sdslen(c->querybuf);
  4078      if (!(c->flags & CLIENT_MASTER) && // master client's querybuf can grow greedy.
  4079          (big_arg || sdsalloc(c->querybuf) < PROTO_IOBUF_LEN)) {
// ... 4080-4088: non-greedy growth for the initial allocation and for big args ...
  4089      } else {
  4090          c->querybuf = sdsMakeRoomFor(c->querybuf, readlen);
  4091
  4092          /* Read as much as possible from the socket to save read(2) system calls. */
  4093          readlen = sdsavail(c->querybuf);
  4094      }
// ... 4095-4099: comment — compression is per client (client_comp.c); nread is
//                decompressed bytes, network_read is bytes off the socket ...
  4100      nread = clientConnRead(c, c->querybuf+qblen, readlen, &network_read);
```

`PROTO_IOBUF_LEN` is 16 KiB (`server.h:207`) but line 4093 is the real policy:
once the buffer has grown, redis asks for *everything the buffer can hold*,
with the comment stating the motive outright — "to save read(2) system calls".

The line to focus on is 4100, the only read on this path. `clientConnRead`
is the hook for per-client compression (`src/client_comp.c`); for a client
without an active decompressor — every client in this chapter — it is exactly
one `connRead` (`client_comp.c:717-720`), and in a build without compression
support it is nothing else (`:860-864`). One syscall, and it is the only one on
the read side.

There is a second memory trick here worth noticing. A client with no partial
command in flight does not own a query buffer at all — it *borrows* a
per-thread reusable one (`networking.c:4055-4074`), which it gives back once
the buffer drains. Ten thousand mostly-idle connections therefore do not cost
ten thousand 16 KiB buffers; they cost one per thread plus whatever the few
mid-command clients hold.

Now the loop the bytes flow into. This is the part most descriptions of redis
get wrong: `processInputBuffer` is not "parse one command, execute it, repeat".
It is a **two-level** loop — an inner loop that parses up to `lookahead`
commands, then an outer loop that executes the parsed ones:

```c
// redis src/networking.c — processInputBuffer, 3801 and 3810-3848 (the batching)
  3801  int processInputBuffer(client *c) {
// ... 3802-3809: count the event; mark the client active (main thread only) ...
  3810      /* We limit the lookahead for unauthenticated connections to 1.
  3811       * This is both to reduce memory overhead, and to prevent errors: AUTH can
  3812       * affect the handling of succeeding commands. Parsing of "large"
  3813       * unauthenticated multibulk commands is rejected, which would cause those
  3814       * commands to incorrectly return an error to the client. */
  3815      const int lookahead = authRequired(c) ? 1 : server.lookahead;
  3816
  3817      /* Keep processing while there is something in the input buffer */
  3818      while ((c->querybuf && c->qb_pos < sdslen(c->querybuf)) ||
  3819             c->pending_cmds.ready_len > 0)
  3820      {
// ... 3821-3842: bail out if the client is blocked, closing, or already has a
//                command in flight ...
  3843          const int parse_more = !c->pending_cmds.ready_len;
// ... 3844-3845: remember the ready count, for the pipeline-length stats ...
  3846          /* Parse up to lookahead commands only if we don't have enough ready commands */
  3847          while (parse_more && c->pending_cmds.ready_len < lookahead &&
  3848                 c->querybuf && c->qb_pos < sdslen(c->querybuf))
```

The line to focus on is 3847: the inner loop's bound is `lookahead`, not one.
`server.lookahead` defaults to **16** (`REDIS_DEFAULT_LOOKAHEAD`,
`server.h:229`; the config is `lookahead`, `config.c:3652`). The parsed
commands land in `c->pending_cmds` (`server.h:1592`), a doubly linked
`pendingCommandList` that counts both its length and how many entries are
complete — `len` and `ready_len` (`server.h:1423-1428`) — of `struct
pendingCommand` (`server.h:2889-2905`). Only then does the outer loop take the
head and execute it (`networking.c:3911`, `:3961`), one command at a time.

Why decouple parsing from execution at all? Because it creates a window in
which redis knows *which keys the next sixteen commands will touch* before it
touches any of them — and can prefetch them:

```c
// redis src/networking.c — processInputBuffer, 3924-3935 (prefetch the parsed batch)
  3924          /* Prefetch the command only when more commands have been parsed and we
  3925           * are in the main thread. If running in an IO thread, prefetch will be
  3926           * deferred until the client is processed by the main thread. Skip prefetch
  3927           * if there are too few commands to avoid meaningless prefetching. */
  3928          if (parse_more && c->running_tid == IOTHREAD_MAIN_THREAD_ID &&
  3929              c->pending_cmds.ready_len > 1)
  3930          {
  3931              /* Prefetch the commands. */
  3932              resetCommandsBatch();
  3933              addCommandToBatch(c);
  3934              prefetchCommands();
  3935          }
```

Those three functions are declared in `src/memory_prefetch.h:26-28`. This is the same
idea the valkey chapter of this topic takes apart at length — hide DRAM latency
by issuing several dependent lookups' cache misses concurrently — and it is
only *possible* because a pipelined client handed the server sixteen commands
in one read. Pipelining does not merely save syscalls; it hands the engine a
batch to be clever with.

The same machinery has a second, *intra-command* entry point, and it makes the
argument from the other side. `dictPrefetchKeys(dicts, keys, nkeys)`
(`memory_prefetch.h:38`, at most `DICT_PREFETCH_MAX_SIZE` = 64 keys per call,
`:37`) lets a single multi-key command prefetch its own key arguments with the
same state machine. MGET and MSET call it through `prefetchKeysBatch`
(`t_string.c:692`), which hands over 16 keys at a time (`PREFETCH_BATCH_SIZE`,
`:687`) while at least 32 remain and the whole remainder otherwise — and skips
the work when the cross-command batch above already warmed those keys
(`PENDING_CMD_KEYS_PREFETCHED`, `:731-733`). How far down an entry the
prefetch reaches is the dictionary's business: two optional `dictType`
callbacks, `prefetchEntryKey` and `prefetchEntryValue` (`dict.h:155-156`),
return the key and value payload addresses behind an entry, and the keyspace's
`dictType` sets them (`server.c:667-668`). Either way the condition is the same:
the prefetch only pays when the engine sees several independent keys before
touching any of them. A pipelined read supplies them across commands; a
multi-key command supplies them within one.

**Pipelining** is the client-side technique of writing many requests without
waiting for each reply. The server needs no feature to support it: the inner
loop at `:3847` drains whatever the buffer holds, and a client that sent 100
commands back to back has all 100 executed off one `read()`.

### Step 4 — the round-trip arithmetic: where 44k becomes 12.3M

> **In:** the measured loopback lane in `notes.md` and a pipeline depth P.
> **Out:** the syscall count and round-trip count *per request*, and the
> division that turns 44,088 ops/s into 12,321,414 ops/s.

This topic's measured headline ([FINDINGS.md](../../FINDINGS.md) row 7) is
about a benchmark that does *no work at all* — no parsing, no store, 32 bytes
in and 8 bytes out — and still spans 279×:

| P | ops/s | µs per request | client syscalls per op | vs P=1 |
|---|---|---|---|---|
| 1 | 44 088 | 22.68 | 2.000 | 1.0× |
| 8 | 353 067 | 2.83 | 0.250 | 8.0× |
| 64 | 2 919 728 | 0.34 | 0.031 | 66.2× |
| 256 | 12 321 414 | 0.08 | 0.008 | **279.5×** |

A **round trip** is one traversal of request-to-server-and-reply-back: the
client cannot send request *n+1* until reply *n* arrives, so its rate is capped
at 1/RTT no matter how fast the server is. Count both quantities per request at
depth P:

```
Per BATCH of P requests, client side:   1 × write()  +  1 × read()   = 2 syscalls
Per BATCH of P requests, server side:   1 × read()   +  1 × write()  = 2 syscalls
Per BATCH of P requests:                                               1 round trip

So per REQUEST:
    client syscalls  = 2 / P
    total syscalls   = 4 / P   (both processes)
    round trips      = 1 / P

    P =   1 :  2.000 client syscalls,  4.000 total,  1.000     RTT per request
    P =   8 :  0.250 client syscalls,  0.500 total,  0.125     RTT per request
    P =  64 :  0.031 client syscalls,  0.062 total,  0.0156    RTT per request
    P = 256 :  0.0078 client syscalls, 0.0156 total, 0.0039    RTT per request
```

The "client syscalls per op" column in `notes.md` is exactly this computed
floor, `2.0/P` — it is arithmetic, not an instrumented count. Say so when you
quote it.

Now the division the headline rests on. Throughput is depth divided by the time
one batch takes:

```
ops/s = P / T_batch

P = 1:      T_batch = 1 / 44,088       = 22.681 µs   (one request per batch)
P = 256:    T_batch = 256 / 12,321,414 = 20.777 µs   (256 requests per batch)

speedup  =  (256 / 20.777 µs) / (1 / 22.681 µs)
         =  256 × (22.681 / 20.777)
         =  256 × 1.0916
         =  279.4×          ← matches the measured 279.5× to rounding
```

Read that middle line again, because it is the whole chapter. **The time to
complete a batch barely changed** — 22.681 µs for one request, 20.777 µs for
two hundred and fifty-six. A batch of 256 requests costs *8% less* than a batch
of 1. Every microsecond of that 22.681 was overhead: two context switches into
the kernel and back on each side, two process wakeups, one loopback traversal.
The payload was never the cost. Check that directly: 40 bytes cross the
loopback per exchange, and even at a conservative 10 GB/s that is 4 ns — 0.02%
of 22.681 µs.

The 279× therefore decomposes cleanly: **256× from amortizing a fixed per-batch
cost over 256 requests, and a further 1.09× because the batched exchange is
itself slightly cheaper per byte than the single one.** That is why `notes.md`
calls `2/P` a floor on the improvement rather than a ceiling.

Turn it around and the design constraint from "the problem in one sentence"
falls out. Suppose you want 1,000,000 ops/s from one thread. That is a 1.000 µs
budget per command, all in. At the measured 1.17 µs per `write()`
([FINDINGS.md](../../FINDINGS.md) row 5):

```
budget per command at 1M ops/s          = 1000 ns
syscall bill at P=1  (2 per command)    = 2 × 1170 ns = 2340 ns   → 2.3× over budget
syscall bill at P=4  (0.5 per command)  = 0.5 × 1170  =  585 ns   → 59% of budget, still awful
syscall bill at P=16 (0.125 per cmd)    = 0.125 × 1170 =  146 ns  → 15% of budget
syscall bill at P=64 (0.031 per cmd)    = 0.031 × 1170 =   37 ns  → 3.7% of budget
```

**A single-threaded server cannot reach 1M ops/s unpipelined, on this hardware,
for arithmetic reasons that have nothing to do with how good its data
structures are.** Any "redis does a million ops per second" claim is either
pipelined or multi-threaded; when you see one, the first question is `-P` what.

Two cautions about numbers you will meet nearby:

- "`redis-benchmark -P 64` is ~10× `-P 1`" is a figure that circulates widely.
  The measured lane says **66.2×** (2,919,728 / 44,088), and that is the figure
  this topic's `README.md` §2 uses. The "~10×" comes from some other machine and
  some other server; the number this repo can defend is 66.2× on an M3 Pro over
  loopback with a do-nothing server. Real redis will land lower, because at P=64 real work starts to matter — that gap
  is precisely what your `notes.md` prediction table is asking you to guess.
- Latency does not get worse when you pipeline here: per-request time *improves*
  from 22.68 µs to 0.08 µs. That is not the usual throughput-for-latency trade,
  because what batching removed was pure round-trip overhead, not queueing
  behind useful work. Server-side batching (group commit, topic 5) is the real
  trade; this is not.

None of this arithmetic survives if the kernel is allowed to sit on your small
writes. **Nagle's algorithm** delays sending a small TCP segment while an
earlier un-acknowledged segment is still outstanding, coalescing small writes
into fewer packets — excellent for a `telnet` session, catastrophic for a
request/reply protocol, where it interacts with delayed ACKs to add tens of
milliseconds to a round trip. **`TCP_NODELAY`** is the socket option that turns
it off. Redis sets it on every client, unconditionally, at creation:

```c
// redis src/networking.c — createClient, 122-136 (nodelay + the 16 KiB reply buffer)
   122  client *createClient(connection *conn) {
   123      client *c = zmalloc(sizeof(client));
   124
// ... 125-128: comment on NULL conn for fake (Lua/AOF) clients ...
   129      if (conn) {
   130          connEnableTcpNoDelay(conn);
   131          if (server.tcpkeepalive)
   132              connKeepAlive(conn,server.tcpkeepalive);
   133          connSetReadHandler(conn, readQueryFromClient);
   134          connSetPrivateData(conn, c);
   135      }
   136      c->buf = zmalloc_usable(PROTO_REPLY_CHUNK_BYTES, &c->buf_usable_size);
```

The line to focus on is 130.

The `setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, ...)` behind that call is at
`src/anet.c:258`, wrapped as `anetEnableTcpNoDelay` at `:266`. This repo's own
loopback bench sets the same option for the same reason — see the
`TCP_NODELAY` note in the `notes.md` baseline header. Note the consequence:
having disabled the kernel's write coalescing, redis has to do its own, which
is Step 6.

### Step 5 — the parser: length-prefixed, resumable, zero-copy for big args

> **In:** whatever bytes happen to be in `querybuf` — possibly half a command,
> possibly nine and a half.
> **Out:** a `pendingCommand` with `argv` populated, or a clean "incomplete"
> that loses nothing.

`processMultibulkBuffer` (`networking.c:3380`) is the RESP parser, and RESP's
design (topic 7 §1) makes it nearly trivial: read `*<argc>`, then per argument
read `$<len>` and then *exactly* len bytes. No payload byte is ever scanned or
compared. Four details repay the read.

**The argument count sizes `argv` once.**

```c
// redis src/networking.c — processMultibulkBuffer, 3406-3429 (parse *argc, size argv)
  3406          serverAssertWithInfo(c,NULL,c->querybuf[c->qb_pos] == '*');
// ... 3407-3416: string2ll the count; reject non-numeric, > INT_MAX, and
//                > 10 args from an unauthenticated client ...
  3417          c->qb_pos = (newline-c->querybuf)+2;
  3418
  3419          if (ll <= 0) return C_OK;
  3420
  3421          c->multibulklen = ll;
  3422          c->bulklen = -1;
  3423
  3424          /* Setup argv array on pending command structure.
  3425           * Reallocate argv array when the requested size is greater than current size. */
  3426          if (c->multibulklen > pcmd->argv_len) {
  3427              zfree(pcmd->argv);
  3428              pcmd->argv_len = min(c->multibulklen, 1024);
  3429              pcmd->argv = zmalloc(sizeof(robj*)*(pcmd->argv_len));
```

Note the `min(..., 1024)` at `:3428`: a client claiming a million arguments
does not get a million-pointer allocation up front. The array grows later, as
arguments actually arrive (`:3540-3543`).

**Resumability.** TCP is a byte stream with no message boundaries; a command
can arrive split across two `read()`s in any position — mid-length, mid-payload,
between the `\r` and the `\n`. Incomplete input loses nothing: the unparsed
bytes stay in `querybuf`, and there are two cases.

- The `*<argc>\r\n` line itself has not fully arrived. The parser returns
  `C_ERR` with no `read_error` set (`:3392-3402`), and `processInputBuffer`
  frees the still-empty `pendingCommand` and waits for the next read
  (`:3876-3881`).
- The count is known but arguments are missing. This is not an error: the
  parser sets `PENDING_CMD_FLAG_INCOMPLETE` and returns `C_OK` (`:3586-3588`).
  The half-built `pendingCommand`, with every argument completed so far already
  in its `argv`, is appended to `pending_cmds` without counting as ready —
  `addPendingCommand` bumps `ready_len` only for complete commands
  (`networking.c:6066`) — and on the next read `processInputBuffer` sees
  `len != ready_len`, pops it back off the tail and continues it
  (`:3869-3871`).

Where *inside* the current command the parser stopped is three fields of
`struct client`:

```c
// redis src/server.h — struct client, 1607-1609 (the parser's position)
  1607      int reqtype;            /* Request protocol type: PROTO_REQ_* */
  1608      int multibulklen;       /* Number of multi bulk arguments left to read. */
  1609      long bulklen;           /* Length of bulk argument in multi bulk request. */
```

That is it — a request type, a count of arguments still expected, and a count
of bytes still expected of the current argument (`-1` while its `$<len>` line
is still unread). Those three integers plus the arguments already parsed into
the parked `pendingCommand` are the entire suspended state of a half-parsed
command, which is why redis can afford to keep ten thousand of them. (The
integers are reset by `resetClientQbufState`, `networking.c:3109-3113`.) Your
Rust parser's partial-input resumption test is testing exactly this property.

**Big-argument zero-copy.** For arguments of at least `PROTO_MBULK_BIG_ARG` =
32 KiB (`server.h:210`), redis arranges for the query buffer to contain *only*
that argument, and then hands the buffer itself to the object system instead of
copying out of it:

```c
// redis src/networking.c — processMultibulkBuffer, 3545-3564 (the sds becomes the object)
  3545              /* Optimization: if a non-master client's buffer contains JUST our bulk element
  3546               * instead of creating a new object by *copying* the sds we
  3547               * just use the current sds string. */
  3548              if (!(c->flags & CLIENT_MASTER) &&
  3549                  c->qb_pos == 0 &&
  3550                  c->bulklen >= PROTO_MBULK_BIG_ARG &&
  3551                  querybuf_len == (size_t)(c->bulklen+2))
  3552              {
  3553                  (pcmd->argv)[(pcmd->argc)++] = createObject(OBJ_STRING,c->querybuf);
  3554                  pcmd->argv_len_sum += c->bulklen;
  3555                  c->all_argv_len_sum += c->bulklen;
  3556                  sdsIncrLen(c->querybuf,-2); /* remove CRLF */
// ... 3557-3563: give the client a fresh querybuf, sized for another fat arg
//                unless that would be more than maxmemory/32 ...
  3564                  sdsclear(c->querybuf);
```

Read the four conditions at `:3548-3551` as a specification of when this is
*safe*: not a replication link, the bulk starts at offset 0, it is at least
32 KiB, and the buffer length is *exactly* `bulklen + 2` — the buffer holds this
argument and nothing else. The last two conditions are not luck; they are
manufactured, on both sides of the read. When the parser reads a `$<len>`
header of at least 32 KiB and the unparsed bytes are no more than the argument
itself, it trims everything before the argument and sizes the buffer for
exactly `len + 2` (`:3501-3527`; `qb_pos = 0` at `:3518`). Then
`readQueryFromClient:4036-4054` sees a big argument in progress and sets
`readlen` to exactly its remaining bytes, deliberately accepting more `read()`
calls to buy the alignment. The optimization fails, and falls to
`createStringObject` at `:3567-3568`, the moment a pipelined client sends
anything after the big `SET` in the same segment — which is the answer to
question 3.

**The inline fallback.** `processInlineBuffer` (`networking.c:3229`) handles
`PING\r\n` typed into `nc`: `strchr` for the newline (`:3236`), then
`sdssplitargs` on the line (`:3253`). It is the only scanning parser anywhere
in the path, it is capped at `PROTO_INLINE_MAX_SIZE` = 64 KiB
(`server.h:209`, checked at `:3240-3242`), and it exists purely so a human with
a terminal can talk to the server. Which form a client is using is decided by a
single byte: `'*'` means multibulk, anything else means inline
(`networking.c:3851-3856`).

### Step 6 — the write path: replies are hoarded, then flushed with one write

> **In:** a batch of executed commands, each of which called `addReply*`.
> **Out:** one write call per client per loop turn — a plain `write()`, or a
> gathered `writev()` once the replies spill past the first buffer — covering
> every reply the client accumulated, not one write per reply.

Here is the surprise. `addReply` does **not** write to the socket:

```c
// redis src/networking.c — addReply, 641-657 (append to a buffer, never write)
   641  /* Add the object 'obj' string representation to the client output buffer. */
   642  void addReply(client *c, robj *obj) {
   643      if (_prepareClientToWrite(c) != C_OK) return;
   644
   645      if (sdsEncodedObject(obj)) {
   646          _addReplyToBufferOrList(c,obj->ptr,sdslen(obj->ptr));
   647      } else if (obj->encoding == OBJ_ENCODING_INT) {
   648          /* For integer encoded strings we just convert it into a string
   649           * using our optimized function, and attach the resulting string
   650           * to the output buffer. */
   651          char buf[32];
   652          size_t len = ll2string(buf,sizeof(buf),(long)obj->ptr);
   653          _addReplyToBufferOrList(c,buf,len);
   654      } else {
   655          serverPanic("Wrong obj->encoding in addReply()");
   656      }
   657  }
```

`_addReplyToBufferOrList` (`:582-585`) wraps its one span as a single
`replySegment` and hands it to the batched form,
`_addReplySegmentsToBufferOrList`, which lets a caller append several pieces of
one reply with one pass of the bookkeeping. That function is the two-tier
buffer, and its final loop says everything:

```c
// redis src/networking.c — _addReplySegmentsToBufferOrList, 536 and 571-579 (buffer, then spill)
   536  void _addReplySegmentsToBufferOrList(client *c, const replySegment *seg, int nseg) {
// ... 537-570: refuse for closing clients, disconnect a replica that replied,
//              account bytes, divert push messages to a separate list ...
   571      for (int i = 0; i < nseg; i++) {
   572          const char *s = seg[i].ptr;
   573          size_t len = seg[i].len;
   574          if (!len) continue;
   575          size_t reply_len = _addReplyPayloadToBuffer(c, s, len, PLAIN_REPLY);
   576          if (len > reply_len)
   577              _addReplyPayloadToList(c, c->reply, s + reply_len, len - reply_len, PLAIN_REPLY);
   578      }
   579  }
```

The lines to focus on are 575-577. A fixed 16 KiB chunk first (`c->buf`,
allocated in `createClient` at `networking.c:136`, sized
`PROTO_REPLY_CHUNK_BYTES` = 16 KiB, `server.h:208`), and only the overflow goes
to a linked list of blocks (`c->reply`, `server.h:1610`). Small replies — which
is nearly all of them — never allocate.

The client is then merely *flagged*, and the comment explaining why is the best
three sentences in the file:

```c
// redis src/networking.c — putClientInPendingWriteQueue, 295-312 (flag, don't write)
   295  void putClientInPendingWriteQueue(client *c) {
// ... 296-303: skip if already flagged, or if a replica cannot receive yet ...
   304          /* Here instead of installing the write handler, we just flag the
   305           * client and put it into a list of clients that have something
   306           * to write to the socket. This way before re-entering the event
   307           * loop, we can try to directly write to the client sockets avoiding
   308           * a system call. We'll only really install the write handler if
   309           * we'll not be able to write the whole reply at once. */
   310          c->flags |= CLIENT_PENDING_WRITE;
   311          listLinkNodeHead(server.clients_pending_write, &c->clients_pending_write_node);
   312      }
```

"Avoiding a system call" — the system call being avoided is the *registration*
of a write event with kqueue/epoll, plus the extra poll wakeup it would cause.
The write itself happens at the top of the next loop turn, in `beforeSleep`
(`server.c:1989`, flush at `:2146`):

```c
// redis src/networking.c — handleClientsWithPendingWrites, 3060-3101 (one pass, one write each)
  3060  int handleClientsWithPendingWrites(void) {
  3061      listIter li;
  3062      listNode *ln;
  3063      int processed = listLength(server.clients_pending_write);
  3064
  3065      listRewind(server.clients_pending_write,&li);
  3066      while((ln = listNext(&li))) {
  3067          client *c = listNodeValue(ln);
// ... 3068-3092: skip replicas owned by IO threads, protected and closing
//                clients; hand the client to an IO thread if one is available ...
  3093          /* Try to write buffers to the client socket. */
  3094          if (writeToClient(c,0) == C_ERR) continue;
  3095
  3096          /* If after the synchronous writes above we still have data to
  3097           * output to the client, we need to install the writable handler. */
  3098          if (clientHasPendingReplies(c)) {
  3099              installClientWriteHandler(c);
  3100          }
  3101      }
```

And `writeToClient` (`:2949`) does not issue one write per buffer either. Its
path for ordinary clients, `_writeToClientNonSlave` (`:2828`), chooses between
two calls. If everything is still in `c->buf`, it is one plain `connWrite` of
that buffer (`:2847`). As soon as the reply has spilled into the `c->reply`
list (`:2832-2833`) — or `c->buf` holds references to large strings sent in
place rather than copied — `_writevToClient` builds an `iovec` array containing
`c->buf` **and** the list nodes and issues a single scatter-gather write, the
`connWritev` at `:2758`:

```c
// redis src/networking.c — _writevToClient, 2701-2722 (gather buf + list into one writev)
  2701  static int _writevToClient(client *c, ssize_t *nwritten) {
  2702      int iovmax = min(IOV_MAX, c->conn->iovcnt);
  2703      struct iovec iov[iovmax];
  2704      ReplyIOV reply_iov = {iov, iovmax};
  2705
  2706      /* Add c->buf to iov array */
  2707      if (c->bufpos > 0) {
  2708          if (likely(!c->buf_encoded)) {
  2709              /* Non-encoded buffer - add directly */
  2710              iov[reply_iov.iovcnt].iov_base = c->buf + c->sentlen;
  2711              iov[reply_iov.iovcnt].iov_len = c->bufpos - c->sentlen;
  2712              reply_iov.iov_bytes_len += iov[reply_iov.iovcnt++].iov_len;
// ... 2713-2720: the encoded-buffer path (bulk strings referenced, not copied) ...
  2721      /* Add c->reply list nodes to iov array */
  2722      if (!replyIOVReachLimit(&reply_iov)) {
```

So: a pipeline of 100 `GET`s produces 100 `addReply` calls, one flag, zero
syscalls during execution, and **one write call** at the top of the next loop
turn — a `write()` while the replies fit in 16 KiB, a `writev()` beyond that.
Combine with Step 3 and the server's whole syscall bill for that pipeline is
one `read()` plus one write — the `4/P` arithmetic of Step 4, with the server's
half of it delivered by these two mechanisms.

Two guards on the batching, both worth knowing:

- `NET_MAX_WRITES_PER_EVENT` = 64 KiB (`server.h:142`) caps how much a single
  normal client may be written per event, so one `KEYS *` over loopback cannot
  starve the other 9,999 clients (`networking.c:2976-2992`). The cap is lifted
  when over `maxmemory`, and for replicas and monitors, whose buffers would
  otherwise grow without bound.
- The writable event is the *exception*, not the rule. `installClientWriteHandler`
  is called only at `:3099`, only when the socket refused the whole reply. Redis
  registers for write readiness only when the kernel has told it, by short
  write, that it must.

Here is the whole design as one Rust sketch. It is **not** redis code — it is
the shape you should be able to reproduce in your own server:

```rust
// ILLUSTRATION — not quoted from redis. The real loop is redis src/ae.c:365
// (aeProcessEvents), the real read side is src/networking.c:3801
// (processInputBuffer), the real flush is src/networking.c:3060
// (handleClientsWithPendingWrites), called from src/server.c:2146.
loop {
    flush_pending_writes(&mut clients);   // beforeSleep: ONE write per client
    let ready = poll.wait(next_timer());  // ONE syscall for the whole fd set
    for fd in ready {
        let c = &mut clients[fd];         // array, not a map — fds are dense
        c.querybuf.extend(read_once(fd)); // ONE read, sized to the whole buffer
        let mut batch = Vec::new();
        while batch.len() < LOOKAHEAD {
            match parse_resp(&c.querybuf[c.pos..]) {
                Parsed { cmd, used } => { c.pos += used; batch.push(cmd) }
                Incomplete => break,      // keep bytes; multibulklen/bulklen resume
            }
        }
        prefetch_keys(&batch);            // the point of parsing ahead
        for cmd in batch {
            execute(cmd, c);              // addReply BUFFERS; it never writes
        }
    }
}
```

### Step 7 — backpressure: the buffer that grows until the axe falls

> **In:** a client that produces faster than the server consumes, or consumes
> slower than the server produces.
> **Out:** a disconnect — because RESP has no way to say "slow down".

Steps 3 and 6 both accumulate unbounded buffers, so redis needs a policy for
clients on either side of the loop that get out of step. It has exactly one
policy, and it is blunt.

Input side: a client streaming commands faster than they execute grows
`querybuf`. When it crosses the limit, the client is freed:

```c
// redis src/networking.c — readQueryFromClient, 4144-4156 (the query-buffer axe)
  4144      if (!(c->flags & CLIENT_MASTER) &&
// ... 4145-4148: comment — queued MULTI args count toward the same budget ...
  4149          (c->mstate.argv_len_sums + sdslen(c->querybuf) > server.client_max_querybuf_len ||
  4150           (c->mstate.argv_len_sums + sdslen(c->querybuf) > 1024*1024 && authRequired(c))))
  4151      {
  4152          c->read_error = CLIENT_READ_REACHED_MAX_QUERYBUF;
  4153          freeClientAsync(c);
  4154          atomicIncr(server.stat_client_qbuf_limit_disconnections, 1);
  4155          goto done;
  4156      }
```

Note `:4150`: an *unauthenticated* client gets a hard 1 MiB ceiling regardless
of config — a pre-auth client cannot make the server allocate.

Output side: a slow reader, or one that issued `KEYS *` against a 10M-key
database, grows `c->reply` until:

```c
// redis src/networking.c — closeClientOnOutputBufferLimitReached, 5551-5575 (the reply axe)
  5551  int closeClientOnOutputBufferLimitReached(client *c, int async) {
  5552      if (!c->conn) return 0; /* It is unsafe to free fake clients. */
// ... 5553-5557: assert reply_bytes sane; nothing to do if the buffer is empty ...
  5558      if (checkClientOutputBufferLimits(c)) {
  5559          sds client = catClientInfoString(sdsempty(),c);
  5560
  5561          if (async) {
  5562              freeClientAsync(c);
// ... 5563-5571: log the disconnect at LL_WARNING, sync path calls freeClient ...
  5572          sdsfree(client);
  5573          server.stat_client_outbuf_limit_disconnections++;
  5574          return  1;
  5575      }
```

Both counters are exported, which tells you the maintainers expect this to
happen in production: `stat_client_qbuf_limit_disconnections` and
`stat_client_outbuf_limit_disconnections`.

Call this **buffer-or-die**. RESP has no flow-control message — nothing a
server can send that means "pause". Contrast the pgwire chapter of this topic,
where a *portal* lets the client ask for `n` rows at a time and the server
simply stops after `n` with a `PortalSuspended`; the protocol carries the
backpressure, so nobody has to be killed.

Now trace a module through it, because that is where this bites in FalkorDB.
`GRAPH.QUERY` returning a million rows calls `RedisModule_ReplyWith*`, and
those are thin wrappers over the same functions:

```c
// redis src/module.c — RM_ReplyWithLongLong, 3171-3178 (module replies are addReply)
  3171  /* Send an integer reply to the client, with the specified `long long` value.
  3172   * The function always returns REDISMODULE_OK. */
  3173  int RM_ReplyWithLongLong(RedisModuleCtx *ctx, long long ll) {
  3174      client *c = moduleGetReplyClient(ctx);
  3175      if (c == NULL) return REDISMODULE_OK;
  3176      addReplyLongLong(c,ll);
  3177      return REDISMODULE_OK;
  3178  }
```

So the path is: module → `RM_ReplyWith*` → `addReply*` → `c->buf` (16 KiB) →
`c->reply` list → possibly `closeClientOnOutputBufferLimitReached`. A module
cannot stream, cannot yield, and cannot be told the client is slow. It
materializes the whole reply in the server's memory and hopes. That constraint
— not query planning — is what shapes how a graph module has to paginate.

## Where each step lives in the code

Everything is `redis/redis@e87dda2a4`. Read `src/ae.c` end to end (516 lines);
from `src/networking.c` (6,146 lines) read only these.

| Anchor | What | Step |
|--------|------|------|
| `aeProcessEvents` — `src/ae.c:365` | one loop turn: `beforesleep` `:382-383`, `aeApiPoll` `:403`, dispatch `:414-418` | 1 |
| `aeApiPoll` — `src/ae_kqueue.c:124` | the single `kevent()` at `:132`/`:135`; the two-pass read/write merge at `:142-173` | 1 |
| `aeSetBeforeSleepProc` — `src/server.c:3274`; `aeMain` — `:8747` | how `beforeSleep` gets wired to the loop | 1, 6 |
| `aeFileEvent` — `src/ae.h:52-57`; `aeFiredEvent` — `:73-76` | 32 B and 8 B per slot | 2 |
| `aeCreateFileEvent` — `src/ae.c:145` | grow-on-demand at `:155-166`, `setsize` cap at `:148` | 2 |
| `aeCreateEventLoop(maxclients+128)` — `src/server.c:3138` | `CONFIG_FDSET_INCR`, `src/server.h:226` | 2 |
| `readQueryFromClient` — `src/networking.c:4004` | one read at `:4100` (`clientConnRead` → `connRead`, `src/client_comp.c:714-720`), sized at `:4029`/`:4093`; reusable buffer `:4055-4074` | 3 |
| `processInputBuffer` — `src/networking.c:3801` | the lookahead parse loop `:3843-3848`, prefetch `:3928-3935`, execute `:3961` | 3 |
| `REDIS_DEFAULT_LOOKAHEAD 16` — `src/server.h:229` | config `lookahead`, `src/config.c:3652` | 3 |
| `pendingCommandList` — `src/server.h:1423-1428`; `struct pendingCommand` — `:2889-2905` | the parsed-ahead queue: `len`, `ready_len` | 3, 5 |
| `dictPrefetchKeys` — `src/memory_prefetch.c:304` | intra-command prefetch, ≤ 64 keys (`memory_prefetch.h:37`); MGET/MSET via `t_string.c:692` | 3 |
| `connEnableTcpNoDelay` — `src/networking.c:130` | Nagle off for every client; `setsockopt` at `src/anet.c:258` | 4 |
| `processMultibulkBuffer` — `src/networking.c:3380` | `*argc` `:3406-3429`, big-arg trim `:3501-3527`, zero-copy `:3545-3564`, incomplete → `C_OK` + flag `:3586-3588` | 5 |
| `multibulklen` / `bulklen` — `src/server.h:1608-1609` | the parser's position inside the current command | 5 |
| `processInlineBuffer` — `src/networking.c:3229` | the `nc` fallback; the only scanning parse, `:3236` | 5 |
| `addReply` — `src/networking.c:642` | → `_addReplyToBufferOrList` `:582` → `_addReplySegmentsToBufferOrList` `:536`, buffer-then-spill `:575-577` | 6 |
| `putClientInPendingWriteQueue` — `src/networking.c:295` | flag, don't write — the comment at `:304-309` | 6 |
| `handleClientsWithPendingWrites` — `src/networking.c:3060` | called from `beforeSleep`, `src/server.c:2146` | 6 |
| `_writeToClientNonSlave` — `src/networking.c:2828` | plain `connWrite` `:2847`, or `_writevToClient` `:2701` (the `connWritev` at `:2758`) | 6 |
| `NET_MAX_WRITES_PER_EVENT` — `src/server.h:142` | the 64 KiB fairness cap, enforced at `networking.c:2989-2992` | 6 |
| query-buffer limit — `src/networking.c:4144-4156` | plus the 1 MiB pre-auth ceiling at `:4150` | 7 |
| `closeClientOnOutputBufferLimitReached` — `src/networking.c:5551` | the output-side axe | 7 |
| `RM_ReplyWithLongLong` — `src/module.c:3173` | modules reply through the same buffers | 7 |

Suggested route: `ae.c` top to bottom first — it is short and it is the
skeleton. Then the read path (Steps 3 and 5) as one continuous trace from
`readQueryFromClient` to `processCommandAndResetClient`. Then the write path
(Step 6) backwards from `beforeSleep`. Then grep the two limits (Step 7).

## Questions to answer in notes.md

1. Count syscalls both ways for a pipeline of 100 `GET`s: (a) as redis does it,
   (b) if `addReply` wrote immediately. Then price the difference at the 1.17 µs
   per `write()` this repo measured ([FINDINGS.md](../../FINDINGS.md) row 5).
   What fraction of a 1 µs-per-command budget does each design spend?
2. `events[fd]` as an array versus a `HashMap<fd, handler>`: why is the array not
   merely faster but *safer*? Write down the specific bug the array makes
   impossible, in terms of fd reuse after `close()`.
3. The big-argument zero-copy at `networking.c:3548-3551` has four conditions.
   For each, construct a client that violates only that one, and say what redis
   does instead. Which are manufactured — one by the parser, one by the *read*
   side — and where?
4. Your tokio server writes once per response future by default. What is the
   tokio equivalent of pending-writes batching, and where in your code does the
   flush have to go so that it is the analogue of `beforeSleep` rather than of
   `addReply`?
5. `notes.md` reports 66.2× for P=64, while the widely repeated rule of thumb
   is "`-P 64` is ~10× `-P 1`". Which conditions would make each true, and what
   does that tell you about quoting a pipelining speedup without its hardware,
   its server and its workload?
6. Redis is registered level-triggered (`ae_kqueue.c:102-111`). Suppose it were
   edge-triggered instead. Which single line of Step 3 becomes a bug, and what
   would you have to add to `readQueryFromClient` to fix it?

## Done when

Answer each before unfolding it.

- [ ] You can state, without looking, how many syscalls and how many round trips
      one request costs at pipeline depth P, and why the syscall figure in
      `notes.md` is arithmetic rather than a measurement.

<details>
<summary>Answer</summary>

Per *batch* of P requests: the client does one `write()` and one `read()`, the
server does one `read()` and one write (`write()` or `writev()`), and the pair
costs one round trip.
So per *request* it is `2/P` client syscalls, `4/P` total across both processes,
and `1/P` round trips. At P=1 that is 2 / 4 / 1; at P=256 it is 0.0078 / 0.0156
/ 0.0039.

The `notes.md` "syscalls per op" column is `2.0/P` computed from that model —
nobody ran `dtrace`. It is the client-side floor: a real client could do worse
(a partial write, a short read), never better.

</details>

- [ ] You can perform the division that turns 44,088 ops/s into 12,321,414 ops/s
      and say which part of the 279× is *not* explained by the syscall count.

<details>
<summary>Answer</summary>

Throughput is `P / T_batch`.

```
P = 1:    T_batch = 1 / 44,088       = 22.681 µs
P = 256:  T_batch = 256 / 12,321,414 = 20.777 µs
speedup   = 256 × (22.681 / 20.777) = 256 × 1.0916 = 279.4×
```

256× of it is pure amortization: one fixed per-batch cost divided over 256
requests. The remaining **1.09×** is *not* explained by syscall count — it is
that a batch of 256 is itself 8% cheaper to move than a batch of 1, because
larger writes amortize per-byte and per-wakeup costs too. This is why `notes.md`
calls `2/P` a floor on the improvement, not a ceiling.

The payload is irrelevant: 40 bytes at even 10 GB/s is 4 ns, 0.02% of 22.681 µs.
The 22.681 µs is context switches and wakeups.

</details>

- [ ] You can show, arithmetically, why a single-threaded server cannot reach
      1M ops/s without pipelining on this hardware.

<details>
<summary>Answer</summary>

1M ops/s on one thread = a 1000 ns budget per command. This repo measured
`write()` at 857k/s = **1170 ns per call** ([FINDINGS.md](../../FINDINGS.md)
row 5). Unpipelined, the server pays 2 syscalls per command:

```
2 × 1170 ns = 2340 ns  →  2.3× over a 1000 ns budget, before parsing a byte
```

At P=16 the bill is `0.125 × 1170 = 146 ns`, 15% of budget, and the target
becomes reachable. So any "1M ops/s" headline is pipelined, multi-threaded, or
measured on hardware with much cheaper syscalls — and the first question to ask
is what `-P` was.

</details>

- [ ] You can explain why `beforesleep` runs *before* `aeApiPoll` and what would
      break if the flush ran after the dispatch loop instead.

<details>
<summary>Answer</summary>

`beforesleep` (`ae.c:382-383`) is the last thing that happens before the thread
blocks in `aeApiPoll` (`:403`). That is precisely the moment when all replies
generated by the *previous* turn's dispatch are complete and none can still be
appended — so it is the latest possible point at which one write call per
client captures the whole batch.

Running it after the dispatch loop would still batch, but it would sit *before*
the timer-driven work and the async-free queue that also run in `beforeSleep`,
and any reply those produced would wait a full extra loop turn. More
importantly the ordering is what lets redis skip registering a write event at
all: because the flush is guaranteed to happen before the sleep, `addReply` can
merely set a flag and link the client into `clients_pending_write`
(`networking.c:310-311`) instead of calling into the kernel.

</details>

- [ ] You can narrate one full loop iteration with three pipelined clients —
      every syscall, every buffer — and say what changes when a 101st, slow
      client is added.

<details>
<summary>Answer</summary>

One turn, three clients each with a 100-command pipeline in flight:

1. `beforeSleep` → `handleClientsWithPendingWrites`: the pending-write list is
   empty on the first turn, so nothing happens. **0 syscalls.**
2. `aeApiPoll` → one `kevent()` returns 3 ready fds. **1 syscall.**
3. For each of the three fds: `readQueryFromClient` does one read
   (`clientConnRead` → `connRead`) into the borrowed per-thread query buffer, up
   to whatever the buffer holds. **3 syscalls.**
4. Each `processInputBuffer` parses up to `lookahead` (16) commands into
   `pending_cmds`, calls `prefetchCommands()` on the batch, executes them one at
   a time, and loops back to parse the next 16 — 100 commands per client, **0
   syscalls**. Each `addReply` appends into `c->buf` (16 KiB) and flags the
   client once.
5. Next turn's `beforeSleep`: three clients on the pending list, one write call
   each — a plain `connWrite` of `c->buf` while the replies fit in it, a
   `_writevToClient` gathering `c->buf` and the `c->reply` nodes once they
   spilled. **3 syscalls.**

Total: 7 syscalls for 300 commands, ≈ 0.023 per command.

Add a 101st client that reads slowly: its write returns short, so
`clientHasPendingReplies` is still true and `installClientWriteHandler`
(`networking.c:3099`) registers a *write* event for it — the only situation in
which redis asks the poll about writability. Its unsent bytes accumulate in
`c->reply`. Meanwhile `NET_MAX_WRITES_PER_EVENT` (64 KiB, `server.h:142`) caps
how much it can consume per event so the other 100 still get served. If it never
drains, `closeClientOnOutputBufferLimitReached` (`networking.c:5551`)
disconnects it and bumps `stat_client_outbuf_limit_disconnections` — RESP has no
way to ask it to slow down.

</details>

- [ ] You can name the four conditions guarding the big-argument zero-copy and
      say which two are manufactured, and by which code.

<details>
<summary>Answer</summary>

`networking.c:3548-3551`: (1) not a master/replication client, (2) `qb_pos == 0`
— the bulk starts at the buffer's origin, (3) `bulklen >= PROTO_MBULK_BIG_ARG`
(32 KiB), (4) `querybuf_len == bulklen + 2` — the buffer contains this argument
and *nothing else*.

Condition (2) is manufactured by the parser: on reading a `$<len>` header of at
least 32 KiB, if the unparsed bytes are no more than `len + 2` it trims
everything before the argument, sets `qb_pos = 0` and sizes the buffer for
exactly `len + 2` (`:3501-3527`). Condition (4) is manufactured by the read
side: `readQueryFromClient:4036-4054` sees a big bulk in progress and sets
`readlen` to exactly that argument's remaining bytes, accepting extra `read()`
syscalls to buy the alignment. When all four hold, the query buffer's sds
*becomes* the string object (`createObject(OBJ_STRING,c->querybuf)`, `:3553`)
with the CRLF trimmed by `sdsIncrLen(..., -2)`, and the client is handed a fresh
buffer. When they fail — a pipelined client sent more bytes after the big `SET`
— the code falls to `createStringObject` at `:3567-3568` and pays the copy.

</details>

- [ ] You can say what `TCP_NODELAY` disables, where redis sets it, and why the
      measured lane would be a different experiment without it.

<details>
<summary>Answer</summary>

Nagle's algorithm delays transmitting a small TCP segment while a previous
segment is still unacknowledged, coalescing small writes into fewer packets.
Combined with delayed ACKs on the peer, a request/reply protocol can stall for
tens of milliseconds. `TCP_NODELAY` turns it off.

Redis sets it on **every** client, unconditionally, in `createClient`
(`networking.c:130`, `connEnableTcpNoDelay`), which reaches
`setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, ...)` at `anet.c:258`.

Without it, the kernel would be doing coalescing of its own, and the
measured curve would no longer isolate the cost of *explicit* batching — the
P=1 number would improve for reasons that have nothing to do with the server,
and the 279× would shrink into meaninglessness. `notes.md` records
`TCP_NODELAY` in the baseline header for exactly this reason. Note the trade
redis makes: having told the kernel not to coalesce, it must coalesce itself,
which is what Step 6 is.

</details>

- [ ] You can explain what `lookahead` buys that a one-command-at-a-time loop
      cannot, and name the file that spends it.

<details>
<summary>Answer</summary>

`processInputBuffer` parses up to `server.lookahead` commands (default 16,
`server.h:229`) into `c->pending_cmds` *before* executing any of them
(`networking.c:3843-3848`). That creates a window in which the server knows
which keys the next sixteen commands will touch, so it can issue their
lookups' cache misses concurrently instead of serially:
`resetCommandsBatch()` / `addCommandToBatch()` / `prefetchCommands()` at
`:3932-3934`, declared in `src/memory_prefetch.h:26-28` and implemented in
`src/memory_prefetch.c`. The same file offers the intra-command form,
`dictPrefetchKeys` (`memory_prefetch.c:304`), which MGET and MSET use to warm
up to 16 of their own keys at a time when the cross-command batch has not
already done it.

A one-command-at-a-time loop has no such window — each lookup's cache miss is
on the critical path of the next. The valkey chapter of this topic measures what
that is worth. Note the dependency chain: the cross-command optimization only
exists because a *pipelined client* handed the server sixteen commands in one
read. Pipelining buys syscalls first and cache parallelism second.

</details>

## References

**Code at this repo's pins** — all `redis/redis@e87dda2a4`, verified with
`tools/pinned-source.py`:

- `src/ae.c` (516 lines, read fully) — the loop, the arrays, grow-on-demand.
- `src/ae.h` — `aeFileEvent` (32 B) and `aeFiredEvent` (8 B).
- `src/ae_kqueue.c` — the backend actually compiled on macOS. `ae_epoll.c` is
  not built on your machine; see the c10k chapter of this topic.
- `src/networking.c` — `createClient` `:122`, `putClientInPendingWriteQueue`
  `:295`, `_addReplySegmentsToBufferOrList` `:536`, `_addReplyToBufferOrList`
  `:582`, `addReply` `:642`, `_writevToClient` `:2701`,
  `_writeToClientNonSlave` `:2828`, `writeToClient` `:2949`,
  `handleClientsWithPendingWrites` `:3060`, `resetClientQbufState` `:3109`,
  `processInlineBuffer` `:3229`, `processMultibulkBuffer` `:3380`,
  `processInputBuffer` `:3801`, `readQueryFromClient` `:4004`,
  `closeClientOnOutputBufferLimitReached` `:5551`, `addPendingCommand` `:6053`.
- `src/client_comp.c:714` — `clientConnRead`, a plain `connRead` unless the
  client has an active decompressor.
- `src/server.c` — `dbDictType` `:656` (prefetch callbacks `:667-668`),
  `beforeSleep` `:1989` with the flush at `:2146`, `aeCreateEventLoop` `:3138`,
  `aeSetBeforeSleepProc` `:3274`, `aeMain` `:8747`.
- `src/server.h` — `NET_MAX_WRITES_PER_EVENT` `:142`, `CONFIG_MIN_RESERVED_FDS`
  `:162`, `PROTO_IOBUF_LEN` `:207`, `PROTO_REPLY_CHUNK_BYTES` `:208`,
  `PROTO_INLINE_MAX_SIZE` `:209`, `PROTO_MBULK_BIG_ARG` `:210`,
  `CONFIG_FDSET_INCR` `:226`, `REDIS_DEFAULT_LOOKAHEAD` `:229`,
  `pendingCommandList` `:1423-1428`, `pending_cmds` `:1592`,
  `reqtype`/`multibulklen`/`bulklen` `:1607-1609`, `c->reply` `:1610`,
  `PENDING_CMD_FLAG_INCOMPLETE` `:2882`, `struct pendingCommand` `:2889-2905`.
- `src/anet.c:258` — the `TCP_NODELAY` `setsockopt`, wrapped at `:266`.
- `src/module.c:3173` — `RM_ReplyWithLongLong`, the module path into `addReply`.
- `src/memory_prefetch.h:26-28` — the cross-command batch-prefetch API;
  `:37-38` — `DICT_PREFETCH_MAX_SIZE` and `dictPrefetchKeys`.
- `src/t_string.c:687-710` — `PREFETCH_BATCH_SIZE` and `prefetchKeysBatch`, the
  MGET/MSET caller; `src/dict.h:155-156` — the `prefetchEntryKey` /
  `prefetchEntryValue` callbacks.
- `src/config.c:3652` — the `lookahead` config, min 1, default 16.

**Measured in this repo:**

- [FINDINGS.md](../../FINDINGS.md) row 7 — 44k ops/s at P=1, 12.3M at P=256,
  **279×**, on identical zero-work requests. Full table, machine and date in
  [notes.md](notes.md).
- [FINDINGS.md](../../FINDINGS.md) row 5 — `write()` at **857k/s** (1.17 µs per
  call), the syscall price used throughout Step 4.
