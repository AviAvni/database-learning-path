//! Provided: exact top-k by linear scan. The recall referee and the
//! QPS floor every index must beat.

use crate::data::Dataset;
use crate::distance::dist::l2_sq;

/// ids of the k nearest vectors to `query`, nearest first.
pub fn top_k(data: &Dataset, query: &[f32], k: usize) -> Vec<u32> {
    // max-heap of (dist, id) capped at k — O(n log k)
    let mut heap: std::collections::BinaryHeap<(ordered, u32)> =
        std::collections::BinaryHeap::with_capacity(k + 1);
    for i in 0..data.len() {
        let d = l2_sq(data.get(i), query);
        heap.push((ordered(d), i));
        if heap.len() > k {
            heap.pop();
        }
    }
    let mut out: Vec<(ordered, u32)> = heap.into_vec();
    out.sort_unstable();
    out.into_iter().map(|(_, i)| i).collect()
}

/// f32 wrapper ordered by value (no NaNs in our data).
#[derive(PartialEq, PartialOrd, Clone, Copy)]
#[allow(non_camel_case_types)]
pub struct ordered(pub f32);

impl Eq for ordered {}
impl Ord for ordered {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap()
    }
}
