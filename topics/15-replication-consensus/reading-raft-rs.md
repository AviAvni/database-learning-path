# raft-rs: consensus with the I/O left out

The production Raft that tikv and qdrant embed, and the design worth
stealing: the library owns ONLY the state machine — no threads, no
I/O, no storage. You drive it with `tick()`/`step(msg)` and it hands
you a `Ready` bundle of work to do. That inversion is what makes
consensus testable — and what our sim-based raft.rs stub imitates.
Before the anchors, this chapter builds the design in steps: why
I/O-free, the driving contract, the ordering rules that carry
safety, the repair loop, and where the paper's Fig 2 lives in the
source. Assumes [reading-raft-paper.md](reading-raft-paper.md) —
terms, the consistency check, and §5.4.2 are used by name.

Every `file:line` below is **raft-rs at `ad13f3d`**, the revision in
this repo's pin table (`resources/codebases.md`). Check any of them
with `python3 tools/pinned-source.py show raft-rs src/raft.rs -r
939:950`. Numbers from other revisions will not line up; `src/raft.rs`
is 2966 lines at this pin and `src/raw_node.rs` is 840.

## The problem in one sentence

A consensus bug surfaces once per thousand failovers under a
specific interleaving of timeouts, crashes, and message
reorderings — so an implementation entangled with real threads,
sockets, and fsyncs can never reproduce its own worst bug, and the
fix is to make the algorithm a pure state machine whose every input
(time included) is a function argument.

## The concepts, step by step

### Step 1 — sans-io: the algorithm as a pure state machine

> **In:** the idea that a consensus algorithm is a function of its
> inputs. **Out:** the shape of raft-rs's public surface — two input
> methods, one output bundle — and the name of the thing the library
> deliberately does not own.

**Sans-io** is the pattern (the phrase postdates raft-rs) of writing
a protocol as a state machine that *describes* I/O rather than
performing it. Nothing in `src/` opens a socket, starts a thread, or
calls `fsync`. `Raft<T: Storage>` (raft.rs:263) is generic over a
**`Storage`** trait — a read-only interface the library calls to
*fetch* log entries it has already handed you, never to write them.
Writing is your job, described by the `Ready` bundle:

```
            ┌────────────── your code ──────────────┐
            │ timer → tick()      network → step()  │
            │                 ▼                     │
            │            RawNode<Storage>           │
            │                 │                     │
            │           has_ready()?                │
            │                 ▼                     │
            │  Ready { messages, persisted_messages │
            │          entries, snapshot, hs, ss,   │
            │          committed_entries }          │
            │   1. persist entries + hardstate      │
            │   2. send messages                    │
            │   3. apply committed_entries          │
            │   4. advance()                        │
            └───────────────────────────────────────┘
```

**HardState** is the paper's Fig-2 "persistent state on all servers"
made concrete: `{ term, vote, commit }` — the three fields that must
survive a crash. **SoftState** is the derived, throwaway pair
`{ leader_id, raft_state }`. The split exists so an embedder knows
exactly which bytes it is obliged to fsync.

Deterministic by construction: the same sequence of tick/step calls
always produces the same Ready bundles — which is exactly why our
`sim.rs` can test consensus without threads (and why topic 16's DST
loves this shape). What it costs: every embedder must implement the
driving loop and get its ordering rules right (Step 4) — the library
moved the hard-to-test part out, not away. Step 1's ordering above is
the *approximate* contract; Step 4 is where it turns out to have a
deliberate exception.

### Step 2 — driving it: tick and step, the only two inputs

> **In:** a running process with a timer and a socket. **Out:** the
> two calls that carry all of it into the library, the real dispatch
> order inside `step`, and the tick-to-milliseconds arithmetic that
> turns "election timeout" into a wall-clock number.

Time and network collapse into two methods. `tick()` — you call it
on your own timer; the library counts ticks, and enough of them
without a heartbeat makes the state machine decide "election
timeout" and emit vote requests in the next Ready. `step(msg)` — you
received a Raft message; hand it over.

The library has **four** roles, not the paper's three
(raft.rs:61-71): `Follower`, `Candidate`, `Leader`, and
**`PreCandidate`** — the extra state implements pre-vote, where a
node polls for votes *without* bumping its term, so a partitioned
node cannot return and force a term change. Pre-vote is off by
default (`pre_vote: false`, config.rs:121).

`step` (raft.rs:1346-1537) does **not** simply do "term logic, then
role dispatch". Read the order:

```rust
// raft.rs — Raft::step, the dispatch skeleton, 1346-1537 (bodies elided)
  1346      pub fn step(&mut self, m: Message) -> Result<()> {
  1348          // Handle the message term, which may result in our stepping down to a follower.
  ....         // ... 1348-1478: term comparison, become_follower, pre-vote replies ...
  1483          match m.get_msg_type() {
  1484              MessageType::MsgHup => self.hup(false),
  1485              MessageType::MsgRequestVote | MessageType::MsgRequestPreVote => {
  ....                 // ... 1486-1528: vote decision, log up-to-dateness, reply ...
  1529              }
  1530              _ => match self.state {
  1531                  StateRole::Candidate | StateRole::PreCandidate => self.step_candidate(m)?,
  1532                  StateRole::Follower => self.step_follower(m)?,
  1533                  StateRole::Leader => self.step_leader(m)?,
  1534              },
  1535          }
```

The load-bearing line is **1530**: role dispatch is the `_` arm, the
*last* case. Two message types are handled before it. `MsgHup`
(1484) is the local "your election timer fired" signal — it has no
sender and no term, so role dispatch would have nothing to dispatch
on. `MsgRequestVote` / `MsgRequestPreVote` (1485-1529) are handled
role-independently because the paper's Fig 2 states the voting rule
under "all servers": grant at most one vote per term, and only to a
log at least as up-to-date as yours. Putting that in one place is
what makes Election Safety a property of `step` rather than of three
separate role handlers agreeing.

Worked arithmetic — turning ticks into milliseconds. The inputs, all
from `src/config.rs`:

```
  heartbeat_tick        = 2          (config.rs:112, 116)
  election_tick         = 2 × 10 = 20 (config.rs:115)
  min_election_tick()   = election_tick     = 20  (config.rs:147-153)
  max_election_tick()   = 2 × election_tick = 40  (config.rs:157-163)

  reset_randomized_election_timeout (raft.rs:2854-2866) draws
  uniformly from [min, max) = [20, 40) ticks.

  A tick is whatever period YOU call tick() on. At qdrant's
  tick_period_ms: 100 (qdrant config/config.yaml:359):

    heartbeat interval =  2 × 100 ms =   200 ms
    election timeout   ∈ [20, 40) × 100 ms = [2.0 s, 4.0 s)

  At a 10 ms tick the same constants give 20 ms heartbeats and a
  200–400 ms election timeout — the Raft paper's §5.6 recommendation
  of 150–300 ms. The constants are unitless; the tick period is the
  whole configuration.
```

So "raft-rs's default election timeout" is not a duration at all.
Anyone quoting one without naming a tick period has skipped a
multiplication.

### Step 3 — Progress: what the leader knows about each follower

> **In:** a leader that has appended entry 7 locally and heard back
> from some followers. **Out:** the two per-follower indexes, the
> three-file path from those indexes to a commit index, and the
> arithmetic on a concrete `matched` vector.

The leader tracks, per follower, two indexes (tracker/progress.rs:10,
12) and a state (progress.rs:22):

```
 matched   highest index KNOWN replicated on that follower  (:10)
 next_idx  next index to send (optimistic; decremented on reject) (:12)
 state     Probe | Replicate | Snapshot  (tracker/state.rs:22-30)
```

**Probe** means "I am unsure where this follower's log diverges —
send one entry and wait"; **Replicate** means "I know, pipeline
freely"; **Snapshot** means "the follower is so far behind that the
entries it needs have been compacted away, so send it a snapshot
instead". Each state is an answer to a different repair cost.

`matched` feeds commitment — and the commit computation is spread
across **three** files, which is the thing to trace rather than
memorise:

```rust
// raft.rs — Raft::maybe_commit, 939-950, the entry point
   939      pub fn maybe_commit(&mut self) -> bool {
   940          let mci = self.mut_prs().maximal_committed_index().0;
   941          if self.r.raft_log.maybe_commit(mci, self.r.term) {
   ...              // update own Progress, return true
   947              return true;
   948          }
   949          false
   950      }
```

```rust
// quorum/majority.rs — MajorityConfig::committed_index, 70-98 (setup elided)
    94          // Reverse sort.
    95          matched.sort_by(|a, b| b.index.cmp(&a.index));
    96
    97          let quorum = crate::majority(matched.len());
    98          let quorum_index = matched[quorum - 1];
```

```rust
// raft_log.rs — RaftLog::maybe_commit, 524-526, where §5.4.2 actually lives
   524      /// Attempts to commit the index and term and returns whether it did.
   525      pub fn maybe_commit(&mut self, max_index: u64, term: u64) -> bool {
   526          if max_index > self.committed && self.term(max_index).is_ok_and(|t| t == term) {
```

Line **526** is §5.4.2 as one boolean: the majority-replicated index
counts only if the entry sitting there is from the *current* term.
Note where it is not — `raft.rs:939` computes the index and delegates
the safety test; the check is in `raft_log.rs`, not in the function
named `maybe_commit` on `Raft`. If you go looking for §5.4.2 in
raft.rs you will not find it.

Worked example. Five voters, `matched = [7, 5, 5, 3, 2]`, leader
term 4:

```
  1. reverse sort (majority.rs:95)      → [7, 5, 5, 3, 2]
  2. majority(5)                        → (5 / 2) + 1 = 3   (util.rs:117-119)
  3. matched[quorum - 1] = matched[2]   → 5                 (majority.rs:98)
  4. raft_log.maybe_commit(5, 4)        → commits iff term(5) == 4
                                          (raft_log.rs:526)

  Read step 3 as: at least 3 of the 5 have index ≥ 5, because the
  vector is sorted descending and position 2 is the third element.
  If term(5) == 3, nothing commits — Figure 8 is exactly the case
  where committing it would be wrong.
```

The library's own doc comment carries a second example to check
yourself against (majority.rs:68, repeated at tracker.rs:282):
`[2,2,2,4,5]` returns **2**.

### Step 4 — the Ready contract: ordering is the safety, with one exception

> **In:** a `Ready` bundle in hand. **Out:** which of its two message
> lists you may send before your fsync completes, why the leader is
> exempt, and the citation that authorises the exemption.

`has_ready()` (raw_node.rs:562) polls for pending work; `ready()`
(:487-558) hands you the bundle; `advance()` (:663) confirms you did
it. The obvious rule — *persist everything before sending anything* —
is what most write-ups state, and it is **not** what raft-rs
implements. `Ready` carries **two** message lists, and one line
decides which one your messages land in:

```rust
// raw_node.rs — the tail of RawNode::ready, 553-556
   553          // Leader can send messages immediately to make replication concurrently.
   554          // For more details, check raft thesis 10.2.1.
   555          rd.is_persisted_msg = raft.state != StateRole::Leader;
   556          rd.light = self.gen_light_ready();
```

`Ready::messages()` (raw_node.rs:184-190) returns the list **only
when `is_persisted_msg` is false** — i.e. only for a leader.
`Ready::persisted_messages()` (:205-211) returns it only when true —
i.e. for a follower, candidate or pre-candidate — and its doc comment
(:202-203) states the obligation: "outbound messages to be sent AFTER
the HardState, Entries and Snapshot are persisted to stable storage."

So the real contract is asymmetric:

| your role | list | may you send before your own fsync? |
|---|---|---|
| Leader | `messages()` | **yes** |
| Follower / Candidate / PreCandidate | `persisted_messages()` | **no** |

The citation at line 554 is Ongaro's **dissertation** §10.2.1 (a
different document from the ATC '14 paper — see the paper chapter),
"Writing to the leader's disk in parallel", pp. 141-142 with Figure
10.2. The argument: an entry is committed when a *majority* has it on
disk; the leader's own disk is one of those, but not a required one.
Quoting the thesis directly: "The leader may even commit an entry
before it has been written to its own disk, if a majority of
followers have written it to their disks; this is still safe.
LogCabin implements this optimization." A follower gets no such
exemption, because its `PREPARE`-equivalent reply (`MsgAppendResponse`)
is the *evidence* the leader counts — acking bytes you have not
persisted is a lie the leader will act on. Likewise a vote response:
`voted_for` must be on disk before `MsgRequestVoteResponse` leaves,
or a crash-and-restart lets the node vote twice in one term and elect
two leaders.

There is a second knob for the same tradeoff. `must_sync()`
(raw_node.rs:223-232) is **false** iff (a) no HardState changed, or
only its `commit` field did, **and** (b) there are no entries and no
snapshot — in which case an asynchronous HardState write is
permissible. It is set true at :517 (vote or term changed), :543
(snapshot present) and :549 (entries present). A `commit`-only
HardState change is not worth an fsync because a lost commit index is
recomputable from the log; a lost `vote` is not.

`advance()` (:663) is `advance_append` + `advance_apply_to`;
`advance_append` (:678-681) lets you ack persistence separately from
apply, which is how you group-commit the raft log — topic 5's fsync
ladder applied to consensus, and directly the reason topic 15's own
measured table moves from 341 entries/s at one fsync per entry to
12,187 entries/s at one per 64.

### Step 5 — the repair loop, and the optimisation the paper doubted

> **In:** a newly elected leader whose log diverges from a follower's
> at index 100 of 1000. **Out:** the round-trip cost of naive
> probing, the paper's fix, and the four line numbers proving
> raft-rs implements it.

`next_idx` implements the paper's repair loop — send from `next_idx`,
and on rejection decrement and retry until the consistency check
passes. Naively that is **one round trip per diverging entry**: 900
entries of divergence at a 1 ms RTT is 900 ms of probing, and the
raft-rs authors are blunter than that. Their comment
(raft.rs:1783-1789) says naive probing "can easily result in hours of
time spent probing and can even cause outright outages."

The fix is in the Raft paper's **§5.3 body text** — a paragraph, not
a footnote, and the paper adds "In practice, we doubt this
optimization is necessary". The rejecting follower returns the *term*
of its conflicting entry plus the first index it stores for that
term; the leader then skips a whole term per round trip instead of
one entry.

raft-rs implements it. Follower side, `handle_append_entries`
(raft.rs:2539-2554): compute `hint_index = min(m.index, last_index)`,
call `find_conflict_by_term` (raft_log.rs:222-248, which walks the
log down while `term > t`), and set both `reject_hint` and `log_term`
on the response. Leader side: read the hint at raft.rs:1747-1750 and
feed it to `pr.maybe_decr_to` at raft.rs:1799. Four anchors, one
optimisation — so the answer to "does raft-rs implement it" is yes,
and the paper's own doubt did not survive contact with production.

### Step 6 — what our raft.rs keeps / drops

> **In:** the library as walked above. **Out:** the subset our
> `experiments/` stub keeps, and the rule for telling a safe
> simplification from a latent bug.

The stub in `experiments/` is this library minus everything the sim
makes unnecessary:

| raft-rs | our stub |
|---|---|
| Ready bundle + advance | direct send via `Sim` (no I/O to defer) |
| Storage trait + persistence | in-memory `Vec<(term, cmd)>` |
| Progress probe/replicate/snapshot states | just `next_idx` decrement |
| `find_conflict_by_term` fast backup | absent — O(divergence) probes |
| joint-consensus membership | fixed peer set |
| pre-vote (the `PreCandidate` role), leases, learners | absent |

Same invariants pinned by tests; ~10× less plumbing. The test to
apply to each row: *does the sim make this unobservable, or merely
unlikely?* Dropping the Ready bundle is safe because the sim has no
I/O to defer — there is no window in which a message can outrun a
write. Dropping `find_conflict_by_term` is safe because it is a
performance optimisation with no safety content. Dropping pre-vote is
**not** in the same category: it changes which terms get created
under a partition, so a sim that exercises partitions will see
different histories with and without it.

## Where each step lives in the code

All anchors are raft-rs at `ad13f3d`.

| anchor | what it is | step |
|---|---|---|
| raft.rs:61-71 | `StateRole` — four variants, incl. `PreCandidate` | 2 |
| raft.rs:263 | `Raft<T: Storage>` — the actual state machine | 1 |
| raw_node.rs:293 | `RawNode` — the public wrapper | 1 |
| raw_node.rs:184-190 / 202-211 | `messages()` vs `persisted_messages()` | 4 |
| raw_node.rs:223-232 | `must_sync()` — when an async HardState write is legal | 4 |
| raw_node.rs:487-558 | `ready()` — collect pending work | 4 |
| raw_node.rs:553-555 | the leader exemption (`is_persisted_msg`) | 4 |
| raw_node.rs:562 | `has_ready()` — the poll predicate | 4 |
| raw_node.rs:663 | `advance()` — "I did the work" | 4 |
| raw_node.rs:678-681 | `advance_append` — split persistence ack | 4 |
| raft.rs:939-950 | `maybe_commit` — computes the index, delegates the test | 3 |
| tracker.rs:284-288 | `maximal_committed_index` | 3 |
| quorum/majority.rs:95/98 | reverse sort, then `matched[quorum-1]` | 3 |
| util.rs:117-119 | `majority(total) = (total / 2) + 1` | 3 |
| raft_log.rs:526 | the §5.4.2 current-term test | 3 |
| raft.rs:1148/1176/1226 | `become_follower/candidate/leader` | 2 |
| raft.rs:1283 | `campaign` | 2 |
| raft.rs:1346-1537 | `step` — term logic, MsgHup, votes, then roles | 2 |
| raft.rs:1530-1534 | the role dispatch, as the `_` arm | 2 |
| raft.rs:1539 | `hup` — election timeout fires | 2 |
| raft.rs:1747-1750 / 1799 | leader consumes the backup hint | 5 |
| raft.rs:1783-1789 | "hours of time spent probing" | 5 |
| raft.rs:2045/2291/2348 | `step_leader/candidate/follower` | 2 |
| raft.rs:2539-2554 | follower builds the backup hint | 5 |
| raft.rs:2854-2866 | `reset_randomized_election_timeout` | 2 |
| raft_log.rs:222-248 | `find_conflict_by_term` | 5 |
| tracker/progress.rs:10/12/22 | `Progress { matched, next_idx, state }` | 3 |
| tracker/state.rs:22-30 | `ProgressState { Probe, Replicate, Snapshot }` | 3 |
| config.rs:112-116, 147-163 | tick defaults and the election-tick range | 2 |

Read order: `raw_node.rs` around `ready()`/`advance()` first (the
contract, including line 555), then `raft.rs:1346` `step` and follow
one message type down each role branch, then the three-file
`maybe_commit` chain. qdrant's production driving loop for this exact
API is the next chapter
([reading-qdrant-consensus.md](reading-qdrant-consensus.md)).

## Questions for notes.md

1. Why does raft-rs contain no `fsync`, no sockets, no threads —
   and what does that buy tikv/qdrant integration-wise?
2. `maybe_commit`: write out the sorted-matched-index computation
   for 5 nodes with matched = [7,5,5,3,2]. Commit index?
3. next_idx decrement-and-retry is O(divergence) round trips — what
   optimization does the paper's §5.3 suggest, and does raft-rs
   implement it?
4. advance_append: how does splitting the persistence ack enable
   pipelining, and what must you still NOT reorder?
5. Map Ready → M15 stage 2: which parts of your WAL commit path
   play the roles of persist/send/apply/advance?

## Done when

Answer each before unfolding it.

- [ ] You can explain what sans-io buys and why raft-rs contains no fsync, no sockets and no threads.

  <details><summary>Answer</summary>

  It makes the algorithm a deterministic function of its inputs.
  Every input that would normally be ambient — the clock, the
  network, the disk — becomes an argument: time arrives via `tick()`,
  messages via `step(msg)`, and all output is a `Ready` value rather
  than a syscall. `Raft<T: Storage>` (raft.rs:263) is generic over a
  trait that only *reads*.

  The payoff is that a bug which needs a specific interleaving of
  timeout, crash and reorder can be reproduced by replaying a
  sequence of `tick`/`step` calls, with no threads and no wall clock
  involved. Our `sim.rs` is the same trick at a smaller scale.

  The cost is that the untestable part did not vanish, it moved: every
  embedder writes its own driving loop and must get Step 4's ordering
  right. qdrant's is `Consensus::start` (qdrant `src/consensus.rs:481`)
  and it is roughly 500 lines.

  </details>

- [ ] You can write out the `maybe_commit` sorted-matched-index computation from memory, and name the file each of its three stages lives in.

  <details><summary>Answer</summary>

  `Raft::maybe_commit` (raft.rs:939-950) asks the tracker for a
  candidate index, then hands it to the log. `ProgressTracker::
  maximal_committed_index` (tracker.rs:284-288) forwards to
  `MajorityConfig::committed_index` (quorum/majority.rs:70-124), which
  reverse-sorts the matched vector at :95 and takes `matched[quorum-1]`
  at :98, with `majority(total) = (total / 2) + 1` (util.rs:117-119).

  For `[7,5,5,3,2]`: sorted descending it is unchanged, `majority(5)`
  is 3, so `matched[2]` = **5** — at least three of five have index
  ≥ 5.

  The §5.4.2 test is *not* in either of those. It is
  `RaftLog::maybe_commit` (raft_log.rs:526): commit only if
  `max_index > self.committed` **and** `term(max_index) == term`. With
  a leader in term 4 and entry 5 from term 3, nothing commits.

  </details>

- [ ] You can state the Ready contract's ordering rules, and say precisely who is allowed to send before persisting and on whose authority.

  <details><summary>Answer</summary>

  Persist entries, snapshot and HardState; send messages; apply
  committed entries in order and never above what is persisted; then
  `advance()`. The exception is the ordering of the first two, and it
  depends on your role. `RawNode::ready` sets `rd.is_persisted_msg =
  raft.state != StateRole::Leader` (raw_node.rs:555), which routes a
  leader's outbound messages into `Ready::messages()` (:184-190,
  sendable immediately) and everyone else's into
  `Ready::persisted_messages()` (:205-211, whose doc comment at
  :202-203 requires the write first).

  The authority is cited in the code at raw_node.rs:554: Ongaro's
  dissertation §10.2.1, pp. 141-142 — "The leader may even commit an
  entry before it has been written to its own disk, if a majority of
  followers have written it to their disks; this is still safe."

  A follower has no such licence, because its `MsgAppendResponse` is
  the evidence the leader counts toward the majority. Same for a vote:
  send `MsgRequestVoteResponse` before `voted_for` is durable and a
  crash-restart lets the node vote twice in one term.

  </details>

- [ ] You can say when an asynchronous HardState write is legal, and why the exception is safe.

  <details><summary>Answer</summary>

  `must_sync()` (raw_node.rs:223-232) is false iff no HardState field
  other than `commit` changed **and** there are no entries and no
  snapshot in the bundle. It is forced true at :517 when `vote` or
  `term` changed, :543 for a snapshot, :549 when entries are present.

  It is safe because `commit` is recoverable and `vote`/`term` are
  not. After a crash a node re-derives its commit index from the log
  and from the leader's next AppendEntries — losing it costs a little
  re-apply work. Losing `vote` costs Election Safety.

  </details>

- [ ] You can explain how splitting the persistence acknowledgement (`advance_append`) enables pipelining without breaking the contract.

  <details><summary>Answer</summary>

  `advance()` (raw_node.rs:663) is `advance_append` plus
  `advance_apply_to`. Calling `advance_append` (:678-681) on its own
  says "the append is durable" without also claiming "the committed
  entries are applied", so the two can proceed at different rates: you
  can batch many Ready-worth of entries into one fsync and ack them
  together, while apply runs behind.

  What must not be reordered is the pairing itself. You may not
  `advance_append` before the write actually returns, and a follower
  may not release its `persisted_messages()` on the strength of a
  queued write. The gain is exactly topic 15's measured ladder: one
  fsync per entry is 341 entries/s, one per 64 is 12,187.

  </details>

- [ ] You can say what the `next_idx` decrement-and-retry loop costs in round trips, what fixes it, and whether raft-rs bothered.

  <details><summary>Answer</summary>

  Naively one round trip per diverging entry — the raft-rs comment at
  raft.rs:1783-1789 says this "can easily result in hours of time
  spent probing and can even cause outright outages."

  The Raft paper's §5.3 body text (not a footnote) describes the fix:
  the follower returns the term of its conflicting entry and the first
  index it holds for that term, so the leader skips a whole term per
  round trip. The paper then says "we doubt this optimization is
  necessary."

  raft-rs implements it anyway. Follower side raft.rs:2539-2554
  (`hint_index`, `find_conflict_by_term`, `reject_hint` + `log_term`),
  with the walk itself at raft_log.rs:222-248; leader side
  raft.rs:1747-1750 reading the hint and :1799 calling
  `pr.maybe_decr_to`.

  </details>

- [ ] You can convert raft-rs's tick constants into a wall-clock election timeout for a given tick period.

  <details><summary>Answer</summary>

  The constants are unitless. `heartbeat_tick` is 2 and
  `election_tick` is `2 × 10 = 20` (config.rs:112-116);
  `min_election_tick()` returns `election_tick` and
  `max_election_tick()` returns `2 × election_tick`
  (config.rs:147-163); `reset_randomized_election_timeout`
  (raft.rs:2854-2866) draws uniformly from `[20, 40)`.

  Multiply by your tick period. qdrant ticks every 100 ms
  (`config/config.yaml:359`), giving 200 ms heartbeats and a
  2.0–4.0 s election timeout. A 10 ms tick gives 20 ms heartbeats and
  200–400 ms, which lands on the paper's §5.6 recommendation of
  150–300 ms.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the Ready-to-M15 mapping.

  <details><summary>Answer</summary>

  The mapping worth writing down: your WAL's `append` is
  `Ready::entries`; your fsync is the persist step; your ack to the
  follower is `persisted_messages()` and your ack to the leader's
  peers is `messages()`; your apply loop is `committed_entries`; and
  your "the batch is durable" callback is `advance_append`.

  The part with no analogue yet is `HardState`. M15 stage 2 needs a
  durable `{term, vote, commit}` beside the log, written under the
  same rules as raw_node.rs:223-232 — synchronously when `vote` or
  `term` moves, lazily when only `commit` does.

  </details>

## References

**Papers**
- The Raft paper itself is
  [reading-raft-paper.md](reading-raft-paper.md) — Fig 2 is the spec
  this code implements, §5.3 is Step 5's optimisation, §5.4.2 is
  raft_log.rs:526
- Diego Ongaro, *Consensus: Bridging Theory and Practice*, Stanford
  PhD dissertation, 2014 — **a different document from the paper**.
  §10.2.1 "Writing to the leader's disk in parallel" (pp. 141-142,
  Figure 10.2) is what raw_node.rs:554 cites.

**Code**
- [raft-rs](https://github.com/tikv/raft-rs) at `ad13f3d` —
  `src/raw_node.rs` (the Ready contract), `src/raft.rs` (the state
  machine), `src/raft_log.rs` (the §5.4.2 test and
  `find_conflict_by_term`), `src/quorum/majority.rs`,
  `src/tracker/progress.rs`, `src/config.rs`; the anchor map above
- qdrant's embedding of it is
  [reading-qdrant-consensus.md](reading-qdrant-consensus.md)
