# Qdrant's consensus: raft for metadata, replica sets for data

The architectural decision worth studying: qdrant runs Raft over
cluster METADATA only — collection schemas, shard placement, peer
membership. The vectors themselves replicate OUTSIDE raft, through
replica sets with an ack-count knob. This chapter walks
`src/consensus.rs` (the raft-rs driving loop from
[reading-raft-rs.md](reading-raft-rs.md), in production) and the
weaker data-path contract in `lib/collection`.

## The split

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

Why: pushing every vector write through raft = majority RTT + log
fsync per upsert on a bulk-ingest workload. Metadata changes are
rare and MUST be agreed on; point writes are frequent and can
tolerate replica-set semantics with repair. Same call as kafka
(controller raft vs ISR data path).

## Anchor map

| anchor | what it is |
|---|---|
| consensus.rs:36 | `type Node = RawNode<ConsensusStateRef>` |
| consensus.rs:48 | `struct Consensus` — the driving loop owner |
| consensus.rs:537 | the ready loop: tick / step / process |
| consensus.rs:877 | `on_ready` — drain the Ready bundle |
| consensus.rs:885/928/1017 | Ready vs LightReady handling |

## 1. The driving loop (`:537`)

Exactly the raft-rs contract from reading-raft-rs.md, in production:
a thread that selects over {incoming raft messages, proposal
channel, tick timer}, calls `step`/`tick`, then `on_ready`.

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

Question: find where snapshots trigger — what happens when a new
peer joins and the log has been compacted?

## 2. on_ready (`:877-1017`)

Follow the ordering: persist entries → send messages → apply
committed entries (which mutate the consensus state = the cluster
topology map) → advance. `LightReady` (:928) is the
`advance_append` optimization — messages that can go out without
waiting for a fresh persistence round.

## 3. The data path's weaker contract

Shard replication (lib/collection): writes go to all replicas of a
shard; `write_consistency_factor` of them must ack. A replica that
misses writes is marked Dead *via raft* and re-synced (transfer)
before serving again. Question: this is valkey's WAIT plus
membership-through-consensus — which failure mode of plain WAIT does
the raft-managed replica-state machine close, and which remains
(hint: acked-but-not-on-all-replicas writes during a failover race)?

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
