//! Provided: deterministic in-process network. Lockstep ticks, seeded
//! delivery order, partition/heal injection. No threads, no wall
//! clock — every "distributed" failure here is reproducible from the
//! seed (topic 16's DST, in miniature).

use crate::raft::Node;
use crate::{Msg, NodeId};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

pub struct Sim {
    pub nodes: Vec<Node>,
    inflight: Vec<(NodeId, NodeId, Msg)>, // (from, to, msg)
    /// partition[i] = group id of node i; messages cross groups only
    /// when the network is healed
    groups: Vec<u8>,
    rng: StdRng,
    pub ticks: u64,
}

impl Sim {
    pub fn new(n: usize, seed: u64) -> Sim {
        let ids: Vec<NodeId> = (0..n as u64).collect();
        Sim {
            nodes: ids.iter().map(|&id| Node::new(id, ids.clone(), seed)).collect(),
            inflight: Vec::new(),
            groups: vec![0; n],
            rng: StdRng::seed_from_u64(seed),
            ticks: 0,
        }
    }

    /// Cut the network: nodes in `side` form one group, the rest the
    /// other. Messages between groups are dropped.
    pub fn partition(&mut self, side: &[NodeId]) {
        for (i, g) in self.groups.iter_mut().enumerate() {
            *g = if side.contains(&(i as u64)) { 1 } else { 0 };
        }
    }

    pub fn heal(&mut self) {
        self.groups.iter_mut().for_each(|g| *g = 0);
    }

    /// One lockstep round: every node ticks, then all messages
    /// produced so far are delivered in seeded-shuffled order
    /// (delivery within a tick, but in adversarial order).
    pub fn tick(&mut self) {
        self.ticks += 1;
        for node in &mut self.nodes {
            let out = node.tick();
            self.inflight.extend(out.into_iter().map(|(to, m)| (node.id, to, m)));
        }
        // deliver until quiescent so RPC chains settle within a tick
        while !self.inflight.is_empty() {
            let mut batch = std::mem::take(&mut self.inflight);
            batch.shuffle(&mut self.rng);
            for (from, to, msg) in batch {
                if self.groups[from as usize] != self.groups[to as usize] {
                    continue; // dropped at the partition
                }
                let out = self.nodes[to as usize].receive(from, msg);
                self.inflight.extend(out.into_iter().map(|(t, m)| (to, t, m)));
            }
        }
    }

    pub fn run(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.tick();
        }
    }

    /// Tick until exactly one node in the majority group is leader,
    /// or panic after `max` ticks.
    pub fn run_until_leader(&mut self, max: u64) -> NodeId {
        for _ in 0..max {
            self.tick();
            if let Some(l) = self.current_leader() {
                return l;
            }
        }
        panic!("no leader after {max} ticks");
    }

    /// The unique leader among reachable-majority nodes, if any.
    /// Ignores stale leaders stranded in a minority partition.
    pub fn current_leader(&self) -> Option<NodeId> {
        let majority_group = {
            let ones = self.groups.iter().filter(|&&g| g == 1).count();
            if ones * 2 > self.groups.len() { 1 } else { 0 }
        };
        let leaders: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|n| n.is_leader() && self.groups[n.id as usize] == majority_group)
            .map(|n| n.id)
            .collect();
        match leaders.as_slice() {
            [l] => Some(*l),
            [] => None,
            many => {
                // two leaders in the same group is only legal in
                // different terms (one is about to step down)
                let max_term = many.iter().map(|&l| self.nodes[l as usize].term).max();
                many.iter()
                    .copied()
                    .find(|&l| Some(self.nodes[l as usize].term) == max_term)
            }
        }
    }

    pub fn propose(&mut self, leader: NodeId, cmd: u64) -> bool {
        let ok = self.nodes[leader as usize].propose(cmd);
        // ship the resulting AppendEntries on the next tick
        ok
    }
}
