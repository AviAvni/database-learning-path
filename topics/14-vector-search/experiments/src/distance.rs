//! Provided: distance kernels (scalar — M17 SIMD-izes these) and the
//! exact top-k oracle.

pub mod dist {
    /// Squared L2 — order-preserving for L2, skip the sqrt.
    pub fn l2_sq(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
    }

    pub fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }
}
