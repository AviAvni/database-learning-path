//! YOU implement: HNSW from the paper (arXiv:1603.09320), Algorithms
//! 1-4. Reference implementations: usearch (reading-usearch.md) and
//! qdrant's graph_layers_builder.rs.
//!
//! Contract (tests pin it down):
//! - level draw: `⌊-ln(U) · 1/ln(m)⌋`, seeded RNG (StdRng from
//!   config.seed) — max level stays O(log n / log m)
//! - insert (Alg 1): greedy descent to level+1, then per-level
//!   beam search (ef_construction) + SELECT-NEIGHBORS-HEURISTIC
//!   (Alg 4: keep candidate c only if closer to the new point than
//!   to every already-kept neighbor), back-link + shrink overfull
//!   neighbors (m per upper level, m0 at level 0)
//! - search (Alg 2): descent with ef=1, then layer-0 beam of size
//!   max(ef, k); results nearest-first
//! - reuse a visited set across the query (stamp or clear — your
//!   call; hop_bench taught the trade)

use crate::data::Dataset;

pub struct HnswConfig {
    pub m: usize,
    pub m0: usize,
    pub ef_construction: usize,
    pub seed: u64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        // usearch's defaults (index.hpp:1563-1573)
        HnswConfig { m: 16, m0: 32, ef_construction: 128, seed: 42 }
    }
}

pub struct Hnsw {
    pub config: HnswConfig,
    pub entry_point: u32,
    pub max_level: usize,
    /// links[level][node] — empty vec for nodes absent from a level
    pub links: Vec<Vec<Vec<u32>>>,
}

impl Hnsw {
    pub fn build(data: &Dataset, config: HnswConfig) -> Hnsw {
        let _ = (data, config);
        todo!()
    }

    /// ids of ~k nearest, nearest first. `ef` is the beam width
    /// (clamped to ≥ k).
    pub fn search(&self, data: &Dataset, query: &[f32], k: usize, ef: usize) -> Vec<u32> {
        let _ = (data, query, k, ef);
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distance::dist::l2_sq;
    use crate::{brute, data, recall};

    #[test]
    fn indexed_vector_is_its_own_nearest() {
        let d = data::clustered(2_000, 16, 10, 1);
        let h = Hnsw::build(&d, HnswConfig::default());
        for i in (0..2_000).step_by(97) {
            let r = h.search(&d, d.get(i), 1, 64);
            assert_eq!(r[0], i, "self-query for {i}");
        }
    }

    #[test]
    fn recall_at_10_beats_090() {
        let d = data::clustered(5_000, 32, 20, 2);
        let q = data::queries(&d, 100, 2);
        let h = Hnsw::build(&d, HnswConfig::default());
        let mut total = 0.0;
        for qi in 0..q.len() {
            let truth = brute::top_k(&d, q.get(qi), 10);
            let found = h.search(&d, q.get(qi), 10, 128);
            total += recall(&found, &truth);
        }
        let avg = total / q.len() as f64;
        assert!(avg >= 0.90, "recall@10 = {avg:.3}, expected >= 0.90");
    }

    #[test]
    fn results_are_sorted_by_distance() {
        let d = data::clustered(1_000, 16, 5, 3);
        let q = data::queries(&d, 5, 3);
        let h = Hnsw::build(&d, HnswConfig::default());
        for qi in 0..q.len() {
            let r = h.search(&d, q.get(qi), 10, 64);
            let dists: Vec<f32> = r.iter().map(|&i| l2_sq(d.get(i), q.get(qi))).collect();
            assert!(dists.windows(2).all(|w| w[0] <= w[1]), "unsorted: {dists:?}");
            assert_eq!(r.len(), 10);
        }
    }

    #[test]
    fn level_distribution_stays_logarithmic() {
        let d = data::clustered(5_000, 8, 5, 4);
        let h = Hnsw::build(&d, HnswConfig::default());
        // E[max level] ≈ ln(5000)/ln(16) ≈ 3
        assert!(h.max_level <= 8, "max_level = {}", h.max_level);
    }

    #[test]
    fn tiny_index_and_k_bigger_than_n() {
        let d = data::clustered(3, 8, 1, 5);
        let h = Hnsw::build(&d, HnswConfig::default());
        let r = h.search(&d, d.get(0), 10, 64);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], 0);
    }
}
