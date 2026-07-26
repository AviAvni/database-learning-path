//! STUB — a consistent-hash ring with virtual nodes (Dynamo §4.2).
//!
//! Each node projects `vnodes` points onto the u64 ring; a key belongs
//! to the owner of the first point clockwise from `splitmix64(key)`
//! (wrapping past u64::MAX to the smallest point). Use
//! `splitmix64(((node as u64) << 32) | i)` for node's i-th point so
//! placement is deterministic.
//!
//! The contracts (the tests): adding a node to an N-node ring moves
//! ≈ 1/(N+1) of keys, and every moved key moves TO the new node;
//! removing a node moves ONLY that node's keys; balance (max/mean load)
//! tightens as vnodes grows — 1 vnode per node is visibly lumpy,
//! hundreds are not (Dynamo's strategy-1 lesson).

pub struct HashRing {
    pub vnodes: u32,
    /// (ring position, node id), kept sorted by position.
    pub points: Vec<(u64, u32)>,
}

impl HashRing {
    pub fn new(vnodes: u32) -> Self {
        HashRing {
            vnodes,
            points: Vec::new(),
        }
    }

    /// Insert this node's `vnodes` points, keeping `points` sorted.
    pub fn add_node(&mut self, node: u32) {
        let _ = node;
        todo!("push (splitmix64((node << 32) | i), node) for i in 0..vnodes, re-sort")
    }

    /// Remove all of this node's points.
    pub fn remove_node(&mut self, node: u32) {
        let _ = node;
        todo!("retain points whose node id differs")
    }

    /// Owner of `key`: first point at or clockwise-after splitmix64(key).
    /// O(log points) — binary search, wrap to points[0].
    pub fn lookup(&self, key: u64) -> u32 {
        let _ = key;
        todo!("partition_point on position, wrap to the first point")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::splitmix64;

    fn owners(ring: &HashRing, n_keys: u64) -> Vec<u32> {
        (0..n_keys).map(|k| ring.lookup(splitmix64(k))).collect()
    }

    /// Growing 4 nodes to 5 moves ≈ 20% of keys (vs mod-N's 80%), and
    /// every key that moved, moved to the NEW node — nothing else
    /// reshuffles. That is the entire point of the ring.
    #[test]
    fn add_node_moves_about_one_fifth_and_only_to_the_new_node() {
        let mut ring = HashRing::new(128);
        for n in 0..4 {
            ring.add_node(n);
        }
        let before = owners(&ring, 50_000);
        ring.add_node(4);
        let after = owners(&ring, 50_000);
        let moved = before.iter().zip(&after).filter(|(b, a)| b != a).count();
        let frac = moved as f64 / before.len() as f64;
        assert!(frac > 0.10 && frac < 0.30, "expected ~0.20, got {frac}");
        for (b, a) in before.iter().zip(&after) {
            if b != a {
                assert_eq!(*a, 4, "a moved key must land on the new node");
            }
        }
    }

    /// Removing a node moves exactly that node's keys and no others:
    /// every key owned by a surviving node keeps its owner.
    #[test]
    fn remove_node_moves_only_its_keys() {
        let mut ring = HashRing::new(128);
        for n in 0..5 {
            ring.add_node(n);
        }
        let before = owners(&ring, 50_000);
        ring.remove_node(2);
        let after = owners(&ring, 50_000);
        for (b, a) in before.iter().zip(&after) {
            if *b != 2 {
                assert_eq!(b, a, "a surviving node's key must not move");
            } else {
                assert_ne!(*a, 2, "node 2's keys must be re-homed");
            }
        }
    }

    /// Dynamo's strategy-1 lesson: random positions are lumpy. More
    /// vnodes per node = smaller, more numerous arcs = tighter balance.
    #[test]
    fn more_vnodes_tighter_balance() {
        let imbalance = |vnodes: u32| -> f64 {
            let mut ring = HashRing::new(vnodes);
            for n in 0..8 {
                ring.add_node(n);
            }
            let mut counts = [0u64; 8];
            for k in 0..100_000u64 {
                counts[ring.lookup(splitmix64(k)) as usize] += 1;
            }
            *counts.iter().max().unwrap() as f64 / (100_000.0 / 8.0)
        };
        let lumpy = imbalance(1);
        let smooth = imbalance(512);
        assert!(smooth < lumpy, "512 vnodes ({smooth}) must beat 1 ({lumpy})");
        assert!(smooth < 1.25, "512 vnodes should be within 25% of ideal, got {smooth}");
    }
}
