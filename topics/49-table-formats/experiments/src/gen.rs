//! The seeded workload: an append-only table fed by many small commits.
//!
//! Each commit adds `files_per_commit` Parquet data files to a table
//! partitioned by day, `commits_per_day` commits to a day — a streaming
//! ingest job that commits every few minutes. The data files themselves are
//! never written (their bytes are not the subject); what every commit
//! produces is the *metadata entry* describing each file: path, partition,
//! record count, size, and per-column min/max/null statistics for the three
//! columns `id: long`, `ts: timestamp`, `v: double`. That is the payload
//! both Iceberg's manifests and Delta's `add` actions carry.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub const SEED: u64 = 49;

#[derive(Clone, Debug, PartialEq)]
pub struct DataFile {
    pub path: String,
    /// partition value: days since 2026-01-01
    pub day: u32,
    pub records: u64,
    pub size: u64,
    pub min_id: u64,
    pub max_id: u64,
    pub min_ts: i64,
    pub max_ts: i64,
    pub min_v: f64,
    pub max_v: f64,
    pub null_v: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Workload {
    pub commits: usize,
    pub files_per_commit: usize,
    pub commits_per_day: usize,
}

/// 2026-01-01T00:00:00Z in microseconds (Iceberg/Delta timestamps).
const EPOCH_US: i64 = 1_767_225_600_000_000;
const DAY_US: i64 = 86_400_000_000;

fn hex32(rng: &mut ChaCha8Rng) -> String {
    let a: u64 = rng.gen();
    let b: u64 = rng.gen();
    format!("{a:016x}{b:016x}")
}

/// `w.commits` commits, each a `Vec` of new data files, fully determined by
/// `seed`. Ids are dense and increasing across the whole stream (append-only),
/// timestamps increase within a day.
pub fn commits(seed: u64, w: Workload) -> Vec<Vec<DataFile>> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut next_id = 0u64;
    let per_commit_us = DAY_US / w.commits_per_day as i64;
    let mut out = Vec::with_capacity(w.commits);
    for c in 0..w.commits {
        let day = (c / w.commits_per_day) as u32;
        let slot = (c % w.commits_per_day) as i64;
        let t0 = EPOCH_US + day as i64 * DAY_US + slot * per_commit_us;
        let mut files = Vec::with_capacity(w.files_per_commit);
        for f in 0..w.files_per_commit {
            let records = rng.gen_range(2_000..20_000u64);
            let min_id = next_id;
            next_id += records;
            let min_ts = t0 + rng.gen_range(0..per_commit_us / 4);
            let max_ts = min_ts + rng.gen_range(per_commit_us / 4..per_commit_us / 2);
            let lo: f64 = rng.gen_range(-1000.0..0.0);
            let hi: f64 = rng.gen_range(0.0..1000.0);
            let null_v = rng.gen_range(0..records / 100);
            files.push(DataFile {
                path: format!(
                    "data/day={day:04}/part-{c:06}-{f:02}-{}.parquet",
                    hex32(&mut rng)
                ),
                day,
                records,
                // ~22 bytes per row for (long, timestamp, double) after encoding
                size: records * 22 + rng.gen_range(4_000..8_000),
                min_id,
                max_id: next_id - 1,
                min_ts,
                max_ts,
                min_v: (lo * 1000.0).round() / 1000.0,
                max_v: (hi * 1000.0).round() / 1000.0,
                null_v,
            });
        }
        out.push(files);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_and_append_only() {
        let w = Workload { commits: 50, files_per_commit: 2, commits_per_day: 10 };
        let a = commits(SEED, w);
        let b = commits(SEED, w);
        assert_eq!(a, b);
        let flat: Vec<_> = a.iter().flatten().collect();
        for p in flat.windows(2) {
            assert_eq!(p[1].min_id, p[0].max_id + 1);
        }
        assert_eq!(flat.last().unwrap().day, 4);
    }
}
