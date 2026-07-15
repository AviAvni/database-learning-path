# A spec is a state machine: TLA+ through raft.tla

TLA+ has one idea — describe your protocol as "which next-states
are allowed" and let TLC enumerate every interleaving. Lamport's
*Specifying Systems* part I (chapters 1-7) teaches the language;
Ongaro's published Raft spec (471 lines) shows what a real protocol
spec looks like. This chapter builds the mental model step by step
— states, actions, the Next disjunction, invariants, model-size
discipline — using our `specs/WalReplication.tla` (94 lines) as the
running example, so both texts read as instances of one idea.

## The problem in one sentence

Even a toy 3-replica, 3-entry WAL-shipping protocol has over a
thousand distinct reachable states across all interleavings of
ship/commit/crash/failover — far too many for a human to reason
through, and exactly the right size for a machine to enumerate in
under a second.

## The concepts, step by step

### Step 1 — a state is a snapshot of the variables

A TLA+ spec picks a handful of **variables**, and a **state** is
one assignment of values to them. Our WalReplication uses four:
`wal` (how many entries each replica has — a log that's always a
prefix can be modeled as just its length), `primary`, `crashed`
(the set of dead replicas), `committed` (how many entries the
protocol has acknowledged). A **behavior** is a sequence of states
— one possible execution. The whole protocol universe at MaxLog=3,
3 replicas is small: `wal ∈ [Replicas → 0..3]` alone is 4³ = 64
combinations. Deliberately small — step 5 is about keeping it so.

### Step 2 — an action is a predicate relating now to next

An **action** describes one atomic step as a boolean predicate
over two states: unprimed variables (`wal`) mean the current
state, **primed** ones (`wal'`) mean the next. No assignment, no
control flow — the action is simply *true* of exactly the
(current, next) pairs it allows. A real one, from our spec:

```tla
\* WAL shipping: backup r pulls the next entry it is missing.
Ship(r) ==
    /\ r # primary /\ r \notin crashed /\ primary \notin crashed
    /\ wal[r] < wal[primary]                    \* enabled only when behind
    /\ wal' = [wal EXCEPT ![r] = @ + 1]         \* ONE entry per action —
    /\ UNCHANGED <<primary, crashed, committed>> \* atomicity IS the model
```

Read it in three layers: the first two lines are the **enabling
condition** (in which states can this happen at all), the third
says what changes, the fourth pins everything else (omit
`UNCHANGED` and you've allowed those variables to change to
*anything*). The comment "atomicity IS the model" is load-bearing:
whatever one action changes is what the model treats as
indivisible — question 3 turns on it.

### Step 3 — Next is a disjunction: concurrency falls out for free

The full spec is one formula:

```
  Spec == Init /\ [][Next]_vars
           │        │
           │        └─ every step satisfies Next (or stutters)
           └─ initial-state predicate

  Next == A1 \/ A2 \/ ∃ r ∈ S : A3(r)     ← actions, primed vars
```

In ours:

```tla
Next ==
    \/ Append
    \/ Commit
    \/ \E r \in Replicas : Ship(r) \/ Crash(r) \/ Failover(r)
```

No processes, no threads: each step of a behavior is *any one*
enabled disjunct. Concurrency falls out of the disjunction — every
interleaving of enabled actions is a behavior, automatically. The
`[]` means "always", and the `_vars` subscript permits
**stuttering** steps (states where nothing changes) — a technical
allowance that's essential for refinement (question 5). That's the
whole language, conceptually; everything else is notation.

### Step 4 — invariants, and TLC's exhaustive breadth-first search

An **invariant** is a predicate on single states that must hold in
every reachable one. Ours:

```tla
\* THE invariant TLC checks on every reachable state:
Durability == primary \notin crashed => committed <= wal[primary]
```

**TLC**, the model checker, does breadth-first search from the
initial states, firing every enabled action at every state,
deduplicating, and checking the invariant on each state found. BFS
means the first violation found is a *shortest* counterexample —
the trace TLC prints is the minimal story of the bug. Our measured
runs: SyncCommit=TRUE → 1080 distinct states, depth 14, holds in
under a second. SyncCommit=FALSE → violated at depth 5 after 123
states, and the 5-step trace (Append → Commit without quorum →
Crash(primary) → Failover to an empty log) is exactly the
PostgreSQL `synchronous_commit = off` data-loss story.

### Step 5 — model-size discipline (why TLC finishes)

TLC enumerates *everything*, so state-count is the budget and
modeling choices are what spend it:

- Logs-as-lengths: our `wal ∈ [Replicas → 0..MaxLog]` gives 4³ log
  states; raft.tla with real sequences and terms explodes — Ongaro
  notes it's checked only for tiny bounds.
- One entry per Ship/AppendEntries action: granularity of atomicity
  IS the model — batching would hide interleavings.
- Small constants (3 replicas, 3 entries) on the small-scope bet
  from [reading-aws-cacm15.md](reading-aws-cacm15.md): protocol
  bugs almost never need N=7.

Small models, real bugs: 123 states were enough to catch the
async-commit data loss no test generator finds *guaranteed*.

### Step 6 — what Raft needs that our toy doesn't: terms

Reading raft.tla after WalReplication, the striking additions are
**terms** (a monotonically increasing epoch number attached to
every leader and log entry) and the **log-matching check**
(followers reject entries whose predecessor doesn't match). Our
model gets away without them because (a) entries are sequential
integers shipped in order, so logs are prefixes by construction,
and (b) crashes are permanent, so there is never a *stale
ex-primary* that can come back and diverge the log. Un-model
either assumption and you re-derive Raft piece by piece — a great
exercise: allow crashed replicas to rejoin and watch TLC show you
why terms exist (question 1). This is the general skill: every
mechanism in a real protocol answers a behavior some simpler model
excluded.

### Step 7 — safety vs liveness

Everything above is **safety** ("nothing bad ever happens" — an
invariant can be violated by a finite trace). **Liveness**
("something good eventually happens") is a different kind of
property: it's violated only by *infinite* behaviors, e.g. one
where shipping simply never runs. Checking it requires **fairness**
assumptions — `WF_vars(Ship(r))` says Ship can't stay enabled
forever without firing — otherwise TLC accepts the do-nothing
behavior. Raft's spec famously checks safety only; so does ours.
Start there; liveness doubles the conceptual load for a different
class of bug (stuck protocols, not corrupt ones).

## How to read the paper (with the concepts in hand)

- **Lamport, *Specifying Systems*, part I (chapters 1-7)** — the
  language behind steps 1-4 and 7, in Lamport's own order (he
  builds from a one-bit clock up to a FIFO). With the steps above
  as scaffolding, these chapters are a fast read; the rest of the
  book is reference material.
- **Our `specs/WalReplication.tla` (94 lines)** — read it in full
  before Raft; every construct in it now has a step number. Run it:
  `java -cp ~/repos/tla2tools.jar tlc2.TLC -deadlock
  WalReplication.tla` (flip `SyncCommit` in the .cfg to see the
  depth-5 trace from step 4 yourself).
- **raft.tla (471 lines)** — read by these anchors:

| line | step | what |
|---|---|---|
| :24 | 1 | message types incl. `AppendEntriesRequest/Response` |
| :155 | 1 | `Init` — everything empty, all followers |
| :204 | 2, 5 | `AppendEntries(i, j)` — leader ships **up to 1 entry** per action (model-size discipline; same reason our `Ship` moves one entry) |
| :229 | 6 | `BecomeLeader(i)` — quorum of votes ⇒ leader |
| :327 | 6 | `HandleAppendEntriesRequest` — the consistency check: term + prevLogIndex/prevLogTerm match, else reject |

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

## References

**Papers**
- Lamport — *Specifying Systems* (Addison-Wesley 2002) — part I,
  chapters 1-7; free PDF from Lamport's site — the rest of the book
  is reference material

**Code**
- [raft.tla](https://github.com/ongardie/raft.tla) `raft.tla` —
  Ongaro's published spec, 471 lines; anchors above
- `specs/WalReplication.tla` (this topic's experiments) — the
  94-line toy to read first
