# Reading guide — Specifying Systems (part I) + Ongaro's raft.tla

Lamport's book part I (chapters 1-7) teaches the language; Ongaro's
published Raft spec ([`~/repos/raft.tla/raft.tla`](https://github.com/ongardie/raft.tla), 471 lines) shows
what a real protocol spec looks like. Read them against our
`specs/WalReplication.tla` (94 lines) — same genre, toy scale.

## TLA+ mental model

A spec is a state machine described in math:

```
  Spec == Init /\ [][Next]_vars
           │        │
           │        └─ every step satisfies Next (or stutters)
           └─ initial-state predicate

  Next == A1 \/ A2 \/ ∃ r ∈ S : A3(r)     ← actions, primed vars
  Invariant: a state predicate TLC checks on EVERY reachable state
```

No control flow, no processes — just "which next-states are allowed".
Concurrency falls out of the disjunction: TLC explores every
interleaving of enabled actions. That's the whole trick.

## raft.tla anchors

| line | what |
|---|---|
| :24 | message types incl. `AppendEntriesRequest/Response` |
| :155 | `Init` — everything empty, all followers |
| :204 | `AppendEntries(i, j)` — leader ships **up to 1 entry** per action (model-size discipline; same reason our `Ship` moves one entry) |
| :229 | `BecomeLeader(i)` — quorum of votes ⇒ leader |
| :327 | `HandleAppendEntriesRequest` — the consistency check: term + prevLogIndex/prevLogTerm match, else reject |

Notice what Raft needs that WalReplication doesn't: **terms** and
the **log-matching check**. Our model gets away without them because
(a) entries are sequential integers shipped in order, so logs are
prefixes by construction, and (b) crashes are permanent, so there is
never a *stale ex-primary* that can come back and diverge the log.
Un-model either assumption and you re-derive Raft piece by piece —
a great exercise: allow crashed replicas to rejoin and watch TLC
show you why terms exist.

## Model-size discipline (why TLC finishes)

- Logs-as-lengths: our `wal ∈ [Replicas → 0..MaxLog]` gives 4³ log
  states; raft.tla with real sequences and terms explodes — Ongaro
  notes it's checked only for tiny bounds.
- One entry per Ship/AppendEntries action: granularity of atomicity
  IS the model — batching would hide interleavings.
- Our measured runs: SyncCommit=TRUE → 1080 distinct states, depth
  14, <1 s. SyncCommit=FALSE → violation at depth 5 after 123
  states. Small models, real bugs.

## Safety vs liveness

Everything above is safety ("nothing bad"). Liveness ("something
good eventually") needs fairness: `WF_vars(Ship(r))` — otherwise TLC
accepts the behavior where shipping just never runs. Raft's spec
famously checks safety only; so does ours. Start there; liveness
doubles the conceptual load for a different class of bug (stuck
protocols, not corrupt ones).

## Questions (answer in notes.md)

1. Add `Rejoin(r)` (crashed → alive, keeping its stale wal) to
   WalReplication. What new invariant is needed, and what trace does
   TLC find without it? (This re-derives Raft's term check.)
2. Why does `Failover` need "longest log among survivors" — exhibit
   the quorum-intersection argument for Quorum=2, |Replicas|=3, and
   the trace when failover picks an arbitrary survivor instead.
3. raft.tla:204 ships ≤1 entry per action. What bug class would a
   "ship everything atomically" model hide in OUR spec?
4. Express topic 8's MVCC snapshot-visibility as a TLA+ invariant
   sketch (what are the variables? what's an action?) — this is the
   M21 deliverable's outline.
5. `[][Next]_vars` allows stuttering. Why is that essential for
   refinement (mapping a detailed spec onto an abstract one)?
