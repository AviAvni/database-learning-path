//! Seeded, Zipfian-skewed graph workload generator — capstone milestone M0.
//!
//! Every later milestone benchmarks against streams produced here, so the same
//! seed must always yield the same op sequence. Skew matters: production graph
//! workloads hammer hub nodes, and a uniform generator would flatter caches
//! (topic 0, "wrong distribution").

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Zipf};

pub type NodeId = u64;

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    InsertNode { id: NodeId },
    InsertEdge { src: NodeId, dst: NodeId },
    ReadNode { id: NodeId },
    TwoHop { start: NodeId },
}

#[derive(Debug, Clone)]
pub struct WorkloadConfig {
    pub seed: u64,
    /// Zipf exponent; 0.99 is the YCSB default, ~1.0 matches skewed graph traffic.
    pub zipf_exponent: f64,
    /// Op mix as weights: (insert_node, insert_edge, read_node, two_hop).
    pub mix: (u32, u32, u32, u32),
}

impl Default for WorkloadConfig {
    fn default() -> Self {
        Self {
            seed: 0xFA1C0DB,
            zipf_exponent: 0.99,
            mix: (10, 30, 40, 20),
        }
    }
}

pub struct Generator {
    rng: StdRng,
    config: WorkloadConfig,
    next_node: NodeId,
    total_weight: u32,
}

impl Generator {
    pub fn new(config: WorkloadConfig) -> Self {
        let rng = StdRng::seed_from_u64(config.seed);
        let (a, b, c, d) = config.mix;
        Self {
            rng,
            config,
            next_node: 0,
            total_weight: a + b + c + d,
        }
    }

    /// Pick an existing node, Zipfian-skewed toward low ids (the "hubs").
    /// Low ids are oldest, matching real graphs where early nodes accrete degree.
    fn skewed_existing(&mut self) -> NodeId {
        debug_assert!(self.next_node > 0);
        let zipf = Zipf::new(self.next_node, self.config.zipf_exponent).unwrap();
        zipf.sample(&mut self.rng) as NodeId - 1
    }

    fn insert_node(&mut self) -> Op {
        let id = self.next_node;
        self.next_node += 1;
        Op::InsertNode { id }
    }
}

impl Iterator for Generator {
    type Item = Op;

    fn next(&mut self) -> Option<Op> {
        // Bootstrap: everything except InsertNode needs existing nodes.
        if self.next_node < 2 {
            return Some(self.insert_node());
        }

        let (a, b, c, _) = self.config.mix;
        let roll = self.rng.gen_range(0..self.total_weight);
        Some(if roll < a {
            self.insert_node()
        } else if roll < a + b {
            let src = self.skewed_existing();
            let dst = self.skewed_existing();
            Op::InsertEdge { src, dst }
        } else if roll < a + b + c {
            Op::ReadNode { id: self.skewed_existing() }
        } else {
            Op::TwoHop { start: self.skewed_existing() }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_across_runs() {
        let ops1: Vec<Op> = Generator::new(WorkloadConfig::default()).take(10_000).collect();
        let ops2: Vec<Op> = Generator::new(WorkloadConfig::default()).take(10_000).collect();
        assert_eq!(ops1, ops2);
    }

    #[test]
    fn different_seeds_differ() {
        let a: Vec<Op> = Generator::new(WorkloadConfig { seed: 1, ..Default::default() })
            .take(1000)
            .collect();
        let b: Vec<Op> = Generator::new(WorkloadConfig { seed: 2, ..Default::default() })
            .take(1000)
            .collect();
        assert_ne!(a, b);
    }

    #[test]
    fn ops_reference_existing_nodes_only() {
        let mut max_node: NodeId = 0;
        for op in Generator::new(WorkloadConfig::default()).take(100_000) {
            match op {
                Op::InsertNode { id } => {
                    assert_eq!(id, max_node);
                    max_node += 1;
                }
                Op::InsertEdge { src, dst } => {
                    assert!(src < max_node && dst < max_node);
                }
                Op::ReadNode { id } => assert!(id < max_node),
                Op::TwoHop { start } => assert!(start < max_node),
            }
        }
    }

    #[test]
    fn reads_are_skewed_toward_hubs() {
        let mut low = 0usize;
        let mut total = 0usize;
        let mut gen = Generator::new(WorkloadConfig::default());
        // Warm up so the id space is large enough for skew to be visible.
        let ops: Vec<Op> = (&mut gen).take(200_000).collect();
        let max_node = ops
            .iter()
            .filter(|o| matches!(o, Op::InsertNode { .. }))
            .count() as NodeId;
        for op in &ops {
            if let Op::ReadNode { id } = op {
                total += 1;
                if *id < max_node / 10 {
                    low += 1;
                }
            }
        }
        // Zipf(0.99): the hottest 10% of keys should absorb well over half the reads.
        assert!(low * 2 > total, "expected skew: {low}/{total} reads in hottest decile");
    }
}
