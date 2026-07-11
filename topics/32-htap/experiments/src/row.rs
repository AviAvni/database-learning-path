//! Row store + changelog — PROVIDED. The OLTP side of the split, and
//! the oracle for the columnar replica. Every write appends a LogRec:
//! the log IS the replication stream (topic 27's thesis, reused), the
//! same role TiKV's Raft log plays for TiFlash.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogRec {
    pub lsn: u64,
    pub key: u64,
    pub a: i64,
    pub b: i64,
}

#[derive(Default)]
pub struct RowStore {
    pub rows: HashMap<u64, (i64, i64)>,
    pub log: Vec<LogRec>,
    pub lsn: u64,
}

impl RowStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Upsert (key -> (a, b)); returns the record's LSN.
    pub fn write(&mut self, key: u64, a: i64, b: i64) -> u64 {
        self.lsn += 1;
        self.rows.insert(key, (a, b));
        self.log.push(LogRec { lsn: self.lsn, key, a, b });
        self.lsn
    }

    /// The analytical query of this topic: sum(a) where b in [b_lo, b_hi].
    /// Row-at-a-time — the oracle the columnar replica must match and
    /// the baseline it must beat.
    pub fn scan_sum_a(&self, b_lo: i64, b_hi: i64) -> i64 {
        self.rows
            .values()
            .filter(|(_, b)| (b_lo..=b_hi).contains(b))
            .map(|(a, _)| a)
            .sum()
    }
}

/// Skewed key pick in [0, n): quadratic skew (u² keeps hot keys hot
/// without YCSB's zeta machinery — good enough for interference lanes).
pub fn skewed_key<R: rand::Rng>(rng: &mut R, n: u64) -> u64 {
    let u: f64 = rng.gen();
    ((u * u) * n as f64) as u64
}

/// Nearest-rank percentile over unsorted ns samples.
pub fn percentile(samples: &mut [u64], p: f64) -> u64 {
    assert!(!samples.is_empty());
    samples.sort_unstable();
    let rank = ((p / 100.0) * samples.len() as f64).ceil() as usize;
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_is_the_write_history() {
        let mut s = RowStore::new();
        let l1 = s.write(1, 10, 5);
        let l2 = s.write(1, 20, 5); // overwrite: log keeps both
        assert!(l2 > l1);
        assert_eq!(s.log.len(), 2);
        assert_eq!(s.rows[&1], (20, 5));
    }

    #[test]
    fn scan_oracle() {
        let mut s = RowStore::new();
        s.write(1, 10, 0);
        s.write(2, 100, 50);
        s.write(3, 1000, 99);
        s.write(2, 200, 50); // latest wins
        assert_eq!(s.scan_sum_a(0, 98), 10 + 200);
    }
}
