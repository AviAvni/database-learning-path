# Raft: logs converge by construction

Paxos won the theory; Raft won the industry (etcd, tikv, CockroachDB,
consul, qdrant's metadata, ...). The pitch is *decomposition*: leader
election, log replication, and safety as separable concerns, plus a
strong-leader design that forbids the log-repair cases Paxos allows.
Before the paper, this chapter builds the algorithm one concept at a
time — the replicated log, terms, elections, the consistency check,
the two safety rules, and the timing inequality — ending on the
Fig 8 trap that every homegrown Raft falls into.

**Read the extended version.** There are two documents and they are
not interchangeable. The ATC '14 conference paper is 16 pages; the
extended version at [raft.github.io/raft.pdf](https://raft.github.io/raft.pdf)
is 18 and adds §7 (log compaction) with Figures 12–13. Figures 1–11
carry the same numbers in both, but the evaluation figures are
renumbered: **extended Fig 14/15/16 = ATC '14 Fig 12/13/14**. Every
figure and section number below is the **extended** version, and
Ongaro's PhD dissertation is a *third* document — do not quote a
section number across them. §5 is the whole game.

## The problem in one sentence

Keep 5 machines agreeing on one sequence of writes such that a write
acknowledged to the client survives **any 2 machines dying at any
moment** — the thing async replication (valkey, next chapter)
explicitly does not promise, since a leader that acks before
replicating can take acked writes to the grave.

## The concepts, step by step

### Step 1 — the replicated log: agree on order, and state follows

> **In:** several servers that must end up holding identical data.
> **Out:** the reduction of "identical state" to "identical log", the
> definition of *committed*, and Raft's one structural restriction.

A **replicated state machine** keeps several servers identical by a
simple trick: if every server starts from the same state and applies
the same commands *in the same order*, they end in the same state.
So the servers don't replicate state — they replicate a **log** (a
numbered, append-only sequence of commands, first index 1 per
Figure 2), and consensus reduces to one question: what is entry #i?

```
        index:   1     2     3     4
 leader   log: [x=1] [y=2] [x=4] [y=9]   ── committed up to 3 ──
 follower log: [x=1] [y=2] [x=4]
 follower log: [x=1] [y=2] [x=4] [y=9]
```

An entry is **committed** when the protocol guarantees it will never
be removed from anyone's log; only committed entries are applied to
the state machine. Figure 2 splits this into two volatile counters:
`commitIndex`, the highest entry known committed, and `lastApplied`,
the highest actually fed to the state machine. Raft's structural
simplification over Paxos: only one node — the **leader** — ever
appends, and entries flow one direction, leader → followers. §5.4.1
states the consequence outright: "log entries only flow in one
direction, from leaders to followers, and leaders never overwrite
existing entries in their logs."

```
 Paxos:  any replica can propose → logs converge by proof gymnastics
 Raft:   ONLY the leader appends → logs converge by construction
         (entries flow one direction: leader → followers)
```

The whole basic algorithm needs exactly **two** RPCs — `RequestVote`
(§5.2) and `AppendEntries` (§5.3), both boxed in Figure 2. A third,
`InstallSnapshot`, arrives only with log compaction in §7.

### Step 2 — terms: a logical clock that fences dead leaders

> **In:** a cluster whose leader can be partitioned away and come
> back. **Out:** the definition of a term, the two comparison rules
> that fence a stale leader, and the price in bytes.

Leaders fail, so leadership must be handed over — and the cluster
needs to distinguish the current leader's messages from a stale
one's. A **term** is a monotonically increasing integer that acts as
a logical clock: time divides into numbered terms, each with at most
one leader (that is Figure 3's *Election Safety*, §5.2). Every
message carries the sender's term; every node tracks the highest it
has seen in `currentTerm`. Two rules do all the fencing: see a
*higher* term → you are stale, become follower and adopt it; see a
*lower* term → the sender is stale, reject (Figure 2's `RequestVote`
receiver rule 1 and `AppendEntries` receiver rule 1 are both "Reply
false if term < currentTerm"). A leader deposed by a partition can't
damage anything after healing: its term is old, so everyone rejects
it. Cost: two integers of state and a comparison per message — the
cheapest fencing token in systems.

### Step 3 — what must be on disk before you answer

> **In:** Figure 2's State box. **Out:** the three fields that must
> be fsynced before an RPC reply, the two that must not bother, and
> the concrete double-vote failure that justifies the split.

Figure 2 names its state box "Persistent state on all servers" and
parenthesises the obligation: *"Updated on stable storage before
responding to RPCs."* The three fields are:

| field | why it must be durable |
|---|---|
| `currentTerm` | forgetting it lets you re-enter an old term |
| `votedFor` | forgetting it lets you vote twice in one term |
| `log[]` | forgetting it un-acks entries you told the leader you had |

And the two that are explicitly volatile — `commitIndex` and
`lastApplied` — need not be, because both are *recomputable*. After
a restart a node relearns its commit index from the next
`AppendEntries` (Figure 2's rule: `commitIndex = min(leaderCommit,
index of last new entry)`), and re-applies from the log. Losing them
costs work, not correctness. This is exactly the distinction raft-rs
encodes in `must_sync()` — see
[reading-raft-rs.md](reading-raft-rs.md) Step 4.

Construct the double-vote failure to see why `votedFor` is in the
first column and not the second:

```
  term 5, five nodes S1..S5. S1 and S2 both campaign.
  S3 grants its vote to S1, replies, then crashes before the
  votedFor write reaches the platter.
  S3 restarts with votedFor = null and grants its vote to S2.

  votes for S1: S1, S3, S4   = 3 of 5  → majority → leader(term 5)
  votes for S2: S2, S3, S5   = 3 of 5  → majority → leader(term 5)

  Two leaders in term 5. Election Safety (Figure 3, §5.2) is gone,
  and with it every argument built on top of it. The double-count is
  possible only because ONE node's vote was counted twice; that is
  what the fsync prevents.
```

On this machine that write is not free. Topic 5 measured the ladder:
a real durable flush on macOS/APFS needs `F_FULLFSYNC`, not
`fsync(2)`, and costs enough to cap commits at **337/s** — which is
why this topic's own `repl_lag` bench sees **341 entries/s** when the
follower fsyncs every entry, and **20,174/s** when it never does. A
59× span, from one line of Figure 2's fine print.

### Step 4 — elections: randomized timeouts, and the numbers behind them

> **In:** a follower that has stopped hearing heartbeats. **Out:**
> the election procedure, the up-to-dateness comparison quoted
> exactly, and the paper's own measurements of what randomization
> buys.

Each node is a follower, candidate, or leader (§5.1; the README's
state diagram). Followers expect periodic heartbeats — Figure 2
describes these as "AppendEntries RPCs that carry no log entries".
A follower that hears nothing for an **election timeout** increments
`currentTerm`, becomes candidate, votes for itself, and sends
`RequestVote` to everyone; a majority makes it leader.

Two details carry the correctness. **One vote per term, persisted** —
Step 3. And **the up-to-dateness test**, which §5.4.1 states in two
sentences worth memorising: *"If the logs have last entries with
different terms, then the log with the later term is more up-to-date.
If the logs end with the same term, then whichever log is longer is
more up-to-date."* Term first, length second — and the ordering of
those two clauses is what Step 6's trap turns on.

**Randomized timeouts** break symmetry. If all nodes timed out
together, votes would split, nobody would reach a majority, and the
cycle would repeat. §5.2 says timeouts are "chosen randomly from a
fixed interval (e.g., 150–300ms)". The paper does not leave that as
folklore; Figure 16 measures it on 5 servers with a broadcast time of
roughly 15 ms, 1000 trials per configuration:

```
  randomness added   result
  ----------------   -------------------------------------------
  none               leader election consistently took > 10 s
                     (many split votes)
  5 ms               median downtime 287 ms
  50 ms              worst case over 1000 trials 513 ms
  timeout 12–24 ms   35 ms average, longest trial 152 ms

  Read the first two rows as the whole argument for randomization:
  10,000 ms → 287 ms is a factor of ~35, bought with 5 ms of
  jitter. Read rows 3 and 4 as the tradeoff: more randomness
  improves the WORST case, a lower timeout improves the AVERAGE.

  The paper still recommends 150–300 ms, ~10× the aggressive
  12–24 ms that measured better, because below that "leaders have
  difficulty broadcasting heartbeats before other servers start
  new elections."
```

### Step 5 — the timing inequality: what "enough" means

> **In:** the three timescales in a real deployment. **Out:** §5.6's
> inequality, the paper's own bounds for each term, and the reason
> the middle one cannot simply be minimised.

§5.6 states the whole availability requirement as one inequality:

```
  broadcastTime  ≪  electionTimeout  ≪  MTBF

  broadcastTime    time to send RPCs to every server in parallel and
                   receive their responses.  §5.6: 0.5–20 ms,
                   "because Raft's RPCs typically require the
                   recipient to persist information to stable
                   storage"  ← the fsync is INSIDE broadcastTime
  electionTimeout  §5.6: likely 10–500 ms
  MTBF             mean time between failures of a single server;
                   §5.6: typically several months

  Check it with this repo's own numbers. Topic 15 measured a WAIT-1
  ack p99 of 3889.5 us with the follower fsyncing every entry —
  call broadcastTime ~3.9 ms, at the top of the paper's range and
  entirely because of the follower's fsync. With the paper's
  recommended 150 ms minimum election timeout:

      150 ms / 3.9 ms  ≈  38x headroom
      several months / 150 ms  ≈  10^7 x headroom

  Now drop the follower's fsync to one per 64 entries: ack p99 falls
  to 2133.0 us, and the headroom rises to ~70x. The left-hand ≪ is
  bought with exactly the durability the right-hand side assumed.
```

The inequality is why the election timeout cannot simply be driven to
zero: shrink it toward broadcastTime and leaders start losing
elections they should have won.

### Step 6 — log replication: the consistency check

> **In:** a leader with a new client command and a follower whose log
> may diverge. **Out:** the two fields that guard every append, the
> induction they support, and the repair loop's cost.

The leader appends a client command to its own log, then sends
`AppendEntries` to followers. The heart of Raft is one guard on that
message:

```
 AppendEntries carries (prevLogIndex, prevLogTerm)
 follower: my log has an entry at prevLogIndex with prevLogTerm?
   yes → append (truncating any conflicting suffix)
   no  → reject; leader decrements nextIndex and retries
```

By induction this gives Figure 3's **Log Matching Property**: "if two
logs contain an entry with the same index and term, then the logs are
identical in all entries up through the given index" (§5.3) — the
follower only accepted each entry after proving the previous one
matched. A follower with a divergent suffix (appended by some dead
leader, never committed) gets it *truncated* and overwritten. The
follower side, as our stub writes it:

```rust
// ILLUSTRATION — not quoted from the paper; this is the shape of
// Figure 2's AppendEntries receiver rules 1-5 as our experiments/
// stub implements them. The production version is raft-rs
// src/raft.rs:2499 (handle_append_entries).
fn handle_append(&mut self, m: AppendEntries) -> bool {
    if m.term < self.term { return false; }           // Fig 2 rule 1
    match self.log.get(m.prev_index) {
        None => false,                                // Fig 2 rule 2: hole
        Some(e) if e.term != m.prev_term => false,    // Fig 2 rule 2: mismatch
        _ => {
            for (i, new) in m.entries.iter().enumerate() {
                let idx = m.prev_index + 1 + i as u64;
                if self.log.term_at(idx) != Some(new.term) {
                    self.log.truncate(idx);           // Fig 2 rule 3
                    self.log.push(new.clone());       // Fig 2 rule 4
                }
            }
            // Fig 2 rule 5
            self.commit_index = m.leader_commit.min(self.log.last_index());
            true
        }
    }
}
```

Cost accounting: one round trip per batch of entries in the common
case; **O(divergence) round trips** to repair a lagging follower,
because `nextIndex` walks back one entry at a time. §5.3 offers a fix
in its body text — the rejecting follower returns the term of its
conflicting entry and the first index it stores for that term, so the
leader skips a whole term per round trip — and then hedges: "In
practice, we doubt this optimization is necessary." It is not a
footnote and the doubt did not hold; raft-rs implements it
(raft.rs:2539-2554 and raft_log.rs:222-248), and its comment at
raft.rs:1783-1789 says naive probing "can easily result in hours of
time spent probing and can even cause outright outages."

Question: why must a follower *truncate* conflicting entries rather
than skip them? Construct the divergent-log picture from Figure 7.

### Step 7 — safety rule 1: the election restriction

> **In:** a committed entry and a leader that just died. **Out:** the
> voting rule that protects it, and the two-majority intersection
> argument in full.

Committed entries must survive leader changes, so Raft never lets a
node that *lacks* a committed entry become leader. A voter refuses
any candidate whose log is less up-to-date than its own, by Step 4's
term-then-length test. §5.4.1 gives the argument in one move: "A
candidate must contact a majority of the cluster in order to be
elected, which means that every committed entry must be present in at
least one of those servers."

Work the arithmetic on five nodes:

```
  |committed set|  ≥  3   (a majority of 5, by definition of commit)
  |voter set|      ≥  3   (a majority of 5, to win the election)
  3 + 3 = 6 > 5    → the two sets share at least 6 − 5 = 1 node

  That node holds the committed entry. If the candidate's log were
  missing it, the shared node's log would end at a later term (or the
  same term but longer), so it refuses the vote — and without that
  vote the candidate cannot reach 3.
```

Hence Figure 3's **Leader Completeness** (§5.4): a committed entry is
present in the logs of the leaders of all higher terms. Raft never
needs to copy entries *into* a new leader — contrast VSR
([reading-vsr.md](reading-vsr.md)), which chose the opposite and
transfers a log during the view change. §5.4.1 names the tradeoff:
the alternatives "contain additional mechanisms to identify the
missing entries and transmit them to the new leader... this results
in considerable additional mechanism and complexity."

### Step 8 — safety rule 2: only current-term entries count for commit

> **In:** a leader that sees an old entry replicated on a majority.
> **Out:** Figure 8's five panels with the paper's own server names,
> and the exact place the Step 7 argument stops working.

The subtle one (§5.4.2): "replicated on a majority" is NOT sufficient
to commit an entry from an *older* term. The paper's own summary
sentence: **"Raft never commits log entries from previous terms by
counting replicas."** A leader may only advance `commitIndex` by
majority-replicating an entry *from its own term*; older entries then
commit indirectly, riding below it.

Figure 8 is the counterexample that forces the rule. The paper's
caption, panel by panel — note that only **term 3** is named for S5;
the term S1 holds in (c) is not stated in the caption, so do not
quote one:

```
  (a) S1 is leader and partially replicates the log entry at index 2.
  (b) S1 crashes. S5 is elected leader for TERM 3 with votes from
      S3, S4, and itself, and accepts a different entry at index 2.
  (c) S5 crashes. S1 restarts, is elected leader, and continues
      replication. The term-2 entry at index 2 is now replicated on
      a MAJORITY of the servers — "but it is not committed."
  (d) If S1 crashes here, S5 can be elected leader (votes from S2,
      S3, and S4) and OVERWRITE index 2 with its own term-3 entry.
  (e) But if S1 first replicates an entry from its CURRENT term on a
      majority, that entry is committed, S5 cannot win an election,
      and "all preceding entries in the log are committed as well."
```

Where Step 7's argument breaks: the intersection argument is still
true — S5's voting majority in (d) does share a node with the set
holding the term-2 entry. What fails is the *inference from sharing
to refusal*. Step 4's up-to-dateness test compares last terms first,
and S5's last entry is from term 3 while the shared node's is from
term 2. A node holding the old entry therefore votes for S5 quite
happily. Replicating one current-term entry closes the hole because
it raises the shared node's last term to the leader's own, so no
surviving candidate can outrank it.

Our `raft.rs` test `stale_leader_uncommitted_overwritten` is exactly
panel (d). Every homegrown Raft that skips §5.4.2 loses acked writes
here. In raft-rs the rule is one boolean at `src/raft_log.rs:526`.

## How to read the paper (with the concepts in hand)

Section and figure numbers are the **extended** version.

| section | what to extract | step |
|---|---|---|
| §5.1 | the three states + the RPC menu (only 2!) | 1, 4 |
| §5.2 | elections: terms, randomized timeouts, 150–300 ms | 2, 4 |
| §5.3 | log replication: the consistency check, repair, the term-skip optimisation | 6 |
| §5.4 | safety — read TWICE, especially §5.4.2 | 7–8 |
| §5.4.1 | the up-to-dateness definition, quoted in Step 4 | 4, 7 |
| §5.6 | the broadcastTime ≪ electionTimeout ≪ MTBF inequality | 5 |
| §6 | membership changes (joint consensus) — skim | — |
| §7 | log compaction / snapshots + `InstallSnapshot` — skim, topic 5 déjà vu | — |
| Fig 2 | the whole algorithm on one page — print it | all |
| Fig 3 | the five safety properties with their section numbers | 7–8 |
| Fig 7 | the divergence zoo (six follower logs, a–f) | 6 |
| Fig 8 | the five-panel commit trap | 8 |
| Fig 16 | the election-timeout measurements (= ATC '14 Fig 14) | 4 |

Figure 3's five properties, in the paper's own order and with its own
section attributions, are the checklist to hold every implementation
against: **Election Safety** (§5.2), **Leader Append-Only** (§5.3),
**Log Matching** (§5.3), **Leader Completeness** (§5.4), **State
Machine Safety** (§5.4.3). Note the last one is §5.4.**3**, not §5.4 —
it is the property, distinct from the leader-completeness lemma that
implies it.

Fig 2 is the spec that raft-rs implements
([reading-raft-rs.md](reading-raft-rs.md)) — keep it printed next to
you for both chapters.

## Questions to answer in notes.md

1. Why persist `(currentTerm, votedFor, log)` but NOT `commitIndex`?
   What recomputes commitIndex after restart?
2. Fig 8 step-by-step: which specific quorum-intersection argument
   fails without the current-term rule?
3. Why does a leader never overwrite/delete its OWN log entries, and
   what breaks if it could?
4. §7: a snapshot at index i replaces the log prefix — what must the
   snapshot record besides the state? (last_included_index/term —
   why the term?)
5. Map to valkey: which Raft properties does async replication give
   up, and what do you get back for each?

## Done when

Answer each before unfolding it.

- [ ] You can explain why agreeing on log order is sufficient for state-machine convergence.

  <details><summary>Answer</summary>

  Because a state machine is deterministic: same start state plus
  same commands in the same order gives the same end state. So the
  replicas never have to compare or reconcile state — they only have
  to agree on the contents of entry #i, for every i.

  That is the reduction the whole paper rests on, and it is why
  Figure 2's state box contains a `log[]` and not a snapshot of the
  data. `lastApplied` is the pointer that turns the agreed log back
  into agreed state.
  </details>

- [ ] You can say what a term is and what it fences.

  <details><summary>Answer</summary>

  A monotonically increasing integer acting as a logical clock: time
  divides into numbered terms, each with at most one leader (Figure
  3, *Election Safety*, §5.2). Every message carries the sender's
  term.

  It fences a deposed leader. A leader partitioned away and returning
  carries an old term, so every receiver applies Figure 2's rule 1 —
  "Reply false if term < currentTerm" — and its writes go nowhere. It
  also fences the node itself: seeing a higher term forces it back to
  follower and adopts the new term. Two integers of state buys the
  entire stale-leader problem.
  </details>

- [ ] You can state exactly which state must be persisted before responding, and why the rest need not be.

  <details><summary>Answer</summary>

  Figure 2, "Persistent state on all servers (Updated on stable
  storage before responding to RPCs)": `currentTerm`, `votedFor`,
  `log[]`.

  `commitIndex` and `lastApplied` are listed as volatile because both
  are recomputable. After a restart the next `AppendEntries` carries
  `leaderCommit` and Figure 2's rule 5 rebuilds `commitIndex =
  min(leaderCommit, index of last new entry)`; `lastApplied` catches
  up by replaying the log. Losing them costs work, not correctness.

  `votedFor` is the one whose loss is unrecoverable: a node that
  forgets its vote can grant a second one in the same term, and two
  candidates can each reach a majority that counts that node — two
  leaders in one term.
  </details>

- [ ] You can state the up-to-dateness comparison in the paper's own order, and both safety rules.

  <details><summary>Answer</summary>

  §5.4.1: if the last entries have different terms, the later term
  wins; if the terms are equal, the longer log wins. Term first,
  length second.

  Rule 1, the **election restriction** (§5.4.1): a voter refuses a
  candidate whose log is less up-to-date than its own. Two majorities
  of five intersect in at least 6 − 5 = 1 node; that node holds every
  committed entry, so it blocks any candidate missing one. Result:
  Figure 3's Leader Completeness.

  Rule 2, **§5.4.2**: "Raft never commits log entries from previous
  terms by counting replicas." A leader advances `commitIndex` only by
  majority-replicating an entry from its own term; older entries
  commit indirectly beneath it.
  </details>

- [ ] You can walk Figure 8's five panels and name the exact inference that fails without the current-term rule.

  <details><summary>Answer</summary>

  (a) S1 partially replicates index 2 (term 2). (b) S1 crashes; S5 is
  elected for **term 3** on votes from S3, S4 and itself, and accepts
  a different entry at index 2. (c) S5 crashes; S1 restarts, is
  re-elected, and continues replication — index 2 now sits on a
  majority "but it is not committed". (d) S1 crashes; S5 is elected
  on votes from S2, S3, S4 and overwrites index 2. (e) Had S1 first
  replicated a current-term entry on a majority, that entry is
  committed, S5 cannot win, and everything below it commits too.

  The intersection argument itself survives — S5's majority does
  share a node with the majority holding the term-2 entry. What fails
  is the step from *sharing* to *refusal*: §5.4.1 compares last terms
  first, and S5's term-3 last entry outranks the shared node's term-2
  one, so that node votes for S5. Replicating a current-term entry
  raises the shared node's last term to the leader's, restoring the
  inference.
  </details>

- [ ] You can explain why a leader never overwrites its own entries, and what that means for the follower repair loop.

  <details><summary>Answer</summary>

  Figure 3, *Leader Append-Only* (§5.3): "a leader never overwrites or
  deletes entries in its log; it only appends new entries." It is
  safe to state as an invariant because of the election restriction —
  a new leader already holds every committed entry, so there is
  nothing it would need to delete.

  The consequence for repair is that all the truncation happens on
  the follower. The leader walks `nextIndex` backwards until the
  `(prevLogIndex, prevLogTerm)` check passes, and the follower
  truncates its divergent suffix (Figure 2, AppendEntries rule 3).
  Cost: O(divergence) round trips, which §5.3's term-skip
  optimisation reduces to O(diverging terms) and raft-rs implements
  at raft.rs:2539-2554.
  </details>

- [ ] You can state §5.6's timing inequality with the paper's bounds, and say what sits inside broadcastTime.

  <details><summary>Answer</summary>

  `broadcastTime ≪ electionTimeout ≪ MTBF`. §5.6 gives broadcastTime
  as 0.5–20 ms, election timeout as likely 10–500 ms, and single-server
  MTBF as typically several months.

  The important sentence is why broadcastTime is that large: "Raft's
  RPCs typically require the recipient to persist information to
  stable storage." The follower's fsync is inside the term. This
  topic's own bench shows it: WAIT-1 ack p99 is 3889.5 µs when the
  follower fsyncs every entry and 2133.0 µs at one fsync per 64,
  which moves the headroom against a 150 ms timeout from ~38× to ~70×.

  Figure 16 measures the middle term directly: no randomness gives
  >10 s elections, 5 ms of randomness gives a 287 ms median, 50 ms of
  randomness caps the worst case over 1000 trials at 513 ms — and the
  paper still recommends 150–300 ms rather than the 12–24 ms that
  measured best, to keep the left-hand ≪ comfortable.
  </details>

- [ ] You wrote answers to all five questions in notes.md, and can predict what `partition_test` must show: 99 never commits, and is truncated everywhere after the heal.

  <details><summary>Answer</summary>

  The test is Figure 8 panel (d) in miniature. The minority-side
  leader appends entry 99 and can never reach a majority, so
  `commitIndex` never covers it and it is never applied. After the
  heal the surviving leader's `AppendEntries` fails its
  `(prevLogIndex, prevLogTerm)` check on that node, `nextIndex` walks
  back, and rule 3 truncates the suffix.

  The assertion worth writing is the negative one: no replica ever
  *applied* 99, so no client could have observed it. A test that only
  checks the logs converge would pass even for an implementation that
  applied and then un-applied it.
  </details>

## References

**Papers**
- Diego Ongaro, John Ousterhout — "In Search of an Understandable
  Consensus Algorithm", USENIX ATC 2014. Read the **extended
  version** ([raft.github.io/raft.pdf](https://raft.github.io/raft.pdf),
  18 pp.), not the 16-page conference paper: it adds §7 and Figures
  12–13, and renumbers the evaluation figures (extended 14/15/16 =
  ATC '14 12/13/14). §5 twice, Fig 2 printed, Fig 8 worked by hand.
- Diego Ongaro — "Consensus: Bridging Theory and Practice", Stanford
  PhD dissertation, 2014. **A third, different document** — its
  section numbers do not correspond to the paper's. §10.2.1 is what
  raft-rs cites for the leader's parallel disk write; §10.2 gives the
  cost model (disk 100 µs–10 ms, network RTT 5 µs–400 ms).

**Code**
- The production implementation is
  [raft-rs](https://github.com/tikv/raft-rs) — walked in
  [reading-raft-rs.md](reading-raft-rs.md). Figure 2's persistent
  state is its `HardState`; §5.4.2 is `src/raft_log.rs:526`; §5.3's
  term-skip optimisation is `src/raft_log.rs:222-248`.
