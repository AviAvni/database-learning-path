# Viewstamped Replication: same invariants, opposite choices

The other consensus protocol — actually the FIRST (VR 1988 predates
Paxos's publication). Read it AFTER Raft: same invariants, opposite
engineering choices at almost every fork. This chapter builds those
forks one at a time — the vocabulary mapping, deterministic
round-robin leadership instead of elections, logs shipped at view
change instead of repaired after, and (the shocker) durability
without disk. TigerBeetle ships VSR in production, so this is not a
museum piece.

## The problem in one sentence

The same problem as Raft — an acked write must survive any f of
2f+1 nodes dying — but VSR asks how much of Raft's machinery is
*forced* and how much is *chosen*: no randomized timeouts, no votes,
and in the pure protocol **zero fsyncs**, versus Raft's fsync of
`voted_for` and log on every vote and append.

## The concepts, step by step

### Step 1 — same machine, different words

VSR replicates a log through a distinguished node exactly like Raft;
only the names differ. Keep this decoder open for the whole paper:

| Raft | VSR |
|---|---|
| term | view |
| leader | primary |
| election | view change |
| log index | op-number |
| commit_index | commit-number |
| RequestVote / AppendEntries | STARTVIEWCHANGE / DOVIEWCHANGE / PREPARE / PREPAREOK |

A **view** is a numbered epoch with one primary — Raft's term,
eleven years earlier. The protocol splits into three sub-protocols
(normal operation, view change, recovery), and the next three steps
take them in order.

### Step 2 — normal operation: the same wire shape as AppendEntries

Client sends the request to the primary; the primary assigns the
next op-number, appends to its log, and broadcasts PREPARE; each
replica appends and answers PREPAREOK; on f PREPAREOKs (f+1 copies
counting the primary = a majority of 2f+1), the primary commits,
executes, and replies to the client:

```
 client ─► primary: PREPARE(view, op-number, request) ─► replicas
                    ◄─ f × PREPAREOK ─┘
           commit, execute, reply     (1 round trip, same as Raft)
```

Same quorum arithmetic as Raft, same one-round-trip latency. The
differences are all in what happens when this smooth path breaks.

### Step 3 — view change: the next primary is scheduled, not elected

Raft elects: candidates race, randomized timeouts break ties, votes
are persisted. VSR schedules: the primary of view v is simply
**replica v mod n** — deterministic, known to everyone in advance.
Suspecting the primary, replicas send STARTVIEWCHANGE for view v+1;
once a replica has seen f+1 of those, it sends DOVIEWCHANGE — *with
its entire log* — to the scheduled next primary. That primary picks
the best log among the f+1 it received (highest last-normal-view,
then highest op-number — Raft's election restriction, applied after
the fact) and installs it everywhere via STARTVIEW:

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

Note what's missing: no votes, no randomized timeouts, no
split-vote livelock — determinism removed them. The costs traded:
DOVIEWCHANGE ships whole logs (bandwidth per view change, where
Raft's election ships nothing and repairs followers lazily), and a
down node's turn in the rotation forces another view change
(question 1). The safety argument is the same quorum intersection as
Raft's: f+1 logs must include at least one node holding any
committed entry.

### Step 4 — recovery: durability from replication, not disk

The shocker. Raft fsyncs `voted_for` and log entries before
answering — a crashed node reads its promises back from disk. VSR's
pure protocol writes NOTHING to disk: a committed entry lives in
f+1 memories, and the protocol tolerates f failures, so *some*
survivor always remembers it. A crashed replica doesn't trust its
disk at all — it runs the **recovery protocol**: rejoin, send a
RECOVERY message with a nonce (a fresh random number that
distinguishes this recovery from any earlier one — question 4), and
rebuild its state from f+1 responses, rejoining only when caught up.

The catch, stated honestly: "f failures" must mean f *independent*
failures. Whole-cluster power loss is f+1 simultaneous memory wipes
— everything is gone, where fsync-per-write Raft replays its disk
(question 3 makes you construct the exact losing sequence). Which is
why TigerBeetle adds disk back but keeps VSR's recovery *thinking*:
a node that cannot trust its own storage (checksum failure, torn
write) recovers from its peers — a fault model Raft ignores
entirely, since Raft assumes whatever was fsynced reads back
faithfully.

### Step 5 — the forks in the road, side by side

The reason to read this paper is the table — every row is a place
where two correct protocols chose differently, which proves the
choice was engineering, not necessity:

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

The invariants underneath are identical: one primary per
view/term, quorum intersection carries committed entries across
changes, and a committed entry is never lost within the fault
model. What differs is *where each protocol spends*: Raft spends
fsyncs and election randomness; VSR spends view-change bandwidth
and a stricter independence assumption.

## How to read the paper (with the concepts in hand)

Read "Viewstamped Replication Revisited" (2012), not the 1988
original:

- **§1–3 (intro, background, the model)** — skim; Step 1's decoder
  makes it fast.
- **§4 (the protocol)** — the payload. §4.1 normal operation is
  Step 2 — map every message onto the AppendEntries flow you know.
  §4.2 view change is Step 3 — check the `install_view` condensation
  above against the real message rules. §4.3 recovery is Step 4 —
  read for the nonce and for what a recovering replica may NOT do.
- **§5 (pragmatics)** — read §5.1 (efficient recovery) and the
  discussion of when disk is reintroduced; this is where the
  no-disk argument gets its fine print.
- **§6–7 (reconfiguration, discussion)** — skim; membership change
  is Raft §6's joint consensus by another road.

Throughout, keep asking Step 5's question: is this rule forced by
the invariants, or is it a choice? That habit is the transferable
skill — it's how you'll evaluate M15 stage 2's design decisions.

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
