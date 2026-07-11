# Viewstamped Replication: same invariants, opposite choices

The other consensus protocol — actually the FIRST (VR 1988 predates
Paxos's publication). Read it AFTER Raft: same invariants, opposite
engineering choices at almost every fork — deterministic round-robin
leadership instead of elections, logs shipped at view change instead
of repaired after, and (the shocker) no disk required. TigerBeetle
ships VSR in production, so this is not a museum piece.

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

The view change, condensed — note what's missing (no votes, no
randomized timeouts):

```rust
// the next primary is DETERMINED: view mod n. it just needs f+1 logs
fn install_view(&mut self, view: u64, msgs: &[DoViewChange]) {
    assert!(msgs.len() >= self.f + 1);            // quorum intersects commits
    let best = msgs.iter()
        .max_by_key(|m| (m.last_normal_view, m.op_number))
        .unwrap();                                // Raft's election restriction,
    self.log = best.log.clone();                  // applied AFTER the fact —
    self.op_number = best.op_number;              // logs ship at view change,
    self.commit_number =                          // where Raft repairs later
        msgs.iter().map(|m| m.commit_number).max().unwrap();
    self.broadcast(StartView { view, log: &self.log });
}
```

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

## References

**Papers**
- Liskov, Cowling — "Viewstamped Replication Revisited"
  (MIT-CSAIL-TR-2012-021, 2012) — the version to read; the three
  sub-protocols plus the no-disk argument
- Oki, Liskov — "Viewstamped Replication: A New Primary Copy Method"
  (PODC 1988) — optional; the original, for the historical claim

**Code**
- [tigerbeetle](https://github.com/tigerbeetle/tigerbeetle) — VSR in
  production Zig, with the storage-fault model bolted on; `src/vsr/`
  if you want to see the protocol shipped
