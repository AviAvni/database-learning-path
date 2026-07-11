//! CPU reference kernels — topic 17's shapes, the bar the GPU must
//! clear INCLUDING its transfer time.

/// 8-accumulator sum (autovectorizes; topic 17 measured ~42 GB/s
/// for the dot variant).
pub fn sum(vals: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let chunks = vals.len() / 8;
    for c in 0..chunks {
        for l in 0..8 {
            acc[l] += vals[c * 8 + l];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for &v in &vals[chunks * 8..] {
        s += v;
    }
    s
}

/// Branchless filter count (topic 17: ~12.7 GB/s flat).
pub fn filter_count(vals: &[f32], t: f32) -> u32 {
    vals.iter().map(|&v| (v < t) as u32).sum()
}

/// One query vs M target vectors, squared L2. dim divides evenly.
pub fn l2_batch(query: &[f32], targets: &[f32], dim: usize) -> Vec<f32> {
    targets
        .chunks_exact(dim)
        .map(|t| {
            let mut s = 0.0f32;
            for i in 0..dim {
                let d = query[i] - t[i];
                s += d * d;
            }
            s
        })
        .collect()
}
