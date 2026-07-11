# Topic 15 notes — replication, consensus & distribution

## Predictions (fill BEFORE implementing raft.rs)

repl_lag baseline (provided, measured 2026-07-10, macOS
F_FULLFSYNC): 2000 × 128 B entries, WAIT-1-style ack per entry:

| follower fsync | entries/s | ack p50 µs | ack p99 µs |
|---|---|---|---|
| every 1 | 339 | 2972.7 | 3506.5 |
| every 8 | 2431 | 18.5 | 3706.3 |
| every 64 | 9665 | 13.6 | 2880.1 |
| never | 18568 | 6.2 | 32.5 |

Topic 5's fsync ladder, now visible as *replication lag*: the p50
drops 160× from every-1 to every-8 (most acks ride a group), but the
p99 stays ~3 ms — someone always pays the F_FULLFSYNC.

| question | prediction | actual |
|---|---|---|
| ticks to first leader, 5 nodes (timeout 10-20, heartbeat 3) | | |
| how often does seed 0..10 hit a split vote (extra term)? | | |
| stale-leader test: how many ticks after heal until logs converge? | | |
| minority leader: does it stay Leader forever during partition? (it hears no higher term...) | | |

## Implementation log

- [ ] raft.rs: tick (election timeout + heartbeats) — election tests
      green
- [ ] receive: RequestVote/Vote — one leader per term across seeds
- [ ] receive: AppendEntries consistency check + truncate;
      AppendResp next_idx repair — replicates_to_all green
- [ ] §5.4.2 commit rule — minority + stale-leader tests green
- [ ] partition_test timeline recorded here:

Surprises / dead ends:

## Questions from the reading guides

### Raft paper (reading-raft-paper.md)

1. Why persist (term, voted_for, log) but not commit_index:
2. Fig 8 without the current-term rule — which intersection fails:
3. Why a leader never overwrites its own entries:
4. Snapshot needs last_included_term because:
5. valkey vs Raft: what async gives up, what it gets back:

### valkey replication.c (reading-valkey-replication.md)

1. Statement-shipping vs WAL-shipping ↔ topic 5 logical/physical:
2. Backlog-size inequality for partial resync success:
3. Chained replication offset coherence:
4. Why full sync forks (COW):
5. M15 stage 1: which PSYNC parts to keep:

### raft-rs (reading-raft-rs.md)

1. Why no fsync/sockets/threads in the library:
2. maybe_commit on matched=[7,5,5,3,2] → commit index:
3. next_idx decrement optimization (§5.3 footnote):
4. advance_append pipelining — what still can't reorder:
5. Ready → M15 stage 2 mapping:

### qdrant consensus (reading-qdrant-consensus.md)

1. 10K upserts/s through raft = ? (use the 3 ms fsync above):
2. Active/Dead/Partial ↔ Progress replicate/probe/snapshot:
3. qdrant vector-read consistency:
4. WAL-through-raft: right for a graph DB? FalkorDB's answer:
5. Storage impl behind ConsensusStateRef:

### VSR (reading-vsr.md)

1. Round-robin primary: removes / costs:
2. DOVIEWCHANGE ships logs vs Raft repairs later — when each wins:
3. VSR-no-disk loses committed data when:
4. Recovery nonce prevents:
5. TigerBeetle's "disk can lie" ↔ topic 5 torn pages:

### DDIA ch. 5/8/9 (reading-ddia-repl.md)

1. {async, semi-sync, raft} × {RYW, monotonic, prefix} matrix:
2. WAIT-1-then-wrong-promotion — which guarantee broke:
3. Terms as fencing tokens in M15's follower:
4. Why FLP doesn't doom Raft in practice:
5. Linearizable reads: lease vs ReadIndex vs quorum — M22 pick:

## Cross-topic threads

- repl_lag IS topic 5's fsync ladder: the follower's durability
  policy becomes the leader's observable ack latency. Consensus
  makes this mandatory (majority must fsync before commit).
- sim.rs = topic 16's DST in miniature: seeded delivery order + no
  wall clock ⇒ every partition bug replays from a u64.
- valkey's replica handshake state machine = topic 7's nonblocking
  event-loop pattern; the shared repl buffer = client output
  buffers.
- (replid, offset) vs (prev_index, prev_term): PSYNC can resume a
  stream but can't DETECT divergence; Raft's consistency check can.
- Hash slots vs ranges = topic 13's problem in disguise: traversals
  (range scans) want locality, uniform load wants hashing.

## M15 log (WAL shipping → Raft)

- [ ] stage 1: M5 WAL streamed over M7 RESP server, PSYNC-shaped
      (replid+offset, backlog ring, +CONTINUE/+FULLRESYNC)
- [ ] WAIT-style ack levels; kill -9 failover: measure acked-write
      loss per ack level
- [ ] stage 2: experiments/raft.rs → the WAL commit path
- [ ] latency: async vs WAIT 1 vs raft, same workload (compare
      against the repl_lag table above)
- [ ] read-path decision (stale follower reads?) recorded for
      M22/M29

## Done when

- All 5 raft tests green across seeds; partition_test timeline
  shows commit freeze + truncation-on-heal; prediction table filled.
- Reading-guide questions answered; M15 stage-1 design sketched.
