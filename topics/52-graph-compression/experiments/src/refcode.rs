//! Lane 3 (EXERCISE, stub): WebGraph-style reference + interval + residual
//! coding, with a decode-and-scan.
//!
//! Implement the BV format of Boldi & Vigna, "The WebGraph Framework I"
//! (WWW'04; see reading-webgraph-framework.md), bit-aligned, per vertex `x`:
//!
//! ```text
//! outdegree d                                       (gamma)
//! if d > 0 and window > 0:
//!   reference r in 0..=window                       (unary; r = 0: none)
//!   if r > 0: block count b, then b copy/skip block lengths (gamma;
//!             the first as is, the rest minus 1, the last block implied)
//! extra nodes = list(x) minus the copied ones
//!   intervals (runs of >= min_interval consecutive extra nodes):
//!     count; per interval the left extreme (the first as zigzag(left - x),
//!     the rest as left - previous_right_end - 2) and length - min_interval
//!   residuals (the other extra nodes):
//!     the first as zigzag(r1 - x), the rest as gap - 1      (zeta_3)
//! ```
//!
//! The exact table is Table 6 of the paper; the layout is yours as long as
//! the contract below holds.
//!
//! Choose `r` per vertex as the best of the previous `window` lists by
//! encoded size (greedy, as the paper does), subject to reference chains no
//! longer than `max_ref` (the paper's R). `decode_scan` must walk the lists
//! without materialising whole adjacency vectors where it can.
//!
//! Contract (the tests below):
//! * `decode_list` returns exactly `g.neighbors(v)` for every vertex;
//! * `decode_scan(vals)` equals `measure::scan_csr`;
//! * with `window = 0` and `min_interval = usize::MAX` the coding is gaps
//!   only; turning on references and intervals never makes a graph whose
//!   list `v` is a copy of list `v-1` bigger, and makes that graph at least
//!   5x smaller than gaps-only;
//! * on `GenParams::SMALL` in planted (community) order, `bits_per_edge` is
//!   below the gamma price of the same graph;
//! * on `GenParams::SMALL_WEB` (lists that are noisy copies of the previous
//!   list) references cut the size of the gaps-only coding by at least 25%.
//!   On `GenParams::SMALL`, where lists are independent draws, they do not —
//!   that is the finding the bench reports.

use crate::csr::Csr;

pub struct Encoded {
    pub n: usize,
    pub m: usize,
    /// Your bit stream and per-vertex bit offsets; layout is yours.
    pub bits: u64,
}

impl Encoded {
    pub fn bits_per_edge(&self) -> f64 {
        self.bits as f64 / self.m as f64
    }

    pub fn decode_list(&self, v: usize) -> Vec<u32> {
        let _ = v;
        todo!("lane 3: decode one list, following its reference chain")
    }

    /// Sum of `vals[target]` over every edge, decoding as it goes.
    pub fn decode_scan(&self, vals: &[u64]) -> u64 {
        let _ = vals;
        todo!("lane 3: sequential decode-and-gather")
    }
}

/// `window` = W (how many previous lists may be a reference), `max_ref` = R,
/// `min_interval` = L_min (use `usize::MAX` to disable intervals).
pub fn encode(g: &Csr, window: usize, max_ref: usize, min_interval: usize) -> Encoded {
    let _ = (g, window, max_ref, min_interval);
    todo!("lane 3: WebGraph BV encoder (Boldi & Vigna, WWW'04, Section 3)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gapcode::gamma_bits;
    use crate::gen::{community_graph, GenParams};
    use crate::measure::scan_csr;

    #[test]
    fn round_trips_every_list() {
        let p = community_graph(GenParams::SMALL);
        let e = encode(&p.g, 7, 3, 4);
        for v in 0..p.g.n {
            assert_eq!(e.decode_list(v), p.g.neighbors(v), "vertex {v}");
        }
    }

    #[test]
    fn scan_matches_csr() {
        let p = community_graph(GenParams::SMALL);
        let vals: Vec<u64> = (0..p.g.n as u64).map(|x| x * 7919 % 10007).collect();
        let e = encode(&p.g, 7, 3, 4);
        assert_eq!(e.decode_scan(&vals), scan_csr(&p.g, &vals));
    }

    #[test]
    fn copies_collapse() {
        // 2,000 vertices; every list is the same 40 targets spread out
        let mut edges = Vec::new();
        for v in 0..2000u32 {
            for k in 0..40u32 {
                edges.push((v, 2000 + k * 37));
            }
        }
        let g = Csr::from_edges(4000, edges);
        let plain = encode(&g, 0, 0, usize::MAX).bits;
        let refd = encode(&g, 7, 3, 4).bits;
        assert!(refd * 5 <= plain, "plain {plain} vs referenced {refd}");
    }

    #[test]
    fn beats_gamma_gaps_on_planted_order() {
        let p = community_graph(GenParams::SMALL);
        let e = encode(&p.g, 7, 3, 4);
        assert!(e.bits < gamma_bits(&p.g), "{} vs {}", e.bits, gamma_bits(&p.g));
    }

    #[test]
    fn similar_lists_reward_references() {
        let p = community_graph(GenParams::SMALL_WEB);
        let plain = encode(&p.g, 0, 0, usize::MAX).bits;
        let refd = encode(&p.g, 7, 3, 4).bits;
        assert!(refd as f64 <= 0.75 * plain as f64, "plain {plain} vs referenced {refd}");
    }
}
