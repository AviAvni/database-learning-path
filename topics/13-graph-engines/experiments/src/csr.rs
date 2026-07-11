//! YOU implement: CSR build + two_hop over slices.
//!
//! Contract (tests pin it down):
//! - `build`: counting sort — degree count, exclusive prefix sum,
//!   scatter. Input edges are sorted+deduped, so scattering in input
//!   order leaves each row sorted ascending (assert it in a debug
//!   build if you like). No per-node allocations.
//! - `offsets.len() == num_nodes + 1`, `targets.len() == edges.len()`
//! - `neighbors(v)` = one slice, zero pointer chases
//! - `two_hop`: same semantics as the adj_list oracle (distinct nodes
//!   at distance 1 or 2, excluding src), reusing the seen/stamp set

pub struct Csr {
    pub offsets: Vec<u32>, // num_nodes + 1
    pub targets: Vec<u32>, // edge count, row-sorted
}

impl Csr {
    pub fn build(num_nodes: u32, edges: &[(u32, u32)]) -> Csr {
        let _ = (num_nodes, edges);
        todo!()
    }

    pub fn neighbors(&self, v: u32) -> &[u32] {
        let _ = v;
        todo!()
    }

    pub fn two_hop(&self, src: u32, seen: &mut [u32], stamp: u32) -> u64 {
        let _ = (src, seen, stamp);
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adj_list::AdjList;
    use crate::{data, new_seen};

    fn tiny() -> Vec<(u32, u32)> {
        // 0->{5,9}, 1->{5}, 2->{}, 3->{0}
        vec![(0, 5), (0, 9), (1, 5), (3, 0)]
    }

    #[test]
    fn build_exact_layout() {
        let c = Csr::build(10, &tiny());
        assert_eq!(c.offsets, vec![0, 2, 3, 3, 4, 4, 4, 4, 4, 4, 4]);
        assert_eq!(c.targets, vec![5, 9, 5, 0]);
    }

    #[test]
    fn neighbors_is_a_sorted_slice() {
        let g = data::random_graph(200, 2000, 7);
        let c = Csr::build(g.num_nodes, &g.edges);
        let mut total = 0;
        for v in 0..g.num_nodes {
            let ns = c.neighbors(v);
            assert!(ns.windows(2).all(|w| w[0] < w[1]), "row {v} not sorted+deduped");
            total += ns.len();
        }
        assert_eq!(total, g.edges.len());
    }

    #[test]
    fn empty_rows_and_last_node() {
        let c = Csr::build(10, &tiny());
        assert_eq!(c.neighbors(2), &[] as &[u32]);
        assert_eq!(c.neighbors(9), &[] as &[u32]);
    }

    #[test]
    fn two_hop_matches_oracle() {
        let g = data::random_graph(500, 4000, 11);
        let a = AdjList::build(g.num_nodes, &g.edges);
        let c = Csr::build(g.num_nodes, &g.edges);
        let mut seen_a = new_seen(g.num_nodes);
        let mut seen_c = new_seen(g.num_nodes);
        for src in 0..g.num_nodes {
            let stamp = src + 1;
            assert_eq!(
                c.two_hop(src, &mut seen_c, stamp),
                a.two_hop(src, &mut seen_a, stamp),
                "two_hop({src})"
            );
        }
    }

    #[test]
    fn two_hop_excludes_self_even_in_a_cycle() {
        // 0->1->0: node 0 reaches itself at distance 2 — must not count
        let edges = vec![(0, 1), (1, 0)];
        let c = Csr::build(2, &edges);
        let mut seen = new_seen(2);
        assert_eq!(c.two_hop(0, &mut seen, 1), 1);
    }
}
