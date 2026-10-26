//! An Iceberg-shaped table: immutable data files, one manifest per fast
//! append, a manifest list per snapshot, a table-metadata file per commit,
//! and a catalog pointer swapped with compare-and-swap.
//!
//! The object layout follows the Iceberg table spec (apache/iceberg
//! `format/spec.md`, v2 fields; "Overview", "Manifests", "Manifest Lists",
//! "Snapshots", "Table Metadata Fields", "Metastore Tables"):
//!
//! ```text
//! ice/catalog                                   -> location of current metadata (CAS)
//! ice/metadata/00042-<uuid>.metadata.json       every snapshot ever kept + logs
//! ice/metadata/snap-<snapshot-id>.manifest-list one entry per live manifest
//! ice/metadata/<snapshot-id>-m0.manifest        one entry per data file
//! ```
//!
//! One deliberate difference: Iceberg writes manifests and manifest lists in
//! Avro; this model writes them as JSON, with the spec's field names, so
//! both formats in this crate pay for the *same* per-file entry encoding and
//! the byte counts differ only by structure. Object and request counts do
//! not depend on the encoding at all.

use crate::gen::DataFile;
use crate::store::{Meter, Store};
use serde_json::{json, Value};

const PREFIX: &str = "ice/";
const CATALOG: &str = "ice/catalog";
/// iceberg-rust's default for `write.metadata.previous-versions-max`
/// (crates/iceberg/src/spec/table_properties.rs:377).
const METADATA_LOG_MAX: usize = 100;

#[derive(Clone, Debug)]
struct ManifestRef {
    path: String,
    length: u64,
    added_snapshot_id: i64,
    seq: i64,
    added_files: u64,
    existing_files: u64,
    added_rows: u64,
    existing_rows: u64,
    day_lo: u32,
    day_hi: u32,
}

#[derive(Clone, Debug)]
struct SnapshotMeta {
    id: i64,
    parent: Option<i64>,
    seq: i64,
    ts_ms: i64,
    manifest_list: String,
    operation: &'static str,
    added_files: u64,
    added_records: u64,
    total_files: u64,
    total_records: u64,
}

/// The writer's view of the table. A single writer keeps its last committed
/// metadata in memory, so a commit issues only writes; lane 2 is where
/// writers race and must re-read.
pub struct Iceberg {
    snapshots: Vec<SnapshotMeta>,
    manifests: Vec<ManifestRef>,
    /// live data files and the snapshot that added them (for rewrites)
    live: Vec<(DataFile, i64, i64)>,
    seq: i64,
    metadata_version: u64,
    metadata_log: Vec<(i64, String)>,
    pointer_tag: Option<u64>,
    /// `Some(k)`: Java's default `newAppend()` (MergeAppend) — when the new
    /// manifest's bin holds `k` manifests, merge them into one
    /// (`commit.manifest.min-count-to-merge`, default 100). `None`: fast
    /// append, the only append iceberg-rust implements.
    merge_min: Option<usize>,
}

/// What a scan planner produced, and what producing it cost.
#[derive(Clone, Debug)]
pub struct Plan {
    /// data-file paths to read, sorted
    pub files: Vec<String>,
    pub meter: Meter,
    /// metadata objects read (GETs) — the headline count
    pub objects: u64,
    /// the version planned: Iceberg sequence number / Delta log version
    pub version: u64,
}

fn snapshot_id(seq: i64) -> i64 {
    // deterministic, distinct, and not equal to the sequence number so the
    // two are never confused when reading the JSON
    7_000_000_000 + seq * 7919
}

fn ts_ms(seq: i64) -> i64 {
    1_767_225_600_000 + seq * 60_000
}

fn data_file_json(f: &DataFile) -> Value {
    json!({
        "content": 0,
        "file_path": f.path,
        "file_format": "PARQUET",
        "partition": {"day": f.day},
        "record_count": f.records,
        "file_size_in_bytes": f.size,
        "value_counts": {"1": f.records, "2": f.records, "3": f.records},
        "null_value_counts": {"1": 0, "2": 0, "3": f.null_v},
        "lower_bounds": {"1": f.min_id, "2": f.min_ts, "3": f.min_v},
        "upper_bounds": {"1": f.max_id, "2": f.max_ts, "3": f.max_v},
    })
}

fn schema_json() -> Value {
    json!({"type": "struct", "schema-id": 0, "fields": [
        {"id": 1, "name": "id", "required": true, "type": "long"},
        {"id": 2, "name": "ts", "required": true, "type": "timestamptz"},
        {"id": 3, "name": "v", "required": false, "type": "double"},
    ]})
}

impl Default for Iceberg {
    fn default() -> Self {
        Self::new()
    }
}

impl Iceberg {
    pub fn new() -> Self {
        Self {
            snapshots: Vec::new(),
            manifests: Vec::new(),
            live: Vec::new(),
            seq: 0,
            metadata_version: 0,
            metadata_log: Vec::new(),
            pointer_tag: None,
            merge_min: None,
        }
    }

    /// Java's `MergeAppend`, merging once `min_count` manifests accumulate.
    /// Every manifest here is a few KB, far below the 8 MB bin target
    /// (`commit.manifest.target-size-bytes`), so all of them share one bin
    /// and the merge folds every live manifest into one.
    pub fn with_merge_append(min_count: usize) -> Self {
        Self { merge_min: Some(min_count), ..Self::new() }
    }

    /// Does the current table metadata still hold snapshot `id`?
    pub fn snapshot_exists(store: &mut Store, id: i64) -> bool {
        let (_, ptr) = store.get(CATALOG).expect("table exists");
        let (_, md) = store.get(&String::from_utf8(ptr).unwrap()).expect("metadata");
        let md: Value = serde_json::from_slice(&md).unwrap();
        md["snapshots"].as_array().unwrap().iter().any(|s| s["snapshot-id"].as_i64() == Some(id))
    }

    pub fn current_snapshot_id(&self) -> Option<i64> {
        self.snapshots.last().map(|s| s.id)
    }

    fn write_manifest(&self, store: &mut Store, path: &str, entries: &[(DataFile, i64, i64, u8)]) -> u64 {
        let es: Vec<Value> = entries
            .iter()
            .map(|(f, sid, seq, status)| {
                json!({
                    "status": status,
                    "snapshot_id": sid,
                    "sequence_number": seq,
                    "file_sequence_number": seq,
                    "data_file": data_file_json(f),
                })
            })
            .collect();
        let body = serde_json::to_vec(&json!({
            "schema-id": 0, "partition-spec-id": 0, "format-version": 2,
            "content": "data", "entries": es,
        }))
        .unwrap();
        let n = body.len() as u64;
        store.put(path, body);
        n
    }

    fn write_manifest_list(&self, store: &mut Store, path: &str) {
        let ms: Vec<Value> = self
            .manifests
            .iter()
            .map(|m| {
                json!({
                    "manifest_path": m.path,
                    "manifest_length": m.length,
                    "partition_spec_id": 0,
                    "content": 0,
                    "sequence_number": m.seq,
                    "min_sequence_number": m.seq,
                    "added_snapshot_id": m.added_snapshot_id,
                    "added_files_count": m.added_files,
                    "existing_files_count": m.existing_files,
                    "deleted_files_count": 0,
                    "added_rows_count": m.added_rows,
                    "existing_rows_count": m.existing_rows,
                    "deleted_rows_count": 0,
                    "partitions": [{"contains_null": false, "lower_bound": m.day_lo, "upper_bound": m.day_hi}],
                })
            })
            .collect();
        store.put(path, serde_json::to_vec(&json!({"manifests": ms})).unwrap());
    }

    fn write_metadata_and_swap(&mut self, store: &mut Store) {
        self.metadata_version += 1;
        let path = format!(
            "{PREFIX}metadata/{:05}-{:016x}.metadata.json",
            self.metadata_version,
            (self.metadata_version as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        );
        let cur = self.snapshots.last();
        let snaps: Vec<Value> = self
            .snapshots
            .iter()
            .map(|s| {
                let mut v = json!({
                    "snapshot-id": s.id,
                    "sequence-number": s.seq,
                    "timestamp-ms": s.ts_ms,
                    "manifest-list": s.manifest_list,
                    "summary": {
                        "operation": s.operation,
                        "added-data-files": s.added_files.to_string(),
                        "added-records": s.added_records.to_string(),
                        "total-data-files": s.total_files.to_string(),
                        "total-records": s.total_records.to_string(),
                    },
                    "schema-id": 0,
                });
                if let Some(p) = s.parent {
                    v["parent-snapshot-id"] = json!(p);
                }
                v
            })
            .collect();
        let snap_log: Vec<Value> = self
            .snapshots
            .iter()
            .map(|s| json!({"timestamp-ms": s.ts_ms, "snapshot-id": s.id}))
            .collect();
        let md_log: Vec<Value> = self
            .metadata_log
            .iter()
            .map(|(t, f)| json!({"timestamp-ms": t, "metadata-file": f}))
            .collect();
        let md = json!({
            "format-version": 2,
            "table-uuid": "49494949-4949-4949-4949-494949494949",
            "location": "s3://bucket/ice",
            "last-sequence-number": self.seq,
            "last-updated-ms": cur.map(|s| s.ts_ms).unwrap_or(0),
            "last-column-id": 3,
            "current-schema-id": 0,
            "schemas": [schema_json()],
            "default-spec-id": 0,
            "partition-specs": [{"spec-id": 0, "fields": [
                {"source-id": 2, "field-id": 1000, "name": "day", "transform": "day"}]}],
            "last-partition-id": 1000,
            "default-sort-order-id": 0,
            "sort-orders": [{"order-id": 0, "fields": []}],
            "properties": {},
            "current-snapshot-id": cur.map(|s| s.id).unwrap_or(-1),
            "refs": {"main": {"snapshot-id": cur.map(|s| s.id).unwrap_or(-1), "type": "branch"}},
            "snapshots": snaps,
            "snapshot-log": snap_log,
            "metadata-log": md_log,
        });
        store.put(&path, serde_json::to_vec(&md).unwrap());
        let tag = store
            .put_if_match(CATALOG, self.pointer_tag, path.clone().into_bytes())
            .expect("single writer: the pointer cannot have moved");
        self.pointer_tag = Some(tag);
        self.metadata_log.push((cur.map(|s| s.ts_ms).unwrap_or(0), path));
        if self.metadata_log.len() > METADATA_LOG_MAX {
            self.metadata_log.remove(0);
        }
    }

    fn new_snapshot(&mut self, operation: &'static str, added_files: u64, added_records: u64) -> SnapshotMeta {
        self.seq += 1;
        let id = snapshot_id(self.seq);
        SnapshotMeta {
            id,
            parent: self.current_snapshot_id(),
            seq: self.seq,
            ts_ms: ts_ms(self.seq),
            manifest_list: format!("{PREFIX}metadata/snap-{id}.manifest-list"),
            operation,
            added_files,
            added_records,
            total_files: self.live.len() as u64,
            total_records: self.live.iter().map(|(f, _, _)| f.records).sum(),
        }
    }

    /// Iceberg's "fast append" (spec, "Snapshots": "Appends can add a new
    /// manifest to minimize the amount of data written"): one new manifest
    /// holding just the new files, a new manifest list naming every live
    /// manifest, a new table-metadata file naming every snapshot, and a CAS
    /// on the catalog pointer. Returns the snapshot id.
    pub fn append(&mut self, store: &mut Store, files: &[DataFile]) -> i64 {
        let seq = self.seq + 1;
        let id = snapshot_id(seq);
        for f in files {
            self.live.push((f.clone(), id, seq));
        }
        let mpath = format!("{PREFIX}metadata/{id}-m0.manifest");
        let entries: Vec<_> = files.iter().map(|f| (f.clone(), id, seq, 1u8)).collect();
        let length = self.write_manifest(store, &mpath, &entries);
        self.manifests.push(ManifestRef {
            path: mpath,
            length,
            added_snapshot_id: id,
            seq,
            added_files: files.len() as u64,
            existing_files: 0,
            added_rows: files.iter().map(|f| f.records).sum(),
            existing_rows: 0,
            day_lo: files.iter().map(|f| f.day).min().unwrap_or(0),
            day_hi: files.iter().map(|f| f.day).max().unwrap_or(0),
        });
        if let Some(k) = self.merge_min {
            if self.manifests.len() >= k {
                self.merge_all_manifests(store, id, seq);
            }
        }
        let snap = self.new_snapshot(
            "append",
            files.len() as u64,
            files.iter().map(|f| f.records).sum(),
        );
        debug_assert_eq!(snap.id, id);
        self.write_manifest_list(store, &snap.manifest_list.clone());
        self.snapshots.push(snap);
        self.write_metadata_and_swap(store);
        id
    }

    /// MergeAppend's merge: read every live manifest and write one holding
    /// all live files — the commit's own files as ADDED, the rest EXISTING.
    fn merge_all_manifests(&mut self, store: &mut Store, id: i64, seq: i64) {
        // the commit's own new manifest is in memory; the older ones are read
        let n = self.manifests.len();
        for m in &self.manifests[..n - 1] {
            store.get(&m.path);
        }
        // the single-commit manifest just written is superseded and removed
        store.delete(&self.manifests[n - 1].path.clone());
        let mpath = format!("{PREFIX}metadata/{id}-m1.manifest");
        let entries: Vec<_> = self
            .live
            .iter()
            .map(|(f, sid, s)| (f.clone(), *sid, *s, if *sid == id { 1u8 } else { 0u8 }))
            .collect();
        let length = self.write_manifest(store, &mpath, &entries);
        let added: Vec<&DataFile> = self.live.iter().filter(|(_, sid, _)| *sid == id).map(|(f, _, _)| f).collect();
        let added_rows: u64 = added.iter().map(|f| f.records).sum();
        let all_rows: u64 = self.live.iter().map(|(f, _, _)| f.records).sum();
        self.manifests = vec![ManifestRef {
            path: mpath,
            length,
            added_snapshot_id: id,
            seq,
            added_files: added.len() as u64,
            existing_files: (self.live.len() - added.len()) as u64,
            added_rows,
            existing_rows: all_rows - added_rows,
            day_lo: self.live.iter().map(|(f, _, _)| f.day).min().unwrap_or(0),
            day_hi: self.live.iter().map(|(f, _, _)| f.day).max().unwrap_or(0),
        }];
    }

    /// Manifest compaction ("rewrite manifests"): read every live manifest,
    /// write one manifest holding every live file as EXISTING, and commit a
    /// `replace` snapshot. Data files are untouched.
    pub fn rewrite_manifests(&mut self, store: &mut Store) {
        for m in &self.manifests {
            store.get(&m.path);
        }
        let seq = self.seq + 1;
        let id = snapshot_id(seq);
        let mpath = format!("{PREFIX}metadata/{id}-m0.manifest");
        let entries: Vec<_> = self.live.iter().map(|(f, sid, s)| (f.clone(), *sid, *s, 0u8)).collect();
        let length = self.write_manifest(store, &mpath, &entries);
        let rows: u64 = self.live.iter().map(|(f, _, _)| f.records).sum();
        self.manifests = vec![ManifestRef {
            path: mpath,
            length,
            added_snapshot_id: id,
            seq,
            added_files: 0,
            existing_files: self.live.len() as u64,
            added_rows: 0,
            existing_rows: rows,
            day_lo: self.live.iter().map(|(f, _, _)| f.day).min().unwrap_or(0),
            day_hi: self.live.iter().map(|(f, _, _)| f.day).max().unwrap_or(0),
        }];
        let snap = self.new_snapshot("replace", 0, 0);
        self.write_manifest_list(store, &snap.manifest_list.clone());
        self.snapshots.push(snap);
        self.write_metadata_and_swap(store);
    }

    /// Snapshot expiry keeping the newest `keep` snapshots (spec, "Snapshot
    /// Retention Policy"). Expired snapshots' manifest lists are deleted; a
    /// manifest is deleted when no kept manifest list can reach it — here,
    /// when it is not live (fast-append manifests stay live until a rewrite).
    pub fn expire_snapshots(&mut self, store: &mut Store, keep: usize) {
        let cut = self.snapshots.len().saturating_sub(keep);
        let expired: Vec<SnapshotMeta> = self.snapshots.drain(..cut).collect();
        for s in &expired {
            store.delete(&s.manifest_list);
            for m in ["m0", "m1"] {
                let mpath = format!("{PREFIX}metadata/{}-{m}.manifest", s.id);
                if !self.manifests.iter().any(|m| m.path == mpath) {
                    store.delete(&mpath);
                }
            }
        }
        if let Some(first) = self.snapshots.first_mut() {
            first.parent = None;
        }
        self.write_metadata_and_swap(store);
    }

    /// Plan a scan at snapshot `at` (None = current), optionally restricted
    /// to one `day` partition, the way the spec's "Scan Planning" reads:
    /// catalog -> table metadata -> manifest list -> (pruned) manifests.
    /// Four dependent rounds whatever the table size; the manifests in the
    /// last round are fetched in parallel.
    pub fn plan(store: &mut Store, at: Option<i64>, day: Option<u32>) -> Plan {
        let before = store.meter;
        store.round();
        let (_, ptr) = store.get(CATALOG).expect("table exists");
        let md_path = String::from_utf8(ptr).unwrap();
        store.round();
        let (_, md) = store.get(&md_path).expect("metadata");
        let md: Value = serde_json::from_slice(&md).unwrap();
        let want = at.unwrap_or_else(|| md["current-snapshot-id"].as_i64().unwrap());
        let snap = md["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["snapshot-id"].as_i64() == Some(want))
            .expect("snapshot not found: expired?");
        let ml_path = snap["manifest-list"].as_str().unwrap().to_string();
        let version = snap["sequence-number"].as_u64().unwrap();
        store.round();
        let (_, ml) = store.get(&ml_path).expect("manifest list");
        let ml: Value = serde_json::from_slice(&ml).unwrap();
        let mut to_read = Vec::new();
        for m in ml["manifests"].as_array().unwrap() {
            if let Some(d) = day {
                let p = &m["partitions"][0];
                let lo = p["lower_bound"].as_u64().unwrap() as u32;
                let hi = p["upper_bound"].as_u64().unwrap() as u32;
                if d < lo || d > hi {
                    continue; // pruned by the manifest list's partition summary
                }
            }
            to_read.push(m["manifest_path"].as_str().unwrap().to_string());
        }
        store.round();
        let mut files = Vec::new();
        for p in &to_read {
            let (_, body) = store.get(p).expect("manifest");
            let m: Value = serde_json::from_slice(&body).unwrap();
            for e in m["entries"].as_array().unwrap() {
                if e["status"].as_u64() == Some(2) {
                    continue; // DELETED entries are not live
                }
                let df = &e["data_file"];
                if let Some(d) = day {
                    if df["partition"]["day"].as_u64() != Some(d as u64) {
                        continue;
                    }
                }
                files.push(df["file_path"].as_str().unwrap().to_string());
            }
        }
        files.sort();
        let meter = store.meter.minus(&before);
        Plan { files, objects: meter.gets, meter, version }
    }

    /// Snapshot ids in commit order (for time travel).
    pub fn snapshot_ids(&self) -> Vec<i64> {
        self.snapshots.iter().map(|s| s.id).collect()
    }

    pub fn prefix() -> &'static str {
        PREFIX
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{commits, Workload, SEED};

    #[test]
    fn plan_sees_every_appended_file_and_time_travels() {
        let w = Workload { commits: 30, files_per_commit: 2, commits_per_day: 10 };
        let cs = commits(SEED, w);
        let mut s = Store::new();
        let mut t = Iceberg::new();
        let mut ids = Vec::new();
        for c in &cs {
            ids.push(t.append(&mut s, c));
        }
        let p = Iceberg::plan(&mut s, None, None);
        assert_eq!(p.files.len(), 60);
        assert_eq!(p.objects, 3 + 30, "catalog + metadata + list + one manifest per commit");
        assert_eq!(p.meter.rounds, 4);
        let p10 = Iceberg::plan(&mut s, Some(ids[9]), None);
        assert_eq!(p10.files.len(), 20);
        let d1 = Iceberg::plan(&mut s, None, Some(1));
        assert_eq!(d1.files.len(), 20);
        assert_eq!(d1.objects, 3 + 10, "manifest-list partition summaries prune other days");
        t.rewrite_manifests(&mut s);
        let after = Iceberg::plan(&mut s, None, None);
        assert_eq!(after.files, p.files);
        assert_eq!(after.objects, 4);
    }
}
