# Reading guide — tikv `raft-rs`

Clone: [`~/repos/raft-rs`](https://github.com/tikv/raft-rs) (`src/`). The production Raft that tikv and
qdrant embed. The design worth stealing: the library owns ONLY the
state machine — no threads, no I/O, no storage. You drive it with
`tick()`/`step(msg)` and it hands you a `Ready` bundle of work to do.

## The shape

```
            ┌────────────── your code ──────────────┐
            │ timer → tick()      network → step()  │
            │                 ▼                     │
            │            RawNode<Storage>           │
            │                 │                     │
            │           has_ready()?                │
            │                 ▼                     │
            │  Ready { messages, entries-to-append, │
            │          committed_entries, hs, ss }  │
            │   1. persist entries + hardstate      │
            │   2. send messages                    │
            │   3. apply committed_entries          │
            │   4. advance()                        │
            └───────────────────────────────────────┘
```

Sans-io before the name existed. Deterministic by construction —
which is exactly why our `sim.rs` can test consensus without threads
(and why topic 16's DST loves this shape).

## Anchor map

| anchor | what it is |
|---|---|
| raw_node.rs:293 | `RawNode` — the public wrapper |
| raw_node.rs:487 | `ready()` — collect pending work |
| raw_node.rs:562 | `has_ready()` — the poll predicate |
| raw_node.rs:663 | `advance()` — "I did the work" |
| raw_node.rs:678 | `advance_append` — split persistence ack |
| raft.rs:263 | `Raft<T: Storage>` — the actual state machine |
| raft.rs:939 | `maybe_commit` — §5.4.2 lives here |
| raft.rs:1148/1176/1226 | `become_follower/candidate/leader` |
| raft.rs:1283 | `campaign` |
| raft.rs:1346 | `step` — the message dispatch root |
| raft.rs:1539 | `hup` — election timeout fires |
| raft.rs:2045/2291/2348 | `step_leader/candidate/follower` |
| raft.rs:2499 | `handle_append_entries` |
| tracker/progress.rs:8-12 | `Progress { matched, next_idx }` |

## 1. The step_* dispatch (`raft.rs:1346`)

`step()` first handles term logic (higher term → become_follower,
lower term → mostly ignore/reject), THEN dispatches on role. Compare
with our `raft.rs` stub: same shape, `match self.role`. Question:
which messages must be handled *before* the role dispatch, and why?
(Term comparison is role-independent — Fig 2's "all servers" rules.)

## 2. Progress tracking (`tracker/progress.rs:8-12`)

Per-follower, leader-side:

```
 matched   highest index KNOWN replicated on that follower
 next_idx  next index to send (optimistic; decremented on reject)
```

`maybe_commit` (raft.rs:939) sorts matched values, takes the
majority-th, commits if that entry's term == current term — §5.4.2
as three lines of code. Question: probe/replicate/snapshot states in
Progress — what problem does each state solve for a lagging
follower?

## 3. The Ready contract (`raw_node.rs:487-678`)

The ordering rules are load-bearing:

- **persist entries + HardState BEFORE sending messages** that
  reference them (a vote you didn't persist can be double-cast after
  crash)
- apply committed_entries in order; never apply above what's
  persisted
- `advance()` tells the library the batch is done; `advance_append`
  lets you ack persistence asynchronously (group-commit the raft
  log — topic 5's ladder again)

Question: what specific safety violation occurs if you send the
vote-response message before fsyncing `voted_for`?

## 4. What our raft.rs keeps / drops

| raft-rs | our stub |
|---|---|
| Ready bundle + advance | direct send via `Sim` (no I/O to defer) |
| Storage trait + persistence | in-memory `Vec<(term, cmd)>` |
| Progress probe/snapshot states | just `next_idx` decrement |
| joint-consensus membership | fixed peer set |
| pre-vote, leases, learners | absent |

Same invariants pinned by tests; ~10× less plumbing.

## Questions for notes.md

1. Why does raft-rs contain no `fsync`, no sockets, no threads —
   and what does that buy tikv/qdrant integration-wise?
2. `maybe_commit`: write out the sorted-matched-index computation
   for 5 nodes with matched = [7,5,5,3,2]. Commit index?
3. next_idx decrement-and-retry is O(divergence) round trips — what
   optimization does the paper's §5.3 footnote suggest, and does
   raft-rs implement it?
4. advance_append: how does splitting the persistence ack enable
   pipelining, and what must you still NOT reorder?
5. Map Ready → M15 stage 2: which parts of your WAL commit path
   play the roles of persist/send/apply/advance?
