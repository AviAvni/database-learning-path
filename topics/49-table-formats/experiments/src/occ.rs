//! LANE 2 (exercise) — optimistic concurrent commits on a conditional put.
//!
//! Every open table format commits the same way: read a snapshot, do the
//! work, then try to install the next version with one conditional write,
//! and if somebody else got there first, decide whether your work still
//! applies (Iceberg spec, "Optimistic Concurrency" and "Commit Conflict
//! Resolution and Retry"; Delta VLDB'20 §3.2.2 and §3.4). Lane 1 measured a
//! single writer. This lane is several writers racing on one Delta-shaped
//! log, where the conditional write is [`Store::put_if_absent`] on
//! `_delta_log/<N+1>.json`.
//!
//! Three kinds of writer, all seeded:
//!
//! - **appender** — a blind append of one new data file per operation;
//! - **compactor** — reads the snapshot, picks the [`COMPACT_INPUTS`] oldest
//!   live *small* files (smallest path, never an earlier compaction's
//!   output), and commits `remove` × 4 + `add` × 1 (one
//!   merged file); Iceberg's `replace` operation;
//! - **deleter** — reads the snapshot, picks one live file at random, and
//!   commits a `remove` for it (a file-granularity delete).
//!
//! A seeded scheduler interleaves them one *step* at a time; a writer's
//! steps are
//!
//!  1. **begin**: reconstruct the latest snapshot with [`Delta::plan`] and
//!     prepare the operation against it (base version = its version);
//!  2. **try commit**: `put_if_absent(commit_key(base + 1), actions)`.
//!     - success: the operation is committed; the writer goes idle.
//!     - `Exists`: a *conflict*. LIST the log after `base`, GET every commit
//!       that won, and validate (spec rules): an append always rebases; a
//!       compaction or delete rebases only if none of the files it removes
//!       was removed by a winner — otherwise it **aborts** (the operation is
//!       dropped and counted). After a rebase, `base` is the newest winner
//!       and the writer tries again at its next scheduled step.
//!
//! Implement [`run_occ`]. The provided [`audit_log`] replays the finished
//! log and fails on the two corruptions a broken validator produces — a
//! `remove` of a file that is not live (a lost update: two writers both
//! removed it) and an `add` of a file that already is. The tests are the
//! specification; reference numbers are in notes.md.

use crate::delta::{add_action, commit_key, Delta};
use crate::gen::{commits, DataFile, Workload};
use crate::store::{Meter, Store};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Files a compactor merges per operation.
pub const COMPACT_INPUTS: usize = 4;
/// Live files in the table before the writers start.
pub const INITIAL_FILES: usize = 40;

#[derive(Clone, Copy, Debug)]
pub struct OccConfig {
    pub appenders: usize,
    pub compactors: usize,
    pub deleters: usize,
    /// operations each writer attempts
    pub ops_per_writer: usize,
    pub seed: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OccReport {
    /// operations that committed (one log version each)
    pub committed: u64,
    /// conditional puts that lost a race
    pub conflicts: u64,
    /// operations dropped by validation
    pub aborted: u64,
    /// operations that found nothing to do at begin (fewer than
    /// [`COMPACT_INPUTS`] small files, or an empty table)
    pub skipped: u64,
    /// commit files fetched to validate after a conflict
    pub validation_reads: u64,
}

/// Version 0: protocol, metaData and [`INITIAL_FILES`] seeded files.
pub fn create_table(store: &mut Store, seed: u64) -> Vec<DataFile> {
    let w = Workload { commits: 1, files_per_commit: INITIAL_FILES, commits_per_day: 1 };
    let files = commits(seed, w).remove(0);
    let mut d = Delta::new(0);
    d.append(store, &files);
    files
}

/// A merged output file for a compaction (deterministic name per writer/op).
pub fn compacted_file(writer: usize, op: usize, inputs: &[DataFile]) -> DataFile {
    DataFile {
        path: format!("data/compacted-w{writer:02}-op{op:03}.parquet"),
        day: inputs.iter().map(|f| f.day).min().unwrap_or(0),
        records: inputs.iter().map(|f| f.records).sum(),
        size: inputs.iter().map(|f| f.size).sum(),
        min_id: inputs.iter().map(|f| f.min_id).min().unwrap_or(0),
        max_id: inputs.iter().map(|f| f.max_id).max().unwrap_or(0),
        min_ts: inputs.iter().map(|f| f.min_ts).min().unwrap_or(0),
        max_ts: inputs.iter().map(|f| f.max_ts).max().unwrap_or(0),
        min_v: inputs.iter().map(|f| f.min_v).fold(f64::INFINITY, f64::min),
        max_v: inputs.iter().map(|f| f.max_v).fold(f64::NEG_INFINITY, f64::max),
        null_v: inputs.iter().map(|f| f.null_v).sum(),
    }
}

pub fn remove_action(path: &str, ts_ms: i64) -> Value {
    json!({"remove": {"path": path, "deletionTimestamp": ts_ms, "dataChange": true}})
}

/// Run the writers to completion. Returns the report and the store holding
/// the finished log (for [`audit_log`]).
///
/// Determinism: one `ChaCha8Rng` seeded from `cfg.seed` picks which active
/// writer steps next (index into a `Vec` of writers, never a hash map);
/// writers are numbered appenders first, then compactors, then deleters.
/// Each appender's files come from [`commits`] with its own seed and a name
/// unique to it; each compactor names its output with [`compacted_file`].
/// An operation that finds nothing to do at begin is `skipped`, not aborted.
#[allow(unused_variables)] // stub: the arguments are the specification
pub fn run_occ(cfg: OccConfig) -> (OccReport, Store) {
    let _helpers: (fn(&DataFile, i64) -> Value, fn(u64) -> String, fn(&mut Store, Option<u64>, Option<u32>) -> crate::iceberg::Plan) =
        (add_action, commit_key, Delta::plan);
    todo!("optimistic commits with conflict validation (see module docs)")
}

/// What [`audit_log`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Audit {
    pub versions: u64,
    pub adds: u64,
    pub removes: u64,
    pub live: Vec<String>,
}

/// Replay every commit in version order and check the log is a valid
/// history: versions contiguous from 0, no `remove` of a non-live file, no
/// `add` of a live one.
pub fn audit_log(store: &mut Store) -> Result<Audit, String> {
    let mut live: BTreeSet<String> = BTreeSet::new();
    let mut a = Audit::default();
    loop {
        let Some((_, body)) = store.get(&commit_key(a.versions)) else { break };
        for line in body.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let act: Value = serde_json::from_slice(line).unwrap();
            if let Some(add) = act.get("add") {
                let p = add["path"].as_str().unwrap().to_string();
                if !live.insert(p.clone()) {
                    return Err(format!("v{}: add of already-live {p}", a.versions));
                }
                a.adds += 1;
            } else if let Some(rm) = act.get("remove") {
                let p = rm["path"].as_str().unwrap();
                if !live.remove(p) {
                    return Err(format!("v{}: remove of non-live {p} (lost update)", a.versions));
                }
                a.removes += 1;
            }
        }
        a.versions += 1;
    }
    if store.object_count("delta/_delta_log/") as u64 != a.versions {
        return Err("a commit exists past a gap in the version sequence".into());
    }
    a.live = live.into_iter().collect();
    Ok(a)
}

/// Used by the bench to print a meter line.
pub fn meter_of(store: &Store) -> Meter {
    store.meter
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(a: usize, c: usize, d: usize) -> OccConfig {
        OccConfig { appenders: a, compactors: c, deleters: d, ops_per_writer: 12, seed: 49 }
    }

    #[test]
    fn single_appender_never_conflicts() {
        let (r, mut s) = run_occ(cfg(1, 0, 0));
        assert_eq!(r, OccReport { committed: 12, conflicts: 0, aborted: 0, skipped: 0, validation_reads: 0 });
        let a = audit_log(&mut s).unwrap();
        assert_eq!(a.versions, 13);
        assert_eq!(a.live.len(), INITIAL_FILES + 12);
    }

    #[test]
    fn concurrent_appends_conflict_but_never_abort() {
        let (r, mut s) = run_occ(cfg(8, 0, 0));
        assert_eq!(r.committed, 8 * 12, "every blind append must eventually commit");
        assert_eq!(r.aborted, 0, "appends have no preconditions (spec: 'can always be applied')");
        assert!(r.conflicts > 0, "8 writers racing for the same next version must collide");
        let a = audit_log(&mut s).unwrap();
        assert_eq!(a.versions, 1 + 8 * 12);
        assert_eq!(a.live.len(), INITIAL_FILES + 8 * 12);
    }

    #[test]
    fn competing_compactors_abort_instead_of_double_removing() {
        // two compactors both want the same 4 oldest files
        let (r, mut s) = run_occ(cfg(0, 2, 0));
        assert!(r.aborted > 0, "the loser of a race over the same inputs must abort");
        let a = audit_log(&mut s).expect("no lost update: a file removed twice is corruption");
        assert_eq!(a.removes, r.committed * COMPACT_INPUTS as u64);
    }

    #[test]
    fn mixed_writers_keep_a_valid_history_and_conserve_rows() {
        let c = cfg(4, 2, 2);
        let (r, mut s) = run_occ(c);
        let a = audit_log(&mut s).unwrap();
        assert_eq!(a.versions, 1 + r.committed);
        let attempted = ((c.appenders + c.compactors + c.deleters) * c.ops_per_writer) as u64;
        assert_eq!(r.committed + r.aborted + r.skipped, attempted);
        // compactions never change the live-file count by anything but -3,
        // appends by +1, deletes by -1
        assert!(a.live.len() > 0);
        assert!(r.validation_reads >= r.conflicts, "every conflict reads at least one winner");
    }

    #[test]
    fn more_writers_more_conflicts_per_commit() {
        let (r2, _) = run_occ(cfg(2, 0, 0));
        let (r16, _) = run_occ(cfg(16, 0, 0));
        let per = |r: OccReport| r.conflicts as f64 / r.committed as f64;
        assert!(per(r16) > 2.0 * per(r2), "{} vs {}", per(r16), per(r2));
    }

    #[test]
    fn seeded_runs_reproduce() {
        let (a, _) = run_occ(cfg(3, 1, 1));
        let (b, _) = run_occ(cfg(3, 1, 1));
        assert_eq!(a, b);
    }
}
