//! YOU implement: scalar u8 quantization + the oversample-and-rescore
//! pipeline (qdrant's encoded_vectors_u8.rs, simplified to one global
//! affine range).
//!
//! Contract:
//! - `encode`: alpha = (max - min) / 255, offset = min, over ALL
//!   values in the dataset; code = round((v - offset) / alpha),
//!   clamped to 0..=255. alpha == 0 (constant data) must not divide
//!   by zero (width-0 bitpacking déjà vu, topic 12)
//! - `decode_dim`: offset + alpha * code — within alpha/2 of the
//!   original (rounding, not truncation)
//! - `dist_l2_sq`: symmetric quantized distance — alpha² · Σ
//!   (q_code - v_code)², integer subtraction in i32 before widening
//! - `search_rescore`: scan ALL codes for top k·oversample by
//!   quantized distance, then rescore those with f32 `l2_sq`, return
//!   top k nearest-first (late materialization, vector edition)

use crate::data::Dataset;

pub struct ScalarQuant {
    pub dim: usize,
    pub alpha: f32,
    pub offset: f32,
    pub codes: Vec<u8>, // n * dim, row-major
}

impl ScalarQuant {
    pub fn encode(data: &Dataset) -> ScalarQuant {
        let _ = data;
        todo!()
    }

    pub fn code(&self, i: u32) -> &[u8] {
        &self.codes[i as usize * self.dim..(i as usize + 1) * self.dim]
    }

    pub fn decode_dim(&self, i: u32, d: usize) -> f32 {
        let _ = (i, d);
        todo!()
    }

    pub fn encode_query(&self, query: &[f32]) -> Vec<u8> {
        let _ = query;
        todo!()
    }

    pub fn dist_l2_sq(&self, i: u32, query_code: &[u8]) -> f32 {
        let _ = (i, query_code);
        todo!()
    }
}

/// Quantized scan + f32 rescore of the top k*oversample shortlist.
pub fn search_rescore(
    data: &Dataset,
    quant: &ScalarQuant,
    query: &[f32],
    k: usize,
    oversample: usize,
) -> Vec<u32> {
    let _ = (data, quant, query, k, oversample);
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{brute, data, recall};

    #[test]
    fn round_trip_error_bounded_by_half_alpha() {
        let d = data::clustered(1_000, 16, 5, 7);
        let q = ScalarQuant::encode(&d);
        for i in (0..d.len()).step_by(53) {
            for j in 0..d.dim {
                let err = (q.decode_dim(i, j) - d.get(i)[j]).abs();
                assert!(err <= q.alpha / 2.0 + 1e-6, "err {err} > alpha/2 {}", q.alpha / 2.0);
            }
        }
    }

    #[test]
    fn constant_data_does_not_explode() {
        let d = Dataset { dim: 4, vectors: vec![3.5; 40] };
        let q = ScalarQuant::encode(&d);
        assert!((q.decode_dim(0, 0) - 3.5).abs() < 1e-6);
        assert_eq!(q.dist_l2_sq(0, &q.encode_query(&[3.5; 4])), 0.0);
    }

    #[test]
    fn quantized_distance_tracks_true_distance() {
        let d = data::clustered(500, 32, 5, 8);
        let q = ScalarQuant::encode(&d);
        let query = d.get(0).to_vec();
        let qc = q.encode_query(&query);
        // nearest by quantized distance must be node 0 itself
        let best = (0..d.len()).min_by(|&a, &b| {
            q.dist_l2_sq(a, &qc).partial_cmp(&q.dist_l2_sq(b, &qc)).unwrap()
        });
        assert_eq!(best, Some(0));
    }

    #[test]
    fn rescored_recall_beats_095() {
        let d = data::clustered(5_000, 32, 20, 9);
        let queries = data::queries(&d, 50, 9);
        let q = ScalarQuant::encode(&d);
        let mut total = 0.0;
        for qi in 0..queries.len() {
            let truth = brute::top_k(&d, queries.get(qi), 10);
            let found = search_rescore(&d, &q, queries.get(qi), 10, 4);
            total += recall(&found, &truth);
        }
        let avg = total / queries.len() as f64;
        assert!(avg >= 0.95, "rescored recall@10 = {avg:.3}");
    }

    #[test]
    fn four_x_compression_exactly() {
        let d = data::clustered(100, 16, 2, 10);
        let q = ScalarQuant::encode(&d);
        assert_eq!(q.codes.len(), d.vectors.len());
    }
}
