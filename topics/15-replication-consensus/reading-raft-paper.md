# Raft: logs converge by construction

Paxos won the theory; Raft won the industry (etcd, tikv, CockroachDB,
consul, qdrant's metadata, ...). The pitch is *decomposition*: leader
election, log replication, and safety as separable concerns, plus a
strong-leader design that forbids the log-repair cases Paxos allows.
Read the extended version — the ATC '14 paper is a cut-down of the
tech report; ~18 pages, but §5 is the whole game.

```
 Paxos:  any replica can propose → logs converge by proof gymnastics
 Raft:   ONLY the leader appends → logs converge by construction
         (entries flow one direction: leader → followers)
```

## Reading order

| section | what to extract |
|---|---|
| §5.1 | the three states + RPC menu (only 2 RPCs!) |
| §5.2 | elections: terms, randomized timeouts |
| §5.3 | log replication: the consistency check + repair |
| §5.4 | safety — read TWICE, especially §5.4.2 |
| §6 | membership changes (joint consensus) — skim |
| §7 | log compaction / snapshots — skim, topic 5 déjà vu |
| Fig 2 | the whole algorithm on one page — print it |

## §5.2 — Elections

- A **term** is a logical clock: monotonically increasing, exchanged
  on every RPC; a node seeing a higher term immediately becomes
  follower and adopts it.
- **One vote per term**, and `voted_for` is PERSISTED before
  answering. Question: what double-vote scenario does a crash+restart
  create if `voted_for` were volatile?
- **Randomized election timeouts** (150–300 ms in the paper) break
  the split-vote livelock. Question: why randomize per-election
  rather than assigning fixed distinct timeouts per node? (Hint:
  what happens after a partition heals with two live candidates?)

## §5.3 — Log replication

The consistency check is the heart:

```
 AppendEntries carries (prev_log_index, prev_log_term)
 follower: my log has an entry at prev_log_index with prev_log_term?
   yes → append (truncating any conflicting suffix)
   no  → reject; leader decrements next_index and retries
```

This induction gives the **Log Matching Property**: if two logs have
the same (index, term) they are identical up to that index. Question:
why must a follower *truncate* conflicting entries rather than skip
them? Construct the divergent-log picture from Fig 7.

The follower side, in full:

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

## §5.4 — Safety (the part that matters)

Two mechanisms, and both are needed:

1. **Election restriction** (§5.4.1): a voter refuses candidates
   whose log is less up-to-date — compare last term first, then
   length. So any elected leader already contains every committed
   entry (a committed entry is on a majority; a winning candidate got
   a majority; the two majorities intersect).
2. **§5.4.2 — the current-term commit rule**: a leader only advances
   `commit_index` via majority replication of an entry *from its own
   term*. Older-term entries commit indirectly when a current-term
   entry above them commits.

Figure 8 is the counterexample that makes rule 2 necessary — work it
by hand:

```
 term 2 entry replicated to 2/5 by S1 → S1 crashes
 S5 elected (term 3), appends locally, crashes
 S1 re-elected (term 4), replicates the OLD term-2 entry to 3/5
   — is it committed? NO. S5 can still win (its term-3 entry
     is "newer" by last-term comparison) and truncate it.
```

Our `raft.rs` test `stale_leader_uncommitted_overwritten` is exactly
this shape. Every homegrown Raft that skips §5.4.2 loses acked
writes here.

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
