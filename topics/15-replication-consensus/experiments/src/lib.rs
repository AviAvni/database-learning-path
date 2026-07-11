//! Topic 15 experiments: Raft over a deterministic simulator, plus a
//! real-fsync replication-lag harness.
//!
//! - `sim`  — PROVIDED: lockstep in-process network with partitions
//! - `raft` — YOU implement: election + log replication
//!
//! Run `cargo test` for the contract; `cargo run --release --bin
//! repl_lag` works without any stubs.

pub mod raft;
pub mod sim;

pub type NodeId = u64;
pub type Term = u64;

/// One log entry: (term it was appended under, opaque command).
pub type Entry = (Term, u64);

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// candidate → all: vote for me in `term`; my log ends at
    /// (last_log_index, last_log_term)
    RequestVote {
        term: Term,
        candidate: NodeId,
        last_log_index: u64,
        last_log_term: Term,
    },
    /// voter → candidate
    Vote { term: Term, from: NodeId, granted: bool },
    /// leader → follower: entries after (prev_index, prev_term).
    /// Empty `entries` = heartbeat.
    AppendEntries {
        term: Term,
        leader: NodeId,
        prev_index: u64,
        prev_term: Term,
        entries: Vec<Entry>,
        leader_commit: u64,
    },
    /// follower → leader: `match_index` valid only when success
    AppendResp {
        term: Term,
        from: NodeId,
        success: bool,
        match_index: u64,
    },
}
