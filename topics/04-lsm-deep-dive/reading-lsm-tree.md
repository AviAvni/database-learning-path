# Reading the `lsm-tree` crate — fjall's engine (read it all, 3 h)

Repo: [`~/repos/lsm-tree`](https://github.com/fjall-rs/lsm-tree) (shallow clone of fjall-rs/lsm-tree). Topic 1 read
fjall's keyspace layer; everything LSM-shaped delegates here. Small enough to
read completely — this guide orders it.

## 1. Block encoding — `src/table/block/`

- `encoder.rs:61–151` — restart intervals + prefix truncation: a FULL item every
  `restart_interval` items; between restarts, items store
  `shared_prefix_len + rest` (`longest_shared_prefix_length`, :142). Optional
  hash index per block (:148–154) — a tiny SwissTable-ish shortcut inside each
  block; topic 2 pattern at yet another scale.
- `header.rs:49–60` — per-block header: type, **xxh3 checksum (u128)**, on-disk
  + uncompressed sizes; the header itself gets a u32 checksum (:109).
- `mod.rs:60–70, 111–120` — LZ4 per block.
- Index blocks: `index_block/block_handle.rs:20–43` — varint offset+size handles.

## 2. Segment writer/reader — `src/table/`

- `writer/mod.rs:40–95` — buffer KVs, flush block at size threshold, feed the
  filter + index writers, trailer/metadata last (single forward pass — an SST
  is written append-only, like everything else in an LSM).
- Read path with filter: `mod.rs:245–290` — filter loaded lazily; if
  `maybe_contains_hash` says no, the point read never touches a data block
  (:281–288).

## 3. Bloom filter — `src/table/filter/standard_bloom/`

- `builder.rs:55–127` — `with_fp_rate` computes m,k from `−n·ln(fpr)/ln²2`
  (:58); `with_bpk` direct (:93).
- **Double hashing** — `builder.rs:10–13` + `mod.rs:102–129`: k probes from two
  hashes via `h1 += h2; h2 *= i` — k memory probes but only ONE real hash
  computation. Compare with RocksDB's cache-local bloom (all k bits in one cache
  line — topic 0 priced why).

## 4. Version + levels — `src/version/`

- `mod.rs:42–114` + `run.rs:51–103` — levels are **runs**; a run is disjoint by
  key range, so `get_for_key` (:99–103) binary-searches segment ranges: one
  segment probed per run. L0 = many runs (each flush is one); L1+ = one run.
- Persistence: `persist.rs:9–45` — new version file written, checksummed,
  `rewrite_atomic` on CURRENT_VERSION_FILE + fsync. This is RocksDB's MANIFEST
  in miniature: **compaction commits by publishing a new version, never by
  mutating the old one**. COW again, at the metadata level.
- Recovery: `recovery.rs:34–95`.

## 5. Compaction — `src/compaction/`

- Trait: `mod.rs:87–98` — `choose(version, config, state) → Merge | Move |
  Drop | DoNothing`. Note `Move`: a segment that doesn't overlap the next level
  is *relinked*, zero IO — find where leveled uses it
  (`leveled/mod.rs:19` pick_minimal_compaction).
- Leveled: `leveled/mod.rs:113–143` — L0 trigger 4 runs, ratio 10.
- Worker: `worker.rs:382–389` — **tombstones evicted only when the output is
  the last level** (`evict_tombstones(is_last_level)`). Dropping one earlier
  would resurrect older versions below. Same reasoning you'll need for M4.
- Merge: `merge.rs:35–99` — k-way merge on an interval heap (double-ended, so
  reverse scans work too).

## 6. Read path end-to-end — `src/tree/mod.rs`

- `get` :639–643 → `get_internal_entry` :696–750: active memtable → sealed
  (newest first) → levels; **seqno filtering at every step** (:701/707/730) —
  MVCC reads pick the newest version ≤ snapshot seqno.
- Hash computed once and shared across all segment filter checks (:721–723) —
  the SipHash-cost lesson from topic 0 applied.
- Tombstones: `value_type.rs:8–27`; hidden at read time (`tree/mod.rs:67–72`),
  dropped at bottom-level compaction.

## Questions to answer in notes.md

1. Why can L0 not be a disjoint run, and what does that cost a point read?
   (Flushes overlap arbitrarily ⇒ probe every L0 run ⇒ the stall trigger.)
2. Restart interval 16: derive the trade (space saved by truncation vs linear
   decode cost per lookup). Why don't B-tree pages (topic 3) do this?
3. The version file is rewritten whole on every compaction. RocksDB instead
   appends VersionEdits to a MANIFEST log. When does lsm-tree's simpler choice
   break down? (Huge segment counts; crash mid-rewrite handled by atomic rename.)

## Done when

You can trace one `get` from `tree/mod.rs:639` to a data-block binary search,
naming every filter/index consulted, and explain why tombstones die only at
the bottom.
