//! Lane 2 — Sortledton-style segmented sorted blocks (EXERCISE).
//!
//! Fuchs, Margan & Giceva (PVLDB 15(6), 2022) store every neighbourhood as a
//! *sorted set* so that scans stay sequential (within a block) and
//! intersections stay linear. The design this lane asks for, per vertex `u`
//! (see reading-sortledton.md for the paper sections and the code anchors):
//!
//! * **small** neighbourhoods (`degree <= block_cap`): one sorted `Vec<u32>`
//!   — the paper's "headerless, power-of-two-sized vectors" (§4.2). One
//!   block, grown by doubling.
//! * **hub** neighbourhoods (`degree > block_cap`): a sequence of sorted
//!   blocks of capacity `block_cap`, plus an index over the blocks' first
//!   (minimum) keys. The paper uses an unrolled skip list; a sorted `Vec` of
//!   block minima searched by binary search is an acceptable substitute here
//!   (say so in your notes — it changes the block-lookup cost, not the layout
//!   a scan walks).
//! * a **full** block splits in two, each half holding half the edges; a block
//!   that falls **below half full** merges with a neighbour (so every block of
//!   a multi-block vertex holds between `block_cap/2` and `block_cap` edges).
//!
//! The scan contract: `for_each(u, f)` visits neighbours in **ascending**
//! order — Sortledton's reason for sorting is that triangle counting needs it.
//!
//! The measured question (README lane 2): what does a block layout cost a
//! two-hop scan relative to one contiguous CSR row, and what does it buy an
//! insert over a splice?

use crate::gen::EdgeList;
use crate::store::{GraphRead, GraphWrite};

pub struct SegBlocks {
    // Your representation.
}

impl SegBlocks {
    /// Build from a sorted, de-duplicated edge list. `block_cap` is the
    /// maximum edges per block (the paper evaluates 256 and 512, §7).
    pub fn from_edges(_el: &EdgeList, _block_cap: usize) -> SegBlocks {
        todo!("lane 2: per-vertex sorted vec (small) / sorted blocks + block index (hub)")
    }

    /// Edges per block of `u`, in order. A vertex with no edges returns an
    /// empty vector; a small vertex returns a single entry.
    pub fn block_sizes(&self, _u: u32) -> Vec<usize> {
        todo!("lane 2")
    }
}

impl GraphRead for SegBlocks {
    fn name(&self) -> String {
        "sorted segmented blocks".into()
    }
    fn num_vertices(&self) -> usize {
        todo!("lane 2")
    }
    fn num_edges(&self) -> usize {
        todo!("lane 2")
    }
    fn for_each<F: FnMut(u32)>(&self, _u: u32, _f: F) {
        todo!("lane 2: ascending order")
    }
    fn contains(&self, _u: u32, _v: u32) -> bool {
        todo!("lane 2: block index, then binary search inside one block")
    }
    fn bytes(&self) -> usize {
        todo!("lane 2: capacities, not lengths — empty slots count")
    }
}

impl GraphWrite for SegBlocks {
    fn insert(&mut self, _u: u32, _v: u32) -> bool {
        todo!("lane 2: locate block, shift inside it, split when full")
    }
    fn delete(&mut self, _u: u32, _v: u32) -> bool {
        todo!("lane 2: remove inside the block, merge when under half full")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{rmat, workload, GraphParams};
    use crate::store::two_hop;
    use crate::vecvec::VecVec;

    fn neighbours<G: GraphRead>(g: &G, u: u32) -> Vec<u32> {
        let mut v = Vec::new();
        g.for_each(u, |x| v.push(x));
        v
    }

    #[test]
    fn build_matches_input_and_scans_ascending() {
        let el = rmat(GraphParams::TINY);
        let g = SegBlocks::from_edges(&el, 16);
        let r = VecVec::from_edges(&el);
        assert_eq!(g.num_edges(), el.edges.len());
        for u in 0..el.n {
            let n = neighbours(&g, u);
            assert!(n.windows(2).all(|w| w[0] < w[1]), "vertex {u} not ascending");
            assert_eq!(n, neighbours(&r, u), "vertex {u}");
        }
    }

    #[test]
    fn small_neighbourhoods_are_one_block_hubs_are_many() {
        let el = rmat(GraphParams::TINY);
        let cap = 16;
        let g = SegBlocks::from_edges(&el, cap);
        let r = VecVec::from_edges(&el);
        let mut hubs = 0;
        for u in 0..el.n {
            let d = r.adj[u as usize].len();
            let b = g.block_sizes(u);
            assert_eq!(b.iter().sum::<usize>(), d);
            if d <= cap {
                assert!(b.len() <= 1, "vertex {u}: degree {d} is a small vertex, one block");
            } else {
                hubs += 1;
                assert!(b.len() >= d.div_ceil(cap), "vertex {u}");
                assert!(b.iter().all(|&x| x <= cap));
            }
        }
        assert!(hubs > 0, "the tiny graph must have a hub for this test to mean anything");
    }

    #[test]
    fn blocks_split_at_full_and_stay_at_least_half_full() {
        let el = EdgeList { n: 4, edges: vec![], perm: vec![0, 1, 2, 3] };
        let cap = 16;
        let mut g = SegBlocks::from_edges(&el, cap);
        // Ascending inserts are the worst case for a half-split policy.
        for v in 1..=200u32 {
            assert!(g.insert(0, v + 1000));
        }
        let b = g.block_sizes(0);
        assert_eq!(b.iter().sum::<usize>(), 200);
        assert!(b.iter().all(|&x| x <= cap && x >= cap / 2), "fills {b:?}");
        assert!(b.len() >= 200usize.div_ceil(cap));
        // And descending, interleaved with some duplicates.
        for v in (1..=200u32).rev() {
            let fresh = g.insert(1, v * 3);
            assert!(fresh);
            assert!(!g.insert(1, v * 3));
        }
        let b = g.block_sizes(1);
        assert!(b.iter().all(|&x| x <= cap && x >= cap / 2), "fills {b:?}");
    }

    #[test]
    fn blocks_merge_when_under_half_full() {
        let el = EdgeList { n: 2, edges: vec![], perm: vec![0, 1] };
        let cap = 16;
        let mut g = SegBlocks::from_edges(&el, cap);
        for v in 0..320u32 {
            g.insert(0, v);
        }
        let before = g.block_sizes(0).len();
        for v in 0..320u32 {
            if v % 10 != 0 {
                assert!(g.delete(0, v));
            }
        }
        let b = g.block_sizes(0);
        assert_eq!(b.iter().sum::<usize>(), 32);
        assert!(b.len() < before, "{before} blocks should have merged down, got {}", b.len());
        if b.len() > 1 {
            assert!(b.iter().all(|&x| x >= cap / 2 && x <= cap), "fills {b:?}");
        }
        assert!(!g.delete(0, 5));
    }

    #[test]
    fn agrees_with_the_reference_through_random_writes() {
        let el = rmat(GraphParams::TINY);
        let wl = workload(&el, GraphParams::TINY.scale, 64, 3000);
        let mut a = SegBlocks::from_edges(&el, 16);
        let mut b = VecVec::from_edges(&el);
        for i in 0..3000 {
            let (u, v) = wl.inserts[i];
            assert_eq!(a.insert(u, v), b.insert(u, v));
            let (u, v) = wl.deletes[i];
            assert_eq!(a.delete(u, v), b.delete(u, v));
        }
        assert_eq!(a.num_edges(), b.num_edges());
        for &s in &wl.starts {
            assert_eq!(two_hop(&a, s), two_hop(&b, s));
        }
        for &(u, v) in wl.inserts.iter().take(200) {
            assert_eq!(a.contains(u, v), b.contains(u, v));
        }
    }

    #[test]
    fn bytes_count_empty_slots() {
        let el = rmat(GraphParams::TINY);
        let g = SegBlocks::from_edges(&el, 16);
        assert!(g.bytes() >= 4 * el.edges.len());
    }
}
