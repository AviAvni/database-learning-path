//! SSSP: binary-heap Dijkstra (PROVIDED oracle) vs delta-stepping
//! (STUB) — Meyer & Sanders' answer to "Dijkstra has no parallelism,
//! Bellman-Ford has no work efficiency".

use crate::graph::Csr;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub const INF: u64 = u64::MAX;

/// Returns (dist, heap_pops). Lazy-deletion Dijkstra: stale heap
/// entries skipped on pop — pops therefore ≥ n, the extra pops are
/// the price of no decrease-key.
pub fn dijkstra(g: &Csr, source: u32) -> (Vec<u64>, usize) {
    let mut dist = vec![INF; g.n];
    let mut heap = BinaryHeap::new();
    dist[source as usize] = 0;
    heap.push(Reverse((0u64, source)));
    let mut pops = 0usize;
    while let Some(Reverse((d, u))) = heap.pop() {
        pops += 1;
        if d > dist[u as usize] {
            continue;
        }
        let (ts, ws) = g.neigh_w(u as usize);
        for (&v, &w) in ts.iter().zip(ws) {
            let nd = d + w as u64;
            if nd < dist[v as usize] {
                dist[v as usize] = nd;
                heap.push(Reverse((nd, v)));
            }
        }
    }
    (dist, pops)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DeltaStats {
    /// edge relaxations performed (incl. redundant re-relaxations)
    pub relaxations: usize,
    /// non-empty buckets processed
    pub buckets: usize,
}

/// STUB — delta-stepping (Meyer & Sanders; gapbs sssp.cc:87
/// `DeltaStep`, LAGraph LAGr_SingleSourceShortestPath.c).
///
/// Recipe:
/// 1. Buckets `bins[i]` hold vertices with tentative dist in
///    [i·delta, (i+1)·delta). `bins[0] = {source}`.
/// 2. Process buckets in order. Repeatedly drain the CURRENT bucket:
///    relax all out-edges of each drained vertex (count every
///    relaxation); an improved vertex goes into bucket
///    `new_dist / delta` (grow `bins` as needed). A LIGHT edge
///    (w < delta) can put a vertex back into the current bucket —
///    that's the inner loop; heavy edges always land later.
///    (gapbs skips the light/heavy split and re-relaxes — sssp.cc:44
///    says redundant work is cheaper than removing stale entries;
///    do the same, but a vertex whose dist < i·delta on drain is
///    stale — skip it, like Dijkstra's lazy deletion.)
/// 3. Done when no non-empty bucket remains. Increment `buckets` per
///    non-empty bucket index processed.
///
/// Contract: dist identical to `dijkstra`. Stats let the bench show
/// the delta trade: small delta → many buckets (ordering overhead),
/// large delta → Bellman-Ford-ish re-relaxations.
pub fn delta_stepping(_g: &Csr, _source: u32, _delta: u64) -> (Vec<u64>, DeltaStats) {
    todo!("bucketed SSSP: drain bucket, relax, re-bucket, count work")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{from_edges, gen_rmat};

    #[test]
    fn dijkstra_tiny() {
        // 0 -1-> 1 -1-> 2, 0 -5-> 2
        let g = from_edges(3, &[(0, 1, 1), (1, 2, 1), (0, 2, 5)], false);
        let (d, _) = dijkstra(&g, 0);
        assert_eq!(d, vec![0, 1, 2]);
    }

    #[test]
    fn delta_stepping_matches_dijkstra() {
        let (n, e) = gen_rmat(10, 8, 5);
        let g = from_edges(n, &e, true);
        for (src, delta) in [(0u32, 32u64), (17, 1), (123, 4096)] {
            let (d1, _) = dijkstra(&g, src);
            let (d2, stats) = delta_stepping(&g, src, delta);
            assert_eq!(d1, d2, "src {src} delta {delta}");
            assert!(stats.relaxations > 0 && stats.buckets > 0);
        }
    }

    #[test]
    fn delta_extremes_still_correct() {
        // delta=1: near-Dijkstra ordering; delta=INF/2: one bucket =
        // Bellman-Ford. Both must be exact — only the WORK differs.
        let (n, e) = gen_rmat(9, 8, 6);
        let g = from_edges(n, &e, true);
        let (d1, _) = dijkstra(&g, 3);
        let (small, s_small) = delta_stepping(&g, 3, 1);
        let (large, s_large) = delta_stepping(&g, 3, 1 << 40);
        assert_eq!(d1, small);
        assert_eq!(d1, large);
        assert!(
            s_large.buckets < s_small.buckets,
            "one giant bucket vs many fine buckets"
        );
    }
}
