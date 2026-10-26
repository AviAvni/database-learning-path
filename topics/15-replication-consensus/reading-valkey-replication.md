# Valkey replication: ack first, replicate later

The canonical async leader/follower design: ack the client
immediately, ship the command stream best-effort, survive disconnects
with a backlog. Everything Raft pays for, valkey skips — and this
chapter builds each skip as its own concept: the zero-RTT ack, the
command stream, the shared buffer, resumable sync, the full-sync
fork, the opt-in semi-sync escape hatch, and the failover dance that
consensus would have made unnecessary. Then it hands you the anchor
map into `replication.c`, sliced, never read linearly.

Every `file:line` below is **valkey at `668d875ca`** (`unstable`,
2026-10-24), the revision in this repo's pin table
(`resources/codebases.md`). Check any of them with `python3
tools/pinned-source.py show valkey src/replication.c -r 688:791`.
`src/replication.c` is **6332** lines, `src/server.c` is 8469 and
`src/t_set.c` is 1714. Several config names and defaults differ
between Redis 6 and Valkey — the ones below are read out of
`src/config.c`, not from any blog.

## The problem in one sentence

Valkey acknowledges a write after **zero** replication round trips —
the client's ack races the replication stream — so a primary that
dies at the wrong moment takes acked writes with it, and every
mechanism in `replication.c` is bookkeeping to make that race cheap,
resumable, and (only if you ask) bounded.

## The concepts, step by step

### Step 1 — async leader/follower: the ack races the stream

> **In:** a client write arriving at a primary with two replicas.
> **Out:** the ordering of ack against replication, the name for the
> gap it creates, and the price list that ordering buys.

**Asynchronous replication** means the primary executes a write,
replies to the client, and *then* ships the write to replicas — the
ack does not wait for anyone:

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
always some bytes behind — **replication lag**, the byte distance
between the primary's stream offset and a replica's acked offset —
and a failover to a lagging replica silently discards the tail of
acked writes.

This topic's `repl_lag` bench prices the other side of that trade.
With WAIT-1 semantics (Step 6) and the follower fsyncing every entry,
throughput is **341 entries/s** and ack p99 is 3889.5 µs; with the
follower never fsyncing, **20,174 entries/s** and p99 64.5 µs. Async
replication is the configuration that does not pay either number on
the client's critical path — it moves the whole ladder off the write
path and into the loss window. Everything below is the machinery that
manages, never eliminates, that window.

### Step 2 — the stream is commands, not pages

> **In:** a `SPOP myset` executed on the primary. **Out:** what
> actually enters the replication stream, the two-layer machinery
> that puts it there, and the WAL analogy.

What flows to replicas is the *command stream* itself
(**statement-based replication**): RESP commands, re-executed by each
replica. Nondeterministic commands would diverge replicas — `SPOP`
pops a *random* member, so two replicas executing it disagree
forever.

The fix is **per command, at the command's own site**, not in a
central rewriter. `spopCommand` calls `setTypePopRandom` to choose
the member and immediately rewrites itself:

```c
// t_set.c — spopCommand, 1024-1030: choose, then rewrite
  1024      /* Pop a random element from the set */
  1025      ele = setTypePopRandom(set);
   ...  // 1026-1028: keyspace notification
  1029      /* Replicate/AOF this command as an SREM operation */
  1030      rewriteClientCommandVector(c, 3, shared.srem, c->argv[1], ele);
```

Line **1030** is the whole idea: the command the replica sees is
`SREM myset <the member the primary actually chose>`, which is
deterministic. The multi-element form is messier and worth reading —
`spopWithCountCommand` rewrites to `DEL`/`UNLINK` when it empties the
set (t_set.c:845-846); otherwise it collects the popped members into
`SREM` argument vectors of at most 1024 members (t_set.c:854), emits
each full vector through `alsoPropagate` — once inside each of its
three pop strategies (t_set.c:894, 916, 977) and once more for the
remainder (t_set.c:992) — and then calls
`preventCommandPropagation(c)` (t_set.c:1004) so the original `SPOP`
never reaches the stream.

Two layers sit below that. `alsoPropagate` (server.c:3922) queues
extra commands; `propagatePendingCommands` (server.c:4000) drains the
queue and wraps a multi-command batch in `MULTI`/`EXEC` (server.c:4028,
4039) so replicas apply it atomically. `propagateNow`
(server.c:3816-3857) is the low-level dispatcher at the bottom — it
does **not** rewrite anything; it fans one already-final command out
to `feedAppendOnlyFile` (:3854), `replicationFeedReplicas` (:3855)
and `clusterFeedSlotExportJobs` (:3856). If you go looking for the
SPOP rewrite in `propagateNow` you will not find it.

This is topic 5's logical-vs-physical WAL choice, made at the
replication layer: statements are compact and human-readable but need
a determinism audit for every command ever added; physical WAL frames
(what M15 stage 1 ships) are dumb but deterministic by construction
(question 1).

### Step 3 — one buffer, many cursors

> **In:** N replicas that each need every byte of the stream.
> **Out:** the data structure that avoids N copies, the three lines
> that hand a replica its cursor, and what a stuck replica costs.

N private copies of every write would multiply memory by N — the
naive design, in which every replica's output buffer holds its own
copy of the stream. Valkey keeps ONE shared list of buffer blocks;
each replica holds a *cursor* (block + offset) into it, and so does
the backlog (Step 4).

`feedReplicationBuffer` (replication.c:688-791) is the hot path;
`feedReplicationBufferWithObject` (:593-606) is the thin wrapper for
`robj` inputs. The cursor handout is the load-bearing part:

```c
// replication.c — feedReplicationBuffer, 757-776 (loop body elided)
   757          while ((ln = listNext(&li))) {
   758              client *replica = ln->value;
   ...  // 759: skip a replica that cannot be fed yet
   760              /* Update shared replication buffer start position. */
   761              if (replica->repl_data->ref_repl_buf_node == NULL) {
   762                  replica->repl_data->ref_repl_buf_node = start_node;
   763                  replica->repl_data->ref_block_pos = start_pos;
   764                  /* Only increase the start block reference count. */
   765                  ((replBufBlock *)listNodeValue(start_node))->refcount++;
   766              }
   767
   768              /* Check output buffer limit only when add new block. */
   769              if (add_new_block) closeClientOnOutputBufferLimitReached(replica, 1);
   ...  // 770-771: end of the replica loop
   772          /* For replication backlog */
   773          if (server.repl_backlog->ref_repl_buf_node == NULL) {
   774              server.repl_backlog->ref_repl_buf_node = start_node;
   775              /* Only increase the start block reference count. */
   776              ((replBufBlock *)listNodeValue(start_node))->refcount++;
```

Lines 761-766 and 773-776 are the same three moves twice: a replica
and the backlog are *the same kind of reader*. Blocks are freed once
every refcount drops; one stuck replica pins the list, which is what
line **769**'s output-buffer-limit kill exists to bound.

Block sizing is worth the arithmetic (replication.c:725-726):

```
  limit = max(repl_backlog_size / 16, PROTO_REPLY_CHUNK_BYTES)
  size  = min(max(len, PROTO_REPLY_CHUNK_BYTES), limit)

  With the default repl-backlog-size = 10 MB (config.c:3650) and
  PROTO_REPLY_CHUNK_BYTES = 16 KB:

    limit = max(10 MB / 16, 16 KB) = max(640 KB, 16 KB) = 640 KB
    a 100-byte write     → size = max(100, 16 KB) = 16 KB, capped
                                  at 640 KB → 16 KB block
    a 2 MB write         → size = max(2 MB, 16 KB) = 2 MB, capped
                                  at 640 KB → 640 KB block

  So small writes are batched into 16 KB blocks (one refcount per
  16 KB of stream, not per write) and a huge write is chopped so no
  single block can pin more than 1/16 of the backlog budget.
```

The append does not itself wake anyone. `prepareReplicasToWrite()`
(replication.c:575-589) is the wake, and `replicationFeedReplicas`
calls it at :863 — *before* any byte of the command is appended. The
command is then serialised once into a RESP staging frame (:876-905)
and handed to `feedReplicationBuffer` in as few calls as possible: one
for a typical command, plus a direct, copy-free call for each bulk
argument longer than `REPL_BULK_INLINE_THRESHOLD` = 256 bytes (:798;
the bypass is :891-897), because below that size one extra `memcpy`
is cheaper than another pass over the replica list. Same shape as topic 7's client output buffers (question:
what else do the two share?).

### Step 4 — PSYNC: resumable replication via (replid, offset)

> **In:** a replica reconnecting after a 30-second network blip.
> **Out:** the two-part identity it presents, the exact inequality
> that decides cheap-vs-expensive, and what the check cannot detect.

Disconnects are routine, and a full snapshot on every blip would be
unusable. So the stream is addressable: every byte has an **offset**,
the primary's history has an id (**replid**, a 40-char hex run id),
and the **backlog** — created in `createReplicationBacklog`
(replication.c:374-385) — keeps the last N bytes of stream as a ring
view over the shared blocks of Step 3. A reconnecting replica sends
`PSYNC <replid> <offset>` and `primaryTryPartialResynchronization`
(:1153) decides.

Two tests, in order. The identity test (:1165-1166): the replid must
match `server.replid`, or match `server.replid2` **and** have
`psync_offset <= server.second_replid_offset`. Then the range test —
this is the inequality to memorise:

```c
// replication.c — primaryTryPartialResynchronization, 1188-1190
  1188      /* We still have the data our replica is asking for? */
  1189      if (!server.repl_backlog || psync_offset < server.repl_backlog->offset ||
  1190          psync_offset > (server.repl_backlog->offset + server.repl_backlog->histlen)) {
```

Read lines 1189-1190 as their negation, the success condition:

```
  backlog->offset  ≤  psync_offset  ≤  backlog->offset + backlog->histlen

  i.e. the requested byte is still inside the ring. Turn it into a
  sizing rule with the defaults (config.c:3650, 3675):

    repl-backlog-size = 10 MB     repl-backlog-ttl = 3600 s

  partial resync succeeds  iff  write_rate × disconnect_seconds
                                  ≤ repl-backlog-size

  At 5 MB/s of replication stream:
      10 MB / 5 MB/s = 2 seconds of tolerable disconnect.
  At 100 KB/s:
      10 MB / 0.1 MB/s = 100 seconds.

  A 30-second blip at 5 MB/s needs 150 MB of backlog to stay cheap.
  The default survives it only if your stream is under 341 KB/s.
```

On success the primary writes `+CONTINUE` (:1234/1236) and replays
the backlog from the requested offset (:1253); on failure
`+FULLRESYNC %s %lld` (:1139) and Step 5's fork.

`replid2` is the failover trick: a promoted replica keeps its old
primary's replid as replid2 with `second_replid_offset` marking where
its own history diverged, so *siblings* of the old primary can still
partial-resync from the new one — but only for offsets at or below
that mark, which is what the `psync_offset > server.second_replid_offset`
refusal at :1166 enforces.

The Raft comparison is exact and damning: `(replid, offset)` is
`(term, index)` with the safety stripped. Raft's consistency check
*verifies* that `prevLogIndex` holds `prevLogTerm` before appending;
PSYNC checks only that the replid matches and the offset is in range —
it never compares the *content* at that offset (question 2: what
divergence can it not detect?).

### Step 5 — full sync: two forks, and a config default that flipped

> **In:** a `+FULLRESYNC` decision. **Out:** the fork, the two
> transports and which one is the default, the codec each replica
> receives, and the exact moment the replica's dataset disappears.

When partial resync is refused, `syncCommand` (:1412) leads to
`startBgsaveForReplication` (:1315). It picks a transport at
:1322-1334:

```
  must_go_diskless = filtered RDB (req & REPLICA_REQ_RDB_MASK)
                     || rdbver != RDB_VERSION
                     || this cohort's codec != dump.rdb's whole-stream codec

  must_go_diskless → socket target (refused if the replicas lack
                     REPLICA_CAPA_EOF, :1327-1330)
  otherwise        → socket iff (mincapa & REPLICA_CAPA_EOF)
                                && server.repl_diskless_sync

  socket → rdbSaveToReplicasSockets()  (:1351)  — diskless
  disk   → rdbSaveBackground()         (:1361)  — dump.rdb, sent verbatim
```

The disk path can only serve replicas that want exactly the bytes of
`dump.rdb` — same version, no filtering, same codec — because it
ships the file as written; every other cohort is pushed to the socket
path so the file on disk stays reusable for the cohorts it fits.

**Both paths fork.** `rdbSaveToReplicasSockets` calls `serverFork` at
rdb.c:4383 and `rdbSaveBackground` at rdb.c:1870. Diskless does not
mean fork-less; it means the child writes the RDB straight into the
replica sockets instead of to a file first. The child serialises a
frozen point-in-time snapshot — copy-on-write does the freezing
(topic 5) — while the parent accumulates new writes in the Step 3
buffer to stream afterwards.

Valkey does have a fork-free snapshot, and full sync does not use it.
`forkless.c` takes an RDB from a background *thread*: it walks the
keyspace through a `bgIterator` created in
`BGITERATOR_CONSISTENCY_START` mode (forkless.c:337-338), which
`bgiteration.h:44-48` defines as a point-in-time iteration — the
client "will see all keys AS THEY EXISTED at the time when the
iterator was created" — with no fork and no copy-on-write. It is
doubly opt-in: `forkless-infrastructure-enabled` (immutable, default
`no`, config.c:3476; `valkey.conf` says it allocates 4 additional
bytes per key) and then `bgsave-default-method forkless` (default
`fork`, config.c:3451). `resolveBgsaveType` (rdb.c:1838-1851) still
falls back to a fork when a loaded module has not declared
`VALKEYMODULE_OPTIONS_HANDLE_FORKLESS`. The choice is made for BGSAVE
and periodic saves through `rdbStartBgsave` (rdb.c:1854-1862);
`startBgsaveForReplication` never goes through it — it calls the two
forking savers directly, and `src/replication.c` contains no reference
to forkless at all. A full sync always forks.

Config defaults, straight out of `src/config.c` — the first one is
the Redis-6 trap:

| config | default | line |
|---|---|---|
| `repl-diskless-sync` | **enabled (1)** | config.c:3460 |
| `repl-diskless-sync-delay` | 5 s | config.c:3585 |
| `repl-diskless-sync-max-replicas` | 0 (no limit) | config.c:3609 |
| `repl-diskless-load` | **disabled** | config.c:3541 |
| `dual-channel-replication-enabled` | no (0) | config.c:3461 |
| `repl-backlog-size` | 10 MB | config.c:3650 |
| `repl-backlog-ttl` | 3600 s | config.c:3675 |
| `repl-compression` | no | config.c:3560 |
| `rdbcompression` | yes (per-string LZF) | config.c:3558 |

Diskless *sync* is on by default; diskless *load* is not. So the
primary streams the RDB without touching its own disk, and the
replica still writes it to a file before loading.

The last two rows decide what crosses the wire. `repl-compression`
takes `no`, `yes` (= LZ4), `lz4` or `zstd` (config.c:197-201) and is a
*ceiling*, negotiated per replica: a replica advertises what it can
decode with `REPLCONF capa lz4` / `capa zstd`, and
`replicaNegotiatedCompressionAlgorithm` (replication.c:151-156) walks
down from the configured codec to the first one the replica accepts,
with plaintext as the floor — so a zstd primary serves an LZ4-only
replica LZ4, and an old replica plaintext. The compressor is
per-replica state (`repl_data->repl_compression`, set up in
`replicaEnableCompressionIfNegotiated`, :225-244) and is switched on
for the command stream right after the plaintext `+CONTINUE`
(:1243-1251). Step 3's shared buffer therefore stays plaintext; each
link compresses its own bytes on the way out, so the CPU cost scales
with the number of replicas. A diskless full sync uses the same
negotiated codec; a disk-based one sends `dump.rdb` as written, so its
codec is whatever `rdbcompression` produced — LZF per string by
default, or a whole-stream `lz4`/`zstd` frame (config.c:181-186) —
and `syncCommand` refuses a replica that can take neither a diskless
sync nor that codec (:1532-1536). Step 2's argument is untouched:
what is compressed is still the statement stream, re-executed by the
replica after decoding.

The replica side is a nonblocking state machine driven by the event
loop (topic 7). The states are `server.h:405-423` — fifteen values,
`REPL_STATE_NONE` through `REPL_STATE_CONNECTED`, with the handshake
sub-range explicitly bracketed by comments at :409 and :420 — and the
driver is `syncWithPrimary` (replication.c:4624-4876), preceded by an
ASCII state diagram in the comment just above it. (`replication.c:4259`
is a *different*, dual-channel variant,
`dualChannelSetupMainConnForPsync`; do not read it as the main path.)

```
 REPL_STATE_CONNECT → CONNECTING → RECEIVE_PING_REPLY → SEND_HANDSHAKE
   → RECEIVE_AUTH_REPLY → RECEIVE_PORT_REPLY → RECEIVE_IP_REPLY
   → RECEIVE_CAPA_REPLY → RECEIVE_VERSION_REPLY
   → [RECEIVE_NODEID_REPLY, cluster only]
   → SEND_PSYNC → RECEIVE_PSYNC_REPLY → TRANSFER → CONNECTED
```

The brutal step: on full sync the replica flushes its ENTIRE dataset.
Precisely — `emptyData()` runs at `rdb.c:3724-3732`, *after* the RDB
magic and version check passed at :3707-3722, which return
`RDB_INCOMPATIBLE` without clearing anything. So an incompatible RDB
leaves the old data intact; a compatible one wipes it before the
first key is loaded. During the wipe the replica keeps the link alive
by sending bare newlines (`replicationEmptyDbCallback`,
replication.c:2535-2538). Cost of a too-small backlog, made visible:
one disconnect longer than Step 4's inequality → fork + full RDB +
full reload, with a window where the replica holds nothing at all.

### Step 6 — WAIT: semi-sync as an opt-in, after the fact

> **In:** a client that has already received `+OK` for its write.
> **Out:** what WAIT counts, what it provably does not promise, and
> the sibling command that counts something stronger.

`WAIT numreplicas timeout` (:5586-5619) is the bounded-loss escape
hatch: block *the client* until n replicas have acked the primary's
current offset. It tries a non-blocking count first (:5603-5607) and
only then blocks via `blockClientForReplicaAck` (:5614). The offset
it waits for is `getClientWriteOffset` (:5543-5548), i.e. `c->woff` — the
stream position after that client's own last write.

Two mechanisms underneath. `replicationRequestAckFromReplicas`
(:5537-5539) does **not** send anything; it sets
`server.get_ack_from_replicas = 1`, and the comment at :5533-5536
explains why — the actual `REPLCONF GETACK` broadcast is grouped in
`beforeSleep()`, so many waiting clients cost one broadcast. And the
counting rule is `replicationCountAcksByOffset` (:5552-5565): a
replica counts if `repl_state == REPLICA_STATE_ONLINE` **and**
`repl_ack_off >= offset`.

That second condition is the whole lesson. `repl_ack_off` is how many
bytes the replica has *received and processed* — not how many it has
fsynced.

```
 WAIT:  execute → ack replicas → unblock client   (write ALREADY applied)
 Raft:  replicate → majority ack → THEN apply/ack
```

WAIT cannot un-apply anything — it only *informs* the client how far
replication got. `WAIT 1 0` returning 1 means "one replica has these
bytes in memory". It does not mean the bytes are on that replica's
disk, and it does not mean the surviving topology after a failover
contains that replica (question: can the write still be lost? — yes,
walk it). Raft's commit is a promise about the future; WAIT is a
report about the present.

Valkey has the stronger sibling: `WAITAOF` (:5623) counts through
`replicationCountAOFAcksByOffset` (:5569) against `repl_aof_off` —
bytes the replica has fsynced to its AOF. That is the command whose
cost this topic's table actually measures: the 341-vs-20,174
entries/s span is the difference between counting fsynced bytes and
counting received bytes.

### Step 7 — failover: the coordination consensus would have given free

> **In:** an operator who wants to move the primary role without
> losing writes. **Out:** the four documented steps in the order the
> code performs them, and the one that has no unplanned equivalent.

`failoverCommand` (:6171) hand-coordinates what Raft's election does
automatically. The happy path is documented in the function's own
header comment (:6148-6155) — note step 3 precedes step 4, i.e. the
primary demotes *itself* before asking the target to take over:

```
 1. primary initiates a client pause write, stopping replication traffic
 2. primary periodically checks whether any replica has consumed the
    entire replication stream, via acks
 3. once a replica has caught up, the primary itself becomes a replica
 4. primary sends PSYNC FAILOVER to the target, which if accepted makes
    the replica the new primary and starts a sync
```

Each step closes a loss window: skip the pause and writes keep racing
ahead of the catch-up check; skip the catch-up and the tail of the
stream dies with the demotion. `FAILOVER ABORT` (:6177-6186) is the
only escape, because `REPLICAOF` is disabled during a failover, and
`FORCE` skips step 2 — which is precisely opting back into the loss
window. `abortFailover` (:6129-6142) unwinds via
`replicationUnsetPrimary` if the failover had already reached
`FAILOVER_IN_PROGRESS`.

And this is the *manual, graceful* path. An unplanned primary death
has no pause and no catch-up, which is where Step 1's loss window
cashes out. Question: which Raft mechanism replaces this entire
dance, and what does it cost per write?

## Where each step lives in the code

All anchors are valkey at `668d875ca`.

| anchor | what it is | step |
|---|---|---|
| t_set.c:1024-1030 | `spopCommand` — the SPOP→SREM rewrite, at the command's own site | 2 |
| t_set.c:845-846, 854, 894/916/977/992, 1004 | `spopWithCountCommand` — DEL rewrite, SREM batches of ≤1024, propagation suppressed | 2 |
| server.c:3922 / 4000 / 4028, 4039 | `alsoPropagate`, `propagatePendingCommands`, the MULTI/EXEC wrap | 2 |
| server.c:3816-3857 | `propagateNow` — the dispatcher (AOF, replicas, cluster); **not** the rewriter | 2 |
| replication.c:575-589 | `prepareReplicasToWrite` — the actual wake | 3 |
| replication.c:593-606 | `feedReplicationBufferWithObject` — the robj wrapper | 3 |
| replication.c:688-791 | `feedReplicationBuffer` — one buffer, many cursors | 3 |
| replication.c:725-726 | block-size clamp: `backlog/16`, `PROTO_REPLY_CHUNK_BYTES` | 3 |
| replication.c:757-776 | replica cursor and backlog cursor, same three moves | 3 |
| replication.c:836-906 | `replicationFeedReplicas`; sub-replica early return at :846, wake at :863, staging frame :876-905 | 2, 3 |
| replication.c:947-968 | `replicationFeedStreamFromPrimaryStream` — chaining verbatim | 3 |
| replication.c:374-385 | `createReplicationBacklog` — the resync ring | 4 |
| replication.c:1153 | `primaryTryPartialResynchronization` — PSYNC accept/deny | 4 |
| replication.c:1165-1166 | the replid / replid2 identity test | 4 |
| replication.c:1188-1190 | the backlog range inequality | 4 |
| replication.c:1139 / 1234 / 1236 | `+FULLRESYNC` and `+CONTINUE` replies | 4 |
| replication.c:1412 | `syncCommand` — full sync entry point | 5 |
| replication.c:1315, 1322-1334, 1351, 1361 | `startBgsaveForReplication`: transport choice, both forking savers | 5 |
| rdb.c:1870, 4383 | the two `serverFork` calls (disk, sockets) | 5 |
| rdb.c:1838-1862 | `resolveBgsaveType` / `rdbStartBgsave` — forkless BGSAVE, not used by full sync | 5 |
| replication.c:151-156, 225-244, 1243-1251 | per-replica codec negotiation and compressor setup | 5 |
| server.h:405-423 | the 15 `REPL_STATE_*` values, handshake range bracketed | 5 |
| replication.c:4624-4876 | `syncWithPrimary` — the replica-side handshake machine | 5 |
| rdb.c:3707-3732 | version check, *then* `emptyData()` | 5 |
| replication.c:2535-2538 | `replicationEmptyDbCallback` — newlines during the wipe | 5 |
| replication.c:5153 | `replicaofCommand` — topology is a runtime command | 5 |
| replication.c:5537-5539 | `replicationRequestAckFromReplicas` — sets a flag, `beforeSleep` broadcasts | 6 |
| replication.c:5552-5565 | `replicationCountAcksByOffset` — counts *received*, not fsynced | 6 |
| replication.c:5586-5619 | `waitCommand` — the semi-sync opt-in | 6 |
| replication.c:5569 / 5623 | `replicationCountAOFAcksByOffset` / `waitaofCommand` | 6 |
| replication.c:6148-6155 | the FAILOVER happy path, in comments | 7 |
| replication.c:6171 | `failoverCommand` | 7 |
| config.c:3460/3461/3541/3558/3560/3585/3609/3650/3675 | the replication config defaults | 4, 5 |

Slice, don't read linearly: start at `feedReplicationBuffer` (the
hot path), then `primaryTryPartialResynchronization` (the decision,
and its two-test structure), then `waitCommand`/`waitaofCommand` and
`failoverCommand` (the two attempts to buy back what async gave up).

## Questions for notes.md

1. Replication is statement-shipping after the per-command rewrites —
   what's the analogue of topic 5's logical-vs-physical WAL choice?
2. Backlog sizing: repl-backlog-size vs write rate vs disconnect
   duration — write the inequality for "partial resync succeeds".
3. Chained replication (replica of a replica): how do offsets stay
   coherent down the chain?
4. Why does full sync fork? Connect to topic 5's copy-on-write
   snapshot discussion.
5. For M15 stage 1: which parts of PSYNC do you keep (replid+offset,
   backlog ring, +CONTINUE/+FULLRESYNC) and which do you simplify?

## Done when

Answer each before unfolding it.

- [ ] You can explain what "ack first, replicate later" means for a client that received a success reply.

  <details><summary>Answer</summary>

  It means the reply is a statement about one machine. The primary
  executed the write and answered; the bytes then entered the shared
  replication buffer (`feedReplicationBuffer`, replication.c:688-791)
  and will reach replicas whenever their sockets drain.

  If the primary dies in that gap, the write is gone and the client
  was told otherwise. The gap is measured in bytes as replication lag
  — the distance between the primary's stream offset and a replica's
  `repl_ack_off`.

  What it buys is that the write path never contains a network round
  trip or a follower fsync. This topic's bench shows the size of what
  was avoided: forcing a durable follower ack per entry takes
  throughput to 341 entries/s with a 3889.5 µs p99.

  </details>

- [ ] You can name where nondeterministic commands get rewritten, and why it is not one central place.

  <details><summary>Answer</summary>

  At each command's own implementation, because only the command
  knows which random choice it made. `spopCommand` calls
  `setTypePopRandom` (t_set.c:1025) and rewrites itself to `SREM
  <key> <that member>` at t_set.c:1030.
  `spopWithCountCommand` rewrites to `DEL`/`UNLINK` when it empties the
  set (t_set.c:845-846), otherwise emits `SREM`s in batches of at most
  1024 members via `alsoPropagate` (t_set.c:894, 916, 977 inside the
  three pop strategies, 992 for the remainder) and suppresses the
  original with `preventCommandPropagation` (t_set.c:1004).

  `propagateNow` (server.c:3816-3857) is *not* the rewrite point. It
  is the dispatcher that fans an already-final command out to
  `feedAppendOnlyFile` (:3854), `replicationFeedReplicas` (:3855) and
  `clusterFeedSlotExportJobs` (:3856). The public queueing API is
  `alsoPropagate` (server.c:3922), drained by
  `propagatePendingCommands` (server.c:4000), which wraps multi-command
  batches in MULTI/EXEC (:4028, :4039).

  The cost of this design is that determinism is an obligation on
  every command ever added, checked by review rather than by
  construction — which is exactly what a physical WAL avoids.

  </details>

- [ ] You can describe PSYNC's `(replid, offset)` scheme, state the range inequality, and say what the check cannot detect.

  <details><summary>Answer</summary>

  A replica sends `PSYNC <replid> <offset>`.
  `primaryTryPartialResynchronization` (replication.c:1153) applies two
  tests. Identity (:1165-1166): the replid must equal `server.replid`,
  or equal `server.replid2` with `psync_offset <=
  server.second_replid_offset`. Range (:1188-1190, read as its
  negation):

      backlog->offset ≤ psync_offset ≤ backlog->offset + backlog->histlen

  Pass both and the primary replies `+CONTINUE` (:1234/1236); fail
  either and `+FULLRESYNC` (:1139) with Step 5's fork.

  What it cannot detect is content divergence at a matching offset.
  Raft's `AppendEntries` verifies that `prevLogIndex` holds
  `prevLogTerm` before appending; PSYNC compares an id and a byte
  count and assumes the bytes below are identical. `replid2` plus
  `second_replid_offset` is the narrow patch for the one case where
  that assumption predictably breaks — a promoted replica's siblings.

  </details>

- [ ] You can size the replication backlog from a write rate and a tolerable disconnect window.

  <details><summary>Answer</summary>

      write_rate × disconnect_seconds ≤ repl-backlog-size

  The default is 10 MB (`config.c:3650`), with a 3600 s TTL
  (`config.c:3675`) after which an idle backlog is freed entirely.

  At 5 MB/s of replication stream that is 2 seconds of tolerable
  disconnect; at 100 KB/s it is 100 seconds. To survive a 30-second
  blip at 5 MB/s you need 150 MB. Note the rate is *stream* bytes,
  not client bytes — the MULTI/EXEC wrapping and the SPOP→SREM
  rewrites change the size.

  The cost of getting it wrong is not a slow resync, it is a fork
  plus a full RDB plus a full reload, during which (rdb.c:3724-3732)
  the replica has flushed its dataset and holds nothing.

  </details>

- [ ] You can explain why full sync forks, which transport is the default, why forkless save does not change that, and connect it to copy-on-write.

  <details><summary>Answer</summary>

  It forks to freeze a point-in-time snapshot without stopping the
  primary: the child inherits a copy-on-write view of the heap, so
  the parent keeps serving writes and only the modified pages are
  duplicated (topic 5). Meanwhile the parent accumulates the new
  writes in the Step 3 buffer to stream after the snapshot.

  `startBgsaveForReplication` (replication.c:1315) chooses the
  transport at :1322-1334. A cohort that needs a filtered RDB, a
  different RDB version, or a codec other than the one `dump.rdb` is
  written in must go diskless; otherwise it goes diskless when the
  replicas advertise `REPLICA_CAPA_EOF` and `repl_diskless_sync` is
  on. Diskless is `rdbSaveToReplicasSockets` (:1351), disk is
  `rdbSaveBackground` (:1361). **Both fork** — `serverFork` at
  rdb.c:4383 and rdb.c:1870 — so diskless removes the file, not the
  fork.

  Forkless save (a background thread over a point-in-time
  `bgIterator`, forkless.c:337-338) is reachable only through
  `rdbStartBgsave` (rdb.c:1854-1862), i.e. BGSAVE and periodic saves,
  and only after two opt-ins (`forkless-infrastructure-enabled`,
  `bgsave-default-method forkless`). Replication calls the two forking
  savers directly and never mentions forkless, so a full sync always
  forks.

  `repl-diskless-sync` defaults to **enabled** (config.c:3460), unlike
  Redis 6. `repl-diskless-load` is **disabled** (config.c:3541), so
  the replica writes the RDB to a file before loading it.

  </details>

- [ ] You can say precisely what WAIT does and does not guarantee, and name the command that guarantees more.

  <details><summary>Answer</summary>

  `waitCommand` (replication.c:5586-5619) blocks the client until n
  replicas have acked the offset of that client's own last write
  (`getClientWriteOffset`, :5543-5548). `replicationCountAcksByOffset`
  (:5552-5565) counts a replica if it is `REPLICA_STATE_ONLINE` and
  `repl_ack_off >= offset` — bytes **received and processed**, not
  fsynced. The GETACK broadcast is not sent by
  `replicationRequestAckFromReplicas` (:5537-5539) itself; that only
  sets a flag which `beforeSleep()` acts on, so many blocked clients
  cost one broadcast.

  So WAIT does not promise durability on the replica, and it does not
  promise that the acking replica survives the next failover. It is a
  report about the present, after the write was already applied; a
  Raft commit is a promise about the future, made before it was.

  `WAITAOF` (:5623) is the stronger one:
  `replicationCountAOFAcksByOffset` (:5569) counts `repl_aof_off`,
  bytes fsynced to the replica's AOF. That is the axis this topic's
  table measures — 341 entries/s at one follower fsync per entry
  versus 20,174 with none.

  </details>

- [ ] You can list FAILOVER's four steps in the order the code performs them, and say which has no unplanned equivalent.

  <details><summary>Answer</summary>

  From `failoverCommand`'s own header comment
  (replication.c:6148-6155): (1) pause client writes; (2) poll acks
  until some replica has consumed the whole stream; (3) the primary
  makes *itself* a replica; (4) send `PSYNC FAILOVER` to the target,
  which promotes it. Steps 3 and 4 are in that order — the demotion
  precedes the handoff.

  Step 2 is the one with no unplanned equivalent. A crashed primary
  cannot poll acks, so an unplanned failover promotes whatever replica
  the operator or sentinel picks, at whatever offset it had reached.
  `FORCE` opts out of step 2 deliberately and is the same bargain.

  Raft replaces the whole dance with the election restriction: a
  candidate whose log is not up-to-date cannot win a vote, so
  "catch-up before promotion" is enforced by every voter on every
  election rather than by a coordinating primary that may be dead.
  The price is one majority round trip on every write.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  Question 3 is the one with a crisp code answer. A sub-replica gets
  the primary's byte stream proxied *verbatim*:
  `replicationFeedStreamFromPrimaryStream` (replication.c:947-968)
  takes the raw buffer and calls `prepareReplicasToWrite()` (:965) and
  `feedReplicationBuffer()` (:966) on it unchanged. It never
  re-encodes commands.

  That is why offsets stay coherent down a chain — every node in the
  chain is measuring the same byte sequence. The matching guard is at
  `replicationFeedReplicas` :846, which returns early on a node that
  has a primary of its own, so an intermediate replica cannot generate
  its own stream and desynchronise the numbering.

  </details>

## References

**Code**
- [valkey](https://github.com/valkey-io/valkey) at `668d875ca` —
  `src/replication.c` (6332 lines; slice it with the anchor map above
  rather than reading linearly), `src/server.c` (the propagation
  dispatcher), `src/t_set.c` (the SPOP rewrite), `src/rdb.c` (the
  two forking savers and the flush-before-load), `src/forkless.c` and
  `src/bgiteration.h` (the fork-free BGSAVE that full sync does not
  use), `src/server.h` (the `REPL_STATE_*` enum), `src/config.c`
  (every default quoted above)

**Papers**
- None — this is a pure code walk; the consensus counterpoint is
  [reading-raft-paper.md](reading-raft-paper.md)
