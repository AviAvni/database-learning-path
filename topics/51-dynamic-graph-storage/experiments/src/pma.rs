//! Lane 3 — a packed-memory-array CSR (EXERCISE).
//!
//! Wheatman & Xu's PCSR (HPEC 2018): keep CSR's single edge array, but leave
//! **gaps** in it so an insert shifts a few elements, not half the array. The
//! array is a *packed memory array* (PMA): conceptually a binary tree over
//! fixed-size leaves, each tree node with a density window; when an insert
//! pushes a leaf over its upper density bound, walk up to the first ancestor
//! whose density is acceptable and spread its elements evenly across it (a
//! **rebalance**); if even the root is too dense, double the array.
//!
//! The graph-specific part is how a vertex finds its edges. PCSR puts a
//! **sentinel** slot at the start of every vertex's run, and keeps a vertex
//! array of (first slot, last slot) positions; a rebalance that moves a
//! sentinel must repair that vertex's entry (`fix_sentinel` in the original
//! code — see reading-packed-csr.md).
//!
//! Spec for this lane:
//!
//! * slots are `u32`; `0` = empty; a sentinel is `0x8000_0000 | u`; an edge to
//!   `v` is stored as `v + 1`. (Vertex ids stay below 2^31.)
//! * `for_each(u, f)` visits `u`'s edges in **ascending** order, skipping gaps.
//! * density bounds: the root's upper bound is 3/4 (the array doubles when the
//!   root would exceed it); leaf bounds are looser, interpolating linearly up
//!   the tree. `capacity()` is always a power of two.
//! * `fill_factor()` is occupied slots (edges + sentinels) over `capacity()`.
//!
//! The measured question (README lane 3): what do the gaps cost a scan (the
//! paper says "a factor of two" on SpMV, §IV), and what do they buy an insert?

use crate::gen::EdgeList;
use crate::store::{GraphRead, GraphWrite};

pub struct PmaCsr {
    // Your representation.
}

impl PmaCsr {
    /// Build from a sorted, de-duplicated edge list; `leaf` is the leaf size in
    /// slots (the paper uses ~log2 N; a power of two near it is fine).
    pub fn from_edges(_el: &EdgeList, _leaf: usize) -> PmaCsr {
        todo!("lane 3: spread edges + sentinels over a power-of-two array")
    }
    /// Total slots; always a power of two.
    pub fn capacity(&self) -> usize {
        todo!("lane 3")
    }
    /// Occupied slots (edges + sentinels) over `capacity()`.
    pub fn fill_factor(&self) -> f64 {
        todo!("lane 3")
    }
    /// Number of rebalance operations (window redistributions, including the
    /// full-array redistribution that follows a doubling) since construction.
    pub fn rebalances(&self) -> usize {
        todo!("lane 3")
    }
    /// Number of times the array doubled.
    pub fn doublings(&self) -> usize {
        todo!("lane 3")
    }
}

impl GraphRead for PmaCsr {
    fn name(&self) -> String {
        "packed-memory-array CSR".into()
    }
    fn num_vertices(&self) -> usize {
        todo!("lane 3")
    }
    fn num_edges(&self) -> usize {
        todo!("lane 3")
    }
    fn for_each<F: FnMut(u32)>(&self, _u: u32, _f: F) {
        todo!("lane 3: vertex array -> sentinel, walk to the next sentinel, skip gaps")
    }
    fn contains(&self, _u: u32, _v: u32) -> bool {
        todo!("lane 3")
    }
    fn bytes(&self) -> usize {
        todo!("lane 3: the slot array and the vertex array, by capacity")
    }
}

impl GraphWrite for PmaCsr {
    fn insert(&mut self, _u: u32, _v: u32) -> bool {
        todo!("lane 3: find slot, shift into the nearest gap, rebalance up the window tree")
    }
    fn delete(&mut self, _u: u32, _v: u32) -> bool {
        todo!("lane 3")
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
        let g = PmaCsr::from_edges(&el, 16);
        let r = VecVec::from_edges(&el);
        assert_eq!(g.num_edges(), el.edges.len());
        for u in 0..el.n {
            let n = neighbours(&g, u);
            assert!(n.windows(2).all(|w| w[0] < w[1]));
            assert_eq!(n, neighbours(&r, u), "vertex {u}");
        }
    }

    #[test]
    fn capacity_is_a_power_of_two_with_gaps_left() {
        let el = rmat(GraphParams::TINY);
        let g = PmaCsr::from_edges(&el, 16);
        assert!(g.capacity().is_power_of_two());
        let f = g.fill_factor();
        assert!(f > 0.25 && f <= 0.75, "fill {f}: gaps are the whole point, but not too many");
    }

    #[test]
    fn hammering_one_vertex_rebalances_and_doubles_without_breaking_order() {
        let el = EdgeList { n: 64, edges: (1..64).map(|v| (0, v)).collect(), perm: (0..64).collect() };
        let mut g = PmaCsr::from_edges(&el, 8);
        let cap0 = g.capacity();
        // Vertex 30 gets every insert: all the pressure lands on one region.
        for v in 100..3100u32 {
            assert!(g.insert(30, v));
        }
        assert!(g.rebalances() > 0);
        assert!(g.doublings() > 0 && g.capacity() > cap0);
        assert!(g.capacity().is_power_of_two());
        assert!(g.fill_factor() <= 0.76, "root bound is 3/4, got {}", g.fill_factor());
        let n = neighbours(&g, 30);
        assert_eq!(n, (100..3100).collect::<Vec<_>>());
        // Other vertices' pointers survived every move.
        assert_eq!(neighbours(&g, 0), (1..64).collect::<Vec<_>>());
        assert!(neighbours(&g, 31).is_empty());
    }

    #[test]
    fn agrees_with_the_reference_through_random_writes() {
        let el = rmat(GraphParams::TINY);
        let wl = workload(&el, GraphParams::TINY.scale, 64, 3000);
        let mut a = PmaCsr::from_edges(&el, 16);
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
    fn bytes_cover_the_whole_slot_array() {
        let el = rmat(GraphParams::TINY);
        let g = PmaCsr::from_edges(&el, 16);
        assert!(g.bytes() >= 4 * g.capacity());
    }
}
