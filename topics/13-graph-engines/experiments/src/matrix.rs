//! YOU implement: two_hop as boolean sparse matrix-vector products.
//!
//! Same CSR data, rewritten as algebra: the frontier is a sparse
//! vector (list of node ids), one hop is `y<¬seen> = x·A` — for each
//! i in x, OR row A(i,:) into y, masked by the visited set. Two hops
//! = two SpMVs. Structurally identical to csr::two_hop; the point is
//! to FEEL where the algebra earns its keep (dedup/masking is the
//! semiring's job, frontiers are reusable buffers, every hop is the
//! same kernel) and where it's overhead (materializing frontier
//! vectors you never read).
//!
//! Contract:
//! - `spmv_masked`: y receives each unseen neighbor exactly once
//!   (stamp it when pushed); y is cleared first; x is untouched
//! - `two_hop`: stamp src, f1 = {src}·A masked, f2 = f1·A masked,
//!   return f1.len() + f2.len()
//! - buffers f1/f2 are caller-provided and reused across queries

use crate::csr::Csr;

/// One masked boolean SpMV step: y = x·A, skipping (and stamping)
/// already-seen targets.
pub fn spmv_masked(a: &Csr, x: &[u32], y: &mut Vec<u32>, seen: &mut [u32], stamp: u32) {
    let _ = (a, x, y, seen, stamp);
    todo!()
}

/// Distinct nodes at distance 1 or 2 from `src`, excluding `src`.
pub fn two_hop(
    a: &Csr,
    src: u32,
    seen: &mut [u32],
    stamp: u32,
    f1: &mut Vec<u32>,
    f2: &mut Vec<u32>,
) -> u64 {
    let _ = (a, src, seen, stamp, f1, f2);
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adj_list::AdjList;
    use crate::{data, new_seen};

    #[test]
    fn spmv_visits_each_target_once() {
        // 0->{2,3}, 1->{2}: frontier {0,1} must yield 2 exactly once
        let edges = vec![(0, 2), (0, 3), (1, 2)];
        let c = crate::csr::Csr::build(4, &edges);
        let mut seen = new_seen(4);
        let mut y = Vec::new();
        spmv_masked(&c, &[0, 1], &mut y, &mut seen, 1);
        y.sort_unstable();
        assert_eq!(y, vec![2, 3]);
    }

    #[test]
    fn spmv_respects_existing_mask() {
        let edges = vec![(0, 2), (0, 3)];
        let c = crate::csr::Csr::build(4, &edges);
        let mut seen = new_seen(4);
        seen[3] = 1; // 3 already visited under stamp 1
        let mut y = Vec::new();
        spmv_masked(&c, &[0], &mut y, &mut seen, 1);
        assert_eq!(y, vec![2]);
    }

    #[test]
    fn two_hop_matches_oracle() {
        let g = data::random_graph(500, 4000, 11);
        let a = AdjList::build(g.num_nodes, &g.edges);
        let c = crate::csr::Csr::build(g.num_nodes, &g.edges);
        let mut seen_a = new_seen(g.num_nodes);
        let mut seen_m = new_seen(g.num_nodes);
        let (mut f1, mut f2) = (Vec::new(), Vec::new());
        for src in 0..g.num_nodes {
            let stamp = src + 1;
            assert_eq!(
                two_hop(&c, src, &mut seen_m, stamp, &mut f1, &mut f2),
                a.two_hop(src, &mut seen_a, stamp),
                "two_hop({src})"
            );
        }
    }

    #[test]
    fn frontiers_hold_the_actual_levels() {
        // 0->1, 1->2, 2->3: from 0, f1={1}, f2={2}
        let edges = vec![(0, 1), (1, 2), (2, 3)];
        let c = crate::csr::Csr::build(4, &edges);
        let mut seen = new_seen(4);
        let (mut f1, mut f2) = (Vec::new(), Vec::new());
        assert_eq!(two_hop(&c, 0, &mut seen, 1, &mut f1, &mut f2), 2);
        assert_eq!(f1, vec![1]);
        assert_eq!(f2, vec![2]);
    }
}
