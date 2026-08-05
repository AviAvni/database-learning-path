# A spec is a state machine: TLA+ through raft.tla

TLA+ has one idea — describe your protocol as "which next-states are allowed"
and let TLC enumerate every interleaving. Lamport's *Specifying Systems* part I
teaches the language; Ongaro's published Raft spec shows what a real protocol
spec looks like. This chapter builds the mental model from the ground up —
states, actions, the `Next` disjunction, invariants, and the state-space
arithmetic that decides whether TLC finishes at all — using this topic's
`specs/WalReplication.tla` as the running example, so both texts read as
instances of one idea.

Two sources, both quoted with real line numbers. `specs/WalReplication.tla` is
**92 lines**, in this repo, and its model lives in `specs/WalReplication.cfg`.
`raft.tla` is **471 lines**, pinned at `ongardie/raft.tla@6ecbdbc`
(`resources/codebases.md` pin table); that repository contains exactly two files,
`raft.tla` and a 9-line `README.md`, and — a fact Step 8 turns on — **no `.cfg`
and no invariants**.

## The problem in one sentence

A toy 3-replica, 3-entry WAL-shipping protocol has **1080 distinct reachable
states** across every interleaving of ship / commit / crash / failover — already
past what a human reviews reliably, and small enough for TLC to enumerate in
under a second; the same protocol written the way Raft actually needs it has on
the order of **10¹¹** type-correct states before you count messages, and the gap
between those two numbers is the entire practical skill.

## The concepts, step by step

### Step 1 — a state is a snapshot of the variables

> **In:** a protocol you can describe in prose.
> **Out:** a finite set of variables, and the size of the space they span —
> computed, because Step 6 is going to spend it.

A TLA+ spec declares a handful of **variables**; a **state** is one assignment of
values to all of them, and a **behavior** is an infinite sequence of states — one
possible execution. There are no threads, no objects, no heap. Just variables.

Ours declares four:

```tla
-- specs/WalReplication.tla, lines 17-23 — the entire state of the protocol
    17  VARIABLES
    18      primary,    \* current primary
    19      crashed,    \* set of crashed replicas (crashes are permanent here)
    20      wal,        \* [Replicas -> 0..MaxLog], length of each prefix log
    21      committed   \* client-visible commit point
    22
    23  vars == <<primary, crashed, wal, committed>>
```

Note what `wal` is *not*: it is not a sequence of entries. Line 20 stores a
single natural number per replica, and the module header (lines 3–4) says why —
"Entries are sequential 1..MaxLog and ship in order, so each log is a prefix of
the primary's — one natural number per replica." That is a **modelling
decision**, and Step 6 shows exactly what it costs and what it buys.

**Count the space.** With `Replicas = {r1,r2,r3}` and `MaxLog = 3`
(`WalReplication.cfg`):

- `primary ∈ Replicas` → 3
- `crashed ⊆ Replicas` → 2³ = 8
- `wal ∈ [Replicas → 0..3]` → 4³ = 64
- `committed ∈ 0..3` → 4

Product: `3 × 8 × 64 × 4 = 6144` **type-correct** states. Not all are reachable:
`Crash(r)` (line 65) is guarded by `Cardinality(Alive \ {r}) >= Quorum` with
`Quorum = 2`, so at most one replica ever crashes and only 4 of the 8 subsets
occur — `3 × 4 × 64 × 4 = 3072`. TLC's measured answer is **1080 distinct**
states (`notes.md`). The remaining factor of 2.8 is what the *action guards*
prune. This gap — type-correct, guard-reachable, actually reachable — is the
thing to keep in your head for Step 6.

### Step 2 — an action is a predicate relating now to next

> **In:** the variables of Step 1.
> **Out:** the only construct in the language that does any work, and the three
> layers every one of them has.

An **action** describes one atomic step as a boolean predicate over *two*
states: unprimed variables (`wal`) denote the current state, **primed** ones
(`wal'`) the next. There is no assignment and no control flow — the action is
simply *true* of exactly the (current, next) pairs it permits.

```tla
-- specs/WalReplication.tla, lines 46-51 — WAL shipping, one action, verbatim
    46  \* WAL shipping: backup r pulls the next entry it is missing.
    47  Ship(r) ==
    48      /\ r # primary /\ r \notin crashed /\ primary \notin crashed
    49      /\ wal[r] < wal[primary]
    50      /\ wal' = [wal EXCEPT ![r] = @ + 1]
    51      /\ UNCHANGED <<primary, crashed, committed>>
```

Read it in three layers:

1. **Enabling condition** (48–49): in which states can this happen at all? Line
   48 is liveness of the participants, line 49 is "`r` is actually behind". If
   no conjunct on these lines holds, the action is **disabled** in that state and
   contributes no successor.
2. **The change** (50). `[wal EXCEPT ![r] = @ + 1]` is a function identical to
   `wal` except at `r`, where `@` denotes the old value. Exactly **one** entry
   moves.
3. **The frame** (51). `UNCHANGED` pins everything else. Omit it and the action
   permits those variables to change to *anything* — the single most common
   beginner bug, and it produces a spec that checks nothing while looking fine.

The granularity of layer 2 is not a detail, it is the model. Whatever one action
changes is what the specification treats as indivisible; a `Ship` that moved the
whole log at once would be asserting that shipping is atomic, and TLC would
never explore the interleavings in which it is not.

### Step 3 — `Next` is a disjunction, and that is where concurrency comes from

> **In:** a set of actions.
> **Out:** the whole spec as a single formula, and the reason you never write a
> scheduler.

```tla
-- specs/WalReplication.tla, lines 81-86 — the whole protocol, five lines
    81  Next ==
    82      \/ Append
    83      \/ Commit
    84      \/ \E r \in Replicas : Ship(r) \/ Crash(r) \/ Failover(r)
    85
    86  Spec == Init /\ [][Next]_vars
```

Each step of a behavior is *any one* enabled disjunct. There are no processes and
no scheduler: **every interleaving of enabled actions is a behavior,
automatically**, because the disjunction does not say which one happens. The `\E`
on line 84 quantifies over replicas, so `Ship(r1)`, `Ship(r2)` and `Ship(r3)` are
three separate disjuncts generated from one line.

`Spec` (86) reads: start in a state satisfying `Init`, and **always** (`[]`)
every step satisfies `Next` — *or* leaves `vars` unchanged. That last clause is
what the `_vars` subscript means, and it is called **stuttering**.

Raft's `Next` has the identical shape at 10× the width:

```tla
-- raft.tla, lines 454-465 (ongardie/raft.tla@6ecbdbc) — same construct, nine actions
   454  Next == /\ \/ \E i \in Server : Restart(i)
   455             \/ \E i \in Server : Timeout(i)
   456             \/ \E i,j \in Server : RequestVote(i, j)
   457             \/ \E i \in Server : BecomeLeader(i)
   458             \/ \E i \in Server, v \in Value : ClientRequest(i, v)
   459             \/ \E i \in Server : AdvanceCommitIndex(i)
   460             \/ \E i,j \in Server : AppendEntries(i, j)
   461             \/ \E m \in DOMAIN messages : Receive(m)
   462             \/ \E m \in DOMAIN messages : DuplicateMessage(m)
   463             \/ \E m \in DOMAIN messages : DropMessage(m)
   464             \* History variable that tracks every log ever:
   465          /\ allLogs' = allLogs \cup {log[i] : i \in Server}
```

Two things to notice. First, lines 462–463: message **duplication and loss are
actions**, so an unreliable network is not an assumption bolted on the side — it
is two more disjuncts. Second, the outer `/\` at 454 with the conjunct at 465:
`allLogs` is updated on *every* step, so it is not one of the disjuncts, it rides
along with all of them.

### Step 4 — stuttering, and why it is not a technicality

> **In:** the `[][Next]_vars` of Step 3.
> **Out:** why a specification must allow steps in which nothing happens.

`[][Next]_vars` is shorthand for `[](Next \/ UNCHANGED vars)`. A behavior may
contain steps where nothing changes at all. This looks like a loophole and is
the opposite.

The reason is **refinement**. If a detailed spec `D` implements an abstract spec
`A`, you show it by mapping each state of `D` to a state of `A` and proving every
`D` step maps to an `A` step. But `D` has more steps than `A` — internal actions
that `A` does not model at all. Those must map to *something*, and what they map
to is a stuttering step of `A`. Without stuttering, `D` could never implement
`A`, and refinement — the reason TLA+ has a temporal logic rather than just a
state machine — would not exist.

The practical consequence: a spec's behaviors are closed under inserting and
deleting finite runs of repeated states, so "how many steps did it take" is never
a meaningful property, and TLC's `-deadlock` flag exists because a state with no
enabled action is normally *fine* under stuttering and only sometimes a bug.

### Step 5 — invariants, and what TLC actually does

> **In:** a spec and a model.
> **Out:** a property, a search, and a counterexample trace — with this topic's
> two measured runs.

An **invariant** is a predicate on single states that must hold in every
reachable one.

```tla
-- specs/WalReplication.tla, lines 88-90 — the property under test
    88  \* THE invariant: a live primary's WAL contains every committed entry.
    89  \* (Logs are prefixes, so "contains entry k" is just wal >= k.)
    90  Durability == primary \notin crashed => committed <= wal[primary]
```

The `.cfg` names the model and the properties:

```
CONSTANTS  Replicas = {r1, r2, r3}   MaxLog = 3   Quorum = 2   SyncCommit = TRUE
INIT Init    NEXT Next    INVARIANTS TypeOK, Durability
```

**TLC** does breadth-first search from the initial states: at each state, fire
every enabled action, deduplicate successors against a set of seen states, check
every invariant on each new state. Because the search is breadth-first, the first
violation found is at minimum depth — the trace TLC prints is a **shortest**
counterexample, which is what makes it a debugging artefact rather than a
failure report.

Measured, from `notes.md`:

| `SyncCommit` | generated | distinct | depth | `Durability` |
|---|---|---|---|---|
| `TRUE` | 2583 | 1080 | 14 | holds |
| `FALSE` | 183 | 123 | 5 | **VIOLATED** |

The 5-step trace is the interesting one: `Append` → `Commit` without a quorum ack
→ `Crash(primary)` → `Failover` to a replica that never saw entry 1 → the
invariant fails. That is PostgreSQL's `synchronous_commit = off` data-loss story,
found by a machine in a fraction of a second, guaranteed rather than sampled.

Why the flip breaks it is one conjunct — line 60, `SyncCommit =>
Cardinality(AckedBy(committed + 1)) >= Quorum`. With `SyncCommit = FALSE` the
implication is vacuously true, the quorum gate disappears, and quorum
intersection — the argument that `Failover`'s "longest surviving log" (line 77)
must hold every committed entry — loses its premise.

### Step 6 — model-size discipline, computed

> **In:** the observation that TLC enumerates everything.
> **Out:** the arithmetic that decides whether a spec is checkable, done on both
> models, with the modelling knobs identified by their cost.

TLC's budget is states, so every modelling choice is a purchase. Here is the
purchase, priced.

**Our model** (Step 1): `3 × 8 × 64 × 4 = 6144` type-correct states, 1080
reachable. Now price the alternative. Suppose `wal` were a real **sequence** of
entries rather than a length, with entries drawn from 3 possible values. Logs of
length 0..3 over 3 values: `1 + 3 + 9 + 27 = 40` distinct logs per replica, so
`wal` alone becomes `40³ = 64,000` instead of `64` — a **1000× multiplier** on
the whole state space, for a protocol in which logs are prefixes by construction
and therefore carry no information beyond their length. That one modelling
decision, at line 20, is the difference between a one-second run and an
overnight one.

**raft.tla** cannot make that simplification, because Raft's whole difficulty is
logs that *diverge*. It declares **13 variables** (`raft.tla:32-85`): `messages`,
`elections`, `allLogs`, `currentTerm`, `state`, `votedFor`, `log`, `commitIndex`,
`votesResponded`, `votesGranted`, `voterLog`, `nextIndex`, `matchIndex`. Take a
deliberately tiny hypothetical model — 3 servers, 1 client value, terms bounded
at 3, logs bounded at length 3 — and price just five of them:

| variable | domain at this model | size |
|---|---|---|
| `currentTerm` | `[Server → 1..3]` | 3³ = 27 |
| `state` | `[Server → {Follower,Candidate,Leader}]` | 3³ = 27 |
| `votedFor` | `[Server → Server ∪ {Nil}]` | 4³ = 64 |
| `log` | `[Server → Seq(entry)]`, 40 logs each | 40³ = 64,000 |
| `commitIndex` | `[Server → 0..3]` | 4³ = 64 |

Product: `27 × 27 × 64 × 64,000 × 64 = 191,102,976,000` ≈ **1.9 × 10¹¹**
type-correct states — from five of thirteen variables. The remaining eight are
worse, not better: `nextIndex` and `matchIndex` are each `[Server → [Server →
0..3]]` = `64³ = 262,144`; `messages` is a *bag* of records with no finite bound
at all; and `allLogs` is a **set of logs**, so its domain is the powerset of the
40 possible logs — `2⁴⁰ ≈ 1.1 × 10¹²` values, single-handedly larger than the
five variables above combined.

That last one is worth sitting with. `allLogs` is declared with the comment "A
history variable used in the proof. This would not be present in an
implementation" (`raft.tla:41-44`), and it is updated on every step
(`raft.tla:465`). A variable added purely to make a proof expressible is the
largest term in the model checker's state space. Proof convenience and checking
cost pull in opposite directions.

So the three knobs, each now with a price attached:

- **Abstract the data.** Logs-as-lengths saved us 1000×. Only legal because the
  prefix property is guaranteed by construction — an assumption Step 7 removes.
- **Keep atomic regions small.** Raft is explicit about this in a comment, and
  it is the same reasoning as our one-entry `Ship`:

```tla
-- raft.tla, lines 201-204 — the model-size argument, in the spec's own words
   201  \* Leader i sends j an AppendEntries request containing up to 1 entry.
   202  \* While implementations may want to send more than 1 at a time, this spec uses
   203  \* just 1 because it minimizes atomic regions without loss of generality.
   204  AppendEntries(i, j) ==
```

  Note the direction: sending one entry at a time makes the *spec* explore more
  interleavings, not fewer. It costs states and buys coverage. "Minimizes atomic
  regions" is the goal; "without loss of generality" is the claim that batching
  adds no behaviors a sequence of single sends cannot produce.
- **Use small constants.** 3 replicas, 3 entries, on the bet that protocol bugs
  do not first appear at N = 7. That bet is the small-scope hypothesis discussed
  in [reading-aws-cacm15.md](reading-aws-cacm15.md) — and note there that it is
  Daniel Jackson's hypothesis, not something the AWS paper claims.

### Step 7 — what Raft needs that our toy does not

> **In:** two specs, one 92 lines and one 471.
> **Out:** the two mechanisms the difference is made of, and the assumption each
> one pays for.

Read `raft.tla` after `WalReplication.tla` and the additions that jump out are
**terms** (a monotonically increasing epoch attached to every leader and every
log entry) and the **log-matching check**:

```tla
-- raft.tla, lines 327-337 — logOk is the log-matching check, reject branch shown
   327  HandleAppendEntriesRequest(i, j, m) ==
   328      LET logOk == \/ m.mprevLogIndex = 0
   329                   \/ /\ m.mprevLogIndex > 0
   330                      /\ m.mprevLogIndex <= Len(log[i])
   331                      /\ m.mprevLogTerm = log[i][m.mprevLogIndex].term
   332      IN /\ m.mterm <= currentTerm[i]
   333         /\ \/ /\ \* reject request
   334                  \/ m.mterm < currentTerm[i]
   335                  \/ /\ m.mterm = currentTerm[i]
   336                     /\ state[i] = Follower
   337                     /\ \lnot logOk
```

`logOk` (328–331) says: the entry before the one you are sending me must exist in
my log **at the same term**. Index agreement is not enough; term agreement is the
point, because two leaders in different terms can write different entries at the
same index.

Our model gets away with neither mechanism because of two assumptions it makes
without saying so loudly:

- Entries are sequential and ship in order (module header, lines 3–4), so logs
  are prefixes **by construction**. There is no index at which two replicas can
  disagree, so there is nothing for a log-matching check to check.
- `Crash(r)` is permanent (line 19's comment: "crashes are permanent here"), so
  there is never a stale ex-primary that comes back and writes. There is no
  second leader, so there is no need for terms to order them.

Remove either assumption and you re-derive Raft piece by piece. Adding a
`Rejoin(r)` action is question 1 for exactly this reason: TLC will hand you the
trace that proves you now need terms. The general skill this teaches is the one
worth taking away — **every mechanism in a real protocol answers a behavior some
simpler model excluded**, and a model checker will tell you which one if you let
the behavior back in.

### Step 8 — safety, liveness, and what raft.tla does not contain

> **In:** the invariant of Step 5.
> **Out:** the second class of property, and an honest reading of what the
> published Raft spec does and does not check.

**Safety** is "nothing bad ever happens" — violated by a finite trace, which is
why an invariant check works and why TLC can print a counterexample. `Durability`
is safety. **Liveness** is "something good eventually happens" — violated only by
an *infinite* behavior, e.g. one in which `Ship` is enabled forever and never
fires. Checking liveness needs **fairness** assumptions: `WF_vars(Ship(r))` (weak
fairness) says `Ship(r)` cannot remain continuously enabled forever without
occurring. Without a fairness conjunct, the behavior that stutters forever
satisfies any spec, so every liveness property fails trivially.

`WalReplication.tla` contains no fairness conjuncts and its `.cfg` lists two
`INVARIANTS` and no `PROPERTIES`. Safety only, deliberately.

Now the correction that matters for reading `raft.tla`. **It does not check
anything at all.** Search the 471 lines for `THEOREM`, `Inv`, `Invariant`,
`PROPERTY`, `WF_` or `SF_` and there are no matches. The file defines constants,
13 variables, helper operators, nine actions, `Init`, `Next` and `Spec` — and
stops at line 471. There is no `.cfg` in the repository; the repository contains
`raft.tla` and a 9-line `README.md`, nothing else. The README's own guidance is:

> "If you're trying to run the TLA+ model checker on this specification, check
> out Jin Li's changes in Pull Request #4."

— i.e. the published spec is not TLC-ready as distributed. The safety argument
lives elsewhere: the README points at "Chapter 8 (Correctness) and Appendix B
(Safety proof and formal specification)" of Ongaro's dissertation.

So the honest statement is not "Raft's spec checks safety only". It is: **the
published Raft spec states the protocol and asserts no properties**; the
properties and their proof are in the dissertation, and running TLC on it is
something you have to set up yourself. Start with safety in your own specs
anyway — liveness roughly doubles the conceptual load and targets a different
class of bug (stuck protocols, not corrupt ones).

## How to read the paper (with the concepts in hand)

- **Lamport, *Specifying Systems*, part I (chapters 1–7)** — the language behind
  Steps 1–5 and 8, in Lamport's own order: he builds from a one-bit clock to an
  asynchronous FIFO. With the steps above as scaffolding these chapters are a
  fast read; the rest of the book is reference material. Chapter 8 is where
  liveness and fairness get their proper treatment.
- **`specs/WalReplication.tla` (92 lines)** — read it in full before Raft; every
  construct in it now has a step number. Then run it:
  `java -cp <tla2tools.jar> tlc2.TLC -deadlock WalReplication.tla`, and flip
  `SyncCommit` to `FALSE` in the `.cfg` to get the depth-5 trace of Step 5
  yourself.
- **`raft.tla` (471 lines, pinned at `6ecbdbc`)** — read by the anchors below,
  in this order: variables first, then `Init`, then the actions in `Next`'s
  order, then `Next` itself. Do not look for invariants; Step 8 explains why.

| raft.tla:line | step | what |
|---|---|---|
| `:23-24` | 3 | message-type constants — `RequestVote*`, `AppendEntries*` |
| `:32-85` | 6 | the 13 `VARIABLE` declarations. `:41-44` is `allLogs`, the history variable, with its own disclaimer |
| `:99` | 7 | `Quorum` — "every quorum overlaps with every other", the property `Failover` needs |
| `:102` | 7 | `LastTerm` — the election-restriction helper |
| `:155` | 5 | `Init` — six conjuncts, one per variable group |
| `:167` / `:178` | 7 | `Restart` (loses everything but `currentTerm`, `votedFor`, `log`) and `Timeout` — the actions our permanent-crash model has no analogue for |
| `:201-204` | 6 | `AppendEntries` — "up to 1 entry … minimizes atomic regions" |
| `:229` | 7 | `BecomeLeader` — a quorum of votes ⇒ leader |
| `:259` | 5 | `AdvanceCommitIndex` — Raft's `Commit`, gated on `matchIndex` |
| `:327-331` | 7 | `HandleAppendEntriesRequest` / `logOk` — the log-matching check |
| `:443` / `:448` | 3 | `DuplicateMessage`, `DropMessage` — the unreliable network, as actions |
| `:454-465` | 3 | `Next`, and `allLogs'` riding along on every step |
| `:469` | 3 | `Spec == Init /\ [][Next]_vars` — and then the file ends |

## Questions (answer in notes.md)

1. Add `Rejoin(r)` (crashed → alive, keeping its stale `wal`) to
   `WalReplication.tla`. What trace does TLC find, and what new mechanism is
   needed to rule it out? (You are re-deriving Raft's term check; compare your
   answer with `logOk` at `raft.tla:328-331`.)
2. Exhibit the quorum-intersection argument for `Quorum = 2`, `|Replicas| = 3`:
   why must the longest surviving log (line 77) hold every committed entry, and
   what exactly fails when `SyncCommit = FALSE` removes the premise? Then write
   the counterexample for a `Failover` that picks an *arbitrary* survivor.
3. `raft.tla:201-203` ships ≤1 entry per action "without loss of generality".
   Work out what bug class a "ship everything atomically" version of *our*
   `Ship` would hide, and say whether the WLOG claim would still hold.
4. Recompute Step 6's raft.tla estimate for 5 servers instead of 3, with the same
   term and log bounds. By what factor does it grow, and which single variable
   dominates? Now do it for `allLogs`.
5. Express topic 8's MVCC snapshot visibility as a TLA+ spec sketch: what are the
   variables, what is one action, what is the invariant? This is the M21
   deliverable's outline.
6. `[][Next]_vars` allows stuttering. Construct a two-spec refinement example
   (an abstract queue and a detailed one with an internal buffer) and identify
   which detailed steps must map to abstract stuttering steps.

## Done when

Answer each before unfolding it.

- [ ] You can explain a state as a variable snapshot and an action as a predicate over two states, and name the three layers of an action.

  <details><summary>Answer</summary>

  A state assigns values to all declared variables
  (`WalReplication.tla:17-21`, four of them); an action is a boolean predicate
  over unprimed (now) and primed (next) variables that is simply true of the
  transitions it permits — no assignment, no control flow.

  The three layers, on `Ship(r)` (lines 47–51): **enabling condition** (48–49) —
  in which states can it happen; **the change** (50) — `[wal EXCEPT ![r] = @+1]`,
  exactly one entry; **the frame** (51) — `UNCHANGED` pins every other variable.
  Dropping the frame permits those variables to take any value, producing a spec
  that checks nothing while still parsing.

  </details>

- [ ] You can compute the state space of `WalReplication.tla` at its `.cfg` model and account for the gap to TLC's measured figure.

  <details><summary>Answer</summary>

  `primary` 3 × `crashed` 2³ = 8 × `wal` 4³ = 64 × `committed` 4 = **6144**
  type-correct states. `Crash(r)` (line 65) requires `Cardinality(Alive \ {r}) >=
  Quorum` = 2, so with three replicas at most one ever crashes and only 4 of the
  8 `crashed` subsets occur: **3072**. TLC reports **1080 distinct** (`notes.md`).

  The remaining ~2.8× is what the other action guards prune — e.g. `committed >
  wal[primary]` is type-correct but unreachable because `Commit` (line 59)
  requires `committed < wal[primary]`, and `Failover` (line 77) requires the new
  primary to have the longest surviving log.

  </details>

- [ ] You can explain why `Next` being a disjunction gives concurrency for free, and point at where Raft's unreliable network lives.

  <details><summary>Answer</summary>

  Each step of a behavior is *any one* enabled disjunct, and nothing says which,
  so every interleaving of enabled actions is a behavior. `\E r \in Replicas`
  (`WalReplication.tla:84`) expands one line into one disjunct per replica. No
  scheduler is written because none is needed.

  Raft's network unreliability is two disjuncts: `DuplicateMessage(m)`
  (`raft.tla:462`) and `DropMessage(m)` (`raft.tla:463`). Loss and duplication
  are transitions, not assumptions.

  </details>

- [ ] You can explain why `[][Next]_vars` permits stuttering, and why removing it would break something real.

  <details><summary>Answer</summary>

  `[][Next]_vars` abbreviates `[](Next \/ UNCHANGED vars)`, so behaviors may
  contain steps in which nothing changes.

  It is required for **refinement**. To show a detailed spec `D` implements an
  abstract spec `A`, you map `D`'s states onto `A`'s and show each `D` step is an
  `A` step. `D` has internal actions `A` does not model — they must map to
  *something*, and what they map to is an `A` stuttering step. Without stuttering
  no implementation could refine any abstraction, and refinement is the reason
  TLA+ is a temporal logic rather than a state-machine notation. A side
  consequence: step counts are never a meaningful property of a behavior.

  </details>

- [ ] You can state the model-size discipline with a price on each knob, including what logs-as-lengths saved.

  <details><summary>Answer</summary>

  **Abstract the data**: `wal` as a length (`WalReplication.tla:20`) gives 4³ = 64
  values; as a sequence over 3 entry values bounded at length 3 it would be
  `(1+3+9+27)³ = 40³ = 64,000` — a **1000×** multiplier, legal only because
  the module header's "entries ship in order" makes logs prefixes by
  construction.

  **Keep atomic regions small**: `raft.tla:201-203` sends ≤1 entry "because it
  minimizes atomic regions without loss of generality". This *costs* states and
  buys interleaving coverage — the opposite direction from the first knob.

  **Small constants**: 3 replicas, 3 entries, on the small-scope bet (Daniel
  Jackson's, discussed in `reading-aws-cacm15.md`).

  For scale: five of raft.tla's thirteen variables at 3 servers / 3 terms /
  length-3 logs already give `27 × 27 × 64 × 64,000 × 64 ≈ 1.9 × 10¹¹`, and
  `allLogs` alone — a *set* of the 40 possible logs — has `2⁴⁰ ≈ 1.1 × 10¹²`
  values.

  </details>

- [ ] You can state the difference between safety and liveness, and say exactly what properties `raft.tla` checks.

  <details><summary>Answer</summary>

  Safety is violated by a finite trace ("nothing bad happens"), so an invariant
  check finds it and TLC prints a shortest counterexample. Liveness is violated
  only by an infinite behavior ("something good eventually happens") and requires
  **fairness** conjuncts such as `WF_vars(Ship(r))`, because without them the
  forever-stuttering behavior satisfies every spec and defeats every liveness
  property.

  `raft.tla` checks **nothing**: there is no `THEOREM`, no invariant definition,
  no `PROPERTY`, no `WF_`/`SF_` in its 471 lines, and no `.cfg` in the
  repository — which contains only `raft.tla` and a 9-line `README.md`. The
  README says to use "Jin Li's changes in Pull Request #4" to run TLC, and points
  at Chapter 8 and Appendix B of Ongaro's dissertation for the safety proof. Our
  own spec checks `TypeOK` and `Durability` — safety only, no `PROPERTIES`.

  </details>

- [ ] You can explain the depth-5 counterexample and which single conjunct causes it.

  <details><summary>Answer</summary>

  Line 60: `SyncCommit => Cardinality(AckedBy(committed + 1)) >= Quorum`. With
  `SyncCommit = FALSE` the implication is vacuously true, so `Commit` no longer
  waits for a quorum ack.

  Trace: `Append` (primary's `wal` → 1) → `Commit` (`committed` → 1 with no
  replica having the entry) → `Crash(primary)` → `Failover(r)` to a survivor
  whose `wal` is 0 → `Durability` (`committed <= wal[primary]`) fails, since
  `1 > 0`. Measured: **123 distinct states, depth 5, VIOLATED**. Breadth-first
  search guarantees this is a *shortest* such trace. It is PostgreSQL's
  `synchronous_commit = off` data-loss story in five steps.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the `Rejoin` trace and the quorum-intersection argument.

  <details><summary>Answer</summary>

  The shape to check yours against on question 1: once a crashed replica can
  rejoin with a stale `wal`, `Failover`'s "longest surviving log" (line 77) can
  select a replica that was primary in an *earlier* epoch and has entries the
  current primary never had — or, more simply, a rejoined stale replica can
  become primary and `Ship` can now move entries *backwards* relative to the
  committed point. The mechanism that rules it out is an epoch number attached to
  both leaders and entries, plus a check that the predecessor entry agrees on
  that epoch — which is exactly `logOk` at `raft.tla:328-331`.

  Question 2's argument in one line: with `|Replicas| = 3` and `Quorum = 2`, any
  two quorums share at least `2 + 2 − 3 = 1` replica, so a committed entry (on a
  quorum) is on at least one member of any surviving quorum, hence on the longest
  surviving log. `SyncCommit = FALSE` removes the premise "committed ⇒ on a
  quorum", and the whole argument collapses.

  </details>

## References

**Books**
- Leslie Lamport — *Specifying Systems: The TLA+ Language and Tools for Hardware
  and Software Engineers* (Addison-Wesley, 2002; free PDF from Lamport's site).
  Part I, chapters 1–7, is the language of Steps 1–5; chapter 8 is liveness and
  fairness (Step 8). The rest is reference material.

**Code**
- [raft.tla](https://github.com/ongardie/raft.tla) at `6ecbdbc` — Diego Ongaro's
  published Raft specification, **471 lines**. The repository contains only
  `raft.tla` and a 9-line `README.md`: **no `.cfg`, no invariants, no
  theorems**. The README directs would-be model checkers to Pull Request #4, and
  the safety proof to Chapter 8 (Correctness) and Appendix B of
  [Ongaro's dissertation](https://github.com/ongardie/dissertation).
- `specs/WalReplication.tla` (**92 lines**) and `specs/WalReplication.cfg` in
  this topic — the toy to read first: 4 variables, 5 actions, 2 invariants,
  1080 reachable states.

**In this topic**
- `notes.md` — the measured TLC runs quoted in Steps 5 and 6.
- [reading-aws-cacm15.md](reading-aws-cacm15.md) — why an organisation pays for
  this, what it costs per bug, and the correct attribution of the small-scope
  hypothesis behind Step 6's third knob.
