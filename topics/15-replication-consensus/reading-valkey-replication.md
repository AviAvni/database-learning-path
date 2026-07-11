# Valkey replication: ack first, replicate later

The canonical async leader/follower design: ack the client
immediately, ship the command stream best-effort, survive disconnects
with a backlog. Everything Raft pays for, valkey skips — this chapter
reads `replication.c` (~5600 lines, sliced by the anchor map) to see
the price of each skip.

## The mental model

```
 client write → primary executes → ack client        ← ZERO repl RTT
                     │
                     ▼
              replication BUFFER (one copy, shared)
               ├──→ replica 1 socket
               ├──→ replica 2 socket
               └──→ backlog (ring view, for partial resync)
```

The replication stream IS the command stream (statement-based, after
`propagateNow` rewrites nondeterminism, e.g. SPOP → SREM).

## Anchor map

| anchor | what it is |
|---|---|
| replication.c:137 | `createReplicationBacklog` — the resync ring |
| replication.c:352-366 | `feedReplicationBufferWithObject` — one buffer, many readers |
| replication.c:449 | `feedReplicationBuffer` — append + wake replicas |
| replication.c:854 | `primaryTryPartialResynchronization` — PSYNC accept/deny |
| replication.c:1077 | `syncCommand` — full sync: fork + RDB + stream |
| replication.c:3731+ | replica-side `REPL_STATE_*` handshake machine |
| replication.c:4564 | `replicaofCommand` — topology is a runtime command |
| replication.c:4947 | `replicationRequestAckFromReplicas` |
| replication.c:4996 | `waitCommand` — the semi-sync opt-in |
| replication.c:5565 | `failoverCommand` — coordinated manual failover |
| server.c:3609 | `propagateNow` — the rewrite point |

## 1. One buffer, many cursors (`:352-449`)

Pre-6.2 lore: each replica had its own output buffer — N replicas =
N copies of every write. Now one shared block list; each replica and
the backlog hold a *reference* (block + offset). Question: what does
this share with topic 7's client output buffers, and why does a slow
replica now cost O(1) memory instead of O(stream)?

## 2. PSYNC — partial resync (`:854`)

Replica reconnects and says `PSYNC <replid> <offset>`:

```
 replid matches (or matches replid2 within second_replid_offset)
 AND offset still inside the backlog ring
   → +CONTINUE: replay backlog from offset      (cheap)
 else
   → +FULLRESYNC: fork, RDB snapshot, then stream   (expensive)
```

`replid2` is the failover trick: a promoted replica keeps its old
primary's replid as replid2, so *siblings* can partial-resync from
the new primary. Question: why is the pair (replid, offset) exactly
Raft's (term, index) with weaker guarantees? What can it NOT detect
that (prev_index, prev_term) can?

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

## 3. The replica handshake (`:3731+`)

`REPL_STATE_CONNECT → CONNECTING → RECEIVE_PING_REPLY → ... →
SEND_PSYNC → RECEIVE_PSYNC_REPLY → TRANSFER → CONNECTED`. A
textbook nonblocking state machine driven by the event loop (topic
7). Note the replica flushes its ENTIRE dataset on full sync.

## 4. WAIT — bounded loss, opt-in (`:4996`)

`WAIT numreplicas timeout`: block the client until n replicas have
acked `primary_repl_offset`. Acks arrive via `REPLCONF ACK <offset>`
(requested at :4947). Crucial asymmetry vs Raft:

```
 WAIT:  execute → ack replicas → unblock client   (write ALREADY applied)
 Raft:  replicate → majority ack → THEN apply/ack
```

Question: WAIT returns 1 (only 1 of 2 replicas acked in time). What
does the client know? What does it NOT know? Can the write still be
lost on failover?

## 5. Failover (`:5565`)

`FAILOVER` coordinates: pause writes → wait for target replica to
catch up → send it `PSYNC FAILOVER` → demote self. Without the
pause+catchup, acked writes die. Question: which Raft mechanism
replaces this entire dance, and what does it cost per write?

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
