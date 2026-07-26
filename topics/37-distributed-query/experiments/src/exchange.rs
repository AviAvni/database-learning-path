//! STUB — the exchange operator's routing core (Volcano §4, DataFusion's
//! `BatchPartitioner` + `preserve_order` merge in miniature).
//!
//! Exchange splits one stream into k streams (partitioning) and, for
//! parallel sort, fuses k sorted streams back into one (merging — which
//! must keep producers' records separate, Volcano §4.4's lesson). The
//! routing policies are the same trio topic 36 applied to stored data,
//! now applied to intermediate results:
//!   - round-robin: perfect balance, no locality;
//!   - hash: same key → same output, always — joins and aggregations
//!     depend on it (DataFusion pins the seed: REPARTITION_RANDOM_STATE).
//!
//! Contracts (the tests): hash routing is deterministic and complete
//! (every row to exactly one output, same key → same output on every
//! call); round-robin balances to within one row; merging k sorted runs
//! yields one sorted run with the same multiset.

pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

pub enum Partitioning {
    RoundRobin,
    Hash,
}

pub struct Exchange {
    pub k: usize,
    pub policy: Partitioning,
    /// Round-robin cursor; persists across `partition` calls.
    pub next_output: usize,
}

impl Exchange {
    pub fn new(k: usize, policy: Partitioning) -> Self {
        Exchange {
            k,
            policy,
            next_output: 0,
        }
    }

    /// Route every row to exactly one of k outputs.
    /// Hash: output = splitmix64(row) % k. Round-robin: rows go to
    /// outputs in rotation, cursor carried across calls.
    pub fn partition(&mut self, input: &[u64]) -> Vec<Vec<u64>> {
        let _ = input;
        todo!("route each row by policy; k output vecs")
    }
}

/// Fuse k individually-sorted runs into one sorted run (the merging
/// exchange). O(total log k) — repeatedly take the smallest head.
pub fn merge_sorted(runs: &[Vec<u64>]) -> Vec<u64> {
    let _ = runs;
    todo!("k-way merge: heap or linear scan over run heads")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_routing_is_deterministic_and_complete() {
        let rows: Vec<u64> = (0..10_000).collect();
        let mut ex = Exchange::new(8, Partitioning::Hash);
        let out1 = ex.partition(&rows);
        let out2 = ex.partition(&rows);
        assert_eq!(out1, out2, "same input must route identically");
        let mut all: Vec<u64> = out1.into_iter().flatten().collect();
        all.sort_unstable();
        assert_eq!(all, rows, "every row exactly once");
    }

    #[test]
    fn round_robin_balances_to_within_one_row() {
        let rows: Vec<u64> = (0..10_001).collect();
        let mut ex = Exchange::new(8, Partitioning::RoundRobin);
        let out = ex.partition(&rows);
        let sizes: Vec<usize> = out.iter().map(Vec::len).collect();
        let (min, max) = (sizes.iter().min().unwrap(), sizes.iter().max().unwrap());
        assert!(max - min <= 1, "sizes {sizes:?}");
        let mut all: Vec<u64> = out.into_iter().flatten().collect();
        all.sort_unstable();
        assert_eq!(all, rows);
    }

    #[test]
    fn merging_exchange_preserves_sorted_order() {
        // 8 sorted runs with interleaved values.
        let runs: Vec<Vec<u64>> = (0..8u64)
            .map(|r| (0..1_000u64).map(|i| i * 8 + r).collect())
            .collect();
        let merged = merge_sorted(&runs);
        let expect: Vec<u64> = (0..8_000).collect();
        assert_eq!(merged, expect, "merge must be globally sorted, same multiset");
    }
}
