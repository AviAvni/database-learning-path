# Raft: logs converge by construction

Paxos won the theory; Raft won the industry (etcd, tikv, CockroachDB,
consul, qdrant's metadata, ...). The pitch is *decomposition*: leader
election, log replication, and safety as separable concerns, plus a
strong-leader design that forbids the log-repair cases Paxos allows.
Before the paper, this chapter builds the algorithm one concept at a
time — the replicated log, terms, elections, the consistency check,
and the two safety rules — ending on the Fig 8 trap that every
homegrown Raft falls into. Read the extended version — the ATC '14
paper is a cut-down of the tech report; ~18 pages, but §5 is the
whole game.

## The problem in one sentence

Keep 5 machines agreeing on one sequence of writes such that a write
acknowledged to the client survives **any 2 machines dying at any
moment** — the thing async replication (valkey, next chapter)
explicitly does not promise, since a leader that acks before
replicating can take acked writes to the grave.

## The concepts, step by step

### Step 1 — the replicated log: agree on order, and state follows

A replicated state machine keeps several servers identical by a
simple trick: if every server starts from the same state and applies
the same commands *in the same order*, they end in the same state.
So the servers don't replicate state — they replicate a **log** (a
numbered, append-only sequence of commands), and consensus reduces
to one question: what is entry #i?

```
        index:   1     2     3     4
 leader   log: [x=1] [y=2] [x=4] [y=9]   ── committed up to 3 ──
 follower log: [x=1] [y=2] [x=4]
 follower log: [x=1] [y=2] [x=4] [y=9]
```

An entry is **committed** when the protocol guarantees it will never
be removed from anyone's log; only committed entries are applied to
the state machine. Raft's structural simplification over Paxos: only
one node — the **leader** — ever appends, and entries flow one
direction, leader → followers:

```
 Paxos:  any replica can propose → logs converge by proof gymnastics
 Raft:   ONLY the leader appends → logs converge by construction
         (entries flow one direction: leader → followers)
```

### Step 2 — terms: a logical clock that fences dead leaders

Leaders fail, so leadership must be handed over — and the cluster
needs to distinguish the current leader's messages from a stale
one's. A **term** is a monotonically increasing integer that acts as
a logical clock: time divides into numbered terms, each with at most
one leader. Every message carries the sender's term; every node
tracks the highest it has seen. Two rules do all the fencing:
see a *higher* term → you are stale, become follower and adopt it;
see a *lower* term → the sender is stale, reject. A leader deposed
by a partition can't damage anything after healing: its term is old,
so everyone rejects it. Cost: two integers of state and a comparison
per message — the cheapest fencing token in systems.

### Step 3 — elections: randomized timeouts, one persisted vote

Each node is a follower, candidate, or leader (the README's state
diagram). Followers expect periodic heartbeats from the leader;
a follower that hears nothing for an **election timeout** increments
the term, becomes candidate, and asks everyone for votes; a majority
of votes makes it leader. Two details carry the correctness:

- **One vote per term, persisted.** Each node grants at most one
  vote per term, and `voted_for` is fsynced to disk *before* the
  vote is sent — a crash+restart must not free the node to vote
  twice in the same term, or two leaders could win one term
  (question: construct the double-vote scenario if `voted_for` were
  volatile).
- **Randomized timeouts** (150–300 ms in the paper) break symmetry.
  If all nodes timed out together, votes would split, nobody would
  get a majority, and the cycle would repeat — a livelock.
  Randomization makes one node usually fire first and win cleanly.
  (Question: why randomize per-election rather than assigning fixed
  distinct timeouts per node? Hint: what happens after a partition
  heals with two live candidates?)

Elections cost nothing during normal operation; the price of this
design is unavailability for ~1 timeout when a leader dies.

### Step 4 — log replication: the consistency check

The leader appends a client command to its own log, then sends
`AppendEntries` to followers. The heart of Raft is one guard on that
message:

```
 AppendEntries carries (prev_log_index, prev_log_term)
 follower: my log has an entry at prev_log_index with prev_log_term?
   yes → append (truncating any conflicting suffix)
   no  → reject; leader decrements next_index and retries
```

By induction this gives the **Log Matching Property**: if two logs
have the same (index, term) at one position, they are identical up
to that position — the follower only accepted each entry after
proving the previous one matched. A follower with a divergent
suffix (appended by some dead leader, never committed) gets it
*truncated* and overwritten. The follower side, in full:

```rust
// the consistency check — Log Matching by induction, one RPC at a time
fn handle_append(&mut self, m: AppendEntries) -> bool {
    if m.term < self.term { return false; }           // stale leader: fenced
    match self.log.get(m.prev_index) {
        None => false,                                // hole → leader backs up
        Some(e) if e.term != m.prev_term => false,    // divergent history
        _ => {
            for (i, new) in m.entries.iter().enumerate() {
                let idx = m.prev_index + 1 + i as u64;
                if self.log.term_at(idx) != Some(new.term) {
                    self.log.truncate(idx);           // conflicting suffix DIES
                    self.log.push(new.clone());       // (it was never committed)
                }
            }
            self.commit_index = m.leader_commit.min(self.log.last_index());
            true
        }
    }
}
```

Question: why must a follower *truncate* conflicting entries rather
than skip them? Construct the divergent-log picture from the paper's
Fig 7. Cost accounting: one round trip per batch of entries in the
common case; O(divergence) retries to repair a lagging follower.

### Step 5 — safety rule 1: the election restriction

Committed entries must survive leader changes, so Raft never lets a
node that *lacks* a committed entry become leader. A voter refuses
any candidate whose log is less up-to-date than its own — compare
last entry's term first, then log length. The quorum argument does
the rest: a committed entry lives on a majority; a winning candidate
convinced a majority; the two majorities intersect in at least one
node, and that node's vote-check blocked any candidate missing the
entry. So an elected leader already contains every committed entry —
which is why Raft never needs to copy entries *into* a new leader
(contrast VSR, [reading-vsr.md](reading-vsr.md), which chose the
opposite).

### Step 6 — safety rule 2: only current-term entries count for commit

The subtle one (§5.4.2): "replicated on a majority" is NOT
sufficient to commit an entry from an *older* term. A leader may
only advance `commit_index` by majority-replicating an entry *from
its own term*; older entries then commit indirectly, riding below
it. Figure 8 is the counterexample that forces the rule — work it by
hand:

```
 term 2 entry replicated to 2/5 by S1 → S1 crashes
 S5 elected (term 3), appends locally, crashes
 S1 re-elected (term 4), replicates the OLD term-2 entry to 3/5
   — is it committed? NO. S5 can still win (its term-3 entry
     is "newer" by last-term comparison) and truncate it.
```

The failure is quorum arithmetic: the election restriction compares
*last terms*, and a majority holding an old-term entry can still
vote for a candidate whose newer-term entry outranks it. Replicating
one current-term entry on a majority closes the hole — now any
future winner provably holds everything below it. Our `raft.rs` test
`stale_leader_uncommitted_overwritten` is exactly this shape. Every
homegrown Raft that skips §5.4.2 loses acked writes here.

## How to read the paper (with the concepts in hand)

| section | what to extract | step |
|---|---|---|
| §5.1 | the three states + RPC menu (only 2 RPCs!) | 1, 3 |
| §5.2 | elections: terms, randomized timeouts | 2–3 |
| §5.3 | log replication: the consistency check + repair | 4 |
| §5.4 | safety — read TWICE, especially §5.4.2 | 5–6 |
| §6 | membership changes (joint consensus) — skim | — |
| §7 | log compaction / snapshots — skim, topic 5 déjà vu | — |
| Fig 2 | the whole algorithm on one page — print it | all |

Fig 2 is the spec that raft-rs implements
([reading-raft-rs.md](reading-raft-rs.md)) — keep it printed next to
you for both chapters. Fig 7 is Step 4's divergence zoo; Fig 8 is
Step 6, and worth an hour.

## Questions to answer in notes.md

1. Why persist `(current_term, voted_for, log)` but NOT
   `commit_index`? What recomputes commit_index after restart?
2. Fig 8 step-by-step: which specific quorum-intersection argument
   fails without the current-term rule?
3. Why does a leader never overwrite/delete its OWN log entries, and
   what breaks if it could?
4. §7: a snapshot at index i replaces the log prefix — what must the
   snapshot record besides the state? (last_included_index/term —
   why the term?)
5. Map to valkey: which Raft properties does async replication give
   up, and what do you get back for each?

## References

**Papers**
- Ongaro, Ousterhout — "In Search of an Understandable Consensus
  Algorithm" (USENIX ATC 2014) — read the extended version (the tech
  report); §5 twice, Fig 2 printed, Fig 8 worked by hand
- Ongaro — "Consensus: Bridging Theory and Practice" (Stanford PhD
  dissertation, 2014) — optional; the long-form version with the
  membership-change fixes

**Code**
- The production implementation is
  [raft-rs](https://github.com/tikv/raft-rs) — walked in
  [reading-raft-rs.md](reading-raft-rs.md)
