# RocksDB compaction: scores, stalls, and the manifest

The lsm-tree crate gave you the clean shape; RocksDB is what a decade of
production adds on top — score-driven compaction picking, write stalls as
back-pressure, partitioned indexes, ribbon filters, and a MANIFEST that does
MVCC for metadata. Before the guided skim, this chapter builds each addition
as its own concept — what problem it solves and what it costs — then maps
every one to its file and line.

## The problem in one sentence

Compaction is a background job competing with foreground writes for the same
disk: pick the wrong level to compact, or let writers outrun the mergers, and
"compaction debt" grows without bound — L0 accumulates files, every point
read probes all of them, and read latency degrades *forever* unless something
pushes back.

## The concepts, step by step

### Step 1 — compaction debt: why compaction needs a scheduler

Compaction debt is the gap between what has been written and what has been
merged — concretely, bytes sitting in levels that exceed their target size,
waiting to be pushed down. Writers add debt (every memtable flush is a new
L0 file); compaction threads pay it off. Since there are finitely many
compaction threads and many levels that could be compacted, some component
must decide *which level's debt hurts most right now*. That component is the
**compaction picker**, and it is a scheduler, not a data structure: its
input is the current shape of the tree, its output is one job. Get the
policy wrong — say, round-robin across levels — and a hot level's debt grows
unboundedly while the picker dutifully polishes cold ones.

### Step 2 — score-driven picking: highest debt first

RocksDB reduces "which level hurts most" to one number per level, the
**score** — how far the level is past its trigger, normalized so scores are
comparable across levels:

- **L0**: `num_files / level0_file_num_compaction_trigger` — L0 is scored by
  *file count*, because every L0 file is an overlapping run that every point
  read must probe (topic README §1); 8 files with trigger 4 ⇒ score 2.0.
- **L1+**: `level_bytes / MaxBytesForLevel` — deeper levels are scored by
  *bytes over target* (targets grow 10× per level: 256 MB, 2.5 GB, 25 GB…).

Highest score ≥ 1.0 compacts first; below 1.0 means no debt, do nothing.
One production subtlety: bytes already being compacted by an in-flight job
are subtracted before scoring — double-booking a level wastes IO. The
scoring loop, reduced to its logic:

```rust
// Compact the level with the highest score ≥ 1.0; the picker is a
// scheduler, so bytes already being compacted don't count twice.
fn pick_compaction_level(&self, v: &Version) -> Option<usize> {
    (0..v.num_levels())
        .map(|lvl| {
            let score = if lvl == 0 {
                v.num_l0_files() as f64 / self.l0_file_trigger as f64
            } else {
                (v.level_bytes(lvl) - v.bytes_being_compacted(lvl)) as f64
                    / self.max_bytes_for_level(lvl) as f64
            };
            (lvl, score)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|&(_, score)| score >= 1.0)   // below 1.0: no debt, do nothing
        .map(|(lvl, _)| lvl)
}
```

Once a level is picked, the job's inputs are expanded to clean key
boundaries and the overlapping next-level files are pulled in — a merge must
consume *every* next-level file its key range touches, or it would create
overlap where the level promises disjointness.

### Step 3 — the merge job: mostly not a merge

The core of a compaction job is the k-way merge you already read in
lsm-tree (pop the smallest key across k sorted inputs, write output
blocks). What RocksDB wraps around it is the production payload:
**compaction filters** (user callbacks that can drop or rewrite each
key-value pair mid-merge — TTL expiry lives here), **snapshot lists** (a
key's old version can only be dropped if no live snapshot might still read
it — MVCC reaching into compaction), and **sub-compaction splitting** (one
job's key range carved into disjoint sub-ranges so multiple threads merge in
parallel). Skim the function for shape; the lesson is how much of an
industrial compaction is *not* the merge.

### Step 4 — write stalls: back-pressure as a feature

A write stall is the engine deliberately slowing or stopping foreground
writes because compaction has fallen behind. It sounds like a bug; it is
load-shedding. Without it, debt is unbounded: L0 grows to hundreds of files
and *every read* pays for it, indefinitely. The stall converts an unbounded
read-amplification problem into a bounded write-latency problem. RocksDB's
triggers, in escalating order:

- L0 files ≥ `level0_slowdown_writes_trigger` (default 20) → **delayed**
  (writes trickled at a reduced rate)
- pending compaction bytes ≥ soft limit → **delayed**
- L0 files ≥ `level0_stop_writes_trigger` (default 36) → **stop**
- pending compaction bytes ≥ hard limit → **stop**

Note the unit of debt: *bytes not yet merged*, not files — a direct measure
of outstanding work. Compare fjall's version of the same valve: a spin-loop
delay when L0 reaches 20–30 runs (fjall `src/keyspace/write_delay.rs:8–16`)
— same idea, 100× simpler. Stalls are the honest choice.

### Step 5 — building the SST: the same block tricks, plus shortened separators

RocksDB's table builder is lsm-tree's Step 2–3 with the serial numbers still
visible: prefix truncation with restart interval 16 (the same constant —
not convergent evolution; LevelDB is the shared ancestor), blocks flushed at
~4 KB, and an index entry recorded per block. The extra trick: the index
doesn't store each block's last key verbatim — `FindShortestSeparator`
shortens it to the smallest string that still separates this block from the
next (between `"userA...zzz"` and `"userB..."`, the separator `"userB"`
suffices). Shorter separators ⇒ smaller index ⇒ more index in cache. This is
SQLite's interior-page separator idea rediscovered — the truncation topic 3
experimented with.

### Step 6 — the read path at scale: partitioning the index

A point read runs filter → index → data block, same as lsm-tree, with one
scale-driven change. On a huge SST (multi-GB), the index block itself
becomes megabytes — too big to pin in cache whole. RocksDB's fix is the
**partitioned index**: cut the index into chunks and build an index *over
the index* — a two-level B-tree, with the small top level pinned in cache
and partitions loaded on demand. The academic alternative (fractional
cascading, threading search hints between levels) never shipped: plain
binary search per level won in practice. Data blocks themselves are probed
through the **block cache** (a shared in-memory cache of decompressed
blocks) before touching disk.

### Step 7 — filters, industrialized: cache-local bloom and ribbon

Two upgrades over the textbook bloom filter, both bought with the topic 0
price list in hand:

- **FastLocalBloom**: a classic bloom's k probes hit k random cache lines —
  k potential cache misses per lookup. RocksDB's variant confines all k
  probes for a key to **one cache line**: one miss max. The price is paid in
  statistics — a blocked bloom has slightly worse false-positive rate at
  equal bits/key (keys crowd within their line). Sizing is in
  `millibits_per_key` — fleet-scale tuning wants sub-bit granularity.
- **Ribbon filters**: a different construction (linear algebra over the key
  hashes) that is ~30% smaller for the same false-positive rate but much
  slower to *build*; if the equation system fails to solve ("banding
  fails"), it retries with a new seed up to 256 times, then falls back to
  bloom. A pure CPU-for-DRAM knob: spend build-time CPU during compaction,
  save filter memory forever after.

### Step 8 — the MANIFEST: MVCC for metadata

Compaction's final act is swapping files: outputs replace inputs. RocksDB
commits this by appending a **VersionEdit** (a delta record: "add these
files, delete those") to the **MANIFEST** — an append-only log of metadata
changes — then pointing the CURRENT file at it. In-memory, each reader
holds a refcounted **Version** (an immutable snapshot of the file layout);
a compaction publishes a new Version, and readers mid-iteration keep using
their old one until they drop the reference. That is multi-version
concurrency control applied to *metadata*: writers never disturb readers,
and crash recovery replays the edit log. lsm-tree rewrites its whole version
file instead — same atomicity, different scale point: a delta log wins when
you have 100K files, a rewrite wins on simplicity when you have 100.

## Where each step lives in the code

- **Steps 1–2 — `db/compaction/compaction_picker_level.cc`**: score
  formulas in comments :229–233 (`L0: num_files /
  level0_file_num_compaction_trigger`; `L1+: level_bytes /
  MaxBytesForLevel`); `LevelCompactionBuilder::PickCompaction` :531 — setup
  inputs, expand to clean key boundaries, grab the overlapping next-level
  files; :596 — score recomputed accounting for in-flight compactions.
- **Step 3 — `db/compaction/compaction_job.cc:1904`**:
  `ProcessKeyValueCompaction` — the k-way merge plus compaction filters,
  snapshot lists, sub-compaction splitting.
- **Step 4 — `db/column_family.cc:1019–1043`**:
  `GetWriteStallConditionAndCause` — the four triggers in Step 4's order.
- **Step 5 — `table/block_based/block_based_table_builder.cc`**: restart
  interval + delta encoding :1096–1097 (default 16); block flush policy
  :1127 (~4 KB); index entry = last key of each block, written on flush
  :1908–1912, shortened via `FindShortestSeparator`.
- **Step 6 — `table/block_based/block_based_table_reader.cc`**: `Get` :3010
  — whole-table filter check first :3040, then index iterator :3044–3053,
  then data block :3071–3096; block cache probe in `GetDataBlockFromCache`
  :2345. Partitioned index :1778 + `partitioned_index_reader.h:15`.
- **Step 7 — `table/block_based/filter_policy.cc`**:
  `FastLocalBloomBitsBuilder` :365–376 (`millibits_per_key`, one cache line
  per key — contrast lsm-tree's double hashing across the whole bit array);
  ribbon :658–686 (falls back to bloom after 256 seed attempts).
- **Step 8 — `db/version_set.cc`**: `LogAndApply` :6778 — append a
  `VersionEdit` (version_edit.h:37–77, 705–744) to the MANIFEST log, point
  CURRENT at it; readers keep iterating their old refcounted Version.

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

## References

**Code**
- [facebook/rocksdb](https://github.com/facebook/rocksdb) —
  `db/compaction/compaction_picker_level.cc`,
  `db/compaction/compaction_job.cc`, `db/column_family.cc` (stalls),
  `table/block_based/block_based_table_builder.cc`,
  `table/block_based/block_based_table_reader.cc`,
  `table/block_based/filter_policy.cc`, `db/version_set.cc` (MANIFEST).
  Local clone at `~/repos/rocksdb`.
- [fjall-rs/fjall](https://github.com/fjall-rs/fjall)
  `src/keyspace/write_delay.rs` — the 100×-simpler stall valve, for
  contrast.
