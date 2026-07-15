# The redis event loop: pipelining for free

One thread, one poll syscall per iteration, and two buffering decisions —
parse everything the read buffer holds, write nothing until beforeSleep —
give redis pipelining and reply batching without any dedicated machinery.
This chapter builds the loop step by step — what an event loop even is, why
the handler table is an array, how the read path turns one syscall into 100
command executions, why replies are hoarded instead of written, and where the
whole thing kills a client — then maps each step to the functions. `ae.c` is
~500 lines, so read it fully (a rare luxury); `networking.c` is huge, so
read only the five functions this chapter walks.

## The problem in one sentence

One redis thread must serve 10,000 concurrent connections at ~1M ops/s,
which leaves a budget of roughly one microsecond of CPU per command —
so the design's whole job is to spend as few syscalls (~1–2 µs each) as
possible per command, ideally amortizing one syscall over a hundred
commands.

## The concepts, step by step

### Step 1 — the event loop: one thread, one poll, many sockets

An **event loop** is a single thread that, instead of dedicating itself to
one connection, repeatedly asks the kernel "which of my sockets have data
waiting right now?" and handles exactly those. The asking is one syscall —
`kqueue` on your Mac, `epoll` on Linux — that takes a set of **fds** (file
descriptors: the small integers the OS uses to name open sockets) and
returns only the *ready* ones, costing O(ready), not O(registered).

`aeProcessEvents` (ae.c:360) is one turn of the loop: run the
`beforesleep` callback (:377–378, important in Step 5), then `aeApiPoll`
(:398) — **one syscall collects all ready fds** — then dispatch each ready
fd to its registered handler. Timers ride the same loop: the poll timeout
is set to the time until the nearest timer. The OS backend is chosen at
compile time (`ae_kqueue.c` / `ae_epoll.c`) behind an abstraction of just 4
functions (add/del/poll/name).

Why it matters: 10K blocked threads would cost stacks and context switches
(topic 7 §3, C10K); one loop thread costs one poll syscall per *batch* of
ready events.

### Step 2 — `events[fd]`: the handler table is an array, not a hash map

The loop needs to map each ready fd to its handler and per-connection
state. `aeCreateEventLoop` (ae.c:47) allocates plain arrays indexed by fd —
`events[fd]` — not a hash table, because fds are exactly the keys arrays
love: small dense integers handed out by the OS from the lowest free slot.
`setsize` = maxclients + headroom.

Beyond speed, the array matches fd *semantics*: when a connection closes,
its fd number is immediately reusable by the next `accept()`, and
`events[fd]` is simply overwritten — a `HashMap<fd, handler>` would need
careful delete-before-reinsert to avoid a stale handler firing on the new
connection (question 2 below).

### Step 3 — the read path: one read() becomes N command executions

When a client's fd is readable, `readQueryFromClient` (networking.c:3715)
reads up to 16 KB (`PROTO_IOBUF_LEN`, server.h:188) into the client's
**querybuf** (a per-client input accumulation buffer), then calls
`processInputBuffer` (:3529), which **loops**: parse one complete command,
execute it, repeat until the buffer has no complete command left.

That loop is the entire implementation of **pipelining** (a client sending
many commands without waiting for replies): if the client sent 100 commands
back-to-back, one 16 KB `read()` swallows them all, and the loop executes
all 100 with zero further syscalls.

```rust
// processInputBuffer: drain every COMPLETE command the buffer holds.
// This loop IS pipelining: 100 commands in one read() = 100 executions,
// zero extra syscalls.
fn process_input(&mut self, c: &mut Client) {
    loop {
        match parse_multibulk(&c.querybuf[c.pos..]) {  // *argc, then $len + bytes per arg
            Parsed { cmd, consumed } => {
                c.pos += consumed;
                execute(&cmd, c);                      // addReply BUFFERS, never writes
            }
            Incomplete => break,      // keep the bytes; multibulklen/bulklen remember
        }                             // where we were — resume on the next readable event
    }
    c.querybuf.drain(..c.pos);
    c.pos = 0;
}
```

Why it matters: this is why `redis-benchmark -P 64` is ~10× `-P 1` — same
command work, 1/64th the syscalls.

### Step 4 — the parser: length-prefixed, resumable, zero-copy for big args

`processMultibulkBuffer` (:3117) is the RESP parser, and RESP's design
(topic 7 §1) makes it almost embarrassingly simple: read `*argc` (:3123–
3157 — the argument count, so `argv[]` is sized once), then per argument
read `$len` and then *exactly* len bytes — no scanning of payload bytes,
ever. Three details worth the read:

- **Resumability.** TCP is a byte stream: a command can arrive split across
  two `read()`s. On incomplete input the parser returns, leaves the bytes
  in querybuf, and stores its progress in two fields — `multibulklen` (args
  still expected) and `bulklen` (bytes still expected of the current arg),
  :184–185 — resuming on the next readable event. Your Rust parser's
  partial-input resumption test mirrors exactly this.
- **Big-arg zero-copy.** Args over `PROTO_MBULK_BIG_ARG` (32 KB,
  server.h:191) get the querybuf *repositioned* so the arg's bytes can
  become an sds string object without a copy — zero-copy for large SETs.
- **The inline fallback.** `processInlineBuffer` (:2968) handles
  `PING\r\n` typed into `nc`: scan for newline (:2975), split on spaces —
  the ONLY scanning parser in the path, kept purely for debuggability.

### Step 5 — the write path: replies are hoarded, then flushed once

The surprise: `addReply` (:572) does NOT write to the socket. It appends
the reply bytes to a per-client buffer — a fixed 16 KB chunk first
(`PROTO_REPLY_CHUNK_BYTES`), overflowing into a list of blocks so small
replies never allocate — and flags the client as pending-write.

The actual writing happens at the *top of the next loop iteration*:
beforeSleep (Step 1) calls `handleClientsWithPendingWrites` (:2802), which
walks the pending clients and issues **one `write()` per client for all
replies accumulated this iteration**. A pipeline of 100 GETs = 100 buffered
replies = 1 syscall. Only if the socket's kernel buffer fills does redis
install a write handler and let the poll wake it when writable — the only
time redis uses write events.

Why it matters: batching by loop iteration is the write-side twin of
Step 3 — together they make the syscall count per iteration ~2 per active
client, independent of pipeline depth.

### Step 6 — backpressure: the buffer that grows until the axe falls

Steps 3 and 5 both accumulate unbounded buffers, so redis needs a policy for
clients that produce faster than they consume. Input side: a client
streaming commands faster than execution grows querybuf toward a max — then
is killed. Output side: a slow reader (or one `KEYS *` returning 10M keys)
grows the reply list until `closeClientOnOutputBufferLimitReached` (grep
it) disconnects it. **Buffer-or-die**: RESP has no way to tell a producer
"slow down" (contrast pgwire's portals, topic 7 §4).

Trace what happens when `GRAPH.QUERY` returns 1M rows through a module:
module → RedisModule_ReplyWith* → these same buffers → possibly the axe.

## Where each step lives in the code

Local clone at `~/repos/redis`; `src/ae.c` read fully, `src/networking.c`
only these functions:

| Anchor | What | Step |
|--------|------|------|
| `aeCreateEventLoop` — ae.c:47 | `events[fd]` arrays, setsize | 2 |
| `aeProcessEvents` — ae.c:360 | beforesleep :377–378, `aeApiPoll` :398 | 1 |
| `ae_kqueue.c` / `ae_epoll.c` | 4-function backend abstraction | 1 |
| `readQueryFromClient` — networking.c:3715 | 16 KB read into querybuf | 3 |
| `processInputBuffer` — networking.c:3529 | the pipelining loop | 3 |
| `processMultibulkBuffer` — networking.c:3117 | RESP parse; resumption state :184–185 | 4 |
| `processInlineBuffer` — networking.c:2968 | the `nc` fallback, newline scan :2975 | 4 |
| `addReply` — networking.c:572 | buffer, don't write | 5 |
| `handleClientsWithPendingWrites` — networking.c:2802 | flush in beforeSleep | 5 |
| `closeClientOnOutputBufferLimitReached` (grep) | the axe | 6 |
| server.h:188/189/191 | `PROTO_IOBUF_LEN`, `PROTO_REPLY_CHUNK_BYTES`, `PROTO_MBULK_BIG_ARG` | 3–5 |

Suggested route: ae.c top to bottom (it's ~500 lines), then the read path
(Steps 3–4) as one trace, then the write path (Step 5), then grep for the
limits (Step 6).

## Questions to answer in notes.md

1. Why write in beforeSleep rather than in addReply? Count syscalls for a
   pipeline of 100 GETs both ways.
2. `events[fd]` arrays vs a `HashMap<fd, handler>`: why is the array not just
   faster but *correct* here? (fd reuse semantics after close.)
3. The big-arg zero-copy: what property of sds + querybuf repositioning
   makes it safe? When does it fail (arg spans two reads)?
4. Your tokio server does a write per response future by default — what's
   the tokio equivalent of pending-writes batching? (Hint: buffered writer +
   flush on yield, or explicit corking.)

## Done when

You can narrate one loop iteration with 3 pipelined clients — every syscall,
every buffer — and explain where a 101st slow client changes the story.

## References

**Code**
- [redis](https://github.com/redis/redis) — `src/ae.c` (read fully),
  `src/networking.c` (the five functions above), plus the buffer-size
  constants in `src/server.h`. Local clone at `~/repos/redis`.
