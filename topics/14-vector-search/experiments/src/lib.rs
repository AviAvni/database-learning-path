pub mod brute;
pub mod data;
pub mod distance;
pub mod hnsw;
pub mod quant;

/// recall@k: fraction of true neighbors found.
pub fn recall(found: &[u32], truth: &[u32]) -> f64 {
    let hits = found.iter().filter(|id| truth.contains(id)).count();
    hits as f64 / truth.len() as f64
}
