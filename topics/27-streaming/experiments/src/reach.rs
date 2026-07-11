//! Semi-naive incremental reachability — the ITERATE side of IVM.
//! Reachability is a recursive query (a fixpoint of join+union+distinct);
//! semi-naive evaluation is the Datalog ancestor of differential's
//! `iterate` (operators/iterate.rs:192 `Variable`) and DBSP's nested
//! circuits: each round joins only the FRONTIER (new facts) against the
//! edges, never the whole reached set.
//!
//! Deliberately insert-only. Deletions are where the hand-rolled version
//! dies: removing an edge may or may not disconnect vertices depending on
//! OTHER paths, so "undo the facts this edge derived" needs either
//! support counting (fragile under cycles) or differential's
//! timestamp-indexed magic — see reading-differential-dataflow.md. Our
//! monotone version is what Datalog engines have always done; DD's
//! contribution is exactly the non-monotone case.

use crate::graph::Edge;
use std::collections::{HashMap, HashSet};

pub struct SemiNaiveReach {
    pub src: u32,
    pub adj: HashMap<u32, Vec<u32>>,
    pub reached: HashSet<u32>,
    /// Work counter: edge relaxations (neighbor visits during BFS from
    /// new frontiers). Across ALL batches, each edge should be relaxed
    /// O(1) times — that's the semi-naive guarantee the tests pin.
    pub relaxations: usize,
}

impl SemiNaiveReach {
    pub fn new(src: u32) -> Self {
        SemiNaiveReach {
            src,
            adj: HashMap::new(),
            reached: HashSet::from([src]),
            relaxations: 0,
        }
    }

    /// STUB — insert a batch of undirected edges and update `reached`:
    /// 1. add each edge to adj (both directions);
    /// 2. seed a frontier: for each new edge with exactly one endpoint
    ///    reached, the other endpoint is newly reached;
    /// 3. BFS from the frontier only, counting each neighbor visit in
    ///    `self.relaxations`.
    /// An edge whose endpoints are both reached (or both not) must cost
    /// zero BFS work — the delta derives nothing.
    pub fn insert_edges(&mut self, _edges: &[Edge]) {
        todo!("frontier = newly-connected endpoints; BFS only from there")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{bfs_reachable, gen_edges};
    use crate::zset::ZSet;

    #[test]
    fn matches_full_bfs_after_each_batch() {
        let all = gen_edges(3000, 15_000, 21);
        let edges: Vec<Edge> = all.iter().map(|(e, _)| *e).collect();
        let mut r = SemiNaiveReach::new(0);
        let mut so_far = Vec::new();
        for chunk in edges.chunks(1000) {
            r.insert_edges(chunk);
            so_far.extend_from_slice(chunk);
            let oracle = bfs_reachable(
                &ZSet::from_updates(so_far.iter().map(|e| (*e, 1)).collect()),
                0,
            );
            assert_eq!(r.reached, oracle);
        }
    }

    #[test]
    fn total_work_is_linear_not_quadratic() {
        // Naive per-batch re-BFS costs sum_i m_i ≈ batches·m/2 total.
        // Semi-naive must relax each edge O(1) times across all batches.
        let all = gen_edges(3000, 30_000, 22);
        let edges: Vec<Edge> = all.iter().map(|(e, _)| *e).collect();
        let mut r = SemiNaiveReach::new(0);
        for chunk in edges.chunks(300) {
            r.insert_edges(chunk);
        }
        assert!(
            r.relaxations <= 4 * edges.len(),
            "{} relaxations for {} edges — re-deriving old facts",
            r.relaxations,
            edges.len()
        );
    }

    #[test]
    fn edge_inside_reached_component_is_free() {
        let mut r = SemiNaiveReach::new(0);
        r.insert_edges(&[(0, 1), (1, 2)]);
        let before = r.relaxations;
        r.insert_edges(&[(0, 2)]); // both endpoints already reached
        assert!(r.relaxations - before <= 1, "closed-triangle edge did BFS work");
        assert_eq!(r.reached.len(), 3);
    }
}
