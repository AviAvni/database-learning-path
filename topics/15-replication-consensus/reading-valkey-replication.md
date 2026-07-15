# Valkey replication: ack first, replicate later

The canonical async leader/follower design: ack the client
immediately, ship the command stream best-effort, survive disconnects
with a backlog. Everything Raft pays for, valkey skips — and this
chapter builds each skip as its own concept: the zero-RTT ack, the
command stream, the shared buffer, resumable sync, the opt-in
semi-sync escape hatch, and the failover dance that consensus would
have made unnecessary. Then it hands you the anchor map into
`replication.c` (~5600 lines, sliced, never read linearly).

## The problem in one sentence

Valkey acknowledges a write after **zero** replication round trips —
the client's ack races the replication stream — so a primary that
dies at the wrong moment takes acked writes with it, and every
mechanism in `replication.c` is bookkeeping to make that race cheap,
resumable, and (only if you ask) bounded.

## The concepts, step by step

### Step 1 — async leader/follower: the ack races the stream

Asynchronous replication means the primary executes a write, replies
to the client, and *then* ships the write to replicas — the ack does
not wait for anyone:

```
 client write → primary executes → ack client        ← ZERO repl RTT
                     │
                     ▼
              replication BUFFER (one copy, shared)
               ├──→ replica 1 socket
               ├──→ replica 2 socket
               └──→ backlog (ring view, for partial resync)
```

Contrast Raft (previous chapter): majority ack BEFORE commit, one
round trip plus an fsync on every write. Valkey's price list is the
inverse: write latency is a pure single-node number, replicas are
always some bytes behind (**replication lag** — the repl_lag
experiment measures its floor), and a failover to a lagging replica
silently discards the tail of acked writes. Everything below is the
machinery that manages — never eliminates — that loss window.

### Step 2 — the stream is commands, not pages

What flows to replicas is the *command stream* itself
(statement-based replication): the same RESP commands clients sent,
re-executed by each replica. Nondeterministic commands would diverge
replicas — `SPOP` pops a *random* member, so two replicas executing
it disagree forever. `propagateNow` (server.c:3609) is the fix:
rewrite nondeterminism before it enters the stream (SPOP → SREM of
the specific member the primary chose). This is topic 5's
logical-vs-physical WAL choice, made at the replication layer:
statements are compact and human-readable; physical WAL frames
(what M15 stage 1 ships) are dumb but deterministic by construction
(question 1).

### Step 3 — one buffer, many cursors

N replicas must each receive the stream, but N private copies of
every write would multiply memory by N. Pre-6.2 valkey did exactly
that — each replica had its own output buffer. Now
(`feedReplicationBufferWithObject`, :352-366; append + wake at :449)
there is ONE shared list of buffer blocks; each replica holds just a
*cursor* (block + offset) into it, and so does the backlog (Step 4).
A slow replica now costs O(1) bookkeeping instead of O(stream) bytes
— same shape as topic 7's client output buffers (question: what
else do the two share?). Blocks are freed once every cursor has
passed them; one stuck replica can still pin the list, which is what
replica output buffer limits are for.

### Step 4 — PSYNC: resumable replication via (replid, offset)

Disconnects are routine, and restarting replication from scratch
(full snapshot) on every blip would be unusable. So the stream is
addressable: every byte has an **offset**, the primary's history has
an id (**replid**), and the backlog (created at :137) keeps the last
N MB of stream in a ring. A reconnecting replica says
`PSYNC <replid> <offset>` and the primary
(`primaryTryPartialResynchronization`, :854) decides:

```
 replid matches (or matches replid2 within second_replid_offset)
 AND offset still inside the backlog ring
   → +CONTINUE: replay backlog from offset      (cheap)
 else
   → +FULLRESYNC: fork, RDB snapshot, then stream   (expensive)
```

```rust
// PSYNC: (replid, offset) is (term, index) with the safety stripped —
// a matching offset is ASSUMED to mean matching history, never checked
fn try_partial_resync(&self, replid: &str, offset: u64) -> Sync {
    let id_ok = replid == self.replid
        || (replid == self.replid2 && offset <= self.second_replid_offset);
    if id_ok && self.backlog.contains(offset) {
        Sync::Continue(self.backlog.since(offset))   // replay the ring: cheap
    } else {
        Sync::Full(self.fork_rdb_snapshot())         // fork + RDB + stream
    }
}
```

`replid2` is the failover trick: a promoted replica keeps its old
primary's replid as replid2, so *siblings* of the old primary can
still partial-resync from the new one. The Raft comparison is exact
and damning: (replid, offset) is (term, index) with the safety
stripped — Raft's consistency check *verifies* that prev_index holds
prev_term before appending; PSYNC just assumes a matching offset
means matching history (question 2: what divergence can it not
detect?).

### Step 5 — full sync and the replica handshake

When partial resync is refused, the primary forks (`syncCommand`,
:1077): the child serializes an RDB snapshot at a frozen
point-in-time (copy-on-write does the freezing — topic 5), while the
parent accumulates new writes in the replication buffer to stream
after the snapshot. The replica side is a textbook nonblocking state
machine driven by the event loop (topic 7), one state per handshake
stage (:3731+):

```
 REPL_STATE_CONNECT → CONNECTING → RECEIVE_PING_REPLY → ...
   → SEND_PSYNC → RECEIVE_PSYNC_REPLY → TRANSFER → CONNECTED
```

Note the brutal step: on full sync the replica flushes its ENTIRE
dataset before loading the RDB. Cost of a too-small backlog, made
visible: one disconnect longer than the ring → fork + full RDB +
full reload (question 2's inequality).

### Step 6 — WAIT: semi-sync as an opt-in, after the fact

`WAIT numreplicas timeout` (:4996) is the bounded-loss escape hatch:
block *the client* until n replicas have acked the primary's current
offset (acks arrive via `REPLCONF ACK`, requested at :4947). The
asymmetry vs consensus is the whole lesson:

```
 WAIT:  execute → ack replicas → unblock client   (write ALREADY applied)
 Raft:  replicate → majority ack → THEN apply/ack
```

WAIT cannot un-apply anything — it only *informs* the client how far
replication got. WAIT returning 1 of 2 means "one replica has it";
it does not mean the surviving topology after a failover contains
that replica (question: can the write still be lost? — yes, walk
it). Raft's commit is a promise about the future; WAIT is a report
about the present.

### Step 7 — failover: the coordination consensus would have given free

`FAILOVER` (:5565) hand-coordinates what Raft's election does
automatically: pause writes → wait for the target replica to catch
up to the primary's offset → send it `PSYNC FAILOVER` (take over the
replid) → demote self to replica. Each step exists to close a loss
window: skip the pause and writes keep racing ahead; skip the
catch-up and the tail of the stream dies with the demotion. And this
is the *manual, graceful* path — an unplanned primary death has no
pause and no catch-up, which is where Step 1's loss window cashes
out. Question: which Raft mechanism replaces this entire dance, and
what does it cost per write?

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| server.c:3609 | `propagateNow` — the rewrite point | 2 |
| replication.c:352-366 | `feedReplicationBufferWithObject` — one buffer, many readers | 3 |
| replication.c:449 | `feedReplicationBuffer` — append + wake replicas | 3 |
| replication.c:137 | `createReplicationBacklog` — the resync ring | 4 |
| replication.c:854 | `primaryTryPartialResynchronization` — PSYNC accept/deny | 4 |
| replication.c:1077 | `syncCommand` — full sync: fork + RDB + stream | 5 |
| replication.c:3731+ | replica-side `REPL_STATE_*` handshake machine | 5 |
| replication.c:4564 | `replicaofCommand` — topology is a runtime command | 5 |
| replication.c:4947 | `replicationRequestAckFromReplicas` | 6 |
| replication.c:4996 | `waitCommand` — the semi-sync opt-in | 6 |
| replication.c:5565 | `failoverCommand` — coordinated manual failover | 7 |

Slice, don't read linearly: start at `feedReplicationBuffer` (the
hot path), then `primaryTryPartialResynchronization` (the decision),
then `waitCommand` and `failoverCommand` (the two attempts to buy
back what async gave up).

## Questions for notes.md

1. Replication is statement-shipping after `propagateNow` rewrites —
   what's the analogue of topic 5's logical-vs-physical WAL choice?
2. Backlog sizing: repl-backlog-size vs write rate vs disconnect
   duration — write the inequality for "partial resync succeeds".
3. Chained replication (replica of a replica): how do offsets stay
   coherent down the chain?
4. Why does full sync fork? Connect to topic 5's copy-on-write
   snapshot discussion.
5. For M15 stage 1: which parts of PSYNC do you keep (replid+offset,
   backlog ring, +CONTINUE/+FULLRESYNC) and which do you simplify?

## References

**Code**
- [valkey](https://github.com/valkey-io/valkey) — `src/replication.c`
  (~5600 lines; slice it with the anchor map above rather than reading
  linearly) and `src/server.c` (`propagateNow`, the statement-rewrite
  point)

**Papers**
- None — this is a pure code walk; the consensus counterpoint is
  [reading-raft-paper.md](reading-raft-paper.md)
