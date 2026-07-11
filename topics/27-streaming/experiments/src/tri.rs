//! Incrementally maintained triangle count. Triangles are a TRILINEAR
//! query (a 3-way self-join), so the delta expansion has 7 terms — but
//! processing one edge change at a time against current state collapses
//! it to: the change contributed by edge (u,v) is |N(u) ∩ N(v)|.
//! This is the hand-built version of what differential gives for free
//! when you write the 3-way join and stream edge changes at it, and it's
//! the delta-matrix wait/DP/DM story from topic 20 in miniature.

use crate::graph::Edge;
use crate::zset::ZSet;
use std::collections::{BTreeSet, HashMap};

pub struct IncrementalTriangles {
    pub adj: HashMap<u32, BTreeSet<u32>>,
    pub count: i64,
    /// Work counter: neighbor-set membership probes. The contract:
    /// delta-sized batches must do delta-sized work, not O(m).
    pub probes: usize,
}

impl IncrementalTriangles {
    pub fn new() -> Self {
        IncrementalTriangles { adj: HashMap::new(), count: 0, probes: 0 }
    }

    /// STUB — apply one delta batch, return the change in triangle count.
    /// Process entries ONE AT A TIME (a batch may contain edges that form
    /// triangles with each other — per-edge sequencing against current
    /// state gets the cross terms right without the 7-term expansion):
    ///   insert (u,v): d = |N(u) ∩ N(v)| in CURRENT state, then add edge
    ///   delete (u,v): remove edge FIRST, then d = -|N(u) ∩ N(v)|
    /// Intersect by iterating the smaller set and probing the larger
    /// (count each probe in `self.probes`). Update self.count, return the
    /// batch's total change.
    pub fn apply(&mut self, _delta: &ZSet<Edge>) -> i64 {
        todo!("per-edge common-neighbor counting against current state")
    }
}

impl Default for IncrementalTriangles {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{count_triangles, gen_edges, ChurnGen};

    #[test]
    fn matches_oracle_under_churn() {
        let base = gen_edges(500, 4000, 11);
        let mut inc = IncrementalTriangles::new();
        inc.apply(&base);
        let mut g = base.clone();
        assert_eq!(inc.count, count_triangles(&g));
        let mut gen = ChurnGen::new(&base, 500, 12);
        for round in 0..50 {
            let d = gen.next_batch(20, 20);
            inc.apply(&d);
            g = g.merge(&d);
            assert_eq!(inc.count, count_triangles(&g), "diverged at round {}", round);
        }
    }

    #[test]
    fn delete_removes_exactly_the_lost_triangles() {
        let mut inc = IncrementalTriangles::new();
        // K4: 4 triangles; removing one edge kills the 2 triangles using it.
        let mut e = Vec::new();
        for u in 0..4u32 {
            for v in u + 1..4 {
                e.push(((u, v), 1));
            }
        }
        inc.apply(&ZSet::from_updates(e));
        assert_eq!(inc.count, 4);
        let d = inc.apply(&ZSet::singleton((0, 1), -1));
        assert_eq!(d, -2);
        assert_eq!(inc.count, 2);
    }

    #[test]
    fn work_is_delta_sized() {
        let base = gen_edges(2000, 40_000, 13);
        let mut inc = IncrementalTriangles::new();
        inc.apply(&base);
        let mut gen = ChurnGen::new(&base, 2000, 14);
        let before = inc.probes;
        let d = gen.next_batch(10, 10);
        inc.apply(&d);
        let work = inc.probes - before;
        // 20 changes on a 40K-edge graph (avg degree 40): work must track
        // batch size x degree, nowhere near a full O(m·d) recompute.
        assert!(work < 4000, "batch of 20 cost {} probes", work);
    }
}
