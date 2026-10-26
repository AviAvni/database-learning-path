//! A Delta-shaped table: immutable data files and a single ordered log of
//! actions, one create-only object per commit, with periodic checkpoints.
//!
//! The object layout follows the Delta protocol (delta-io/delta
//! `PROTOCOL.md`: "Delta Log Entries", "Checkpoints", "Last Checkpoint
//! File", "Action Reconciliation") and the read/write protocols of Armbrust
//! et al., "Delta Lake" (VLDB'20 §3.2):
//!
//! ```text
//! delta/_delta_log/00000000000000000000.json             protocol, metaData, add...
//! delta/_delta_log/00000000000000000001.json             commitInfo, add...
//! delta/_delta_log/00000000000000000010.checkpoint.json  every live add, reconciled
//! delta/_delta_log/_last_checkpoint                      {"version": 10, "size": ...}
//! ```
//!
//! Commits are newline-delimited JSON exactly as the protocol specifies.
//! The one deliberate difference: real checkpoints are Parquet; this model
//! writes them as the same newline-delimited JSON, so every byte count here
//! uses the same per-file `add` encoding as the commits do.

use crate::gen::DataFile;
use crate::iceberg::Plan;
use crate::store::Store;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const LOG: &str = "delta/_delta_log/";
const LAST_CP: &str = "delta/_delta_log/_last_checkpoint";

/// The paper's default: "By default, our clients write checkpoints every
/// 10 transactions" (Delta VLDB'20 §3.1.3).
pub const DEFAULT_CHECKPOINT_INTERVAL: u64 = 10;

pub fn commit_key(v: u64) -> String {
    format!("{LOG}{v:020}.json")
}

pub fn checkpoint_key(v: u64) -> String {
    format!("{LOG}{v:020}.checkpoint.json")
}

/// Parse `<20 digits>.json` / `<20 digits>.checkpoint.json` from a listed key.
fn parse_log_key(key: &str) -> Option<(u64, bool)> {
    let name = key.strip_prefix(LOG)?;
    let (num, rest) = name.split_at(name.find('.')?);
    let v = num.parse().ok()?;
    match rest {
        ".json" => Some((v, false)),
        ".checkpoint.json" => Some((v, true)),
        _ => None,
    }
}

pub fn add_action(f: &DataFile, ts_ms: i64) -> Value {
    let stats = json!({
        "numRecords": f.records,
        "minValues": {"id": f.min_id, "ts": f.min_ts, "v": f.min_v},
        "maxValues": {"id": f.max_id, "ts": f.max_ts, "v": f.max_v},
        "nullCount": {"id": 0, "ts": 0, "v": f.null_v},
    });
    json!({"add": {
        "path": f.path,
        "partitionValues": {"day": f.day.to_string()},
        "size": f.size,
        "modificationTime": ts_ms,
        "dataChange": true,
        // the protocol stores stats as a JSON *string* inside the action
        "stats": stats.to_string(),
    }})
}

fn protocol_and_metadata() -> [Value; 2] {
    let schema = json!({"type": "struct", "fields": [
        {"name": "id", "type": "long", "nullable": false, "metadata": {}},
        {"name": "ts", "type": "timestamp", "nullable": false, "metadata": {}},
        {"name": "v", "type": "double", "nullable": true, "metadata": {}},
        {"name": "day", "type": "string", "nullable": false, "metadata": {}},
    ]});
    [
        json!({"protocol": {"minReaderVersion": 1, "minWriterVersion": 2}}),
        json!({"metaData": {
            "id": "49494949-4949-4949-4949-494949494949",
            "format": {"provider": "parquet", "options": {}},
            "schemaString": schema.to_string(),
            "partitionColumns": ["day"],
            "configuration": {},
            "createdTime": 1_767_225_600_000i64,
        }}),
    ]
}

fn ndjson(actions: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for a in actions {
        serde_json::to_writer(&mut out, a).unwrap();
        out.push(b'\n');
    }
    out
}

fn ts_ms(v: u64) -> i64 {
    1_767_225_600_000 + v as i64 * 60_000
}

/// The writer's view: the next version to write and the reconciled live
/// state (kept in memory so it can checkpoint without re-reading the log).
pub struct Delta {
    next_version: u64,
    live: BTreeMap<String, Value>,
    /// write a checkpoint after every `interval` commits; 0 = never
    pub checkpoint_interval: u64,
}

impl Delta {
    pub fn new(checkpoint_interval: u64) -> Self {
        Self { next_version: 0, live: BTreeMap::new(), checkpoint_interval }
    }

    pub fn version(&self) -> Option<u64> {
        self.next_version.checked_sub(1)
    }

    /// One blind append: write version N+1 with put-if-absent (Delta VLDB'20
    /// §3.2.2 step 4), then optionally checkpoint (step 5). Returns the
    /// committed version.
    pub fn append(&mut self, store: &mut Store, files: &[DataFile]) -> u64 {
        let v = self.next_version;
        let mut actions = Vec::new();
        actions.push(json!({"commitInfo": {
            "timestamp": ts_ms(v), "operation": "WRITE",
            "operationParameters": {"mode": "Append"}, "isBlindAppend": true,
        }}));
        if v == 0 {
            actions.extend(protocol_and_metadata());
        }
        for f in files {
            let a = add_action(f, ts_ms(v));
            self.live.insert(f.path.clone(), a.clone());
            actions.push(a);
        }
        store
            .put_if_absent(&commit_key(v), ndjson(&actions))
            .expect("single writer: version cannot be taken");
        self.next_version += 1;
        if self.checkpoint_interval > 0 && v > 0 && v % self.checkpoint_interval == 0 {
            self.checkpoint(store);
        }
        v
    }

    /// Write a checkpoint of the latest version: protocol, metaData, and
    /// every live `add` (PROTOCOL.md "Checkpoints"), then point
    /// `_last_checkpoint` at it.
    pub fn checkpoint(&mut self, store: &mut Store) {
        let v = self.version().expect("nothing to checkpoint");
        let mut actions: Vec<Value> = protocol_and_metadata().to_vec();
        actions.extend(self.live.values().cloned());
        let n = actions.len();
        store.put(&checkpoint_key(v), ndjson(&actions));
        store.put(LAST_CP, serde_json::to_vec(&json!({"version": v, "size": n})).unwrap());
    }

    /// Reconstruct the snapshot at version `at` (None = latest) and list its
    /// data files, optionally only those in partition `day` — the read
    /// protocol of Delta VLDB'20 §3.2.1:
    ///
    /// 1. read `_last_checkpoint` (latest only; it may point past `at`),
    /// 2. LIST the log from that checkpoint (or from 0) — one round per page,
    /// 3. GET the newest checkpoint <= target and every later commit, in
    ///    parallel, and replay them (PROTOCOL.md "Action Reconciliation").
    ///
    /// Partition pruning happens *after* replay: the log has no index over
    /// partition values, so a one-day query reads the same metadata as a
    /// full scan.
    pub fn plan(store: &mut Store, at: Option<u64>, day: Option<u32>) -> Plan {
        let before = store.meter;
        let mut start_after: Option<String> = None;
        if at.is_none() {
            store.round();
            if let Some((_, b)) = store.get(LAST_CP) {
                let lc: Value = serde_json::from_slice(&b).unwrap();
                let c = lc["version"].as_u64().unwrap();
                // list from just before "<c>.checkpoint.json"
                start_after = Some(format!("{LOG}{c:020}"));
            }
        }
        let limit = at.unwrap_or(u64::MAX);
        let keys = store.list(LOG, start_after.as_deref(), |k| {
            matches!(parse_log_key(k), Some((v, _)) if v > limit)
        });
        let mut checkpoint: Option<u64> = None;
        let mut commits: Vec<u64> = Vec::new();
        for k in &keys {
            if let Some((v, is_cp)) = parse_log_key(k) {
                if v > limit {
                    continue;
                }
                if is_cp {
                    checkpoint = Some(v);
                } else {
                    commits.push(v);
                }
            }
        }
        let target = commits.iter().copied().max().unwrap_or(0).max(checkpoint.unwrap_or(0));
        store.round();
        let mut live: BTreeMap<String, Value> = BTreeMap::new();
        let mut replay = |body: Vec<u8>| {
            for line in body.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                let a: Value = serde_json::from_slice(line).unwrap();
                if let Some(add) = a.get("add") {
                    live.insert(add["path"].as_str().unwrap().to_string(), add.clone());
                } else if let Some(rm) = a.get("remove") {
                    live.remove(rm["path"].as_str().unwrap());
                }
            }
        };
        if let Some(c) = checkpoint {
            replay(store.get(&checkpoint_key(c)).expect("checkpoint").1);
        }
        for v in commits {
            if checkpoint.map_or(true, |c| v > c) && v <= target {
                replay(store.get(&commit_key(v)).expect("commit").1);
            }
        }
        let mut files: Vec<String> = live
            .iter()
            .filter(|(_, a)| match day {
                Some(d) => a["partitionValues"]["day"].as_str() == Some(&d.to_string()),
                None => true,
            })
            .map(|(p, _)| p.clone())
            .collect();
        files.sort();
        let meter = store.meter.minus(&before);
        Plan { files, objects: meter.gets, meter, version: target }
    }

    pub fn prefix() -> &'static str {
        "delta/"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{commits, Workload, SEED};

    #[test]
    fn keys_sort_checkpoint_before_commit_of_same_version() {
        assert!(checkpoint_key(10) < commit_key(10));
        assert!(commit_key(10) < commit_key(11));
        assert_eq!(parse_log_key(&checkpoint_key(7)), Some((7, true)));
        assert_eq!(parse_log_key(LAST_CP), None);
    }

    #[test]
    fn replay_matches_ground_truth_with_and_without_checkpoints() {
        let w = Workload { commits: 35, files_per_commit: 2, commits_per_day: 10 };
        let cs = commits(SEED, w);
        let mut truth: Vec<String> = cs.iter().flatten().map(|f| f.path.clone()).collect();
        truth.sort();
        for interval in [0, 10] {
            let mut s = Store::new();
            let mut d = Delta::new(interval);
            for c in &cs {
                d.append(&mut s, c);
            }
            let p = Delta::plan(&mut s, None, None);
            assert_eq!(p.files, truth, "interval {interval}");
            if interval == 10 {
                // _last_checkpoint, checkpoint 30, commits 31..=34
                assert_eq!(p.objects, 1 + 1 + 4);
            } else {
                // the _last_checkpoint GET is paid even when it misses
                assert_eq!(p.objects, 1 + 35);
            }
            let p12 = Delta::plan(&mut s, Some(12), None);
            assert_eq!(p12.files.len(), 26);
        }
    }
}
