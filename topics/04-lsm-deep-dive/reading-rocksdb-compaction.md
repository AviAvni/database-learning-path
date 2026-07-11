# Reading RocksDB compaction + SST format (guided skim, 2–3 h)

Repo: [`~/repos/rocksdb`](https://github.com/facebook/rocksdb). You read the lsm-tree crate first; now the industrial
version — read for what production adds: stalls, partitioned indexes, ribbon
filters, universal compaction.

## 1. Leveled picking — `db/compaction/compaction_picker_level.cc`

- Score formulas in comments :229–233 —
  `L0: num_files / level0_file_num_compaction_trigger`;
  `L1+: level_bytes / MaxBytesForLevel`. Highest score wins.
- `LevelCompactionBuilder::PickCompaction` :531 — setup inputs, expand to clean
  key boundaries, grab the overlapping next-level files.
- :596 — score recomputed accounting for **in-flight** compactions — the picker
  is a scheduler; double-booking a level wastes IO.

## 2. The merge itself — `compaction_job.cc:1904`

`ProcessKeyValueCompaction`: a k-way merge (like lsm-tree's) plus production
concerns — compaction filters (user callbacks), snapshot lists (which old
versions must survive), sub-compaction splitting for parallelism. Skim for
shape; the interesting part is how much of it is *not* the merge.

## 3. Stalls — `db/column_family.cc:1019–1043`

`GetWriteStallConditionAndCause`:
- L0 files ≥ `level0_stop_writes_trigger` → **stop**
- pending compaction bytes ≥ hard limit → **stop**
- L0 files ≥ `level0_slowdown_writes_trigger` → **delayed**
- pending bytes ≥ soft limit → **delayed**

Compaction debt is measured in *bytes not yet merged*; stalls convert an
unbounded read-amp problem into a bounded write-latency problem. Compare
fjall's version: a spin-loop delay at 20–30 L0 runs (fjall
`src/keyspace/write_delay.rs:8–16`) — same valve, 100× simpler.

## 4. SST building — `table/block_based/block_based_table_builder.cc`

- Restart interval + delta encoding :1096–1097 (default 16 — same constant as
  lsm-tree; convergent evolution or shared ancestry? LevelDB is the ancestor).
- Block flush policy :1127 (~4KB).
- Index entry = last key of each block, written on flush :1908–1912; SQLite's
  interior separators, rediscovered — and RocksDB DOES shorten them
  (FindShortestSeparator), the truncation topic 3 experimented with.

## 5. Read path — `block_based_table_reader.cc`

- `Get` :3010 — whole-table filter check first :3040, then index iterator
  :3044–3053, then data block :3071–3096, block cache probe in
  `GetDataBlockFromCache` :2345.
- **Partitioned index** :1778 + `partitioned_index_reader.h:15` — the index
  itself becomes a 2-level B-tree when tables are huge: top level pinned in
  cache, partitions loaded on demand. No fractional cascading in practice —
  plain binary search per level won.

## 6. Filters — `table/block_based/filter_policy.cc`

- `FastLocalBloomBitsBuilder` :365–376 — `millibits_per_key`; probes stay
  within one cache line per key (contrast lsm-tree's double hashing across the
  whole bit array — k cache lines).
- Ribbon :658–686 — ~30% smaller for the same FPR, costlier to build; falls
  back to bloom if banding fails after 256 seed attempts. CPU-for-DRAM knob.

## 7. The manifest — `db/version_set.cc`

- `LogAndApply` :6778 — compaction output replaces inputs by appending a
  `VersionEdit` (version_edit.h:37–77, 705–744) to the MANIFEST log, then
  pointing CURRENT at it. Readers keep iterating their old Version (refcounted)
  — MVCC for *metadata*. lsm-tree rewrites the whole version file instead;
  same atomicity, different scale point.

## Questions to answer in notes.md

1. Why does leveled compaction pick by *score* rather than round-robin?
   Construct a workload where round-robin lets one level grow unboundedly.
2. Partitioned index vs lsm-tree's per-block hash index — both attack "index
   too big for cache". Which helps point reads, which helps scans, why?
3. FastLocalBloom does k probes in one cache line — what does that cost in FPR
   vs a classic bloom at equal bits/key? (Blocked blooms have slightly worse
   FPR — the locality is paid for in statistics.)

## Done when

You can list the three stall triggers from memory and explain LogAndApply's
refcounted-Version scheme as "MVCC for metadata".
