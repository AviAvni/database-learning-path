# Viewstamped Replication: same invariants, opposite choices

The other consensus protocol — actually the FIRST (VR 1988 predates
Paxos's 1998 publication). Read it AFTER Raft: same invariants,
opposite engineering choices at almost every fork. This chapter
builds those forks one at a time — the vocabulary mapping,
deterministic round-robin leadership instead of elections, logs
shipped at view change instead of repaired after, and (the shocker)
durability without disk. TigerBeetle ships VSR in production, so this
is not a museum piece.

**Two documents, and they are not the same protocol.** Every claim
below names which one it comes from:

- **VR Revisited** — Liskov & Cowling, *Viewstamped Replication
  Revisited*, MIT-CSAIL-TR-2012-021, 16 pp. State-machine
  replication, three sub-protocols, no disk in normal operation *or*
  view change. This is the one to read.
- **VR 1988** — Oki & Liskov, *Viewstamped Replication: A New Primary
  Copy Method to Support Highly-Available Distributed Systems*, MIT
  LCS / PODC 1988, 10 pp. A *transactional* system: its actors are
  "cohorts", a view is a **set** of cohorts plus a designated primary
  (not just a number), and operations carry **viewstamps**. It does
  write to stable storage during a view change.

VR Revisited says so itself, in §4.3: "The original VR specification
used a protocol that wrote to disk during the view change but did not
require writing to disk during normal case processing." Quoting the
2012 no-disk result as a 1988 result is the standard error.

## The problem in one sentence

The same problem as Raft — an acked write must survive any f of
2f+1 nodes dying — but VSR asks how much of Raft's machinery is
*forced* and how much is *chosen*: no randomized timeouts, no votes,
and in VR Revisited's protocol **zero disk I/O in normal operation
and view change**, versus Raft's fsync of `votedFor` and log on every
vote and append.

## The concepts, step by step

### Step 1 — same machine, different words

> **In:** Raft's vocabulary from the previous chapters. **Out:** the
> decoder that makes VR Revisited readable in one sitting, plus the
> two 1988 terms that decode to nothing in Raft.

VSR replicates a log through a distinguished node exactly like Raft;
only the names differ. Keep this decoder open for the whole paper:

| Raft | VR Revisited (2012) |
|---|---|
| term | view-number |
| leader | primary |
| election | view change |
| log index | op-number |
| commitIndex | commit-number |
| — | status ∈ {normal, view-change, recovering} |
| RequestVote / AppendEntries | STARTVIEWCHANGE / DOVIEWCHANGE / PREPARE / PREPAREOK |

A **view** in VR Revisited is a numbered epoch with one primary —
Raft's term, twenty-four years earlier. A **status** is the extra
concept with no Raft counterpart: §4.1 opens by saying replicas
"participate in processing of client requests only when their status
is normal", and calls that constraint "critical for correctness".
Raft has no such flag; a Raft node is always willing to append.

Figure 2 of VR Revisited lists the whole per-replica state: the
configuration, the replica number, view-number, status, op-number,
the log, the commit-number, and the client-table. Two of those
deserve a second look. The **configuration** is "a sorted array
containing the IP addresses of each of the 2f + 1 replicas" — sorted,
because Step 3's rotation is an index into it. The **client-table**
records each client's most recent request number and its result, and
exists because §4 allows a client "just one outstanding request at a
time"; it is how VSR deduplicates retries, a problem Raft's paper
leaves to §8.

Two 1988 words that do not decode: a **cohort** is a replica, and a
**viewstamp** is the pair ⟨viewid, timestamp⟩ that gave the protocol
its name. VR Revisited §4.2 explains that it dropped them: "VR as
originally defined used a slightly different approach: it assigned
each operation a viewstamp... At any op-number, VR retained the
request with the higher viewstamp. VR got its name from these
viewstamps." The 2012 protocol takes the whole log from the latest
previous active view instead.

Concretely: your own `struct Node` in
`experiments/src/raft.rs:44-60` already holds most of Figure 2.
`term` (:48) is view-number, `log` (:50) is the log, `commit_index`
(:51) is commit-number, `peers` (:46) is the configuration. Three
fields have no VSR counterpart — `voted_for` (:49), `votes_received`
(:59), and the randomized `election_timeout` (:57) — and three VSR
fields are missing: op-number, status, and the client-table. That
diff is the whole chapter in one screen.

### Step 2 — normal operation: the same wire shape as AppendEntries

> **In:** a client request arriving at the primary of view v.
> **Out:** the five message types in order, the exact quorum count
> (which is *f*, not f+1), and the two ways a view-number mismatch is
> handled.

VR Revisited §4.1, step by step. The client sends
⟨REQUEST op, c, s⟩ to the primary. The primary checks the
client-table (a stale request-number is dropped; the most recent one
is answered from the cached result), advances op-number, appends, and
broadcasts ⟨PREPARE v, m, n, k⟩ — where `n` is the new op-number and
`k` is the current commit-number, so commits piggyback on the next
prepare. Backups process PREPAREs **in order**, doing state transfer
if they are missing earlier entries, then reply ⟨PREPAREOK v, n, i⟩.

The quorum count is the detail people misquote:

```
  §4.1 step 5: "The primary waits for f PREPAREOK messages from
  different backups; at this point it considers the operation (and
  all earlier ones) to be committed."

  f PREPAREOKs, not f+1 — the primary's own copy is the +1.

    f = 2, n = 2f + 1 = 5
    2 PREPAREOKs + the primary itself = 3 copies = majority of 5  ✓

  Compare Raft: matched[majority(5) - 1] = matched[2], where the
  leader counts ITSELF in the matched vector. Same 3, arrived at by
  counting a different thing. Get this wrong by one in either
  direction and you have either a stall or a split brain.
```

Then ⟨REPLY v, s, x⟩ to the client. Backups learn of the commit from
the `k` in the next PREPARE; if no client request arrives "in a timely
way" the primary sends ⟨COMMIT v, k⟩ instead — VSR's heartbeat, and
note it exists to carry the commit-number, not to prove liveness.

The mismatch handling is the other thing to take from §4.1: "Replicas
only process normal protocol messages containing a view-number that
matches the view-number they know. If the sender is behind, the
receiver drops the message. If the sender is ahead, the replica
performs a state transfer." Raft's rule is symmetric — a higher term
always makes you a follower — where VSR distinguishes *stale sender*
(drop) from *stale self* (go fetch, §5.2).

Same quorum arithmetic as Raft, same one-round-trip latency. The
differences are all in what happens when this smooth path breaks.

### Step 3 — view change: the next primary is scheduled, not elected

> **In:** replicas that have stopped hearing from the primary of view
> v. **Out:** the formula that names the next primary, the two
> distinct quorum sizes in the protocol, and the log-selection rule
> with its tie-break.

Raft elects: candidates race, randomized timeouts break ties, votes
are persisted. VSR schedules. VR Revisited §4 states it plainly: "The
identity of the primary isn't recorded in the state but rather is
computed from the view-number and the configuration... The primary is
chosen round-robin, starting with replica 1, as the system moves to
new views." Replicas are numbered by sorted IP address, smallest
first.

§4.2's three-message protocol, with **two different quorum sizes**:

1. A replica noticing the need advances its view-number, sets status
   to `view-change`, and broadcasts ⟨STARTVIEWCHANGE v, i⟩.
2. On receiving STARTVIEWCHANGE for its view-number **from f other
   replicas**, it sends ⟨DOVIEWCHANGE v, l, v', n, k, i⟩ to the node
   that will be primary — where `l` is its whole log and `v'` is "the
   view number of the latest view in which its status was normal".
3. The new primary waits for **f + 1 DOVIEWCHANGE messages from
   different replicas (including itself)**, then selects the log from
   the message with the largest `v'`, breaking ties on the largest
   `n`. It takes the largest commit-number it saw, sets status
   `normal`, and broadcasts ⟨STARTVIEW v, l, n, k⟩.

```rust
// ILLUSTRATION — not quoted from anything. This is VR Revisited §4.2
// step 3 written in the idiom of your own Raft node; the state it
// mutates is the VSR analogue of experiments/src/raft.rs:44-60
// (`struct Node`), and the method it would replace is the vote
// tally reached from experiments/src/raft.rs:95 (`receive`).
// Check it against the paper's own wording before trusting it.
fn install_view(&mut self, view: u64, msgs: &[DoViewChange]) {
    assert!(msgs.len() >= self.f + 1);            // §4.2 step 3
    let best = msgs.iter()
        .max_by_key(|m| (m.last_normal_view, m.op_number))
        .unwrap();                                // largest v', then largest n
    self.log = best.log.clone();
    self.op_number = best.op_number;
    self.commit_number =                          // largest k received,
        msgs.iter().map(|m| m.commit_number).max().unwrap();   // not best's
    self.broadcast(StartView { view, log: &self.log });
}
```

The subtlety the assert hides: `f` and `f + 1` are both quorum sizes
in this protocol and they are not interchangeable. Step 2's threshold
is f *other* replicas (f+1 including self, a majority); step 3's is
f+1 *including* self. Getting step 3 down to f would let a new
primary install a log without a majority behind it, and the
intersection argument — f+1 logs must include at least one node
holding any committed entry — collapses.

On receiving STARTVIEW, replicas replace their log wholesale, and if
it contains uncommitted operations they send PREPAREOK for them
(§4.2 step 5) — which is how the new primary learns what to commit.

What is missing: no votes, no randomized timeouts, no split-vote
livelock — determinism removed them. What it costs is bandwidth, and
the bandwidth is computable:

```
  Take this topic's own bench shape: 2000 entries x 128 B = 256 KB
  of log, f = 2, n = 5.

  VSR view change:
    DOVIEWCHANGE inbound   (f + 1) x 256 KB  =   768 KB
    STARTVIEW  outbound    (n - 1) x 256 KB  =  1024 KB
    total                                    ~ 1.75 MB per view change

  Raft election:
    RequestVote carries only (term, candidateId, lastLogIndex,
    lastLogTerm) — 4 integers. Broadcast to 4 peers:
                                             ~ 128 BYTES

  ~14,000x more bytes per leadership change. Raft pays it back later,
  one AppendEntries at a time, only to the followers that actually
  diverged; VSR pays it up front, always, to everyone.
```

That is the real trade — not "VSR is wasteful" but *when* the repair
cost is paid. If view changes are rare and logs are long, Raft wins;
if divergence is common and logs are short, shipping them once beats
probing. And a down node still takes its turn in the rotation,
forcing another view change (question 1).

### Step 4 — recovery: durability from replication, not disk

> **In:** a replica that has just rebooted with an empty memory.
> **Out:** the three-message recovery protocol, the two quorum
> conditions on its responses, and the exact sentence in which the
> paper qualifies the no-disk claim.

The shocker. Raft fsyncs `votedFor` and log entries before answering
— a crashed node reads its promises back from disk. VR Revisited's
protocol writes nothing to disk in normal operation or view change: a
committed entry lives in f+1 memories, and the protocol tolerates f
failures, so *some* survivor always remembers it. A crashed replica
does not trust its own memory at all — it sets status `recovering`
and runs §4.3:

1. Send ⟨RECOVERY i, x⟩ to all other replicas, where `x` is a
   **nonce**.
2. A replica replies **only if its status is `normal`**, with
   ⟨RECOVERYRESPONSE v, x, l, n, k, j⟩ — and `l`, `n`, `k` are
   **nil unless j is the primary of its view**. Only the primary
   ships a log.
3. The recovering replica waits for **at least f + 1**
   RECOVERYRESPONSEs from different replicas, all carrying its own
   nonce, **including one from the primary of the latest view it
   learns of in these messages**. Then it updates from the primary's
   message and sets status `normal`.

Two conditions on step 3, and both are load-bearing. f+1 responses
give the intersection argument. The "including the primary of the
latest view" clause is what makes the log it copies authoritative —
without it a recovering node could rebuild from f+1 backups that are
all behind. And while recovering it "does not participate in either
the request processing protocol or the view change protocol", which
has a consequence §4.3 spells out: if the recovering replica would be
the primary of a view change in progress, that view change cannot
complete, and the group must do a further one.

The nonce is not decoration. §4.3: "The protocol uses the nonce to
ensure that the recovering replica accepts only RECOVERYRESPONSE
messages that are for this recovery and not an earlier one. It can
produce the nonce by reading its clock; this will produce a unique
nonce assuming clocks always advance. Alternatively, it could
maintain a counter on disk and advance this counter on each
recovery." Without it, responses from a *previous* crash-and-recover
cycle — stale logs still in flight — would be accepted as current.

The catch, stated by the paper itself and not softened. §4.3 gives
the alternative it rejected: fsync before PREPARE at the primary and
before PREPAREOK at the backups, which "adds a delay to normal case
processing". Then the justification, with its condition attached:

> the disk write is "unnecessary because the state is also stored at
> the other replicas and can be retrieved from them, using a recovery
> protocol. Retrieving state will be successful **provided replicas
> are failure independent**, i.e., highly unlikely to fail at the same
> time. If all replicas were to fail simultaneously, state will be
> lost if the information on disk isn't up to date."

Named mitigations, all outside the protocol: UPSs, non-volatile
memory, and placing replicas in different geographic locations. So
the honest summary is that VSR moved a durability requirement from
the storage layer to the deployment, and the deployment has to hold
up its end.

Price that against what fsync costs here. Topic 5 measured a real
`F_FULLFSYNC` on macOS/APFS at 337 commits/s, and this topic's
`repl_lag` bench gets 341 entries/s with the follower fsyncing every
entry versus 20,174 with none. That 59× is exactly the number VSR is
declining to pay — and the UPS is what it pays instead.

The 1988 paper reached the same place by a different road and
described the failure mode more precisely than most retellings do.
Its §4.2 ("Stable Storage") assumes most cohort state is volatile,
defines a *catastrophe* as a majority crashing simultaneously, and
then says something surprising: "a catastrophe does not cause a group
to enter a new view missing some needed information. Rather, it
causes the algorithm to never again form a new view." It stalls; it
does not silently lose. Its conclusion is candid about the whole
experiment: "we chose to avoid the use of stable storage as much as
possible because we were interested in understanding the extent to
which having several replicas eliminated the need for stable storage.
We found that catastrophes... could sometimes occur in our system."

### Step 5 — the forks in the road, side by side

> **In:** both protocols, understood. **Out:** the four decisions
> that differ, with the invariant that is identical underneath each
> — and therefore the evidence that each was a choice.

The reason to read this paper is the table — every row is a place
where two correct protocols chose differently, which proves the
choice was engineering, not necessity:

```
 choice              Raft                    VR Revisited (2012)
 ─────────────────────────────────────────────────────────────────
 who leads next      any up-to-date node     ROUND-ROBIN: computed
                     that wins votes         from view-number and
                                             the sorted configuration
 log transfer        new leader repairs      new primary RECEIVES f+1
                     followers forward,      logs in DOVIEWCHANGE and
                     one AppendEntries       picks max (v', n)
                     at a time
 durability          fsync currentTerm,      NO DISK in normal
                     votedFor, log before    operation or view change;
                     responding (Fig 2)      recovery protocol replaces
                                             it, "provided replicas are
                                             failure independent"
 stale participation always willing to       status must be `normal`;
                     append                  a recovering replica
                                             answers nothing
```

The invariants underneath are identical: one primary per
view/term, quorum intersection carries committed entries across
changes, and a committed entry is never lost within the fault
model. What differs is *where each protocol spends*: Raft spends
fsyncs and election randomness; VSR spends view-change bandwidth
and a stricter independence assumption.

TigerBeetle is the third answer. It ships VSR in Zig
(`src/vsr/replica.zig`, `docs/internals/vsr.md`) and puts disk back —
but keeps VSR's recovery *thinking* and extends it to a fault Raft's
model excludes entirely: storage that lies. Its docs cite the CTRL
protocol from Alagappan et al., *Protocol-Aware Recovery for
Consensus-Based Storage* (FAST '18), and state the rule that follows
from it — a replica does **not** nack a corrupt log entry, "since it
_might_ be the prepare being requested". Raft's Figure 2 has no
vocabulary for "I have an entry but cannot read it"; VSR's
recover-from-peers instinct does. TigerBeetle is **not** in this
repo's pin table, so no line anchors are given for it and `main`
moves — treat those two paths as pointers, not citations.

## How to read the paper (with the concepts in hand)

Read *Viewstamped Replication Revisited* (2012). Section numbers
below are that document's.

- **§1–3 (intro, background, the model)** — skim; Step 1's decoder
  makes it fast. §3 is where the 2f+1 / f fault model is fixed.
- **§4 (the protocol)** — the payload, and Figure 2 (replica state)
  is the page to keep open. §4.1 normal operation is Step 2 — map
  every message onto the AppendEntries flow you know, and note the
  quorum is f PREPAREOKs. §4.2 view change is Step 3 — check the
  `install_view` illustration against the real message rules, and
  read the viewstamp paragraph at the end for the 1988 contrast.
  §4.3 recovery is Step 4 — read for the nonce, for the "including
  one from the primary" clause, and for what a recovering replica may
  NOT do. §4.4 covers non-deterministic operations, which is valkey's
  SPOP problem in another vocabulary.
- **§5 (pragmatics)** — §5.1 is efficient recovery (§4.3's protocol
  is expensive precisely because logs are big), §5.2 is state
  transfer, and this is where the recovery cost gets bounded.
- **§6 (optimizations)** and **§7 (reconfiguration)** — skim;
  reconfiguration is Raft §6's joint consensus by another road.
- **§8 (correctness)** — read the paragraph on why `status` must
  gate participation; it is the argument Step 1 flagged.

The 1988 paper is worth 20 minutes only for §4.2 ("Stable Storage")
and the conclusions, where the no-disk experiment is stated in the
authors' own words — and for seeing how different a *transactional*
formulation looks.

Throughout, keep asking Step 5's question: is this rule forced by
the invariants, or is it a choice? That habit is the transferable
skill — it's how you'll evaluate M15 stage 2's design decisions.

## Questions for notes.md

1. Round-robin primary (computed from view-number and the sorted
   configuration): what does this remove from the protocol (no
   vote-splitting, no randomized timeouts) and what does it cost (a
   down node's turn)?
2. DOVIEWCHANGE ships whole logs to the new primary — Raft ships
   nothing at election, repairing later. Bandwidth vs latency: when
   is each better?
3. The no-disk argument: write the failure sequence where VSR-
   without-disk loses committed data but Raft-with-fsync doesn't.
4. Why does the recovery protocol need a nonce?
5. TigerBeetle: which VSR feature makes "disk can lie" (checksum
   fails, torn write) survivable, where Raft's model assumes storage
   is faithful? Connect to topic 5's torn-page discussion.

## Done when

Answer each before unfolding it.

- [ ] You can state which VSR concepts are Raft's under other names, which are genuinely different, and which belong only to the 1988 paper.

  <details><summary>Answer</summary>

  Renames: view-number = term, primary = leader, view change =
  election, op-number = log index, commit-number = commitIndex.

  Genuinely different in VR Revisited: **status** ∈ {normal,
  view-change, recovering}, which gates participation (§4.1 calls
  that "critical for correctness") and has no Raft counterpart; the
  **client-table**, which deduplicates client retries inside the
  protocol; and the **configuration** as a sorted IP array, because
  the primary is an index into it rather than an election winner.

  Only in 1988: **cohort** for replica, a **view** as a *set* of
  cohorts plus a designated primary rather than a number, and the
  **viewstamp** ⟨viewid, timestamp⟩ that named the protocol. VR
  Revisited §4.2 says it replaced viewstamps with "take the log from
  the latest previous active view".

  </details>

- [ ] You can state VSR's normal-operation quorum exactly, and say why it looks smaller than Raft's.

  <details><summary>Answer</summary>

  §4.1 step 5: "The primary waits for **f** PREPAREOK messages from
  different backups". With f = 2 and n = 5 that is 2 messages — plus
  the primary's own copy, which is 3, a majority of 5.

  It looks smaller because it counts a different set. Raft's
  `maybe_commit` puts the leader's own `matched` into the vector and
  takes `matched[majority(5) - 1] = matched[2]`, i.e. it counts the
  leader. VSR counts only the backups and adds the primary
  implicitly. Same 3 copies either way.

  The other §4.1 rule worth memorising is the view-number mismatch
  handling: sender behind → drop the message; sender ahead → do a
  state transfer (§5.2) before processing. Raft's rule is symmetric —
  a higher term always demotes you.

  </details>

- [ ] You can explain what round-robin primary selection removes from the protocol, what it costs, and where the two different quorum sizes appear.

  <details><summary>Answer</summary>

  It removes candidacy entirely: no votes, no randomized timeouts, no
  split-vote livelock, no persisted `votedFor`. The primary of a view
  is computed from the view-number and the sorted configuration
  (§4), so every replica already knows who it is.

  The cost is that the rotation is blind. If the scheduled next
  primary is down, the group must burn another view change to skip
  it — and §4.3 adds a nastier case: a *recovering* replica does not
  answer DOVIEWCHANGE, so if it is the scheduled primary the view
  change stalls until another one is triggered.

  Two quorum sizes, §4.2: a replica sends DOVIEWCHANGE after seeing
  STARTVIEWCHANGE from **f other** replicas (step 2); the new primary
  installs the view after **f + 1 including itself** (step 3). Both
  are majorities of 2f+1, counted from different starting points.

  </details>

- [ ] You can compare DOVIEWCHANGE's whole-log shipping against Raft's incremental repair, with numbers, and say when each is cheaper.

  <details><summary>Answer</summary>

  Take a 256 KB log (this topic's 2000 × 128 B bench), f = 2, n = 5.
  A VSR view change moves (f+1) × 256 KB = 768 KB inbound as
  DOVIEWCHANGE plus (n−1) × 256 KB = 1 MB outbound as STARTVIEW —
  about 1.75 MB. A Raft election broadcasts RequestVote carrying four
  integers to four peers: roughly 128 bytes. Four orders of magnitude.

  Raft does not avoid the cost, it defers and targets it: the new
  leader repairs only the followers that actually diverged, one
  AppendEntries at a time, and §5.3's term-skip optimisation bounds
  even that. VSR pays up front, unconditionally, to everyone.

  So: long logs and rare view changes favour Raft; short logs and
  frequent divergence favour shipping once. The selection rule is
  also strictly simpler in VSR — max on `(v', n)` in one place versus
  Raft's per-follower `nextIndex` walk.

  </details>

- [ ] You can write the failure sequence the no-disk argument depends on, and quote the condition the paper attaches to it.

  <details><summary>Answer</summary>

  The losing sequence: 3 of 5 replicas hold entry 42 in memory, the
  primary has replied to the client, and then the whole rack loses
  power. Nothing was on disk, so entry 42 is gone despite having been
  acked. Raft-with-fsync replays it from any of the three logs.

  VR Revisited §4.3 does not hide this. Its justification for
  skipping the write is that the state "is also stored at the other
  replicas and can be retrieved from them, using a recovery protocol.
  Retrieving state will be successful **provided replicas are failure
  independent**, i.e., highly unlikely to fail at the same time. If
  all replicas were to fail simultaneously, state will be lost if the
  information on disk isn't up to date." The mitigations it names —
  UPSs, non-volatile memory, geographic separation — are all outside
  the protocol.

  The 1988 paper's §4.2 describes the same event and calls the
  outcome different: "a catastrophe does not cause a group to enter a
  new view missing some needed information. Rather, it causes the
  algorithm to never again form a new view." Stall rather than silent
  loss.

  The price being declined is measurable here: 337 commits/s at a
  real `F_FULLFSYNC` (topic 5), 341 entries/s with a per-entry
  follower fsync versus 20,174 without (this topic).

  </details>

- [ ] You can say why the recovery protocol needs a nonce, and what the second condition on its responses is for.

  <details><summary>Answer</summary>

  §4.3: "The protocol uses the nonce to ensure that the recovering
  replica accepts only RECOVERYRESPONSE messages that are for this
  recovery and not an earlier one." Without it, replies still in
  flight from a *previous* crash-recover cycle would be accepted, and
  the node could rebuild from a log that was current two crashes ago.
  The paper suggests generating it from the clock, or from a counter
  kept on disk — which is, amusingly, the one disk write the protocol
  will admit to.

  The second condition is that the f+1 responses must **include one
  from the primary of the latest view the recovering replica learns
  of**. Only the primary sends a log at all (`l`, `n`, `k` are nil in
  a backup's response, §4.3 step 2), so this is what makes the copied
  state authoritative rather than merely majority-endorsed.

  And responders must have status `normal` — a replica in the middle
  of its own view change or recovery answers nothing.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the TigerBeetle checksum point.

  <details><summary>Answer</summary>

  The TigerBeetle answer: Raft's Figure 2 assumes stable storage is
  faithful — whatever was fsynced reads back. TigerBeetle assumes it
  is not, and the VSR feature that makes that survivable is
  recover-from-peers: a replica whose own log entry fails its
  checksum is in the same position as a replica that never had it,
  and §4.3's recovery already knows how to refill from f+1 peers.

  TigerBeetle's `docs/internals/vsr.md` cites the CTRL protocol from
  Alagappan et al., *Protocol-Aware Recovery for Consensus-Based
  Storage* (FAST '18), and states the consequence: a replica does not
  nack a corrupt entry, "since it _might_ be the prepare being
  requested". Nacking would let the cluster conclude an entry was
  never accepted when it was.

  Connect to topic 5's torn page: the write that half-landed is
  exactly this fault, and a single-node WAL can only detect it
  (checksum) and then truncate. Replication is what lets you *repair*
  it. TigerBeetle is not in this repo's pin table, so treat these as
  pointers rather than pinned anchors.

  </details>

## References

**Papers**
- Barbara Liskov, James Cowling — *Viewstamped Replication Revisited*,
  MIT-CSAIL-TR-2012-021, 2012 (16 pp.) — **the version to read**.
  Figure 2 is the replica state; §4.1 normal operation (f PREPAREOKs),
  §4.2 view change (f others, then f+1 including self), §4.3 recovery
  and the failure-independence caveat.
- Brian M. Oki, Barbara H. Liskov — *Viewstamped Replication: A New
  Primary Copy Method to Support Highly-Available Distributed
  Systems*, MIT LCS / PODC 1988 (10 pp.) — **a different protocol**:
  transactional, cohorts, viewstamps, and it *does* write to stable
  storage during a view change. Read §4.2 and the conclusions.
- Ramnatthan Alagappan et al. — *Protocol-Aware Recovery for
  Consensus-Based Storage*, USENIX FAST 2018 — the CTRL protocol
  TigerBeetle cites for the corrupt-entry rule in Step 5.

**Code**
- [tigerbeetle](https://github.com/tigerbeetle/tigerbeetle) — VSR in
  production Zig, with the storage-fault model bolted on;
  `src/vsr/replica.zig` and `docs/internals/vsr.md`. **Not in this
  repo's pin table**, so no line anchors are given and the paths are
  read at a moving `main`.
