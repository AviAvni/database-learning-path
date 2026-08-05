# Qdrant's consensus: raft for metadata, replica sets for data

The architectural decision worth studying: qdrant runs Raft over
cluster METADATA only — collection schemas, shard placement, peer
membership. The vectors themselves replicate OUTSIDE raft, through
replica sets with an ack-count knob. This chapter builds the design
step by step — the split, the arithmetic that forces it, the
production raft-rs driving loop, the *real* ordering inside
`on_ready`, and the weaker data-path contract — walking
`src/consensus.rs` (the loop from
[reading-raft-rs.md](reading-raft-rs.md), in production) and
`lib/collection`. Assumes both the Raft paper and raft-rs chapters.

Every `file:line` below is **qdrant at `44ad62f`**, the revision in
this repo's pin table (`resources/codebases.md`). Check any of them
with `python3 tools/pinned-source.py show qdrant src/consensus.rs -r
926:1007`. At this pin `src/consensus.rs` is 1605 lines.

## The problem in one sentence

Pushing every vector upsert through Raft costs a majority round trip
plus a log fsync per write — and this repo has measured that floor,
not guessed it: topic 5's `F_FULLFSYNC` rung is **337 commits/s** and
topic 15's own `repl_lag` bench gets **341 entries/s** when the
follower fsyncs every entry — so a bulk ingest doing 10K upserts/s
would be running ~29× over the ceiling, and qdrant routes the 10K/s
through a cheaper path while reserving Raft for the ~1/minute
decisions that must never fork.

## The concepts, step by step

### Step 1 — two planes: what must agree vs what must flow

> **In:** every kind of write a vector database takes. **Out:** the
> partition of those writes by the cost of a disagreement, and the
> two consistency stories that partition creates.

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
 │ (11 variants, Step 5)             — LOW volume │
 └────────────────────────────────────────────────┘
 ┌─ data path (NO raft) ──────────────────────────┐
 │ point upserts → forwarded to ALL replicas of   │
 │ the shard; ack policy = write_consistency      │
 │ _factor                          — HIGH volume │
 └────────────────────────────────────────────────┘
```

Same call as kafka (controller raft vs ISR data path). The cost:
the system now has TWO consistency stories, and every failure
scenario must be reasoned about across both — and as Step 5 shows,
the two are not actually independent, because the data path escalates
into consensus when a replica fails.

### Step 2 — the arithmetic that forces the split

> **In:** two write rates — metadata changes and point upserts — and
> this repo's measured fsync ladder. **Out:** the ratio that makes
> one of them affordable through Raft and the other not, computed
> rather than asserted.

Metadata changes happen when an operator creates a collection or a
node dies. Point upserts arrive at 10K+/s during ingest. Put real
numbers on both sides:

```
  Measured floors from this repo (Apple M3 Pro / APFS, 2026-07-28):

    topic 5,  F_FULLFSYNC rung          337 commits/s
    topic 15, repl_lag, fsync-every-entry
                                        341 entries/s   ack p50 2967.0 us
    topic 15, repl_lag, fsync every 64  12,187 entries/s ack p50   14.0 us

  Note the first two agree to ~1%. That is the point: a Raft commit
  with a durable follower ack IS a durable flush, so the consensus
  path cannot beat topic 5's fsync rung.

  Control plane:  1 metadata change / minute = 0.017 ops/s
                  0.017 / 341  =  0.005%  of one node's commit budget

  Data plane:     10,000 upserts/s
                  10,000 / 341  =  29.3x  over the ceiling

  Batching moves the ceiling, not the shape: group-commit at 64 gives
  12,187 entries/s, still 1.2x short of 10K/s with zero headroom for
  the p99 (2133.0 us at that setting), and it does nothing about the
  log being ONE serialized sequence through ONE leader.
```

That last clause is the deeper reason. Even a free fsync would leave
consensus imposing a total order on writes that do not need one:
upserts to different points commute. Consensus buys a property (one
agreed order, no acked-write loss) that the data plane does not need,
at a price it cannot pay. Question 1 makes you redo this with your
own hardware's numbers.

### Step 3 — the driving loop: raft-rs's contract, in production

> **In:** a `RawNode` and a process that must feed it. **Out:** the
> event sources, the batching constants, and the tick-to-milliseconds
> conversion for qdrant's actual config.

`Consensus` (consensus.rs:48) owns `type Node =
RawNode<ConsensusStateRef>` (:36) and runs the loop the raft-rs
chapter promised someone must write. `Consensus::start` (:481-562) is
that loop:

```rust
// ILLUSTRATION — the shape of Consensus::start, not a quote.
// The real loop is consensus.rs:499-561; advance_node is :564-631;
// recv_update's tokio::select! is :633-641; on_ready is :877-902.
loop {
    let raft_messages = self.advance_node(tick_period)?;   // :501
    // ... elapsed-tick bookkeeping, :504-529 ...
    for _ in 0..report_ticks { self.node.tick(); }         // :532-534
    let (stop_consensus, is_idle) = self.on_ready()?;      // :537
    if stop_consensus { return Ok(()); }
    // ... idle-cycle counting, :543-560 ...
}
```

Three things in that loop are worth the read rather than the summary.

**Batching.** `advance_node` (:564-631) drains up to
`RAFT_BATCH_SIZE = 128` events per iteration (:575, :625), waiting
`tick_period` for the first and only `tick_period / 10` for each
subsequent one (:578-585). A conf-change breaks the batch early
(:597-608, :625) because raft-rs allows only one in flight.

**Tick arithmetic.** raft-rs's constants are unitless; qdrant
supplies the unit:

```
  qdrant  config/config.yaml:359   tick_period_ms: 100
  raft-rs config.rs:112-116        heartbeat_tick 2, election_tick 20
  raft-rs config.rs:147-163        election range [election_tick, 2 x election_tick)
  raft-rs raft.rs:2854-2866        draws uniformly from [20, 40)

    heartbeat   = 2 x 100 ms  = 200 ms
    election    = [20, 40) x 100 ms = [2.0 s, 4.0 s)

  The Raft paper (§5.6, Fig 16) recommends 150-300 ms. qdrant's floor
  is 2.0 s — about 7x the paper's ceiling.
```

Why so slack? Because the loop body at :537 does disk work — WAL
appends, snapshot application, WAL compaction (:899) — and a tight
timeout would misfire whenever a write is slow. qdrant says so in the
code: the comment at :509-519 explains that reported ticks are capped
at `election_tick - 5` = 15 (:521-529) precisely so that "if last
iteration of the loop took too long to complete" it does not "trigger
unnecessary leader election."

**What the state machine is.** The replicated state machine is the
cluster topology map — `handle_committed_entries` (:997, :1044)
mutates which peers exist and where shards live. Compaction is at
:899 with `compact_wal_entries: 128` (config.yaml:365), which is what
lets a new peer join from a snapshot rather than replaying history.

### Step 4 — on_ready: the ordering rules, as actually written

> **In:** a `Ready` bundle from `RawNode::ready()`. **Out:** the true
> order of the seven operations qdrant performs on it, which of them
> is a safety requirement, and the one that contradicts the naive
> "persist before send" rule.

`on_ready` (:877-902) is three calls: `process_ready` (:926-1007),
then `process_light_ready` (:1015-1050), then `process_role_change`
(:904-918). Both inner functions open with the same warning comment
(:922, :1011): *"The order of operations in this functions is
critical, changing it might lead to bugs."*

Read the actual order in `process_ready`, because it is **not**
persist-then-send:

```rust
// consensus.rs — process_ready, 939-1005 (logging and error paths elided)
   939          if !ready.messages().is_empty() {
   941              self.send_messages(ready.take_messages());          // 1. SEND
   942          }
   944          if !ready.snapshot().is_empty() {
   950              if let Err(err) = store.apply_snapshot(&snapshot)? {  // 2. snapshot
   953          }
   955          if !ready.entries().is_empty() {
   961                  .append_entries(ready.take_entries())            // 3. log append
   963          }
   965          if let Some(hs) = ready.hs() {
   971                  .set_hard_state(hs.clone())                      // 4. HardState
   973          }
   984          if !ready.persisted_messages().is_empty() {
   990              self.send_messages(ready.take_persisted_messages()); // 5. SEND (gated)
   991          }
   993          let committed_entries = ready.take_committed_entries();
   997          let stop_consensus = handle_committed_entries(...)       // 6. apply
  1005          let light_rd = self.node.advance(ready);                 // 7. advance
```

Line **941 sends before line 961 persists.** That is not a bug and it
is not qdrant being sloppy — it is raft-rs's leader exemption,
consumed correctly. `Ready::messages()` is non-empty only when
`is_persisted_msg` is false, which `RawNode::ready` sets as
`raft.state != StateRole::Leader` (raft-rs raw_node.rs:555, citing
Ongaro's *dissertation* §10.2.1 at :554). So the list drained at 941
is a leader's, and a leader may replicate before its own disk write.

The safety-critical ordering is between **3/4 and 5**:
`persisted_messages()` at 984 is drained *after* the append at 961
and the HardState write at 971. Those are the messages a follower or
candidate sends — the acks and vote responses the leader will count —
and they must not leave before the write. A second ordering
constraint is spelled out in the comment at :996: committed entries
are handled after the HardState save "so that `applied` index is
never bigger than `commit`."

`process_light_ready` (:1015-1050) then does commit index (:1029-1036)
→ send (:1038) → apply (:1040-1045) → `advance_apply` (:1048). This is
raft-rs's `advance_append`/`advance_apply` split in action: entries
were made durable in the first phase, so the second phase's messages
and applies do not wait on another disk round.

Where the fsync actually happens is worth checking yourself, because
neither write in that listing is obviously durable.
`ConsensusOpWal::append_entries`
(lib/storage/src/content_manager/consensus/consensus_wal.rs:160-262)
ends with **one** `self.wal.flush_open_segment()` at :259 — one flush
per Ready batch, not per entry. That is group commit, and it is why
the arithmetic in Step 2 uses the batched rung. The HardState is
separate: `Persistent::save`
(lib/storage/src/content_manager/consensus/persistent.rs:375-384)
serialises `{term, vote, commit}` (the `HardStateDef` at :455-459) as
JSON through `atomicwrites::AtomicFile`, and the only flush in
qdrant's own code is a `BufWriter::flush` at :379 — a userspace
flush. Whether that is durable depends on the `atomicwrites` crate's
temp-file-and-rename policy, not on anything in this tree. That is
question 5's real target.

### Step 5 — the data plane: replica sets with a knob, membership by raft

> **In:** a point upsert for a shard with three replicas. **Out:**
> the ack rule, the real replica-state enum, and the moment the data
> path stops being independent of consensus.

A point upsert goes to ALL replicas of its shard;
`write_consistency_factor` of them must ack before the client does —
valkey's WAIT as a per-write policy (previous chapters' axis: WHO
acks). The rule is `minimal_success_count =
write_consistency_factor.min(replica_count)`
(lib/collection/src/shards/replica_set/update.rs:460), so the factor
is **clamped** to the replica count and cannot make a write fail for
asking more acks than there are replicas.

Both defaults are **1**: `default_replication_factor`
(lib/collection/src/config.rs:223-225) and
`default_write_consistency_factor` (:227-229), matching
`config/config.yaml:211`. Out of the box qdrant is a single-copy
system; the knob only starts meaning something once you raise the
replication factor.

Replica state is not the three-state triangle it is usually drawn as.
`ReplicaState`
(lib/collection/src/shards/replica_set/replica_set_state.rs:100-133)
has **eleven** variants: `Active`, `Dead`, `Partial`, `Initializing`,
`Listener`, `PartialSnapshot`, `Recovery`, `Resharding`,
`ReshardingScaleDown`, `ActiveRead`, `ManualRecovery`. Three
predicates carve them up — `is_active` (:138-153, source of truth),
`is_readable` (:156-171), `is_updatable` (:173-) — and they do not
agree: `ActiveRead` is readable but not a source of truth,
`ReshardingScaleDown` is both. The simplified triangle is a teaching
aid, not the enum:

```
 ILLUSTRATION of the main cycle only — the real enum has 11 variants
 at replica_set_state.rs:100-133

 Active ──(missed writes, marked via raft)──► Dead
   ▲                                            │
   └──(shard transfer completes)── Partial ◄────┘
```

Now the part that makes this better than plain WAIT, and the part
that makes the two planes *not* independent. When a write succeeds on
enough replicas but fails on others, qdrant deactivates the failed
ones **through consensus** and blocks the client until that
deactivation commits (update.rs:530-590). If it does not commit in
`DEFAULT_SHARD_DEACTIVATION_TIMEOUT` = 30 s (update.rs:30) the client
gets an error whose text is the honest summary of the whole design:

```
  "Some replica of shard N failed to apply operation and deactivation
   timed out after 30s. Consistency of this update is not guaranteed.
   Please retry."                            — update.rs:585-586
```

So a qdrant upsert on the happy path pays no consensus cost, and a
qdrant upsert that touches a failing replica pays a full Raft round
trip before it can answer. The escalation is what closes plain WAIT's
nastiest hole — valkey can promote a replica nobody agrees is
current, silently dropping acked writes; qdrant's failover choices
are constrained by an agreed replica-state map.

What remains open: a write acked at `write_consistency_factor = 1`
that dies with its only holder during a failover race — the
consensus layer agrees on *who is Dead*, not on *every write* (the
chapter's question 3 territory, and exactly why the capstone's
stage 2 pushes the WAL itself through raft).

## Where each step lives in the code

All anchors are qdrant at `44ad62f`.

| anchor | what it is | step |
|---|---|---|
| src/consensus.rs:36 | `type Node = RawNode<ConsensusStateRef>` | 3 |
| src/consensus.rs:48 | `struct Consensus` — the driving loop owner | 3 |
| src/consensus.rs:481-562 | `start` — the loop; `on_ready` called at :537 | 3 |
| src/consensus.rs:509-529 | why reported ticks are capped at 15 | 3 |
| src/consensus.rs:564-631 | `advance_node` — batch of 128, conf-change breaks early | 3 |
| src/consensus.rs:633-641 | `recv_update` — the `tokio::select!` | 3 |
| src/consensus.rs:877-902 | `on_ready` — the three-call skeleton | 4 |
| src/consensus.rs:899 | `compact_wal` | 3 |
| src/consensus.rs:904-918 | `process_role_change` | 3 |
| src/consensus.rs:922 / 1011 | "the order of operations ... is critical" | 4 |
| src/consensus.rs:939-1005 | `process_ready` — send, snapshot, entries, HardState, persisted-send, apply, advance | 4 |
| src/consensus.rs:996 | why apply follows the HardState save | 4 |
| src/consensus.rs:1015-1050 | `process_light_ready` — commit index, send, apply, advance_apply | 4 |
| config/config.yaml:359 / 365 | `tick_period_ms: 100`, `compact_wal_entries: 128` | 3 |
| config/config.yaml:211 | `write_consistency_factor: 1` | 5 |
| lib/collection/src/config.rs:223-229 | replication/write-consistency defaults, both 1 | 5 |
| .../replica_set/replica_set_state.rs:100-133 | `ReplicaState` — eleven variants | 5 |
| .../replica_set/replica_set_state.rs:138-171 | `is_active` / `is_readable` — and where they disagree | 5 |
| .../replica_set/update.rs:30 | `DEFAULT_SHARD_DEACTIVATION_TIMEOUT` = 30 s | 5 |
| .../replica_set/update.rs:452-460 | `minimal_success_count` and the clamp | 5 |
| .../replica_set/update.rs:530-590 | deactivate-through-consensus, and the client block | 5 |
| .../consensus/consensus_wal.rs:160-262 | `append_entries`; one `flush_open_segment` at :259 | 4 |
| .../consensus/persistent.rs:375-384 | `save` — HardState via `AtomicFile`, `BufWriter::flush` | 4 |
| .../consensus/persistent.rs:455-459 | `HardStateDef { term, vote, commit }` | 4 |

Read order: the loop at :481 first, then `process_ready` line by line
against raft-rs's `raw_node.rs:553-555` open in another window (that
is where line 941 stops looking wrong), then `update.rs:452-590` for
the data path. Finally hunt the two persistence sites named above —
they are question 5, and one of them has no fsync in qdrant's own
code.

## Questions for notes.md

1. Why is metadata volume low enough for raft but point writes not?
   Estimate: 10K upserts/s × majority fsync (topic 5 numbers) = ?
2. Replica states — map the main ones onto a Raft `Progress` state
   (Replicate/Probe/Snapshot). Same problem, different layer?
3. What consistency does a qdrant READ get on vectors? Is it
   linearizable? Under what config?
4. For the capstone: M15 puts the WAL itself through raft (stage 2)
   — qdrant chose not to. Which is right for a graph database's
   write volume, and why might FalkorDB's answer differ from
   qdrant's?
5. Where does qdrant persist the raft log and HardState, and is
   either write actually durable? Find the flush in each.

## Done when

Answer each before unfolding it.

- [ ] You can state the arithmetic that forces the metadata/data plane split, using measured numbers rather than an estimate.

  <details><summary>Answer</summary>

  The ceiling is this repo's own measurement, not a guess: topic 5's
  `F_FULLFSYNC` rung is 337 commits/s and topic 15's `repl_lag` bench
  gets 341 entries/s with the follower fsyncing every entry. Those
  agree because a Raft commit with a durable follower ack *is* a
  durable flush.

  Control plane at 1 change/minute is 0.017 ops/s — 0.005% of that
  budget. Data plane at 10,000 upserts/s is 29.3× over it. Group
  commit at 64 raises the rung to 12,187 entries/s, still short of
  10K/s once you leave headroom for the 2133.0 µs p99.

  And the ratio is not the whole argument. Raft imposes one
  serialized order through one leader on writes that commute —
  upserts to different points have no ordering requirement at all, so
  even a free fsync would leave the data plane paying for a property
  it does not use.

  </details>

- [ ] You can state qdrant's heartbeat and election timeouts in milliseconds, and explain why they are far above the paper's recommendation.

  <details><summary>Answer</summary>

  `tick_period_ms: 100` (config/config.yaml:359) times raft-rs's
  unitless constants: `heartbeat_tick` 2 and `election_tick` 20
  (raft-rs config.rs:112-116), with the randomized draw taken from
  `[election_tick, 2 × election_tick)` = `[20, 40)` (config.rs:147-163,
  raft.rs:2854-2866). That is a 200 ms heartbeat and a 2.0–4.0 s
  election timeout, against the paper's §5.6 recommendation of
  150–300 ms.

  The reason is in the code. The loop body at consensus.rs:537 does
  disk work — WAL append, snapshot apply, `compact_wal` at :899 — so
  a single iteration can be slow. The comment at :509-519 says
  reported ticks are capped (at `election_tick - 5` = 15, :521-529)
  so that a long iteration does not "trigger unnecessary leader
  election."

  The paper's own framing covers this: §5.6's `broadcastTime ≪
  electionTimeout` puts the fsync inside broadcastTime, so a system
  with slow durable writes must widen the election timeout to match.

  </details>

- [ ] You can describe the real `process_ready` ordering, and say why sending before persisting is correct there.

  <details><summary>Answer</summary>

  consensus.rs:939-1005, in order: send `ready.messages()` (:941),
  apply snapshot (:950), append entries (:961), set HardState (:971),
  soft state (:981), send `ready.persisted_messages()` (:990), handle
  committed entries (:997), `advance` (:1005). Both `process_ready`
  and `process_light_ready` open with "The order of operations in this
  functions is critical" (:922, :1011).

  The send at 941 precedes the append at 961, which contradicts the
  naive rule — and is correct. `Ready::messages()` is non-empty only
  when `is_persisted_msg` is false, and raft-rs sets that as
  `raft.state != StateRole::Leader` (raw_node.rs:555), citing
  Ongaro's dissertation §10.2.1 at :554. A leader may replicate before
  its own disk write, because the commit needs a majority of disks and
  its own is not required to be among them.

  The safety-critical order is 961/971 before 990:
  `persisted_messages()` carries a follower's or candidate's acks and
  vote responses, which are the evidence the leader counts. The second
  constraint is at :996 — apply after the HardState save, so `applied`
  never exceeds `commit`.

  </details>

- [ ] You can say how many replica states qdrant really has, and name a case where "active" and "readable" disagree.

  <details><summary>Answer</summary>

  Eleven, at replica_set_state.rs:100-133: `Active`, `Dead`,
  `Partial`, `Initializing`, `Listener`, `PartialSnapshot`,
  `Recovery`, `Resharding`, `ReshardingScaleDown`, `ActiveRead`,
  `ManualRecovery`. The Active/Dead/Partial triangle is a teaching
  simplification of the main transfer cycle.

  `ActiveRead` is the disagreement: `is_readable` (:156-171) returns
  true for it, `is_active` (:138-153) returns false. Its comment
  (:125) says "Active for readers, Partial for writers" — it can serve
  a query but is not a source of truth for a recovery.
  `ReshardingScaleDown` goes the other way and is true for both.

  The mapping worth writing in notes: `Partial` is Raft's
  `ProgressState::Snapshot` (catching up by bulk transfer), `Active`
  is `Replicate`, and `Dead` has no Raft analogue at all — Raft never
  removes a voter for lagging, it just keeps probing.

  </details>

- [ ] You can explain how a failed replica turns a consensus-free write path into one that blocks on a Raft commit.

  <details><summary>Answer</summary>

  update.rs:530-590. When `successes.len() >= minimal_success_count`
  but some replicas failed, `handle_failed_replicas` (:544) proposes
  their deactivation *through consensus*, and if the client asked for
  a callback the request then blocks on
  `replica_state.wait_for(...)` (:563-579) until every failed peer is
  no longer `can_be_source_of_truth()`.

  The timeout is `DEFAULT_SHARD_DEACTIVATION_TIMEOUT` = 30 s
  (update.rs:30), and on expiry the client gets an explicit
  "Consistency of this update is not guaranteed. Please retry."
  (:585-586).

  So the two planes are not independent. The happy path pays nothing
  for consensus; the failure path pays a full Raft round trip
  *synchronously*, because the alternative — acking a write while some
  replica still claims to be a source of truth without it — is how
  you lose acked data during the next failover. That is precisely the
  hole plain valkey WAIT leaves open.

  </details>

- [ ] You can say where the raft log and HardState are persisted, and whether either write is demonstrably durable.

  <details><summary>Answer</summary>

  The log: `ConsensusOpWal::append_entries`
  (consensus_wal.rs:160-262), which ends with a single
  `self.wal.flush_open_segment()` at :259 — one flush per Ready
  batch, not per entry. That is group commit, and it is the reason
  Step 2's arithmetic uses the batched rung rather than the
  fsync-every-entry one.

  The HardState: `Persistent::save` (persistent.rs:375-384), which
  writes `{term, vote, commit}` plus the ConfState (the `HardStateDef`
  at :455-459) as JSON through `atomicwrites::AtomicFile`.

  Only one of the two is demonstrably durable from this tree. The
  only flush in `save` is `writer.flush()` at :379 — a `BufWriter`
  flush into the file descriptor, not an fsync. Durability there rests
  entirely on what the `atomicwrites` crate does on commit, which is
  outside qdrant's source. Given Figure 2 lists `votedFor` as
  must-be-durable-before-responding, that is the line to go read.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including where the raft log and HardState are persisted.

  <details><summary>Answer</summary>

  Question 3 is the one with no single code answer, which is itself
  the finding. A read served by a replica in `ActiveRead` or `Partial`
  can be stale, because the data path has no commit index — there is
  no equivalent of `commitIndex` for vectors, only per-replica
  acceptance.

  What *is* linearizable is the metadata: collection existence, shard
  placement and replica state all go through the Raft log at
  consensus.rs:997/1044. So "which replicas may answer" is agreed even
  when "what those replicas contain" is not. Writing that sentence
  down is the point of the question.

  </details>

## References

**Code**
- [qdrant](https://github.com/qdrant/qdrant) at `44ad62f` —
  `src/consensus.rs` (the driving loop; the anchor map above),
  `lib/collection/src/shards/replica_set/` (`update.rs` for the ack
  rule and the deactivation escalation, `replica_set_state.rs` for the
  eleven-variant enum), `lib/collection/src/config.rs` (the defaults),
  `lib/storage/src/content_manager/consensus/` (`consensus_wal.rs` and
  `persistent.rs` — the two persistence sites), `config/config.yaml`
- The library it embeds is [raft-rs](https://github.com/tikv/raft-rs)
  — walked in [reading-raft-rs.md](reading-raft-rs.md); `raw_node.rs:553-555`
  is what makes `process_ready`'s first send legal

**Papers**
- Diego Ongaro, *Consensus: Bridging Theory and Practice* (Stanford
  PhD dissertation, 2014), §10.2.1 — the authority raft-rs cites for
  the leader's parallel disk write, and therefore for consensus.rs:941
