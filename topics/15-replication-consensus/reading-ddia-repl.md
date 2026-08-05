# Lag, lies, and linearizability

The concepts layer over this topic's code — Kleppmann's three
chapters give the vocabulary for everything valkey and Raft do. This
chapter builds that vocabulary step by step: what lag does to
readers (ch. 5), why partial failure and lying clocks make
distribution hard (ch. 8), and what linearizability and consensus
actually promise (ch. 9). Read ch. 5 alongside valkey's
`replication.c` and ch. 9 alongside the Raft paper; ch. 8 is the
connective tissue.

**On sourcing.** *Designing Data-Intensive Applications* (O'Reilly,
2017) is a copyrighted book and nothing here quotes it — chapters are
cited by number and every idea is restated in this repo's own words.
That has a happy side effect: **every number below comes from
somewhere you can re-run or re-open** — this topic's `repl_lag`
bench, topic 5's fsync ladder, the Raft paper, or pinned source.
Anchors are valkey at `8891441ab` and raft-rs at `ad13f3d` (the pin
table in `resources/codebases.md`); paths starting `experiments/` are
this topic's own crate. Check any of them with
`python3 tools/pinned-source.py show …`.

## The problem in one sentence

An async replica is always some milliseconds (or, during a
compaction stall, some *minutes*) behind its leader, and a client
whose reads land on different replicas can watch its own write
vanish, see time run backwards, or read an answer before its
question — unless the system promises one of a small set of
precisely-named guarantees, each with a price.

## The concepts, step by step

### Step 1 — replication lag: the gap between ack and everywhere

> **In:** a write that the leader has already acked. **Out:** a
> definition of replication lag, the measured size of it in this
> topic's own bench, and the reason lag is invisible until you scale
> reads out.

**Replication lag** is the delay between a write committing on the
leader and becoming visible on a given replica. Under **asynchronous
replication** — the leader acks the client without waiting for any
replica, valkey's design and the previous chapter's subject — lag is
unbounded by construction, because the ack never waited for anything.
Under **synchronous** or **semi-synchronous** replication the ack
waits for one or more replicas, which bounds lag at the cost of
putting a network round trip (and possibly an fsync) on the write
path.

You do not have to guess how big lag is; this topic measured it.
`./verify.sh 15` runs `repl_lag` with 2000 entries × 128 B,
group-commit every 64, WAIT-1 semantics, and varies only the
*follower's* fsync policy:

| follower fsync | entries/s | ack p50 | ack p99 |
|---|---|---|---|
| every entry | 341 | 2967.0 µs | 3889.5 µs |
| every 8 | 2730 | 22.2 µs | 2979.8 µs |
| every 64 | 12187 | 14.0 µs | 2133.0 µs |
| never | 20174 | 13.8 µs | 64.5 µs |

Read the p50/p99 gap as the honest picture of lag: at *every 8* the
median ack is 22 µs and the 99th percentile is 2980 µs — **134×
worse**. Lag is not a number, it is a distribution with a long tail,
and the tail is what your users hit. The tail sources are the ones
ch. 5 lists: a replica doing a full resync, a replica that hit disk,
a GC pause.

Convert the tail into staleness — the quantity a reader actually
cares about:

```
  How far behind is a replica at p99, in entries?

    inputs (notes.md baseline, Apple M3 Pro / APFS, 2026-07-28):
      throughput at "fsync every 64"   = 12,187 entries/s
      ack p99 at that setting          =  2,133 us = 2.133e-3 s

    entries the leader accepts inside one p99 window:
      12,187 x 2.133e-3 = 26.0 entries

  So a reader landing on that replica at the wrong moment sees a
  snapshot ~26 writes old. At "every entry" the throughput collapses
  to 341/s and the same arithmetic gives 341 x 3.8895e-3 = 1.3
  entries — the replica is nearly current, because the system is
  barely moving. Bounding lag by slowing down is a real option and a
  terrible one.
```

Lag is invisible to anyone who only talks to the leader; it becomes
real the moment reads are scaled out to replicas — which is the
entire reason to have replicas. So the question "what does a reader
see?" needs a taxonomy — Step 2.

### Step 2 — the anomaly catalog: three ways lag bites readers

> **In:** a client issuing a sequence of reads while lag is nonzero.
> **Out:** three named anomalies, the guarantee that kills each, and
> the specific thing each guarantee costs you.

Ch. 5's catalog is three reader experiences, each with a specific,
priced fix. The names are the point — "eventually consistent" is not
a specification, these are:

```
 anomaly                 guarantee that kills it    what it costs
 ─────────────────────────────────────────────────────────────────
 read-your-writes        read-your-writes /         offset bookkeeping
   (I posted, refresh,   read-after-write           per session; reads
    it's gone)                                      may block or divert
                                                    to the leader
 monotonic reads         monotonic reads            load-balancing
   (time goes backward                              freedom — the
    across refreshes)                               session is pinned
                                                    to one replica
 consistent prefix       consistent prefix reads    ordering machinery
   (answer arrives                                  across partitions,
    before question)                                or one partition
```

Two definitions worth being exact about, because they are commonly
blurred. **Read-your-writes** says a client sees its *own* writes; it
says nothing about anyone else's. **Monotonic reads** says a client
never sees the clock run backwards; it also says nothing about
freshness — a session pinned to a replica that is 26 entries behind
(Step 1's arithmetic) gets monotonic reads and stale data at the same
time. Neither is linearizability (Step 5); they are strictly weaker,
which is exactly why they are affordable.

The implementable version of read-your-writes is an **offset token**:
the client remembers the replication offset its write reached and
refuses any replica behind that offset. valkey exposes the raw
material — `getClientWriteOffset` (`src/replication.c:4953`) is how
WAIT learns the offset a client's write landed at, and each replica's
progress is tracked as `repl_ack_off`, counted by
`replicationCountAcksByOffset` (`src/replication.c:4962-4975`). What
valkey does *not* ship is the read-side check; that is your M15
stage-2 work.

Price the fix with the measured table. If a session must not read a
replica more than one entry stale, the wait is the ack latency:
13.8 µs at p50 in the *never fsync* row, 64.5 µs at p99. If the
follower fsyncs every 8, the same wait is 22.2 µs at p50 but
2979.8 µs at p99 — **the fix's cost is set by the durability policy,
not by the read path.** Question per anomaly: which does our M15
stage-1 follower exhibit, and what does the fix cost?

### Step 3 — what actually ships: statements, WAL bytes, or rows

> **In:** a committed write on the leader. **Out:** three candidate
> encodings for putting it on the wire, what each one breaks, and the
> exact line in valkey where the statement-shipping tax gets paid.

Ch. 5's other half is the replication-log format menu, and this topic
implements two of the three:

- **Statement-based** — ship the commands. Compact, and readable in
  `MONITOR`, but any **nondeterminism** (a random choice, `NOW()`, an
  auto-increment, a side effect that depends on local state) must be
  rewritten into a deterministic form before it leaves the leader, or
  the replicas diverge. This is valkey.
- **Physical WAL** — ship the storage engine's own log bytes.
  Deterministic by construction, because the replica is not
  re-deciding anything; but the stream is coupled to the engine
  version and page layout, so a replica must run compatible code.
  This is our M15 stage 1, and it is topic 5's WAL wearing a network
  card.
- **Logical (row-based)** — ship "row X became Y". Decoupled from the
  engine, therefore upgradable and consumable by outsiders; this is
  the format change-data-capture wants, and it is the fattest of the
  three on the wire.

The nondeterminism tax is not abstract — you can open it. valkey's
`SPOP` removes a *random* member, so shipping the command verbatim
would give every replica a different set. The rewrite happens
per-command, inside the command implementation: `spopCommand` picks
the member at `src/t_set.c:970` and then immediately rewrites the
client's own command vector into a deterministic `SREM` at
`src/t_set.c:975`:

```c
// valkey src/t_set.c — spopCommand, 969-978 (verbatim, no elisions)
   969      /* Pop a random element from the set */
   970      ele = setTypePopRandom(set);
   971  
   972      notifyKeyspaceEvent(NOTIFY_SET, "spop", c->argv[1], c->db->id);
   973  
   974      /* Replicate/AOF this command as an SREM operation */
   975      rewriteClientCommandVector(c, 3, shared.srem, c->argv[1], ele);
   976  
   977      /* Add the element to the reply */
   978      addReplyBulk(c, ele);
```

Line 975 is the whole idea of statement-based replication in one
call, and the comment above it at 974 says so out loud: what the
replica receives is never the command the client sent.
The count variant does the same thing in bulk —
`spopWithCountCommand` batches `SREM`s through `alsoPropagate`
(`src/t_set.c:922` and `:937`) or turns the whole thing into a
`DEL`/`UNLINK` when the set is emptied (`:790-791`), and suppresses
the original with `preventCommandPropagation` at `:949`. **Every one
of those is a place a contributor can forget**, which is the argument
against statement shipping stated as engineering rather than theory.

The tradeoff table maps onto topic 5's logging choices one-to-one
(physical vs logical redo is the same fork). Ch. 5's multi-leader and
leaderless sections preview topic 31 (CRDTs) — skim them on this pass.

### Step 4 — partial failure: timeouts guess, clocks lie, tokens fence

> **In:** silence from a node. **Out:** why silence is
> undiagnosable, what a fencing token is, and the two lines of pinned
> code where Raft has one and valkey's replication stream does not.

Ch. 8 is one argument: in a distributed system you cannot distinguish
{slow node, dead node, slow network, lost packet} — all four look
like silence. Three consequences to extract:

**Timeouts are the only failure detector**, and every timeout is a
guess. Guess short and you declare live nodes dead; guess long and
real failures stall the system. Your own crate makes the guess
explicit: `ELECTION_TIMEOUT_MIN = 10` and `ELECTION_TIMEOUT_MAX = 20`
ticks (`experiments/src/raft.rs:33-34`) against
`HEARTBEAT_INTERVAL = 3` (`:35`).

```
  What ratio of "detector" to "heartbeat" are you actually running?

    experiments/src/raft.rs:33-35
      election timeout  10..20 ticks
      heartbeat          3 ticks
      ratio              10/3 = 3.3x  to  20/3 = 6.7x

    raft-rs src/config.rs:112,115-116 (pinned defaults)
      const HEARTBEAT_TICK = 2
      election_tick     HEARTBEAT_TICK * 10  = 20
      heartbeat_tick    HEARTBEAT_TICK       =  2
      ratio             10x, written literally as "* 10" in the source

  Raft §5.6 requires broadcastTime << electionTimeout. 3.3x is thin:
  ONE dropped heartbeat plus one late one starts an election. Note
  also that experiments/src/raft.rs:79 currently derives the timeout
  from (id + seed) rather than drawing from `rng` — deterministic, so
  the tests are reproducible, but NOT the randomization Raft §5.2
  asks for. Fixing that is part of the exercise; the docstring at
  experiments/src/raft.rs:8 says so.
```

**Process pauses**: a GC or VM pause makes a live leader
dead-then-alive — it wakes *believing it still leads*. Scale it: at
qdrant's `tick_period_ms: 100` (`config/config.yaml:359`) a 2-second
stop-the-world pause is 20 ticks, past `ELECTION_TIMEOUT_MAX`, so the
pause *alone* elects someone else and the sleeper wakes stale. The
defense is a **fencing token**: a monotonically increasing number
issued with each grant of authority, checked by everyone downstream,
so the stale leader's older token is rejected.

Raft terms ARE fencing tokens, and you can point at the check. In
raft-rs, `Raft::step` tests `m.term < self.term` at `src/raft.rs:1416`
and, in the general case, drops the message:

```rust
// raft-rs src/raft.rs — Raft::step, the stale-term arm, 1416-1477
  1416          } else if m.term < self.term {
  1417              if (self.check_quorum || self.pre_vote)
  ...
  1466              } else {
  1467                  // ignore other cases
  1468                  info!(
  1469                      self.logger,
  1470                      "ignored a message with lower term from {from}",
  ...
  1476              }
  1477              return Ok(());
```

The woken leader's `MsgAppend` carries its old term, hits line 1416,
and dies at 1477 without touching any log. (The two branches above
1466 are refinements, not exceptions: 1417-1443 replies to a
stale-term heartbeat so the sender learns it is behind, and 1444
handles pre-vote, whose whole job is to *avoid* the term inflation
this rule punishes.)

valkey's replication stream has no such number. Its identity is
`replid`, 40 hex characters (`CONFIG_RUN_ID_SIZE = 40`,
`src/server.h:152`), and `changeReplicationId`
(`src/replication.c:2063-2066`) fills it with `getRandomHexChars` —
**random, therefore unordered**. Two replids cannot be compared to
decide which is newer, so a replica cannot reject a stale primary on
the strength of its id; it can only detect that the history differs
and full-resync. valkey does own a real fencing token, but it lives
in the cluster gossip layer, not the replication stream:
`currentEpoch` and `configEpoch` (`src/cluster_legacy.h:278-281`) are
monotonic `uint64_t`s. Standalone replication, the subject of the
previous chapter, has nothing in this slot — which is why its
failover story ends in split-brain and Raft's does not.

**Clock skew**: wall clocks drift and jump, so "leader for the next 5
seconds" (a **lease**) requires bounded clock error, while ReadIndex
(Step 5) needs no clock at all — it uses a message round instead of
time. That is not a philosophical preference; it is the difference
between an assumption you can test and one you can only hope for.

### Step 5 — linearizability: the single-copy illusion, defined

> **In:** a Raft cluster whose *writes* are already linearizable.
> **Out:** the definition, the reason reads are a separate problem,
> and the two priced fixes as they appear in raft-rs's own config.

**Linearizability** is the strongest single-object guarantee: the
system behaves as if there were exactly ONE copy of the data, with
every operation taking effect atomically at some instant between its
start and its ack. The test-worthy form: there exists a single total
order of operations, consistent with real time — once any read
returns a value, all later reads return it or newer.

Say what it is *not*, because ch. 7 and ch. 9 get conflated. It is a
**recency** guarantee about one object, not an **isolation level**
about many. Serializability says a set of multi-object transactions
is equivalent to *some* serial order; linearizability additionally
pins that order to real time. You can have either without the other.

The trap this topic keeps stepping on: Raft gives linearizable
WRITES, but reading from the leader without care is NOT linearizable
— a deposed leader partitioned from the majority can serve stale
reads while a new leader commits fresh writes. Your own test builds
exactly that world:
`stale_leader_uncommitted_entry_is_overwritten`
(`experiments/src/raft.rs:180`) strands a leader with one buddy in a
2-of-5 minority, lets the majority commit under a higher term, then
heals and asserts the stale entry is gone. Everything that test does
to *writes*, an uninstrumented read would have exposed to a client.

raft-rs prices the two fixes as an enum. `ReadOnlyOption`
(`src/read_only.rs:26-37`) has exactly two variants, and its own
doc comments state the trade:

```rust
// raft-rs src/read_only.rs — ReadOnlyOption, 24-37
    24  /// Determines the relative safety of and consistency of read only requests.
    25  #[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
    26  pub enum ReadOnlyOption {
    27      /// Safe guarantees the linearizability of the read only request by
    28      /// communicating with the quorum. It is the default and suggested option.
    29      #[default]
    30      Safe,
    31      /// LeaseBased ensures linearizability of the read only request by
    32      /// relying on the leader lease. It can be affected by clock drift.
    33      /// If the clock drift is unbounded, leader might keep the lease longer than it
    34      /// should (clock can move backward/pause without any bound). ReadIndex is not safe
    35      /// in that case.
    36      LeaseBased,
    37  }
```

And it spends them at `src/raft.rs:2168-2182`: `Safe` calls
`bcast_heartbeat_with_ctx` (`:2174`) — one heartbeat round to the
quorum before the read is answered — while `LeaseBased` answers
immediately from `self.raft_log.committed` (`:2177-2180`). Before
either, `:2146-2154` refuses the read outright if the leader has not
yet committed an entry in its own term (`commit_to_current_term()`),
which is Raft §8's no-op-entry rule showing up as a guard clause.

The defaults tell you which one the authors trust:

```
  raft-rs pinned defaults, src/config.rs
    read_only_option : ReadOnlyOption::Safe   (:124, and #[default] at read_only.rs:29)
    check_quorum     : false                  (:120)

  And LeaseBased is not merely discouraged, it is REFUSED unless you
  opt in: src/config.rs:204-206 returns a config error for
    read_only_option == LeaseBased && !check_quorum

  Cost per read, n = 5:
    Safe       1 broadcast (4 msgs) + acks from a quorum
               -> ~1 RTT added to EVERY read; no clock assumption
    LeaseBased 0 messages
               -> free, and correctness now rests on bounded clock
                  error, i.e. Step 4's unverifiable assumption

  Using this topic's measured ack p50 of 13.8 us as an RTT proxy, Safe
  costs ~14 us of added latency per read. That is the price of not
  trusting a clock.
```

Async replicas serve stale reads by design; that's not a bug, it's
the A in Step 6.

### Step 6 — CAP, consensus equivalence, and the FLP dodge

> **In:** a network partition, and a pile of impossibility results.
> **Out:** CAP stated narrowly enough to be true, the equivalence
> that closes the escape hatches, and why FLP does not doom Raft.

The closing vocabulary, three items:

- **CAP, properly**: during a network Partition, choose
  Available-but-stale or Consistent-but-unavailable on the minority
  side. It is a claim about one failure mode, not a general
  three-way menu — with no partition you get both. valkey chose A;
  Raft chose C. Your `minority_partition_cannot_commit` test
  (`experiments/src/raft.rs:158-177`) IS the C choice, executed: it
  strands a leader with one buddy, proposes, and asserts
  `committed() == []` — three nodes keep committing, two freeze,
  forever, by design.
- **Consensus ≡ atomic broadcast ≡ linearizable compare-and-set**:
  ch. 9's equivalence results. Solve any one and you have solved the
  others, and — the direction that matters — needing any one means
  you need consensus. "Just use a CAS register" and "just use a
  totally-ordered log" are not escapes; they are the same problem
  renamed. Worth carrying into design review, where it kills a lot of
  proposals in one sentence.
- **FLP**: in a fully asynchronous system (no timing assumptions at
  all), no deterministic consensus protocol can be *guaranteed* to
  terminate. Raft's randomized timeouts are the practical dodge —
  the protocol is no longer deterministic, so termination with
  probability 1 is available — and timeouts smuggle in the timing
  assumption FLP forbids. Raft §5.6 states the assumption openly as
  `broadcastTime ≪ electionTimeout ≪ MTBF`. FLP says you cannot get
  *guaranteed* termination for free; Raft agrees and buys it. One-
  sentence version for question 4.

## How to read the chapters (with the concepts in hand)

- **Ch. 5 (Replication)** — Steps 1–3. Read the anomaly catalog
  slowly and the log-format section with valkey's `SPOP` rewrite open
  ([reading-valkey-replication.md](reading-valkey-replication.md)
  Step 2 is the same fork, and `src/t_set.c:975` is the punchline).
  Skim multi-leader/leaderless — they return in topic 31.
- **Ch. 8 (The Trouble with Distributed Systems)** — Step 4. The
  chapter is long; extract exactly three things — timeouts as
  guesses, pauses + fencing tokens, clock skew vs leases — and move
  on. When it reaches fencing tokens, stop and open
  `raft-rs src/raft.rs:1416`; the chapter's argument and that line
  are the same claim.
- **Ch. 9 (Consistency and Consensus)** — Steps 5–6, with the Raft
  paper ([reading-raft-paper.md](reading-raft-paper.md)) beside it.
  The linearizability definition deserves a re-read until the
  deposed-leader timeline is obvious; then run
  `experiments/src/raft.rs:180` in your head. The equivalence section
  can be read for the statements alone, proofs skimmed.

## Questions for notes.md

1. Build the 2×3 matrix: {async, semi-sync, raft} × {read-your-
   writes, monotonic reads, consistent prefix} — which combos hold?
2. A client's WAIT 1 returns success, then the primary dies and a
   NON-acked replica is promoted. Which ch. 5 guarantee broke, and
   which ch. 9 property would have prevented it?
3. Fencing tokens: sketch how M15's follower rejects a stale
   leader's WAL stream using terms.
4. Why does FLP not doom Raft in practice? One sentence.
5. Linearizable-read options: leader lease vs ReadIndex vs quorum
   read — cost per read of each, and which M22 (the capstone's
   read-path milestone) should pick.

## Done when

Answer each before unfolding it.

- [ ] You can name the three read anomalies lag produces, give a user-visible symptom for each, and say what the fix costs.

  <details><summary>Answer</summary>

  **Read-your-writes**: you post a comment, refresh, and it is gone —
  the refresh landed on a replica behind your write. Fix: track the
  replication offset your write reached and refuse replicas behind
  it (valkey exposes the raw material at `src/replication.c:4953`
  and `:4962-4975`). Cost: per-session bookkeeping, plus a wait or a
  divert to the leader.

  **Monotonic reads**: you refresh twice and the second refresh shows
  *less* than the first — two reads landed on replicas at different
  offsets. Fix: pin the session to one replica. Cost: load-balancing
  freedom, and no freshness guarantee at all — a pinned session can
  be monotonic *and* 26 entries stale (Step 1's arithmetic).

  **Consistent prefix**: you see the answer before the question,
  because two causally-related writes went to differently-lagged
  partitions. Fix: causally-ordered delivery, or keep the causal set
  in one partition. Cost: ordering machinery across partitions.

  None of the three is linearizability; all three are cheaper, which
  is the point of naming them separately.

  </details>

- [ ] You can state what statement, WAL-byte and row shipping each make hard, and point at the line where valkey pays the statement tax.

  <details><summary>Answer</summary>

  Statement-based makes **nondeterminism** hard: every random choice,
  clock read, or local side effect must be rewritten before it ships.
  Physical WAL makes **version coupling** hard: the replica must
  understand the leader's page layout and engine version. Logical/row
  makes **size** hard, and needs a schema-aware encoder — in exchange
  it is the only one an outside consumer can read, which is why CDC
  uses it.

  valkey is statement-based, and the tax is paid per command inside
  the command: `spopCommand` picks a random member at
  `src/t_set.c:970` and rewrites the command into a deterministic
  `SREM` at `src/t_set.c:975`. The count variant does it in bulk —
  batched `alsoPropagate` SREMs at `src/t_set.c:922` and `:937`, a
  `DEL`/`UNLINK` when the set empties at `:790-791`, and
  `preventCommandPropagation` at `:949` to suppress the original.

  M15 stage 1 ships physical WAL bytes, so it has none of this
  problem and all of the coupling one.

  </details>

- [ ] You can explain what a fencing token prevents that a timeout cannot, and name the pinned line where Raft checks one.

  <details><summary>Answer</summary>

  A timeout decides *when* to stop believing in a node; it cannot
  stop a node that has already stopped believing in itself and then
  changed its mind. The GC-paused leader wakes convinced it still
  leads, and no timeout on any other node can prevent it from sending
  a write. A **fencing token** — a monotonically increasing number
  issued with authority and checked by every downstream recipient —
  can, because the sleeper's number is old.

  In raft-rs the token is the term, and the check is
  `} else if m.term < self.term {` at `src/raft.rs:1416`, whose
  general arm logs "ignored a message with lower term" and returns at
  `:1466-1477` without touching the log.

  valkey's replication stream has no ordered token: `replid` is 40
  random hex chars (`CONFIG_RUN_ID_SIZE = 40`, `src/server.h:152`;
  `changeReplicationId` calls `getRandomHexChars` at
  `src/replication.c:2063-2066`), so two ids cannot be ranked.
  valkey's real epochs — `currentEpoch` / `configEpoch`,
  `src/cluster_legacy.h:278-281` — are monotonic but live in cluster
  gossip, not in the replication stream.

  </details>

- [ ] You can define linearizability precisely enough to say why it is a recency guarantee and not an isolation level.

  <details><summary>Answer</summary>

  There exists a single total order over all operations on the
  object, consistent with real time, such that each operation appears
  to take effect atomically at one instant between its invocation and
  its response. Consequence: once any read returns a value, every
  later read returns that value or a newer one.

  It is about **one object** and about **recency**. Serializability
  is about **many objects** and about **equivalence to some serial
  order**, with no requirement that the order respect wall-clock
  precedence — a serializable system may legally order your
  transaction before one that finished an hour earlier.
  Strict serializability is the conjunction of the two.

  So "we're serializable" does not answer "will my read see my
  write", and "we're linearizable" does not answer "can two of my
  updates interleave".

  </details>

- [ ] You can state the cost per read of ReadIndex versus a leader lease, and say which one raft-rs makes you opt into.

  <details><summary>Answer</summary>

  **ReadIndex** (`ReadOnlyOption::Safe`, `src/read_only.rs:26-30`):
  the leader records its commit index, broadcasts a heartbeat with a
  context (`bcast_heartbeat_with_ctx`, `src/raft.rs:2174`), and
  answers only once a quorum has replied. Cost: one round trip per
  read — roughly 14 µs using this topic's measured ack p50 of
  13.8 µs as an RTT proxy. Assumption: none about clocks.

  **Leader lease** (`LeaseBased`, `read_only.rs:31-36`): answer
  immediately from `self.raft_log.committed`
  (`src/raft.rs:2177-2180`). Cost: zero messages. Assumption:
  bounded clock error — and the enum's own doc says an unbounded
  drift means "ReadIndex is not safe in that case".

  raft-rs defaults to `Safe` (`#[default]` at `read_only.rs:29`,
  `src/config.rs:124`) and actively refuses `LeaseBased` unless you
  also set `check_quorum`, which defaults to `false`
  (`src/config.rs:120`, error at `:204-206`). Both paths are gated by
  `commit_to_current_term()` at `src/raft.rs:2146-2154` — a leader
  that has not yet committed in its own term serves no reads at all.

  For M22, `Safe` is the default answer: one RTT is cheap next to the
  measured 2133 µs p99 the write path already carries, and it costs
  no assumption you cannot test.

  </details>

- [ ] You can fill in the 2x3 matrix of {async, semi-sync, raft} x {read-your-writes, monotonic reads, consistent prefix}.

  <details><summary>Answer</summary>

  The trap is that none of the three rows gives you any of the three
  columns *by itself* — every cell is "only if you also do X", and
  naming X is the exercise.

  Async replication with replica reads: none of the three hold. It
  gives read-your-writes only if the session is routed to the leader
  or gated on an offset; monotonic reads only if the session is
  pinned; consistent prefix only within a single partition's stream.

  Semi-sync (valkey `WAIT n`): read-your-writes becomes *purchasable*
  — the write blocks until n replicas ack — but note what the ack
  means. `replicationCountAcksByOffset`
  (`src/replication.c:4962-4975`) counts replicas whose
  `repl_ack_off` has passed the offset, i.e. **received**, not
  fsynced. `WAITAOF` (`waitaofCommand`, `src/replication.c:5030`,
  counting via `:4979`) is the durability-aware sibling. Monotonic
  reads and consistent prefix are unchanged: still routing problems.

  Raft with ReadIndex reads: all three hold, because linearizability
  implies all three. Raft with *unguarded leader reads*: none are
  guaranteed, per Step 5's deposed-leader case — which is the whole
  reason `ReadOnlyOption` exists.

  </details>

- [ ] You can trace the WAIT-1-then-failover sequence and name both the ch. 5 guarantee that broke and the ch. 9 property that would have held.

  <details><summary>Answer</summary>

  Sequence: the client writes and calls `WAIT 1`; replica B acks
  receipt at the right offset, so `replicationCountAcksByOffset`
  (`src/replication.c:4962-4975`) returns 1 and `waitCommand`
  (`src/replication.c:4996-5026`) succeeds. The primary dies. The
  operator promotes replica **C**, which never received the write.
  `shiftReplicationId` (`src/replication.c:2082-2095`) gives C a new
  random `replid` and history continues without the entry. The client
  reads and its acked write is gone.

  Broken: durability of an acknowledged write, and with it
  read-your-writes. The ch. 9 property that would have prevented it
  is Raft's **Leader Completeness** — a node lacking a committed
  entry cannot win an election, enforced by the up-to-dateness check
  in §5.4.1. valkey has no such restriction because it has no votes;
  promotion is whatever the operator or sentinel says.

  Two further traps in the same sequence. `WAIT` counts *received*,
  not fsynced (`WAITAOF`, `src/replication.c:5030`, is the one that
  counts `repl_aof_off`); and even a fsynced ack is only durable if
  the flush was a real one — topic 5 measured that on macOS/APFS
  `F_FULLFSYNC` runs at 337 commits/s, which is why this topic's
  per-entry-fsync row sits at 341 entries/s. A cheap `fsync(2)` that
  returns in microseconds on that platform proved nothing.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including why FLP does not doom Raft in practice.

  <details><summary>Answer</summary>

  One sentence: FLP forbids *guaranteed* termination for a
  deterministic protocol in a fully asynchronous model, and Raft is
  neither — its election timeouts assume bounded-enough timing
  (§5.6's `broadcastTime ≪ electionTimeout ≪ MTBF`) and its
  randomization makes it non-deterministic, so it terminates with
  probability 1 rather than by proof.

  The empirical half of the answer is Figure 16 of the extended Raft
  paper: with no randomness the 5-server cluster consistently took
  over 10 s to elect; with 5 ms of randomness the median downtime was
  287 ms; with a 12–24 ms timeout the average was 35 ms and the worst
  case 152 ms. FLP is not violated by any of that — none of it is a
  *guarantee* — and none of it matters to an operator.

  Your own crate is where the assumption becomes a constant:
  `ELECTION_TIMEOUT_MIN/MAX = 10/20` ticks against
  `HEARTBEAT_INTERVAL = 3` (`experiments/src/raft.rs:33-35`), a
  3.3–6.7× ratio where raft-rs ships 10× (`src/config.rs:112,115-116`,
  where the constant is literally `HEARTBEAT_TICK * 10`).

  </details>

## References

**Papers / Books**
- Martin Kleppmann — *Designing Data-Intensive Applications*
  (O'Reilly, 2017) — ch. 5 (Replication), ch. 8 (The Trouble with
  Distributed Systems), ch. 9 (Consistency and Consensus). Pair ch. 5
  with [reading-valkey-replication.md](reading-valkey-replication.md)
  and ch. 9 with [reading-raft-paper.md](reading-raft-paper.md).
  Copyrighted; cited by chapter number and paraphrased here, never
  quoted.
- Ongaro, Ousterhout — *In Search of an Understandable Consensus
  Algorithm (Extended Version)* — §5.4.1 (up-to-dateness, the
  Leader Completeness enforcement), §5.6 (the timing inequality),
  §8 (the no-op entry behind `commit_to_current_term`), Figure 16
  (measured election downtime). See
  [reading-raft-paper.md](reading-raft-paper.md) for the
  extended-vs-ATC'14 disambiguation.

**Code** — all anchors are valkey at `8891441ab` and raft-rs at
`ad13f3d`; `experiments/` is this topic's own crate
- [valkey](https://github.com/valkey-io/valkey) —
  `src/t_set.c:970,975` (the SPOP rewrite), `src/replication.c:4953`,
  `:4962-4975`, `:4996-5026`, `:5030` (WAIT / WAITAOF),
  `:2063-2066`, `:2082-2095` (replid), `src/cluster_legacy.h:278-281`
  (the epochs that *are* fencing tokens)
- [raft-rs](https://github.com/tikv/raft-rs) — `src/raft.rs:1416`
  and `:1466-1477` (terms as fencing tokens),
  `src/read_only.rs:26-37` and `src/raft.rs:2146-2182` (ReadIndex vs
  lease), `src/config.rs:120`, `:124`, `:204-206` (the defaults that
  encode the authors' opinion)
