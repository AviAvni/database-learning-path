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
`redis/redis@a176d1225`, which is what `tools/pinned-source.py` will hand you:

```
$ tools/pinned-source.py ref redis
redis  redis/redis  a176d1225

$ tools/pinned-source.py show redis src/ae.c -r 360:420
$ tools/pinned-source.py check redis src/networking.c:2802 --contains 'handleClientsWithPendingWrites'
```

You do **not** need a local clone. If you have one and its numbers disagree,
your clone is at a different commit — this file's line numbers are only true at
`a176d1225`. `src/ae.c` is 511 lines, so read it end to end (a rare luxury in
this repo). `src/networking.c` is 5,775 lines, so read only the ten functions
this chapter walks.

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
// redis src/ae.c — aeProcessEvents, 360-413 (the shape of one loop turn)
   360  int aeProcessEvents(aeEventLoop *eventLoop, int flags)
   361  {
   362      int processed = 0, numevents;
// ... 363-376: early-out when neither file nor time events were requested,
//              and compute the poll timeout from the nearest timer ...
   377          if (eventLoop->beforesleep != NULL && (flags & AE_CALL_BEFORE_SLEEP))
   378              eventLoop->beforesleep(eventLoop);
// ... 379-395: tvp = 0 if AE_DONT_WAIT, else time until the earliest timer ...
   396          /* Call the multiplexing API, will return only on timeout or when
   397           * some event fires. */
   398          numevents = aeApiPoll(eventLoop, tvp);
// ... 399-408: zero numevents if file events were not requested; aftersleep ...
   409          for (j = 0; j < numevents; j++) {
   410              int fd = eventLoop->fired[j].fd;
   411              aeFileEvent *fe = &eventLoop->events[fd];
   412              int mask = eventLoop->fired[j].mask;
   413              int fired = 0; /* Number of events fired for current fd. */
```

Three things to notice, in order of how much they will surprise you:

1. **`beforesleep` runs *before* the poll, not after the dispatch** (`:377-378`).
   The name is accurate — it is the last thing that happens before the thread
   goes to sleep in `aeApiPoll`. This is where the entire write path lives
   (Step 6). It is wired up once, at `src/server.c:3069`
   (`aeSetBeforeSleepProc(server.el, beforeSleep)`), and `aeMain(server.el)` at
   `src/server.c:8027` is the whole server's main loop.
2. **One `aeApiPoll` collects *all* ready fds** (`:398`). Not one syscall per
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
   until the nearest timer (`ae.c:388-395`), so redis needs no timer thread and
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

Step 1's dispatch loop does `&eventLoop->events[fd]` (`ae.c:411`). That is a
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
(`src/server.c:2937`), and `CONFIG_FDSET_INCR` is `32 + 96 = 128`
(`src/server.h:143`, `:207`). So at `maxclients 10000`:

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

And it is not even eagerly allocated. Modern redis grows the arrays on demand:

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
(`networking.c:3715`, registered at `:132` inside `createClient`). It sizes a
read, does exactly one, and hands the bytes to the parse-and-execute loop.

The sizing is not just "16 KB":

```c
// redis src/networking.c — readQueryFromClient, 3732 and 3780-3798 (one read, sized up)
  3732      readlen = PROTO_IOBUF_LEN;
// ... 3733-3779: if the next thing on the wire is a >= 32 KB bulk argument,
//                set readlen to exactly the remaining bytes of that argument
//                (Step 5's zero-copy depends on this); otherwise borrow the
//                per-thread reusable query buffer ...
  3780      qblen = sdslen(c->querybuf);
  3781      if (!(c->flags & CLIENT_MASTER) && // master client's querybuf can grow greedy.
  3782          (big_arg || sdsalloc(c->querybuf) < PROTO_IOBUF_LEN)) {
// ... 3783-3791: non-greedy growth for the initial allocation and for big args ...
  3792      } else {
  3793          c->querybuf = sdsMakeRoomFor(c->querybuf, readlen);
  3794
  3795          /* Read as much as possible from the socket to save read(2) system calls. */
  3796          readlen = sdsavail(c->querybuf);
  3797      }
  3798      nread = connRead(c->conn, c->querybuf+qblen, readlen);
```

`PROTO_IOBUF_LEN` is 16 KiB (`server.h:188`) but line 3796 is the real policy:
once the buffer has grown, redis asks for *everything the buffer can hold*,
with the comment stating the motive outright — "to save read(2) system calls".
The read at `:3798` is one syscall, and it is the only one on the read side.

There is a second memory trick here worth noticing. A client with no partial
command in flight does not own a query buffer at all — it *borrows* a
per-thread reusable one (`networking.c:3766-3776`), which it gives back once
the buffer drains. Ten thousand mostly-idle connections therefore do not cost
ten thousand 16 KiB buffers; they cost one per thread plus whatever the few
mid-command clients hold.

Now the loop the bytes flow into. This is the part most descriptions of redis
get wrong, because it changed: `processInputBuffer` is not "parse one command,
execute it, repeat". It is a **two-level** loop — an inner loop that parses up
to `lookahead` commands, then an outer loop that executes the parsed ones:

```c
// redis src/networking.c — processInputBuffer, 3529-3546 and 3563-3567 (the batching)
  3529  int processInputBuffer(client *c) {
  3530      /* We limit the lookahead for unauthenticated connections to 1.
  3531       * This is both to reduce memory overhead, and to prevent errors: AUTH can
  3532       * affect the handling of succeeding commands. Parsing of "large"
  3533       * unauthenticated multibulk commands is rejected, which would cause those
  3534       * commands to incorrectly return an error to the client. */
  3535      const int lookahead = authRequired(c) ? 1 : server.lookahead;
  3536
  3537      /* Keep processing while there is something in the input buffer */
  3538      while ((c->querybuf && c->qb_pos < sdslen(c->querybuf)) ||
  3539             c->pending_cmds.ready_len > 0)
  3540      {
// ... 3541-3562: bail out if the client is blocked, closing, or already has a
//                command in flight; decide whether to parse more ...
  3563          const int parse_more = !c->pending_cmds.ready_len;
  3564
  3565          /* Parse up to lookahead commands only if we don't have enough ready commands */
  3566          while (parse_more && c->pending_cmds.ready_len < lookahead &&
  3567                 c->querybuf && c->qb_pos < sdslen(c->querybuf))
```

`server.lookahead` defaults to **16** (`REDIS_DEFAULT_LOOKAHEAD`,
`server.h:210`; the config is `lookahead`, `config.c:3246`). The parsed
commands land in `c->pending_cmds`, a list of `pendingCommand` structs
(`server.h:1444-1445`), and only then does the outer loop pull them off the
head and execute them one at a time.

Why decouple parsing from execution at all? Because it creates a window in
which redis knows *which keys the next sixteen commands will touch* before it
touches any of them — and can prefetch them:

```c
// redis src/networking.c — processInputBuffer, 3635-3646 (prefetch the parsed batch)
  3635          /* Prefetch the command only when more commands have been parsed and we
  3636           * are in the main thread. If running in an IO thread, prefetch will be
  3637           * deferred until the client is processed by the main thread. Skip prefetch
  3638           * if there are too few commands to avoid meaningless prefetching. */
  3639          if (parse_more && c->running_tid == IOTHREAD_MAIN_THREAD_ID &&
  3640              c->pending_cmds.ready_len > 1)
  3641          {
  3642              /* Prefetch the commands. */
  3643              resetCommandsBatch();
  3644              addCommandToBatch(c);
  3645              prefetchCommands();
  3646          }
```

Those three functions live in `src/memory_prefetch.h:22-24`. This is the same
idea the valkey chapter of this topic takes apart at length — hide DRAM latency
by issuing several dependent lookups' cache misses concurrently — and it is
only *possible* because a pipelined client handed the server sixteen commands
in one read. Pipelining does not merely save syscalls; it hands the engine a
batch to be clever with.

**Pipelining** is the client-side technique of writing many requests without
waiting for each reply. The server needs no feature to support it: the inner
loop at `:3566` drains whatever the buffer holds, and a client that sent 100
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
Per BATCH of P requests, server side:   1 × read()   +  1 × writev() = 2 syscalls
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

Two corrections to numbers you will meet nearby:

- This topic's `README.md` §2 says "`redis-benchmark -P 64` is ~10× `-P 1`".
  The measured lane says **66.2×** (2,919,728 / 44,088). The "~10×" is folklore
  from a different machine and a different server; the number this repo can
  defend is 66.2× on an M3 Pro over loopback with a do-nothing server. Real
  redis will land lower, because at P=64 real work starts to matter — that gap
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
// redis src/networking.c — createClient, 121-135 (nodelay + the 16 KiB reply buffer)
   121  client *createClient(connection *conn) {
   122      client *c = zmalloc(sizeof(client));
   123
// ... 124-127: comment on NULL conn for fake (Lua/AOF) clients ...
   128      if (conn) {
   129          connEnableTcpNoDelay(conn);
   130          if (server.tcpkeepalive)
   131              connKeepAlive(conn,server.tcpkeepalive);
   132          connSetReadHandler(conn, readQueryFromClient);
   133          connSetPrivateData(conn, c);
   134      }
   135      c->buf = zmalloc_usable(PROTO_REPLY_CHUNK_BYTES, &c->buf_usable_size);
```

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

`processMultibulkBuffer` (`networking.c:3117`) is the RESP parser, and RESP's
design (topic 7 §1) makes it nearly trivial: read `*<argc>`, then per argument
read `$<len>` and then *exactly* len bytes. No payload byte is ever scanned or
compared. Four details repay the read.

**The argument count sizes `argv` once.**

```c
// redis src/networking.c — processMultibulkBuffer, 3142-3165 (parse *argc, size argv)
  3142          serverAssertWithInfo(c,NULL,c->querybuf[c->qb_pos] == '*');
// ... 3143-3152: string2ll the count; reject non-numeric, > INT_MAX, and
//                > 10 args from an unauthenticated client ...
  3153          c->qb_pos = (newline-c->querybuf)+2;
  3154
  3155          if (ll <= 0) return C_OK;
  3156
  3157          c->multibulklen = ll;
  3158          c->bulklen = -1;
  3159
  3160          /* Setup argv array on pending command structure.
  3161           * Reallocate argv array when the requested size is greater than current size. */
  3162          if (c->multibulklen > pcmd->argv_len) {
  3163              zfree(pcmd->argv);
  3164              pcmd->argv_len = min(c->multibulklen, 1024);
  3165              pcmd->argv = zmalloc(sizeof(robj*)*(pcmd->argv_len));
```

Note the `min(..., 1024)` at `:3164`: a client claiming a million arguments
does not get a million-pointer allocation up front. The array grows later, as
arguments actually arrive (`:3276-3279`).

**Resumability.** TCP is a byte stream with no message boundaries; a command
can arrive split across two `read()`s in any position — mid-length, mid-payload,
between the `\r` and the `\n`. On incomplete input the parser simply returns
`C_ERR` with no `read_error` set, leaves the bytes in `querybuf`, and stores its
progress in two fields of `struct client`:

```c
// redis src/server.h — struct client, 1459-1461 (the entire parser resume state)
  1459      int reqtype;            /* Request protocol type: PROTO_REQ_* */
  1460      int multibulklen;       /* Number of multi bulk arguments left to read. */
  1461      long bulklen;           /* Length of bulk argument in multi bulk request. */
```

That is it — a request type, a count of arguments still expected, and a count
of bytes still expected of the current argument. Three integers are the entire
suspended state of a half-parsed command, which is why redis can afford to keep
ten thousand of them. (They are reset by `resetClientQbufState`,
`networking.c:2848-2852`.) Your Rust parser's partial-input resumption test is
testing exactly this property.

**Big-argument zero-copy.** For arguments of at least `PROTO_MBULK_BIG_ARG` =
32 KiB (`server.h:191`), redis arranges for the query buffer to contain *only*
that argument, and then hands the buffer itself to the object system instead of
copying out of it:

```c
// redis src/networking.c — processMultibulkBuffer, 3281-3300 (the sds becomes the object)
  3281              /* Optimization: if a non-master client's buffer contains JUST our bulk element
  3282               * instead of creating a new object by *copying* the sds we
  3283               * just use the current sds string. */
  3284              if (!(c->flags & CLIENT_MASTER) &&
  3285                  c->qb_pos == 0 &&
  3286                  c->bulklen >= PROTO_MBULK_BIG_ARG &&
  3287                  querybuf_len == (size_t)(c->bulklen+2))
  3288              {
  3289                  (pcmd->argv)[(pcmd->argc)++] = createObject(OBJ_STRING,c->querybuf);
  3290                  pcmd->argv_len_sum += c->bulklen;
  3291                  c->all_argv_len_sum += c->bulklen;
  3292                  sdsIncrLen(c->querybuf,-2); /* remove CRLF */
// ... 3293-3299: give the client a fresh querybuf, sized for another fat arg
//                unless that would be more than maxmemory/32 ...
  3300                  sdsclear(c->querybuf);
```

Read the four conditions at `:3284-3287` as a specification of when this is
*safe*: not a replication link, the bulk starts at offset 0, it is at least
32 KiB, and the buffer length is *exactly* `bulklen + 2` — the buffer holds this
argument and nothing else. The last condition is not luck. It is manufactured by
the read side: `readQueryFromClient:3739-3752` detects that the next thing on
the wire is a big argument and sets `readlen` to exactly the argument's
remaining bytes, deliberately accepting more `read()` calls to buy the alignment.
The optimization fails, and falls to `createStringObject` at `:3303-3304`, the
moment a pipelined client sends anything after the big `SET` in the same
segment — which is the answer to question 3.

**The inline fallback.** `processInlineBuffer` (`networking.c:2968`) handles
`PING\r\n` typed into `nc`: `strchr` for the newline (`:2975`), then
`sdssplitargs` on the line (`:2992`). It is the only scanning parser anywhere
in the path, it is capped at `PROTO_INLINE_MAX_SIZE` = 64 KiB
(`server.h:190`, checked at `:2979-2981`), and it exists purely so a human with
a terminal can talk to the server. Which form a client is using is decided by a
single byte: `'*'` means multibulk, anything else means inline
(`networking.c:3570-3575`).

### Step 6 — the write path: replies are hoarded, then flushed with one `writev`

> **In:** a batch of executed commands, each of which called `addReply*`.
> **Out:** one gathered `writev()` per client per loop turn, covering every
> reply the client accumulated — not one write per reply.

Here is the surprise. `addReply` does **not** write to the socket:

```c
// redis src/networking.c — addReply, 571-587 (append to a buffer, never write)
   571  /* Add the object 'obj' string representation to the client output buffer. */
   572  void addReply(client *c, robj *obj) {
   573      if (_prepareClientToWrite(c) != C_OK) return;
   574
   575      if (sdsEncodedObject(obj)) {
   576          _addReplyToBufferOrList(c,obj->ptr,sdslen(obj->ptr));
   577      } else if (obj->encoding == OBJ_ENCODING_INT) {
   578          /* For integer encoded strings we just convert it into a string
   579           * using our optimized function, and attach the resulting string
   580           * to the output buffer. */
   581          char buf[32];
   582          size_t len = ll2string(buf,sizeof(buf),(long)obj->ptr);
   583          _addReplyToBufferOrList(c,buf,len);
   584      } else {
   585          serverPanic("Wrong obj->encoding in addReply()");
   586      }
   587  }
```

`_addReplyToBufferOrList` is the two-tier buffer, and its last three lines say
everything:

```c
// redis src/networking.c — _addReplyToBufferOrList, 485 and 517-520 (buffer, then spill)
   485  void _addReplyToBufferOrList(client *c, const char *s, size_t len) {
// ... 486-516: refuse for closing clients, disconnect a replica that replied,
//              account bytes, divert push messages to a separate list ...
   517      size_t reply_len = _addReplyPayloadToBuffer(c, s, len, PLAIN_REPLY);
   518      if (len > reply_len)
   519          _addReplyPayloadToList(c, c->reply, s + reply_len, len - reply_len, PLAIN_REPLY);
   520  }
```

A fixed 16 KiB chunk first (`c->buf`, allocated in `createClient` at
`networking.c:135`, sized `PROTO_REPLY_CHUNK_BYTES` = 16 KiB,
`server.h:189`), and only the overflow goes to a linked list of blocks
(`c->reply`, `server.h:1462`). Small replies — which is nearly all of them —
never allocate.

The client is then merely *flagged*, and the comment explaining why is the best
three sentences in the file:

```c
// redis src/networking.c — putClientInPendingWriteQueue, 282-299 (flag, don't write)
   282  void putClientInPendingWriteQueue(client *c) {
// ... 283-290: skip if already flagged, or if a replica cannot receive yet ...
   291          /* Here instead of installing the write handler, we just flag the
   292           * client and put it into a list of clients that have something
   293           * to write to the socket. This way before re-entering the event
   294           * loop, we can try to directly write to the client sockets avoiding
   295           * a system call. We'll only really install the write handler if
   296           * we'll not be able to write the whole reply at once. */
   297          c->flags |= CLIENT_PENDING_WRITE;
   298          listLinkNodeHead(server.clients_pending_write, &c->clients_pending_write_node);
   299      }
```

"Avoiding a system call" — the system call being avoided is the *registration*
of a write event with kqueue/epoll, plus the extra poll wakeup it would cause.
The write itself happens at the top of the next loop turn, in `beforeSleep`
(`server.c:1857`, flush at `:1998`):

```c
// redis src/networking.c — handleClientsWithPendingWrites, 2802-2843 (one pass, one write each)
  2802  int handleClientsWithPendingWrites(void) {
  2803      listIter li;
  2804      listNode *ln;
  2805      int processed = listLength(server.clients_pending_write);
  2806
  2807      listRewind(server.clients_pending_write,&li);
  2808      while((ln = listNext(&li))) {
  2809          client *c = listNodeValue(ln);
// ... 2810-2834: skip replicas owned by IO threads, protected and closing
//                clients; hand the client to an IO thread if one is available ...
  2835          /* Try to write buffers to the client socket. */
  2836          if (writeToClient(c,0) == C_ERR) continue;
  2837
  2838          /* If after the synchronous writes above we still have data to
  2839           * output to the client, we need to install the writable handler. */
  2840          if (clientHasPendingReplies(c)) {
  2841              installClientWriteHandler(c);
  2842          }
  2843      }
```

And `writeToClient` does not issue one write per buffer either. `_writevToClient`
builds an `iovec` array containing `c->buf` **and** the `c->reply` list nodes and
issues a single scatter-gather write:

```c
// redis src/networking.c — _writevToClient, 2474-2495 (gather buf + list into one writev)
  2474  static int _writevToClient(client *c, ssize_t *nwritten) {
  2475      int iovmax = min(IOV_MAX, c->conn->iovcnt);
  2476      struct iovec iov[iovmax];
  2477      ReplyIOV reply_iov = {iov, iovmax};
  2478
  2479      /* Add c->buf to iov array */
  2480      if (c->bufpos > 0) {
  2481          if (likely(!c->buf_encoded)) {
  2482              /* Non-encoded buffer - add directly */
  2483              iov[reply_iov.iovcnt].iov_base = c->buf + c->sentlen;
  2484              iov[reply_iov.iovcnt].iov_len = c->bufpos - c->sentlen;
  2485              reply_iov.iov_bytes_len += iov[reply_iov.iovcnt++].iov_len;
// ... 2486-2493: the copy-avoidance encoded-buffer path ...
  2494      /* Add c->reply list nodes to iov array */
  2495      if (!replyIOVReachLimit(&reply_iov)) {
```

So: a pipeline of 100 `GET`s produces 100 `addReply` calls, one flag, zero
syscalls during execution, and **one `writev()`** at the top of the next loop
turn. Combine with Step 3 and the server's whole syscall bill for that pipeline
is one `read()` plus one `writev()` — the `4/P` arithmetic of Step 4, with the
server's half of it delivered by these two mechanisms.

Two guards on the batching, both worth knowing:

- `NET_MAX_WRITES_PER_EVENT` = 64 KiB (`server.h:123`) caps how much a single
  normal client may be written per event, so one `KEYS *` over loopback cannot
  starve the other 9,999 clients (`networking.c:2718-2734`). The cap is lifted
  when over `maxmemory`, and for replicas and monitors, whose buffers would
  otherwise grow without bound.
- The writable event is the *exception*, not the rule. `installClientWriteHandler`
  is called only at `:2841`, only when the socket refused the whole reply. Redis
  registers for write readiness only when the kernel has told it, by short
  write, that it must.

Here is the whole design as one Rust sketch. It is **not** redis code — it is
the shape you should be able to reproduce in your own server:

```rust
// ILLUSTRATION — not quoted from redis. The real loop is redis src/ae.c:360
// (aeProcessEvents), the real read side is src/networking.c:3529
// (processInputBuffer), the real flush is src/networking.c:2802
// (handleClientsWithPendingWrites), called from src/server.c:1998.
loop {
    flush_pending_writes(&mut clients);   // beforeSleep: ONE writev per client
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
// redis src/networking.c — readQueryFromClient, 3838-3850 (the query-buffer axe)
  3838      if (!(c->flags & CLIENT_MASTER) &&
// ... 3839-3842: comment — queued MULTI args count toward the same budget ...
  3843          (c->mstate.argv_len_sums + sdslen(c->querybuf) > server.client_max_querybuf_len ||
  3844           (c->mstate.argv_len_sums + sdslen(c->querybuf) > 1024*1024 && authRequired(c))))
  3845      {
  3846          c->read_error = CLIENT_READ_REACHED_MAX_QUERYBUF;
  3847          freeClientAsync(c);
  3848          atomicIncr(server.stat_client_qbuf_limit_disconnections, 1);
  3849          goto done;
  3850      }
```

Note `:3844`: an *unauthenticated* client gets a hard 1 MiB ceiling regardless
of config — a pre-auth client cannot make the server allocate.

Output side: a slow reader, or one that issued `KEYS *` against a 10M-key
database, grows `c->reply` until:

```c
// redis src/networking.c — closeClientOnOutputBufferLimitReached, 5215-5239 (the reply axe)
  5215  int closeClientOnOutputBufferLimitReached(client *c, int async) {
  5216      if (!c->conn) return 0; /* It is unsafe to free fake clients. */
// ... 5217-5221: assert reply_bytes sane; nothing to do if the buffer is empty ...
  5222      if (checkClientOutputBufferLimits(c)) {
  5223          sds client = catClientInfoString(sdsempty(),c);
  5224
  5225          if (async) {
  5226              freeClientAsync(c);
// ... 5227-5235: log the disconnect at LL_WARNING, sync path calls freeClient ...
  5236          sdsfree(client);
  5237          server.stat_client_outbuf_limit_disconnections++;
  5238          return  1;
  5239      }
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
// redis src/module.c — RM_ReplyWithLongLong, 3095-3102 (module replies are addReply)
  3095  /* Send an integer reply to the client, with the specified `long long` value.
  3096   * The function always returns REDISMODULE_OK. */
  3097  int RM_ReplyWithLongLong(RedisModuleCtx *ctx, long long ll) {
  3098      client *c = moduleGetReplyClient(ctx);
  3099      if (c == NULL) return REDISMODULE_OK;
  3100      addReplyLongLong(c,ll);
  3101      return REDISMODULE_OK;
  3102  }
```

So the path is: module → `RM_ReplyWith*` → `addReply*` → `c->buf` (16 KiB) →
`c->reply` list → possibly `closeClientOnOutputBufferLimitReached`. A module
cannot stream, cannot yield, and cannot be told the client is slow. It
materializes the whole reply in the server's memory and hopes. That constraint
— not query planning — is what shapes how a graph module has to paginate.

## Where each step lives in the code

Everything is `redis/redis@a176d1225`. Read `src/ae.c` end to end (511 lines);
from `src/networking.c` (5,775 lines) read only these.

| Anchor | What | Step |
|--------|------|------|
| `aeProcessEvents` — `src/ae.c:360` | one loop turn: `beforesleep` `:377-378`, `aeApiPoll` `:398`, dispatch `:409-413` | 1 |
| `aeApiPoll` — `src/ae_kqueue.c:124` | the single `kevent()` at `:132`/`:135`; the two-pass read/write merge at `:142-173` | 1 |
| `aeSetBeforeSleepProc` — `src/server.c:3069`; `aeMain` — `:8027` | how `beforeSleep` gets wired to the loop | 1, 6 |
| `aeFileEvent` — `src/ae.h:52-57`; `aeFiredEvent` — `:73-76` | 32 B and 8 B per slot | 2 |
| `aeCreateFileEvent` — `src/ae.c:145` | grow-on-demand at `:155-166`, `setsize` cap at `:148` | 2 |
| `aeCreateEventLoop(maxclients+128)` — `src/server.c:2937` | `CONFIG_FDSET_INCR`, `src/server.h:207` | 2 |
| `readQueryFromClient` — `src/networking.c:3715` | one `connRead` at `:3798`, sized at `:3732`/`:3796`; reusable buffer `:3766-3776` | 3 |
| `processInputBuffer` — `src/networking.c:3529` | the lookahead parse loop `:3563-3567`, prefetch `:3639-3646`, execute `:3672` | 3 |
| `REDIS_DEFAULT_LOOKAHEAD 16` — `src/server.h:210` | config `lookahead`, `src/config.c:3246` | 3 |
| `connEnableTcpNoDelay` — `src/networking.c:129` | Nagle off for every client; `setsockopt` at `src/anet.c:258` | 4 |
| `processMultibulkBuffer` — `src/networking.c:3117` | `*argc` `:3142-3165`, big-arg zero-copy `:3281-3300` | 5 |
| `multibulklen` / `bulklen` — `src/server.h:1460-1461` | the entire parser resume state | 5 |
| `processInlineBuffer` — `src/networking.c:2968` | the `nc` fallback; the only scanning parse, `:2975` | 5 |
| `addReply` — `src/networking.c:572` | → `_addReplyToBufferOrList` `:485`, buffer-then-spill `:517-519` | 6 |
| `putClientInPendingWriteQueue` — `src/networking.c:282` | flag, don't write — the comment at `:291-296` | 6 |
| `handleClientsWithPendingWrites` — `src/networking.c:2802` | called from `beforeSleep`, `src/server.c:1998` | 6 |
| `_writevToClient` — `src/networking.c:2474` | one gathered write over `c->buf` + `c->reply` | 6 |
| `NET_MAX_WRITES_PER_EVENT` — `src/server.h:123` | the 64 KiB fairness cap, enforced at `:2718-2734` | 6 |
| query-buffer limit — `src/networking.c:3838-3850` | plus the 1 MiB pre-auth ceiling at `:3844` | 7 |
| `closeClientOnOutputBufferLimitReached` — `src/networking.c:5215` | the output-side axe | 7 |
| `RM_ReplyWithLongLong` — `src/module.c:3097` | modules reply through the same buffers | 7 |

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
3. The big-argument zero-copy at `networking.c:3284-3287` has four conditions.
   For each, construct a client that violates only that one, and say what redis
   does instead. Which of the four is manufactured by the *read* side, and where?
4. Your tokio server writes once per response future by default. What is the
   tokio equivalent of pending-writes batching, and where in your code does the
   flush have to go so that it is the analogue of `beforeSleep` rather than of
   `addReply`?
5. `notes.md` reports 66.2× for P=64 while this topic's `README.md` §2 says
   "~10×". Both cannot be right. Which conditions would make each true, and what
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
server does one `read()` and one `writev()`, and the pair costs one round trip.
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

`beforesleep` (`ae.c:377-378`) is the last thing that happens before the thread
blocks in `aeApiPoll` (`:398`). That is precisely the moment when all replies
generated by the *previous* turn's dispatch are complete and none can still be
appended — so it is the latest possible point at which one `writev()` per client
captures the whole batch.

Running it after the dispatch loop would still batch, but it would sit *before*
the timer-driven work and the async-free queue that also run in `beforeSleep`,
and any reply those produced would wait a full extra loop turn. More
importantly the ordering is what lets redis skip registering a write event at
all: because the flush is guaranteed to happen before the sleep, `addReply` can
merely set a flag and link the client into `clients_pending_write`
(`networking.c:291-298`) instead of calling into the kernel.
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
3. For each of the three fds: `readQueryFromClient` does one `connRead` into the
   borrowed per-thread query buffer, up to whatever the buffer holds. **3
   syscalls.**
4. Each `processInputBuffer` parses up to `lookahead` (16) commands into
   `pending_cmds`, calls `prefetchCommands()` on the batch, executes them one at
   a time, and loops back to parse the next 16 — 100 commands per client, **0
   syscalls**. Each `addReply` appends into `c->buf` (16 KiB) and flags the
   client once.
5. Next turn's `beforeSleep`: three clients on the pending list, one
   `_writevToClient` each, gathering `c->buf` and any `c->reply` nodes into a
   single `writev`. **3 syscalls.**

Total: 7 syscalls for 300 commands, ≈ 0.023 per command.

Add a 101st client that reads slowly: its `writev` returns short, so
`clientHasPendingReplies` is still true and `installClientWriteHandler`
(`networking.c:2841`) registers a *write* event for it — the only situation in
which redis asks the poll about writability. Its unsent bytes accumulate in
`c->reply`. Meanwhile `NET_MAX_WRITES_PER_EVENT` (64 KiB, `server.h:123`) caps
how much it can consume per event so the other 100 still get served. If it never
drains, `closeClientOnOutputBufferLimitReached` (`networking.c:5215`)
disconnects it and bumps `stat_client_outbuf_limit_disconnections` — RESP has no
way to ask it to slow down.
</details>

- [ ] You can name the four conditions guarding the big-argument zero-copy and
      say which one the read path exists to manufacture.

<details>
<summary>Answer</summary>

`networking.c:3284-3287`: (1) not a master/replication client, (2) `qb_pos == 0`
— the bulk starts at the buffer's origin, (3) `bulklen >= PROTO_MBULK_BIG_ARG`
(32 KiB), (4) `querybuf_len == bulklen + 2` — the buffer contains this argument
and *nothing else*.

Condition (4) — and by extension (2) — is manufactured by the read side.
`readQueryFromClient:3739-3752` notices that the next thing on the wire is a big
bulk and sets `readlen` to exactly that argument's remaining bytes, accepting
extra `read()` syscalls to buy the alignment. When it holds, the query buffer's
sds *becomes* the string object (`createObject(OBJ_STRING, c->querybuf)`,
`:3289`) with the CRLF trimmed by `sdsIncrLen(..., -2)`, and the client is
handed a fresh buffer. When it fails — a pipelined client sent more bytes after
the big `SET` — the code falls to `createStringObject` at `:3303-3304` and pays
the copy.
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
(`networking.c:129`, `connEnableTcpNoDelay`), which reaches
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
`server.h:210`) into `c->pending_cmds` *before* executing any of them
(`networking.c:3563-3567`). That creates a window in which the server knows
which keys the next sixteen commands will touch, so it can issue their
lookups' cache misses concurrently instead of serially:
`resetCommandsBatch()` / `addCommandToBatch()` / `prefetchCommands()` at
`:3643-3645`, declared in `src/memory_prefetch.h:22-24`.

A one-command-at-a-time loop has no such window — each lookup's cache miss is
on the critical path of the next. The valkey chapter of this topic measures what
that is worth. Note the dependency chain: this optimization only exists because
a *pipelined client* handed the server sixteen commands in one read. Pipelining
buys syscalls first and cache parallelism second.
</details>

## References

**Code at this repo's pins** — all `redis/redis@a176d1225`, verified with
`tools/pinned-source.py`:

- `src/ae.c` (511 lines, read fully) — the loop, the arrays, grow-on-demand.
- `src/ae.h` — `aeFileEvent` (32 B) and `aeFiredEvent` (8 B).
- `src/ae_kqueue.c` — the backend actually compiled on macOS. `ae_epoll.c` is
  not built on your machine; see the c10k chapter of this topic.
- `src/networking.c` — `createClient` `:121`, `putClientInPendingWriteQueue`
  `:282`, `addReply` `:572`, `_addReplyToBufferOrList` `:485`,
  `_writevToClient` `:2474`, `writeToClient` `:2691`,
  `handleClientsWithPendingWrites` `:2802`, `processInlineBuffer` `:2968`,
  `processMultibulkBuffer` `:3117`, `processInputBuffer` `:3529`,
  `readQueryFromClient` `:3715`, `closeClientOnOutputBufferLimitReached` `:5215`.
- `src/server.c` — `aeCreateEventLoop` `:2937`, `aeSetBeforeSleepProc` `:3069`,
  `beforeSleep` `:1857` with the flush at `:1998`, `aeMain` `:8027`.
- `src/server.h` — `NET_MAX_WRITES_PER_EVENT` `:123`, `CONFIG_MIN_RESERVED_FDS`
  `:143`, `PROTO_IOBUF_LEN` `:188`, `PROTO_REPLY_CHUNK_BYTES` `:189`,
  `PROTO_INLINE_MAX_SIZE` `:190`, `PROTO_MBULK_BIG_ARG` `:191`,
  `CONFIG_FDSET_INCR` `:207`, `REDIS_DEFAULT_LOOKAHEAD` `:210`,
  `multibulklen`/`bulklen` `:1460-1461`.
- `src/anet.c:258` — the `TCP_NODELAY` `setsockopt`, wrapped at `:266`.
- `src/module.c:3097` — `RM_ReplyWithLongLong`, the module path into `addReply`.
- `src/memory_prefetch.h:22-24` — the batch-prefetch API.
- `src/config.c:3246` — the `lookahead` config, min 1, default 16.

**Measured in this repo:**

- [FINDINGS.md](../../FINDINGS.md) row 7 — 44k ops/s at P=1, 12.3M at P=256,
  **279×**, on identical zero-work requests. Full table, machine and date in
  [notes.md](notes.md).
- [FINDINGS.md](../../FINDINGS.md) row 5 — `write()` at **857k/s** (1.17 µs per
  call), the syscall price used throughout Step 4.

**Corrections made to the previous version of this chapter:**

- `multibulklen` / `bulklen` were cited as `server.h:184-185`. They are at
  **`server.h:1460-1461`**; lines 184-191 are the `PROTO_*` size constants.
- "parse one complete command, execute it, repeat" no longer describes
  `processInputBuffer`. It parses up to `lookahead` (default 16) commands into
  `c->pending_cmds` *first*, prefetches their keys, and only then executes —
  `networking.c:3563-3567` and `:3639-3646`.
- "`aeCreateEventLoop` allocates plain arrays … `setsize` = maxclients +
  headroom" was half right. `setsize` is a *cap* (`server.c:2937`,
  `maxclients + 128`); the arrays start at `min(setsize, 1024)` and double on
  demand in `aeCreateFileEvent` (`ae.c:155-166`).
- "reads up to 16 KB" understated it. `PROTO_IOBUF_LEN` is the starting size,
  but `networking.c:3796` resets `readlen` to the whole available buffer —
  "to save read(2) system calls" — and the big-argument path sets it to exactly
  one argument's remaining bytes.
- "one `write()` per client" is now "one `writev()` per client":
  `_writevToClient` (`:2474-2495`) gathers `c->buf` **and** the `c->reply` list
  into a single scatter-gather call.
- "This is why `redis-benchmark -P 64` is ~10× `-P 1`" — the measured lane says
  **66.2×** (2,919,728 / 44,088). The same "~10×" appears in this topic's
  `README.md` §2 and is likewise unsupported by anything measured here.
- The unanchored Rust pseudocode is now marked `ILLUSTRATION` and points at the
  three real functions it compresses.
- Removed: the claim that syscalls cost "~1–2 µs each" as a general fact. The
  only syscall cost this repo has measured is `write()` at 1.17 µs
  ([FINDINGS.md](../../FINDINGS.md) row 5), and that is what Step 4 uses.
- Removed: "Local clone at `~/repos/redis`". There is no clone; use
  `tools/pinned-source.py`, which pins the commit these line numbers are true at.
