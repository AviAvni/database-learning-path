# raft-rs: consensus with the I/O left out

The production Raft that tikv and qdrant embed, and the design worth
stealing: the library owns ONLY the state machine — no threads, no
I/O, no storage. You drive it with `tick()`/`step(msg)` and it hands
you a `Ready` bundle of work to do. That inversion is what makes
consensus testable — and what our sim-based raft.rs stub imitates.
Before the anchors, this chapter builds the design in steps: why
I/O-free, the driving contract, the ordering rules that carry
safety, and where the paper's Fig 2 lives in the source. Assumes
[reading-raft-paper.md](reading-raft-paper.md) — terms, the
consistency check, and §5.4.2 are used by name.

## The problem in one sentence

A consensus bug surfaces once per thousand failovers under a
specific interleaving of timeouts, crashes, and message
reorderings — so an implementation entangled with real threads,
sockets, and fsyncs can never reproduce its own worst bug, and the
fix is to make the algorithm a pure state machine whose every input
(time included) is a function argument.

## The concepts, step by step

### Step 1 — sans-io: the algorithm as a pure state machine

The sans-io pattern (before the name existed): the library performs
no I/O — it *describes* I/O. raft-rs has no `fsync`, no sockets, no
timers, no threads; you feed it inputs and it returns instructions:

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

Deterministic by construction: the same sequence of tick/step calls
always produces the same Ready bundles — which is exactly why our
`sim.rs` can test consensus without threads (and why topic 16's DST
loves this shape). What it costs: every embedder must implement the
driving loop and get its ordering rules right (Step 4) — the library
moved the hard-to-test part out, not away.

### Step 2 — driving it: tick and step, the only two inputs

Time and network collapse into two methods. `tick()` — you call it
on your own timer; enough ticks without a heartbeat and the state
machine decides "election timeout" and emits vote requests (in the
next Ready). `step(msg)` — you received a Raft message; hand it
over. Internally, `step` (raft.rs:1346) first handles term logic —
higher term → become_follower, lower term → mostly ignore/reject —
and only THEN dispatches on role
(`step_leader/step_candidate/step_follower`). Compare our `raft.rs`
stub: same shape, `match self.role`. Question: which messages must
be handled *before* the role dispatch, and why? (Term comparison is
role-independent — Fig 2's "all servers" rules.)

### Step 3 — Progress: what the leader knows about each follower

The leader tracks, per follower, two indexes (tracker/progress.rs:8-12):

```
 matched   highest index KNOWN replicated on that follower
 next_idx  next index to send (optimistic; decremented on reject)
```

`next_idx` implements the paper's repair loop — send from `next_idx`,
on rejection decrement and retry until the consistency check passes.
`matched` feeds commitment: `maybe_commit` (raft.rs:939) sorts all
matched values descending, takes the majority-th one, and commits it
*only if that entry's term is the current term* — §5.4.2 as three
lines of code:

```rust
// §5.4.2, executable: the majority-replicated index counts only if
// the entry there is from MY term — older entries then ride along
fn maybe_commit(&mut self) -> bool {
    let mut matched: Vec<u64> =
        self.progress.values().map(|p| p.matched).collect();
    matched.sort_unstable_by(|a, b| b.cmp(a));       // descending
    let quorum_idx = matched[self.quorum() - 1];     // majority-th highest
    if quorum_idx > self.commit_index
        && self.log.term_at(quorum_idx) == Some(self.term)
    {
        self.commit_index = quorum_idx;
        return true;                                 // Fig 8 cannot happen
    }
    false
}
```

Worked example (question 2): 5 nodes, matched = [7,5,5,3,2] →
majority-th (3rd) highest = 5 → commit 5, if entry 5 is
current-term. Progress also carries probe/replicate/snapshot states —
question: what problem does each state solve for a lagging follower?

### Step 4 — the Ready contract: ordering is the safety

`has_ready()` (raw_node.rs:562) polls for pending work; `ready()`
(:487) hands you the bundle; `advance()` (:663) confirms you did it.
The ordering rules between those calls are load-bearing — they are
where the paper's durability requirements become YOUR obligations:

- **Persist entries + HardState BEFORE sending messages** that
  reference them. The HardState holds `(term, voted_for, commit)` —
  a vote you didn't fsync can be double-cast after a crash, electing
  two leaders in one term (the paper's Step 3 persistence rule,
  enforced by discipline alone).
- **Apply committed_entries in order; never apply above what's
  persisted.**
- `advance()` tells the library the batch is done; `advance_append`
  (:678) lets you ack persistence asynchronously — group-commit the
  raft log, topic 5's fsync ladder applied to consensus.

Question: what specific safety violation occurs if you send the
vote-response message before fsyncing `voted_for`?

### Step 5 — what our raft.rs keeps / drops

The stub in `experiments/` is this library minus everything the sim
makes unnecessary:

| raft-rs | our stub |
|---|---|
| Ready bundle + advance | direct send via `Sim` (no I/O to defer) |
| Storage trait + persistence | in-memory `Vec<(term, cmd)>` |
| Progress probe/snapshot states | just `next_idx` decrement |
| joint-consensus membership | fixed peer set |
| pre-vote, leases, learners | absent |

Same invariants pinned by tests; ~10× less plumbing. The exercise of
the topic is noticing which drops are safe *because the sim is
deterministic* and which would be real bugs in production.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| raw_node.rs:293 | `RawNode` — the public wrapper | 1 |
| raw_node.rs:487 | `ready()` — collect pending work | 4 |
| raw_node.rs:562 | `has_ready()` — the poll predicate | 4 |
| raw_node.rs:663 | `advance()` — "I did the work" | 4 |
| raw_node.rs:678 | `advance_append` — split persistence ack | 4 |
| raft.rs:263 | `Raft<T: Storage>` — the actual state machine | 1 |
| raft.rs:939 | `maybe_commit` — §5.4.2 lives here | 3 |
| raft.rs:1148/1176/1226 | `become_follower/candidate/leader` | 2 |
| raft.rs:1283 | `campaign` | 2 |
| raft.rs:1346 | `step` — the message dispatch root | 2 |
| raft.rs:1539 | `hup` — election timeout fires | 2 |
| raft.rs:2045/2291/2348 | `step_leader/candidate/follower` | 2 |
| raft.rs:2499 | `handle_append_entries` | 3 |
| tracker/progress.rs:8-12 | `Progress { matched, next_idx }` | 3 |

Read order: `raw_node.rs` around `ready()`/`advance()` first (the
contract), then `raft.rs:1346` `step` and follow one message type
down each role branch, then `maybe_commit`. qdrant's production
driving loop for this exact API is the next chapter
([reading-qdrant-consensus.md](reading-qdrant-consensus.md)).

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

## References

**Papers**
- The Raft paper itself is
  [reading-raft-paper.md](reading-raft-paper.md) — Fig 2 is the spec
  this code implements

**Code**
- [raft-rs](https://github.com/tikv/raft-rs) — `src/raw_node.rs` (the
  Ready contract), `src/raft.rs` (the state machine; the anchor map
  above), `src/tracker/progress.rs`; qdrant's embedding of it is
  [reading-qdrant-consensus.md](reading-qdrant-consensus.md)
