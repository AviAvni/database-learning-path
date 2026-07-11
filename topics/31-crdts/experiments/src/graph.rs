//! Graph CRDT — YOUR JOB, and the direct dress rehearsal for M31
//! (FalkorDB active-active). Composition: OR-Set of node ids, OR-Set of
//! edges (src, dst), and an LWW map of properties per node.
//!
//! The design decision this module forces — the dangling-edge problem:
//! replica A deletes node n while replica B concurrently adds edge
//! n->m. After merge the edge exists but its endpoint doesn't. Our
//! policy (chosen in the README, §graph): HIDE, don't delete. The edge
//! stays in the edge OR-Set; `edges()` only *shows* edges whose both
//! endpoints are currently visible. If n is re-added (add-wins makes
//! that easy), the edge resurfaces. Alternative policies (cascade
//! delete via observed-remove on edges, or node-delete loses to edge-
//! add) are exercise 4 in the README.
//!
//! Contract fixed by the tests below:
//! - add_node / remove_node / add_edge / remove_edge delegate to the
//!   OR-Sets. remove_node does NOT touch the edge set.
//! - set_prop(node, key, val) writes into that node's LwwMap with a
//!   caller-supplied timestamp (an HLC upstairs in a real system).
//!   Properties survive node remove/re-add — they're keyed by node id,
//!   not by dot. (Automerge makes the other choice; README exercise 5.)
//! - nodes() = visible node set. edges() = edge set filtered to
//!   visible endpoints. get_prop only answers for visible nodes.
//! - merge: merge both OR-Sets and every property map.

use crate::clock::ReplicaId;
use crate::lww::LwwMap;
use crate::orset::OrSet;
use std::collections::{HashMap, HashSet};

pub type NodeId = u64;

#[derive(Clone, Debug)]
pub struct GraphCrdt {
    pub replica: ReplicaId,
    pub nodes: OrSet<NodeId>,
    pub edges: OrSet<(NodeId, NodeId)>,
    pub props: HashMap<NodeId, LwwMap<String, String>>,
}

impl GraphCrdt {
    pub fn new(replica: ReplicaId) -> Self {
        Self {
            replica,
            nodes: OrSet::new(replica),
            edges: OrSet::new(replica),
            props: HashMap::new(),
        }
    }

    pub fn add_node(&mut self, n: NodeId) {
        let _ = n;
        todo!()
    }

    pub fn remove_node(&mut self, n: NodeId) {
        let _ = n;
        todo!("remove from node OR-Set only — edges stay, hidden")
    }

    pub fn add_edge(&mut self, src: NodeId, dst: NodeId) {
        let _ = (src, dst);
        todo!()
    }

    pub fn remove_edge(&mut self, src: NodeId, dst: NodeId) {
        let _ = (src, dst);
        todo!()
    }

    pub fn set_prop(&mut self, n: NodeId, key: &str, val: &str, ts: u64) {
        let _ = (n, key, val, ts);
        todo!("write into props[n] with (ts, self.replica)")
    }

    pub fn nodes(&self) -> HashSet<NodeId> {
        todo!()
    }

    /// Only edges with BOTH endpoints visible.
    pub fn edges(&self) -> HashSet<(NodeId, NodeId)> {
        todo!()
    }

    pub fn get_prop(&self, n: NodeId, key: &str) -> Option<String> {
        let _ = (n, key);
        todo!("None if node not visible")
    }

    pub fn merge(&mut self, other: &GraphCrdt) {
        let _ = other;
        todo!("merge nodes, edges, and each per-node LwwMap")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_synced() -> (GraphCrdt, GraphCrdt) {
        let mut a = GraphCrdt::new(1);
        a.add_node(1);
        a.add_node(2);
        a.add_edge(1, 2);
        let mut b = a.clone();
        b.replica = 2;
        b.nodes.replica = 2;
        b.edges.replica = 2;
        (a, b)
    }

    #[test]
    fn basic_graph_ops() {
        let (a, _) = two_synced();
        assert_eq!(a.nodes(), HashSet::from([1, 2]));
        assert_eq!(a.edges(), HashSet::from([(1, 2)]));
    }

    #[test]
    fn dangling_edge_is_hidden_not_deleted() {
        let (mut a, mut b) = two_synced();
        // Concurrently: a deletes node 2; b adds edge 2->1.
        a.remove_node(2);
        b.add_edge(2, 1);
        a.merge(&b);
        b.merge(&a);

        // Node 2 removal wins over b's edge-add (edge-add didn't re-add
        // the node), so both edges touching 2 are hidden...
        assert_eq!(a.nodes(), HashSet::from([1]));
        assert_eq!(a.edges(), HashSet::new());
        assert_eq!(b.edges(), a.edges());

        // ...but not deleted: re-adding node 2 resurrects them.
        a.add_node(2);
        assert_eq!(a.edges(), HashSet::from([(1, 2), (2, 1)]));
    }

    #[test]
    fn concurrent_node_readd_wins_and_keeps_props() {
        let (mut a, mut b) = two_synced();
        a.set_prop(2, "name", "old", 10);
        b.merge(&a);
        // Concurrently: a removes node 2, b re-adds it (fresh tag).
        a.remove_node(2);
        b.add_node(2);
        a.merge(&b);
        b.merge(&a);
        assert!(a.nodes().contains(&2), "add wins");
        assert_eq!(a.get_prop(2, "name").as_deref(), Some("old"));
        assert_eq!(b.get_prop(2, "name").as_deref(), Some("old"));
    }

    #[test]
    fn props_use_lww() {
        let (mut a, mut b) = two_synced();
        a.set_prop(1, "color", "red", 10);
        b.set_prop(1, "color", "blue", 11);
        a.merge(&b);
        b.merge(&a);
        assert_eq!(a.get_prop(1, "color").as_deref(), Some("blue"));
        assert_eq!(b.get_prop(1, "color").as_deref(), Some("blue"));
    }

    #[test]
    fn hidden_node_hides_props() {
        let (mut a, _) = two_synced();
        a.set_prop(2, "name", "x", 5);
        a.remove_node(2);
        assert_eq!(a.get_prop(2, "name"), None);
    }
}
