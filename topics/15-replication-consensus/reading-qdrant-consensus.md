# Qdrant's consensus: raft for metadata, replica sets for data

The architectural decision worth studying: qdrant runs Raft over
cluster METADATA only — collection schemas, shard placement, peer
membership. The vectors themselves replicate OUTSIDE raft, through
replica sets with an ack-count knob. This chapter builds the design
step by step — the split, the arithmetic that forces it, the
production raft-rs driving loop, and the weaker data-path contract —
walking `src/consensus.rs` (the loop from
[reading-raft-rs.md](reading-raft-rs.md), in production) and
`lib/collection`. Assumes both the Raft paper and raft-rs chapters.

## The problem in one sentence

Pushing every vector upsert through Raft costs a majority round trip
plus a log fsync per write — at topic 5's ~1 ms-ish fsync floor
that's a ceiling around **~1K sequential commits/s** against a bulk
ingest doing 10K+ upserts/s — so qdrant routes the 10K/s through a
cheaper path and reserves Raft for the ~1/minute decisions that
must never fork.

## The concepts, step by step

### Step 1 — two planes: what must agree vs what must flow

Split the system's writes by what a disagreement would cost. If two
nodes disagree on *where shard 3 lives* or *whether collection X
exists*, the cluster is broken — routing forks, splits brain. If two
replicas briefly disagree on *one vector's latest value*, a repair
can fix it later. So: a **control plane** (the metadata — low
volume, must never fork → consensus) and a **data plane** (the
vectors — high volume, tolerates repair → replica sets):

```
 ┌─ raft (consensus.rs) ──────────────────────────┐
 │ topology: which peers exist, which shard lives │
 │ where, collection create/drop, replica state   │
 │ (Active/Dead/Partial)             — LOW volume │
 └────────────────────────────────────────────────┘
 ┌─ data path (NO raft) ──────────────────────────┐
 │ point upserts → forwarded to ALL replicas of   │
 │ the shard; ack policy = write_consistency      │
 │ _factor                          — HIGH volume │
 └────────────────────────────────────────────────┘
```

Same call as kafka (controller raft vs ISR data path). The cost:
the system now has TWO consistency stories, and every failure
scenario must be reasoned about across both (Step 5's question).

### Step 2 — the arithmetic that forces the split

Metadata changes happen when an operator creates a collection or a
node dies — call it once a minute. Point upserts arrive at 10K+/s
during ingest. Raft's per-commit price (majority RTT + leader and
follower log fsyncs, serialized by the log) is irrelevant at
1/minute and fatal at 10K/s — batching helps but the log is still
one serialized sequence through one leader, for writes that don't
need a total order in the first place: upserts to different points
commute. Consensus buys a property (one agreed order, no acked-write
loss) that the data plane doesn't need at a price it can't pay —
question 1 makes you run the numbers with topic 5's fsync
measurements.

### Step 3 — the driving loop: raft-rs's contract, in production

`Consensus` (consensus.rs:48) owns `type Node =
RawNode<ConsensusStateRef>` (:36) and runs the loop the raft-rs
chapter promised someone must write: a thread selecting over
{incoming raft messages, a proposal channel, a tick timer}, calling
`step`/`tick`, then draining Ready (:537 the loop, :877 `on_ready`):

```rust
// the whole of consensus.rs, condensed: raft-rs decides, this loop does
fn run(&mut self) {
    loop {
        match self.select_with_timeout(TICK) {
            Recv::RaftMsg(m)  => self.node.step(m).ok(),   // network in
            Recv::Propose(op) => self.node.propose(vec![], op.encode()),
            Recv::Timeout     => self.node.tick(),         // clock in
        }
        if !self.node.has_ready() { continue; }
        let mut rd = self.node.ready();
        self.storage.persist(rd.entries(), rd.hs());       // 1. fsync FIRST
        self.transport.send(rd.take_messages());           // 2. then talk
        for e in rd.take_committed_entries() {
            self.topology.apply(e);      // 3. committed → cluster metadata
        }
        self.node.advance(rd);                             // 4. done
    }
}
```

The "state machine" being replicated is the cluster topology map —
`apply(e)` mutates which peers exist and where shards live. Question:
find where snapshots trigger — what happens when a new peer joins
and the raft log has been compacted?

### Step 4 — on_ready: the ordering rules, obeyed and optimized

Follow `on_ready` (:877-1017) and check the raft-rs contract's
ordering: persist entries + HardState → send messages → apply
committed entries → advance. This is the part the library couldn't
enforce, done right in production — the fsync-before-send rule from
the raft-rs chapter, visible as real code. The optimization:
`LightReady` (:928, vs full Ready handling at :885/:1017) is
raft-rs's `advance_append` split in action — messages that don't
depend on fresh persistence go out without waiting for the fsync
round, pipelining the raft log the way topic 5 group-commits a WAL.

### Step 5 — the data plane: replica sets with a knob, membership by raft

A point upsert goes to ALL replicas of its shard;
`write_consistency_factor` of them must ack before the client does —
valkey's WAIT as a per-write policy (previous chapters' axis: WHO
acks). The twist that makes it better than plain WAIT: **replica
state lives in raft**. A replica that misses writes is marked Dead
*through consensus* — every node agrees it's Dead — and must
complete a shard transfer (re-sync) before becoming Active again:

```
 Active ──(missed writes, marked via raft)──► Dead
   ▲                                            │
   └──(shard transfer completes)── Partial ◄────┘
```

That closes plain WAIT's nastiest hole: valkey can promote a replica
nobody agrees is current, silently dropping acked writes; qdrant's
failover choices are constrained by an agreed replica-state map.
What remains open: a write acked at `write_consistency_factor = 1`
that dies with its only holder during a failover race — the
consensus layer agrees on *who is Dead*, not on *every write* (the
chapter's question 3 territory, and exactly why the capstone's
stage 2 pushes the WAL itself through raft).

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| consensus.rs:36 | `type Node = RawNode<ConsensusStateRef>` | 3 |
| consensus.rs:48 | `struct Consensus` — the driving loop owner | 3 |
| consensus.rs:537 | the ready loop: tick / step / process | 3 |
| consensus.rs:877 | `on_ready` — drain the Ready bundle | 4 |
| consensus.rs:885/928/1017 | Ready vs LightReady handling | 4 |
| lib/collection | shard replication, `write_consistency_factor`, replica states | 5 |

Read order: the loop at :537 with the condensed Rust above in hand,
then `on_ready` checking the 1-2-3-4 ordering, then grep
`lib/collection` for `write_consistency_factor` and the
Active/Dead/Partial state machine. Also hunt the Storage impl behind
`ConsensusStateRef` — where the raft log and HardState actually get
persisted (question 5).

## Questions for notes.md

1. Why is metadata volume low enough for raft but point writes not?
   Estimate: 10K upserts/s × majority fsync (topic 5 numbers) = ?
2. Replica states Active/Dead/Partial — map each to a Raft Progress
   state (replicate/probe/snapshot). Same problem, different layer?
3. What consistency does a qdrant READ get on vectors? Is it
   linearizable? Under what config?
4. For the capstone: M15 puts the WAL itself through raft (stage 2)
   — qdrant chose not to. Which is right for a graph database's
   write volume, and why might FalkorDB's answer differ from
   qdrant's?
5. Where does qdrant persist the raft log and HardState? Find the
   Storage impl behind ConsensusStateRef.

## References

**Code**
- [qdrant](https://github.com/qdrant/qdrant) — `src/consensus.rs`
  (the driving loop; the anchor map above) and `lib/collection`
  (shard replication, `write_consistency_factor`, replica states)
- The library it embeds is [raft-rs](https://github.com/tikv/raft-rs)
  — walked in [reading-raft-rs.md](reading-raft-rs.md)
