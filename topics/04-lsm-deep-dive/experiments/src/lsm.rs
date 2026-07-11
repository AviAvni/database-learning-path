//! The LSM engine — YOUR implementation.
//!
//! put/delete/get + flush + pluggable compaction. Instrumentation is the
//! point: Stats must count every byte written and every segment probed, or
//! the write-amp experiment is fiction.
//!
//! Leveled (ratio 10): level full ⇒ merge ENTIRE level into the next
//! (whole-level granularity — the design-space paper explains what that
//! costs in tail latency; you may measure it).
//! Tiered (K=4): K runs of similar size at a level ⇒ merge them into one run
//! at the next level. L0 in both: every flush is one overlapping run.

use crate::memtable::Memtable;
use crate::sst::{SstReader, SstWriter};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq)]
pub enum CompactionStrategy {
    Leveled { ratio: usize },
    Tiered { k: usize },
}

#[derive(Default, Debug)]
pub struct Stats {
    pub user_bytes_written: u64,
    pub disk_bytes_written: u64, // flushes + compaction outputs
    pub segments_probed: u64,
    pub bloom_negative: u64, // probes skipped thanks to bloom
    pub gets: u64,
}

impl Stats {
    pub fn write_amp(&self) -> f64 {
        self.disk_bytes_written as f64 / self.user_bytes_written.max(1) as f64
    }
    pub fn read_amp(&self) -> f64 {
        self.segments_probed as f64 / self.gets.max(1) as f64
    }
}

pub struct Lsm {
    pub stats: Stats,
    memtable: Memtable,
    levels: Vec<Vec<SstReader>>, // levels[0] = L0 runs, newest LAST
    dir: PathBuf,
    strategy: CompactionStrategy,
    next_id: u64,
}

impl Lsm {
    pub fn create(dir: PathBuf, strategy: CompactionStrategy) -> std::io::Result<Self> {
        let _ = (dir, strategy);
        todo!()
    }

    pub fn put(&mut self, key: &[u8], value: &[u8]) -> std::io::Result<()> {
        let _ = (key, value);
        todo!("memtable.put; if full: flush + maybe_compact")
    }

    pub fn delete(&mut self, key: &[u8]) -> std::io::Result<()> {
        let _ = key;
        todo!("tombstone into memtable")
    }

    pub fn get(&mut self, key: &[u8]) -> std::io::Result<Option<Vec<u8>>> {
        let _ = key;
        todo!("memtable → L0 newest-first → deeper levels; L1+ disjoint ⇒ pick by key range; count stats")
    }

    fn flush(&mut self) -> std::io::Result<()> {
        todo!("write memtable to a new L0 SST via SstWriter; add finish() bytes to stats")
    }

    fn maybe_compact(&mut self) -> std::io::Result<()> {
        todo!("per strategy: pick level, k-way merge runs (drop shadowed versions; drop tombstones ONLY into last level), replace inputs with output")
    }

    pub fn describe(&self) -> String {
        todo!("one line per level: run count + total bytes — print during experiments")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(s: CompactionStrategy) -> (tempfile::TempDir, Lsm) {
        let dir = tempfile::tempdir().unwrap();
        let lsm = Lsm::create(dir.path().to_path_buf(), s).unwrap();
        (dir, lsm)
    }

    #[test]
    fn put_get_across_flushes() {
        let (_d, mut lsm) = engine(CompactionStrategy::Leveled { ratio: 10 });
        let big = vec![7u8; 512]; // force several flushes
        for i in 0..20_000u64 {
            lsm.put(&i.to_be_bytes(), &big).unwrap();
        }
        for i in (0..20_000u64).step_by(97) {
            assert_eq!(lsm.get(&i.to_be_bytes()).unwrap(), Some(big.clone()), "key {i}");
        }
    }

    #[test]
    fn overwrite_returns_newest() {
        let (_d, mut lsm) = engine(CompactionStrategy::Leveled { ratio: 10 });
        let v1 = vec![1u8; 600];
        let v2 = vec![2u8; 600];
        for i in 0..5_000u64 {
            lsm.put(&i.to_be_bytes(), &v1).unwrap();
        }
        for i in 0..5_000u64 {
            lsm.put(&i.to_be_bytes(), &v2).unwrap();
        }
        assert_eq!(lsm.get(&42u64.to_be_bytes()).unwrap(), Some(v2));
    }

    #[test]
    fn delete_stays_deleted_across_compaction() {
        let (_d, mut lsm) = engine(CompactionStrategy::Tiered { k: 4 });
        let v = vec![3u8; 600];
        for i in 0..5_000u64 {
            lsm.put(&i.to_be_bytes(), &v).unwrap();
        }
        lsm.delete(&100u64.to_be_bytes()).unwrap();
        for i in 5_000..10_000u64 {
            lsm.put(&i.to_be_bytes(), &v).unwrap(); // push compactions
        }
        assert_eq!(lsm.get(&100u64.to_be_bytes()).unwrap(), None);
    }

    #[test]
    fn write_amp_is_tracked() {
        let (_d, mut lsm) = engine(CompactionStrategy::Leveled { ratio: 10 });
        let v = vec![9u8; 512];
        for i in 0..30_000u64 {
            lsm.put(&i.to_be_bytes(), &v).unwrap();
        }
        assert!(lsm.stats.user_bytes_written > 0);
        assert!(
            lsm.stats.disk_bytes_written > lsm.stats.user_bytes_written,
            "compaction must rewrite data: WA > 1"
        );
    }
}
