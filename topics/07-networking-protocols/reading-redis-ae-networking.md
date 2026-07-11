# The redis event loop: pipelining for free

One thread, one poll syscall per iteration, and two buffering decisions —
parse everything the read buffer holds, write nothing until beforeSleep —
give redis pipelining and reply batching without any dedicated machinery.
`ae.c` is ~500 lines, so read it fully (a rare luxury); `networking.c` is
huge, so read only the five functions this chapter walks.

## 1. ae.c — the whole loop

- `aeCreateEventLoop` — ae.c:47: arrays indexed by fd (`events[fd]`), not a
  hash — fds are small dense integers, the OS hands you the perfect array
  index. `setsize` = maxclients + headroom.
- `aeProcessEvents` — :360: the core. beforesleep callback (:377–378), then
  `aeApiPoll` (:398) — ONE syscall per iteration collects all ready fds.
- Backend selection: `ae_kqueue.c` (your Mac), `ae_epoll.c` (Linux),
  compile-time — the abstraction is 4 functions (add/del/poll/name).
- Timers ride the same loop: poll timeout = time to nearest timer.

## 2. The read path — networking.c

- `readQueryFromClient` — :3715: connection is readable ⇒ read up to 16KB
  (`PROTO_IOBUF_LEN`, server.h:188) into `querybuf`, then parse.
- `processInputBuffer` — :3529: **loop** — parse as many complete commands
  as the buffer holds, executing each. This loop IS pipelining: 100
  commands in one read = 100 executions, zero extra syscalls.
- `processMultibulkBuffer` — :3117: the RESP parser. Read `*argc` (:3123–
  3157), then per arg read `$len` then exactly len bytes. Note
  `PROTO_MBULK_BIG_ARG` (server.h:191, 32KB): big args get the querybuf
  *repositioned* so the arg can become an sds object without a copy —
  zero-copy for large SETs.
- `processInlineBuffer` — :2968: the `nc`-friendly fallback — scan for
  newline (:2975), split on spaces. The ONLY scanning parser in the path.
- Incomplete input ⇒ return, keep bytes in querybuf, wait for the next
  readable event. State lives in `multibulklen`/`bulklen` (:184–185) — your
  Rust parser's resumption test mirrors exactly this.

The read path's shape, in one loop:

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

## 3. The write path — the part that surprises people

- `addReply` — :572: does NOT write to the socket. Appends to the client's
  reply buffer/list and flags the client as pending-write.
- `handleClientsWithPendingWrites` — :2802: called from beforeSleep — walk
  pending clients, `writeToClient` each (one write() syscall for ALL replies
  accumulated this iteration). Socket buffer full ⇒ install a write handler
  and let the loop wake us when writable (the only time redis uses write
  events).
- Reply buffering structure: fixed 16KB buffer first (`PROTO_REPLY_CHUNK_
  BYTES`), overflow into a list of blocks — small replies never allocate.

## 4. Backpressure

Find `closeClientOnOutputBufferLimitReached` (grep it): a slow consumer or
a huge reply grows the list until the configured limit kills the client.
Trace what happens when `GRAPH.QUERY` returns 1M rows through a module:
module → RedisModule_ReplyWith* → these same buffers → possibly the axe.

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
