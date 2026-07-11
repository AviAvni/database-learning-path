pub mod data;
pub mod volcano;
pub mod vectorized;
pub mod kernels;

/// The one query, three ways:
///   SELECT k, SUM(v) FROM t WHERE f < threshold GROUP BY k
/// k is dense in 0..NUM_GROUPS, so the "hash table" can be a flat array.
pub const NUM_GROUPS: usize = 64;

/// Scalar oracle — the trivially-correct answer all engines must match.
pub fn oracle(t: &data::Table, threshold: u32) -> Vec<i64> {
    let mut sums = vec![0i64; NUM_GROUPS];
    for i in 0..t.len() {
        if t.f[i] < threshold {
            sums[t.k[i] as usize] += t.v[i] as i64;
        }
    }
    sums
}
