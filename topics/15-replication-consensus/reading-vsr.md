# Reading guide — "Viewstamped Replication Revisited" (Liskov & Cowling, 2012)

The other consensus protocol — actually the FIRST (VR 1988 predates
Paxos's publication). Read it AFTER Raft: same invariants, opposite
engineering choices at almost every fork. TigerBeetle ships VSR in
production, so this is not a museum piece.

## Terminology decoder

| Raft | VSR |
|---|---|
| term | view |
| leader | primary |
| election | view change |
| log index | op-number |
| commit_index | commit-number |
| RequestVote / AppendEntries | STARTVIEWCHANGE / DOVIEWCHANGE / PREPARE / PREPAREOK |

## The three sub-protocols

1. **Normal operation**: client → primary → PREPARE to all → wait f
   PREPAREOKs (f+1 including self = majority) → commit → reply.
   Same wire shape as AppendEntries.
2. **View change**: on suspicion, replicas send STARTVIEWCHANGE; on
   f+1, send DOVIEWCHANGE *with their log* to the new primary. The
   new primary picks the best log (highest view, then op-number)
   and installs it via STARTVIEW.
3. **Recovery**: a restarted replica asks the group for state
   instead of reading disk.

## The forks in the road (the reason to read this)

```
 choice              Raft                    VSR (Revisited)
 ─────────────────────────────────────────────────────────────
 who leads next      any up-to-date node     ROUND-ROBIN: view mod n
                     that wins votes         (deterministic!)
 log transfer        new leader repairs      new primary RECEIVES logs
                     followers forward       in DOVIEWCHANGE, picks best
 durability          fsync log before ack    NO DISK REQUIRED — 
                                             durability from replication;
                                             recovery protocol replaces it
 vote persistence    voted_for fsynced       view number in memory;
                                             recovery rejoins carefully
```

The no-disk claim is the shocker: VSR argues f+1 replicas holding an
entry in MEMORY is durable (survives f failures), so fsync per write
is optional. The catch: correlated failures (whole-cluster power
loss) lose everything — which is why TigerBeetle adds disk back but
uses VSR's recovery thinking to handle *corrupted* disks (a fault
model Raft ignores entirely).

## Questions for notes.md

1. Round-robin primary (view mod n): what does this remove from the
   protocol (no vote-splitting, no randomized timeouts) and what
   does it cost (a down node's turn)?
2. DOVIEWCHANGE ships whole logs to the new primary — Raft ships
   nothing at election, repairing later. Bandwidth vs latency: when
   is each better?
3. The no-disk argument: write the failure sequence where VSR-
   without-disk loses committed data but Raft-with-fsync doesn't.
4. Why does the recovery protocol need a nonce?
5. TigerBeetle: which VSR feature makes "disk can lie" (checksum
   fails, torn write) survivable, where Raft's model assumes storage
   is faithful? Connect to topic 5's torn-page discussion.
