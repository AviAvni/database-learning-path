//! Columnar replica with delta+main storage — YOUR JOB. This is
//! TiFlash's DeltaTree in miniature (Segment.h:84: a delta layer of
//! recent writes over a stable layer of sorted columns), which is also
//! SAP HANA's delta+main and — look closely — FalkorDB's delta-matrix
//! pattern and topic 4's LSM: writes land in an append-friendly
//! structure, reads merge it with a scan-friendly one, compaction folds.
//!
//! Contract fixed by the tests below:
//! - `apply(recs)`: append LogRecs to `delta`, advance `applied_lsn`
//!   (recs arrive in lsn order; applied_lsn = last lsn seen).
//! - `scan_sum_a(b_lo, b_hi)`: sum of `a` over the LATEST version of
//!   every key whose latest `b` is in range. Delta overrides main;
//!   within delta, the highest lsn per key wins. Main has at most one
//!   entry per key.
//! - `merge_delta()`: fold delta into main — one entry per key, main
//!   sorted by key, delta emptied, applied_lsn unchanged, scan results
//!   identical before/after (this is TiFlash's segmentMergeDelta /
//!   HANA's delta merge / your LSM minor compaction).

use crate::row::LogRec;

#[derive(Default)]
pub struct ColumnarReplica {
    pub main_keys: Vec<u64>,
    pub main_a: Vec<i64>,
    pub main_b: Vec<i64>,
    pub delta: Vec<LogRec>,
    pub applied_lsn: u64,
}

impl ColumnarReplica {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, recs: &[LogRec]) {
        let _ = recs;
        todo!("append to delta, advance applied_lsn")
    }

    pub fn scan_sum_a(&self, b_lo: i64, b_hi: i64) -> i64 {
        let _ = (b_lo, b_hi);
        todo!("main + delta, latest version per key, delta overrides main")
    }

    pub fn merge_delta(&mut self) {
        todo!("fold delta into sorted main; delta.clear(); same scan results")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row::RowStore;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    fn workload(n: usize, keys: u64, seed: u64) -> RowStore {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut s = RowStore::new();
        for _ in 0..n {
            s.write(rng.gen_range(0..keys), rng.gen_range(-100..100), rng.gen_range(0..1000));
        }
        s
    }

    #[test]
    fn replica_matches_row_oracle() {
        let s = workload(5_000, 500, 32);
        let mut r = ColumnarReplica::new();
        r.apply(&s.log);
        assert_eq!(r.applied_lsn, s.lsn);
        for (lo, hi) in [(0, 999), (100, 200), (500, 499)] {
            assert_eq!(r.scan_sum_a(lo, hi), s.scan_sum_a(lo, hi));
        }
    }

    #[test]
    fn delta_overrides_main_and_latest_wins() {
        let mut s = RowStore::new();
        s.write(7, 10, 5);
        let mut r = ColumnarReplica::new();
        r.apply(&s.log);
        r.merge_delta(); // key 7 now lives in main
        let before = s.log.len();
        s.write(7, 999, 5); // newer version arrives in delta
        s.write(7, 20, 5); // and an even newer one
        r.apply(&s.log[before..]);
        assert_eq!(r.scan_sum_a(0, 10), 20);
    }

    #[test]
    fn merge_preserves_scans_and_sorts_main() {
        let s = workload(5_000, 500, 33);
        let mut r = ColumnarReplica::new();
        // apply in two chunks with a merge between — mid-stream compaction
        r.apply(&s.log[..2_500]);
        r.merge_delta();
        r.apply(&s.log[2_500..]);
        let before = r.scan_sum_a(0, 999);
        r.merge_delta();
        assert_eq!(r.scan_sum_a(0, 999), before);
        assert_eq!(r.scan_sum_a(0, 999), s.scan_sum_a(0, 999));
        assert!(r.delta.is_empty());
        assert!(r.main_keys.windows(2).all(|w| w[0] < w[1]), "sorted, one per key");
        assert_eq!(r.applied_lsn, s.lsn);
    }

    #[test]
    fn freshness_is_visible() {
        let s = workload(1_000, 100, 34);
        let mut r = ColumnarReplica::new();
        r.apply(&s.log[..600]);
        // The replica KNOWS how stale it is — that lsn gap is the number
        // learner reads wait on.
        assert_eq!(r.applied_lsn, s.log[599].lsn);
        assert!(r.applied_lsn < s.lsn);
    }
}
