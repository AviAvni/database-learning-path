//! YOU implement: Raft election + log replication over the sim —
//! the raft-rs shape (tick/step state machine) without the Ready
//! plumbing. See reading-raft-paper.md Fig 2; the tests below pin
//! the safety properties.
//!
//! Contract:
//! - `tick`: followers/candidates count down an election timeout
//!   (randomize it from `rng` in ELECTION_TIMEOUT_MIN..=MAX, reset
//!   on granting a vote or hearing from the current leader; persist
//!   nothing — the sim never crashes nodes, only partitions them).
//!   On expiry: term += 1, become candidate, vote for self, send
//!   RequestVote to all peers. Leaders instead send heartbeats every
//!   HEARTBEAT_INTERVAL ticks (empty AppendEntries carrying the
//!   consistency check + leader_commit).
//! - `receive`: Fig 2 rules. Any msg with term > self.term →
//!   become follower, adopt term. Vote granting: not voted this term
//!   AND candidate log at least as up-to-date (last term, then
//!   length). AppendEntries: reject unless log has prev_term at
//!   prev_index; on accept, truncate conflicts, append, advance
//!   commit_index to min(leader_commit, last index). AppendResp:
//!   update match_index/next_idx; on reject decrement next_idx and
//!   retry; commit = majority match AND entry.term == current term
//!   (§5.4.2!).
//! - `propose`: leader-only — append (term, cmd) locally, send
//!   AppendEntries to all. Returns false on non-leaders.
//! - Log indexing is 1-based (index 0 = the empty prefix,
//!   prev_term 0). `log[i-1]` holds index i.

use crate::{Entry, Msg, NodeId, Term};
use rand::rngs::StdRng;
use rand::SeedableRng;

pub const ELECTION_TIMEOUT_MIN: u64 = 10;
pub const ELECTION_TIMEOUT_MAX: u64 = 20;
pub const HEARTBEAT_INTERVAL: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role {
    Follower,
    Candidate,
    Leader,
}

pub struct Node {
    pub id: NodeId,
    pub peers: Vec<NodeId>, // includes self
    pub role: Role,
    pub term: Term,
    pub voted_for: Option<NodeId>,
    pub log: Vec<Entry>,
    pub commit_index: u64,
    // leader state
    pub next_idx: Vec<u64>,
    pub match_idx: Vec<u64>,
    // timers
    pub ticks_since_reset: u64,
    pub election_timeout: u64,
    pub rng: StdRng,
    pub votes_received: u64,
}

/// Messages a node wants delivered: (destination, msg).
pub type Outbox = Vec<(NodeId, Msg)>;

impl Node {
    pub fn new(id: NodeId, peers: Vec<NodeId>, seed: u64) -> Node {
        let n = peers.len();
        Node {
            id,
            peers,
            role: Role::Follower,
            term: 0,
            voted_for: None,
            log: Vec::new(),
            commit_index: 0,
            next_idx: vec![1; n],
            match_idx: vec![0; n],
            ticks_since_reset: 0,
            election_timeout: ELECTION_TIMEOUT_MIN + (id + seed) % (ELECTION_TIMEOUT_MAX - ELECTION_TIMEOUT_MIN),
            rng: StdRng::seed_from_u64(seed ^ id),
            votes_received: 0,
        }
    }

    pub fn is_leader(&self) -> bool {
        self.role == Role::Leader
    }

    /// One logical clock tick. May start an election or heartbeat.
    pub fn tick(&mut self) -> Outbox {
        todo!()
    }

    /// Handle one message from `from`; return replies/broadcasts.
    pub fn receive(&mut self, from: NodeId, msg: Msg) -> Outbox {
        let _ = (from, msg);
        todo!()
    }

    /// Leader-only: append cmd, replicate. False if not leader.
    pub fn propose(&mut self, cmd: u64) -> bool {
        let _ = cmd;
        todo!()
    }

    /// Committed commands, in order (for test assertions).
    pub fn committed(&self) -> Vec<u64> {
        self.log[..self.commit_index as usize].iter().map(|&(_, c)| c).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Sim;

    #[test]
    fn elects_exactly_one_leader() {
        let mut sim = Sim::new(5, 42);
        let leader = sim.run_until_leader(200);
        sim.run(30);
        let leaders: Vec<_> = sim.nodes.iter().filter(|n| n.is_leader()).map(|n| n.id).collect();
        assert_eq!(leaders, vec![leader], "leadership must be stable without failures");
    }

    #[test]
    fn at_most_one_leader_per_term() {
        // run several seeds; in every observed state, no two leaders
        // may share a term (THE core safety property of elections)
        for seed in 0..10 {
            let mut sim = Sim::new(5, seed);
            for _ in 0..300 {
                sim.tick();
                let mut leader_terms: Vec<Term> =
                    sim.nodes.iter().filter(|n| n.is_leader()).map(|n| n.term).collect();
                let before = leader_terms.len();
                leader_terms.dedup();
                assert_eq!(before, leader_terms.len(), "two leaders in one term, seed {seed}");
            }
        }
    }

    #[test]
    fn replicates_to_all() {
        let mut sim = Sim::new(5, 7);
        let leader = sim.run_until_leader(200);
        for cmd in [10, 20, 30] {
            assert!(sim.propose(leader, cmd));
            sim.run(5);
        }
        sim.run(20);
        for node in &sim.nodes {
            assert_eq!(node.committed(), vec![10, 20, 30], "node {} log", node.id);
        }
    }

    #[test]
    fn minority_partition_cannot_commit() {
        let mut sim = Sim::new(5, 11);
        let leader = sim.run_until_leader(200);
        // strand the leader with one follower (2/5 = minority)
        let buddy = (0..5).find(|&i| i != leader).unwrap();
        sim.partition(&[leader, buddy]);
        sim.propose(leader, 99);
        sim.run(100);
        assert_eq!(
            sim.nodes[leader as usize].committed(),
            Vec::<u64>::new(),
            "minority leader must NOT commit"
        );
        // majority side elects a new leader and can commit
        let new_leader = sim.current_leader().expect("majority side must elect");
        assert_ne!(new_leader, leader);
        sim.propose(new_leader, 42);
        sim.run(30);
        assert_eq!(sim.nodes[new_leader as usize].committed(), vec![42]);
    }

    #[test]
    fn stale_leader_uncommitted_entry_is_overwritten() {
        let mut sim = Sim::new(5, 13);
        let old_leader = sim.run_until_leader(200);
        let buddy = (0..5).find(|&i| i != old_leader).unwrap();
        sim.partition(&[old_leader, buddy]);
        // stale leader appends 99 — replicated to at most 2 nodes,
        // never committed
        sim.propose(old_leader, 99);
        sim.run(50);
        // majority commits 42 under a higher term
        let new_leader = sim.current_leader().unwrap();
        sim.propose(new_leader, 42);
        sim.run(30);
        // heal: the stale leader must step down, truncate 99, adopt 42
        sim.heal();
        sim.run(60);
        for node in &sim.nodes {
            assert_eq!(node.committed(), vec![42], "node {} after heal", node.id);
            assert!(
                !node.log.iter().any(|&(_, c)| c == 99),
                "node {} still holds the truncated entry",
                node.id
            );
        }
    }
}
