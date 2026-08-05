# RocksDB compaction: scores, stalls, and the manifest

The lsm-tree crate gave you the clean shape; RocksDB is what a decade of
production adds on top — score-driven compaction picking, write stalls as
back-pressure, partitioned indexes, ribbon filters, and a MANIFEST that does
MVCC for metadata. Before the guided skim, this chapter builds each addition
as its own concept — what problem it solves and what it costs — then maps
every one to its file and line.

Every anchor is **facebook/rocksdb at `7c80a5a`**, the commit this repo pins
(`tools/pinned-source.py ref rocksdb`); check any of them with
`tools/pinned-source.py show rocksdb <path> -r <range>`. The defaults quoted
throughout are the shipped ones, and each is cited where it is declared, because
almost every number below is a default someone has since re-tuned.

## The problem in one sentence

Compaction is a background job competing with foreground writes for the same
disk: pick the wrong level to compact, or let writers outrun the mergers, and
"compaction debt" grows without bound — L0 accumulates files, every point
read probes all of them, and read latency degrades *forever* unless something
pushes back.

## The concepts, step by step

### Step 1 — compaction debt: why compaction needs a scheduler

> **In:** the LSM shape from the lsm-tree chapter — levels, runs, a flush that
> keeps adding files at the top.
> **Out:** the quantity every later step manipulates: *debt*, measured two ways
> (L0 file count, and bytes over target), which Step 2 turns into a number.

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

The shape the debt accumulates in, at the shipped defaults:

```
 level0_file_num_compaction_trigger = 4       include/rocksdb/options.h:255
 max_bytes_for_level_base           = 256 MB  include/rocksdb/options.h:303
 max_bytes_for_level_multiplier     = 10      include/rocksdb/advanced_options.h:671

 L0   4 files, each ~one memtable, mutually overlapping   ← debt in *files*
 L1   256 MB target                                       ← debt in *bytes*
 L2   2.5 GB
 L3   25 GB
 L4   250 GB
```

Two units, because L0 is a different animal. RocksDB says so itself, in the
comment that opens the scoring function:

```cpp
// db/version_set.cc — inside VersionStorageInfo::ComputeCompactionScore, 4002-4021
  4002      if (level == 0) {
  4003        // We treat level-0 specially by bounding the number of files
  4004        // instead of number of bytes for two reasons:
  4005        //
  4006        // (1) With larger write-buffer sizes, it is nice not to do too
  4007        // many level-0 compactions.
  4008        //
  4009        // (2) The files in level-0 are merged on every read and
  4010        // therefore we wish to avoid too many files when the individual
  4011        // file size is small (perhaps because of a small write-buffer
  4012        // setting, or very high compression ratios, or lots of
  4013        // overwrites/deletions).
  4014        int num_sorted_runs = 0;
  4015        uint64_t total_size = 0;
  4016        for (auto* f : files_[level]) {
  4017          total_downcompact_bytes += static_cast<double>(f->fd.GetFileSize());
  4018          if (!f->being_compacted) {
  4019            total_size += f->compensated_file_size;
  4020            num_sorted_runs++;
  4021          }
  4022        }
```

Reason (2) on line 4009 is the whole reason this topic exists: an L0 file is a
sorted run all by itself, so it is one more place every point read must look.
Note line 4018 too — a file already claimed by a running compaction does not
count as debt, because someone is already paying it.

### Step 2 — score-driven picking: highest debt first

> **In:** Step 1's two debt units, per level.
> **Out:** one `double` per level, sorted descending, and from it a single
> compaction job — the input Step 3 executes.

RocksDB reduces "which level hurts most" to one number per level, the
**score** — how far the level is past its trigger, normalized so scores are
comparable across levels:

- **L0**: `num_sorted_runs / level0_file_num_compaction_trigger`
  (`db/version_set.cc:4077-4078`), where `num_sorted_runs` is the count of L0
  files *not already being compacted*.
- **L1+**: `level_bytes_no_compacting / MaxBytesForLevel(level)`
  (`:4136-4137`), where the numerator again excludes in-flight files
  (`:4129-4134`) and the denominator is the level's target from Step 1.

```cpp
// db/version_set.cc — the L1+ branch of ComputeCompactionScore, 4125-4137
  4125      } else {  // level > 0
  4126        // Compute the ratio of current size to size limit.
  4127        uint64_t level_bytes_no_compacting = 0;
  4128        uint64_t level_total_bytes = 0;
  4129        for (auto f : files_[level]) {
  4130          level_total_bytes += f->fd.GetFileSize();
  4131          if (!f->being_compacted) {
  4132            level_bytes_no_compacting += f->compensated_file_size;
  4133          }
  4134        }
  4135        if (!immutable_options.level_compaction_dynamic_level_bytes) {
  4136          score = static_cast<double>(level_bytes_no_compacting) /
  4137                  MaxBytesForLevel(level);
```

Work it on a concrete tree, at the Step 1 defaults:

```
 L0   8 files, none being compacted   score = 8 / 4          = 2.00
 L1   512 MB, none being compacted    score = 512 / 256      = 2.00
 L2   1.0 GB                          score = 1.0 / 2.5      = 0.40
 L3   30 GB                           score = 30 / 25        = 1.20

 sorted descending: L0 (2.00), L1 (2.00), L3 (1.20), L2 (0.40)
 → pick L0.  Now start one L1 compaction covering 256 MB of L1:
 L1 recomputed = (512 − 256) / 256 = 1.00, and it drops behind L3.
```

That last line is the "subtract in-flight bytes" rule doing its job: without it
the picker would keep choosing L1 and double-book the same files. Two wrinkles
the arithmetic above hides. First, `kScoreScale = 10.0` (`:3996`): under
`level_compaction_dynamic_level_bytes` any score above 1.0 is multiplied by ten
so that the comparison has room to express priorities *within* the "needs
compaction" band — the raw number you read in a log may be 20.0, not 2.0.
Second, the levels are sorted by a **bubble sort** (`:4173-4186`, with RocksDB's
own justification: "the number of entries are small"), and the picker relies on
that order:

```cpp
// db/compaction/compaction_picker_level.cc — inside SetupInitialFiles, 210-235 and 255-259
   210    for (int i = 0; i < compaction_picker_->NumberLevels() - 1; i++) {
   211      start_level_score_ = vstorage_->CompactionScore(i);
   212      start_level_ = vstorage_->CompactionScoreLevel(i);
   213      assert(i == 0 || start_level_score_ <= vstorage_->CompactionScore(i - 1));
   214      if (start_level_score_ >= 1) {
   // ... 215-220: skip LBase if an L0→LBase compaction is already pending ...
   221        output_level_ =
   222            (start_level_ == 0) ? vstorage_->base_level() : start_level_ + 1;
   223        bool picked_file_to_compact = PickFileToCompact();
   // ... 224-227: sync point, then "found the compaction!" ...
   228          if (start_level_ == 0) {
   229            // L0 score = `num L0 files` / `level0_file_num_compaction_trigger`
   230            compaction_reason_ = CompactionReason::kLevelL0FilesNum;
   231          } else {
   232            // L1+ score = `Level files size` / `MaxBytesForLevel`
   233            compaction_reason_ = CompactionReason::kLevelMaxLevelSize;
   234          }
   235          break;
   // ... 236-254: nothing pickable — clear inputs, and for L0 try an intra-L0 merge ...
   255      } else {
   256        // Compaction scores are sorted in descending order, no further scores
   257        // will be >= 1.
   258        break;
   259      }
```

Line **214** is the threshold (`>= 1`, i.e. at or past target) and line **258**
is why the loop can stop early. The two comments on 229 and 232 are the score
formulas restated at the point of use — they are documentation, though; the
arithmetic itself lives in `version_set.cc` as quoted above.

Once a level is picked, `PickCompaction`
(`db/compaction/compaction_picker_level.cc:531-558`) expands the job: other L0
files if this is an L0 job (`:542`), then `SetupOtherInputsIfNeeded` (`:548`)
which pulls in *every* overlapping next-level file — a merge must consume all of
them, or it would create overlap where the level promises disjointness. The last
thing `GetCompaction` does is recompute all the scores (`:596`), because
registering this job just changed every `being_compacted` flag it touched.

One escape hatch worth knowing: when L0 is over its trigger but an L0→LBase
compaction cannot start (one is already running), the picker falls back to
`PickIntraL0Compaction` (`:248-252`, defined at `:924`) — merging L0 files into
*fewer, bigger L0 files*. It moves no data downward and pays no debt, but it
reduces the run count, which is exactly what Step 4 is about to stall on.

### Step 3 — the merge job: mostly not a merge

> **In:** the job Step 2 picked — a set of input files and an output level.
> **Out:** new SST files (Step 5 builds them) and a `VersionEdit` (Step 8
> commits it).

The core of a compaction job is the k-way merge you already read in
lsm-tree (pop the smallest key across k sorted inputs, write output
blocks). What RocksDB wraps around it is the production payload, and the
proportions are the lesson.

**Sub-compaction splitting** comes first: one job's key range is carved into
disjoint sub-ranges and each gets a thread.

```cpp
// db/compaction/compaction_job.cc — inside CompactionJob::RunSubcompactions, 727-741
   727    const size_t num_threads = compact_->sub_compact_states.size();
   728    assert(num_threads > 0);
   729    compact_->compaction->GetOrInitInputTableProperties();
   730
   731    // Launch a thread for each of subcompactions 1...num_threads-1
   732    std::vector<port::Thread> thread_pool;
   733    thread_pool.reserve(num_threads - 1);
   734    for (size_t i = 1; i < compact_->sub_compact_states.size(); i++) {
   735      thread_pool.emplace_back(&CompactionJob::ProcessKeyValueCompaction, this,
   736                               &compact_->sub_compact_states[i]);
   737    }
   738
   739    // Always schedule the first subcompaction (whether or not there are also
   740    // others) in the current thread to be efficient with resources
   741    ProcessKeyValueCompaction(compact_->sub_compact_states.data());
```

`ProcessKeyValueCompaction` (`:1904`) is therefore the per-thread body, and the
first thing it does is not merging: it resolves the **compaction filter** — a
user callback consulted for every key-value pair mid-merge, where TTL expiry
lives — at `:1920-1924`, then builds a `CompactionIterator` (`:1621-1632`)
handing it the **snapshot list** (`job_context_->snapshot_seqs`, `:1623`).

The snapshot list is MVCC reaching into compaction: a key's old version can be
dropped only if no live snapshot might still read it. The rule for *tombstones*
is where RocksDB is strictly cleverer than lsm-tree, and it is worth reading
side by side with `evict_tombstones(is_last_level)`:

```cpp
// db/compaction/compaction_iterator.cc — the early tombstone drop, 1152-1159 and 1164-1170
  1152      } else if (compaction_ != nullptr &&
  1153                 (ikey_.type == kTypeDeletion ||
  1154                  (ikey_.type == kTypeDeletionWithTimestamp &&
  1155                   cmp_with_history_ts_low_ < 0)) &&
  1156                 !compaction_->allow_ingest_behind() &&
  1157                 DefinitelyInSnapshot(ikey_.sequence, earliest_snapshot_) &&
  1158                 compaction_->KeyNotExistsBeyondOutputLevel(ikey_.user_key,
  1159                                                            &level_ptrs_)) {
   // ... 1160-1163: a TODO about this being the only use of compaction_ ...
  1164        // For this user key:
  1165        // (1) there is no data in higher levels
  1166        // (2) data in lower levels will have larger sequence numbers
  1167        // (3) data in layers that are being compacted here and have
  1168        //     smaller sequence numbers will be dropped in the next
  1169        //     few iterations of this loop (by rule (A) above).
  1170        // Therefore this deletion marker is obsolete and can be dropped.
```

Line **1158** is the difference. lsm-tree drops a tombstone only when the output
*is* the bottom level, because that is the cheap sufficient condition. RocksDB
asks the sharper question — `KeyNotExistsBeyondOutputLevel`, "does this user key
appear in any level below the output?" — and can therefore retire a tombstone at
L2 if nothing below L2 holds that key, freeing the space several levels earlier.
The bottommost case is still handled separately at `:1188-1191`. Same
correctness argument (never resurrect data), a tighter test, and one more
`level_ptrs_` cursor to maintain.

Skim `ProcessKeyValueCompaction` for shape rather than detail; the lesson is how
much of an industrial compaction is *not* the merge.

### Step 4 — write stalls: back-pressure as a feature

> **In:** the debt Step 1 measured and Step 2 is failing to pay down fast
> enough.
> **Out:** a `WriteStallCondition` — `kNormal`, `kDelayed` or `kStopped` —
> applied to foreground writers.

A write stall is the engine deliberately slowing or stopping foreground
writes because compaction has fallen behind. It sounds like a bug; it is
load-shedding. Without it, debt is unbounded: L0 grows to hundreds of files
and *every read* pays for it, indefinitely. The stall converts an unbounded
read-amplification problem into a bounded write-latency problem.

The whole policy is one if-else chain, and reading it in order matters, because
the order *is* the priority — the first matching condition wins:

```cpp
// db/column_family.cc — ColumnFamilyData::GetWriteStallConditionAndCause, 1016-1045
  1016    if (num_unflushed_memtables >= mutable_cf_options.max_write_buffer_number) {
  1017      return {WriteStallCondition::kStopped, WriteStallCause::kMemtableLimit};
  1018    } else if (!mutable_cf_options.disable_auto_compactions &&
  1019               num_l0_files >= mutable_cf_options.level0_stop_writes_trigger) {
  1020      return {WriteStallCondition::kStopped, WriteStallCause::kL0FileCountLimit};
  1021    } else if (!mutable_cf_options.disable_auto_compactions &&
  1022               mutable_cf_options.hard_pending_compaction_bytes_limit > 0 &&
  1023               num_compaction_needed_bytes >=
  1024                   mutable_cf_options.hard_pending_compaction_bytes_limit) {
  1025      return {WriteStallCondition::kStopped,
  1026              WriteStallCause::kPendingCompactionBytes};
   // ... 1027-1032: memtable number − 1 → kDelayed ...
  1033    } else if (!mutable_cf_options.disable_auto_compactions &&
  1034               mutable_cf_options.level0_slowdown_writes_trigger >= 0 &&
  1035               num_l0_files >=
  1036                   mutable_cf_options.level0_slowdown_writes_trigger) {
  1037      return {WriteStallCondition::kDelayed, WriteStallCause::kL0FileCountLimit};
  1038    } else if (!mutable_cf_options.disable_auto_compactions &&
  1039               mutable_cf_options.soft_pending_compaction_bytes_limit > 0 &&
  1040               num_compaction_needed_bytes >=
  1041                   mutable_cf_options.soft_pending_compaction_bytes_limit) {
  1042      return {WriteStallCondition::kDelayed,
  1043              WriteStallCause::kPendingCompactionBytes};
  1044    }
  1045    return {WriteStallCondition::kNormal, WriteStallCause::kNone};
```

Six conditions, not four, and three causes × two severities:

| condition | cause | threshold | default |
|---|---|---|---|
| stop | memtable limit | unflushed memtables ≥ `max_write_buffer_number` | 2 (`advanced_options.h:271`) |
| stop | L0 file count | ≥ `level0_stop_writes_trigger` | 36 (`:554`) |
| stop | pending bytes | ≥ `hard_pending_compaction_bytes_limit` | 256 GB (`:717`) |
| delay | memtable limit | ≥ `max_write_buffer_number − 1`, only if that is > 3 | — (`column_family.cc:1027-1031`) |
| delay | L0 file count | ≥ `level0_slowdown_writes_trigger` | 20 (`advanced_options.h:547`) |
| delay | pending bytes | ≥ `soft_pending_compaction_bytes_limit` | 64 GB (`:709`) |

Two things to take from the table. The stalls are tested **stop-first**: the
chain checks the severe conditions before the mild ones, so a database at 40 L0
files reports `kStopped`, never `kDelayed`. And the L0 numbers frame the whole
topic — compaction is triggered at 4 files, writers are slowed at 20 and stopped
at 36, so the design intent is that a read never probes more than a few dozen L0
runs. Put the lsm-tree chapter's filter arithmetic on that: at 0.844% false
positives per run, 4 L0 runs plus 6 levels cost 0.084 wasted block reads per
absent key, while 36 L0 runs plus 6 levels cost 0.35 — four times the wasted IO,
which is the badness the stop trigger exists to bound.

Compare fjall's version of the same valve, which is the entire file:

```rust
// fjall src/keyspace/write_delay.rs — the whole valve, 5-16 (fjall-rs/fjall@80cf6bc)
     5  const STEP_SIZE: usize = 10_000;
     6  const THRESHOLD: usize = 20;
     7
     8  pub fn perform_write_stall(l0_runs: usize) {
     9      if let THRESHOLD..30 = l0_runs {
    10          let d = l0_runs - THRESHOLD;
    11
    12          for _ in 0..(d * STEP_SIZE) {
    13              std::hint::black_box(());
    14          }
    15      }
    16  }
```

Same idea — slow the writer in proportion to how far past 20 runs L0 is — and
100× simpler, but read line 9 carefully: the range pattern `THRESHOLD..30` is
*exclusive*, so a keyspace at 30 or more L0 runs gets **no delay at all**. fjall
has the delay valve and not the stop valve; RocksDB's `kStopped` tier is the
part that makes the bound actually a bound. Stalls are the honest choice, and
the hard stop is the half people leave out.

### Step 5 — building the SST: the same block tricks, plus shortened separators

> **In:** the merged key-value stream from Step 3.
> **Out:** a block-based SST — data blocks, an index of shortened separators
> (Step 6 searches it), a filter (Step 7 builds it).

RocksDB's table builder is lsm-tree's Step 2-3 with the serial numbers still
visible. The constants are identical, and not by convergent evolution — LevelDB
is the shared ancestor:

```
 block_size                   = 4 * 1024   include/rocksdb/table.h:400
 block_restart_interval       = 16         include/rocksdb/table.h:413
 index_block_restart_interval = 1          include/rocksdb/table.h:416
 metadata_block_size          = 4096       include/rocksdb/table.h:423
```

Restart interval and delta encoding are wired into the data-block builder at
`table/block_based/block_based_table_builder.cc:1096-1097`, and the flush
decision is delegated to a policy object at `:1126-1128`.

The extra trick is in the index. RocksDB does not store each block's last key
verbatim; it stores the shortest string that still *separates* one block from
the next, and it explains itself at the call site:

```cpp
// table/block_based/block_based_table_builder.cc — inside WriteBlock's caller, 1901-1912
  1901    if (LIKELY(ok())) {
  1902      // We do not emit the index entry for a block until we have seen the
  1903      // first key for the next data block.  This allows us to use shorter
  1904      // keys in the index block.  For example, consider a block boundary
  1905      // between the keys "the quick brown fox" and "the who".  We can use
  1906      // "the r" as the key for the index block entry since it is >= all
  1907      // entries in the first block and < all entries in subsequent
  1908      // blocks.
  1909      r->index_builder->AddIndexEntry(
  1910          last_key_in_current_block, first_key_in_next_block, r->pending_handle,
  1911          &r->index_separator_scratch, skip_delta_encoding);
  1912    }
```

Work RocksDB's own example through the algorithm in `util/comparator.cc:42-101`,
which is four lines of real logic:

```
 start = "the quick brown fox"   (19 bytes)
 limit = "the who"

 1. common prefix scan (:47-50)      → diff_index = 4  ('q' vs 'w')
 2. start_byte = 'q' = 0x71, limit_byte = 'w' = 0x77   (:55-56)
 3. start_byte < limit_byte, and 0x71 + 1 = 0x72 < 0x77  (:57, :64)
 4. so: (*start)[4]++  → 'r';  resize to diff_index + 1 = 5   (:65-66)

 separator = "the r"   (5 bytes)   →  19 − 5 = 14 bytes saved, 74% of the entry
```

The guard on line 64 is the case people get wrong: if incrementing the byte
would reach `limit` exactly, the code cannot use it and walks forward looking
for the first non-`0xFF` byte instead (`:67-80`). And `FindShortestSeparator`
operates on *user* keys, so the index builder has to tack a sequence number back
on afterwards to keep the internal-key ordering valid
(`table/block_based/index_builder.cc:78-101`, the fixup at `:89-97`).

Shorter separators ⇒ smaller index ⇒ more index resident in cache. That is
SQLite's interior-page separator idea rediscovered — the truncation topic 3
experimented with — and it is what makes Step 6's problem tractable at all.

### Step 6 — the read path at scale: partitioning the index

> **In:** the SST Step 5 built, and one key.
> **Out:** at most one data-block read, having consulted a filter and a
> possibly two-level index.

A point read runs filter → index → data block, same as lsm-tree:

```cpp
// table/block_based/block_based_table_reader.cc — inside BlockBasedTable::Get, 3039-3053 and 3071
  3039    const bool may_match =
  3040        FullFilterKeyMayMatch(filter, key, prefix_extractor, get_context,
  3041                              &lookup_context, read_options);
  3042    TEST_SYNC_POINT("BlockBasedTable::Get:AfterFilterMatch");
  3043    if (may_match) {
  3044      IndexBlockIter iiter_on_stack;
   // ... 3045-3050: disable BlockPrefixIndex if the prefix extractor changed ...
  3051      auto iiter =
  3052          NewIndexIterator(read_options, need_upper_bound_check, &iiter_on_stack,
  3053                           get_context, &lookup_context);
   // ... 3054-3070: iterator ownership, timestamp size, blob scratch ...
  3071      for (iiter->Seek(key); iiter->Valid() && !done; iiter->Next()) {
```

Line **3043** is the same gate as lsm-tree's `return Ok(None)`: no index
iterator is even constructed for a filtered-out key. The data block itself is
fetched through `NewDataBlockIterator` (`:3092-3096`), which goes to the **block
cache** — a shared in-memory cache of uncompressed blocks — before touching disk
(`GetDataBlockFromCache`, `:2010`, called at `:2345`).

The scale-driven change is the index. Size it from Step 5's numbers: a 256 MB
SST at 4 KB blocks has ~65,500 data blocks, so with a 20-byte shortened
separator and a handle per entry the index block is on the order of 2 MB — one
allocation, one cache entry, all-or-nothing. RocksDB's fix is the **partitioned
index**: cut that index into ~4 KB partitions (`metadata_block_size`,
`table.h:423`) and build an index *over the index*.

```cpp
// table/block_based/partitioned_index_reader.h — the two-level reader, 14-15 and 27-28
    14  // Index that allows binary search lookup in a two-level index structure.
    15  class PartitionIndexReader : public BlockBasedTable::IndexReaderCommon {
   ...
    27    // return a two-level iterator: first level is on the partition index
    28    InternalIteratorBase<IndexValue>* NewIterator(
```

Only the small top level is pinned; partitions are loaded on demand through the
same block cache (`block_based_table_reader.cc:1776-1780` decides the pinning
for `kTwoLevelIndexSearch`). A point read now costs one top-level binary search
plus one partition lookup, in exchange for never having to hold 2 MB of index
resident to answer a single key.

The academic alternative — fractional cascading, threading search hints from one
level's index into the next — never shipped: plain binary search per level won
in practice, because it composes with a cache and cascading does not.

### Step 7 — filters, industrialized: cache-local bloom and ribbon

> **In:** the keys of one SST as Step 5 writes them.
> **Out:** a filter block whose false-positive rate and build cost are now both
> tuning knobs — the gate Step 6 checks at line 3039.

Two upgrades over the textbook bloom filter, both bought with the topic 0
price list in hand.

**FastLocalBloom** (`table/block_based/filter_policy.cc:365-377`) fixes a memory
problem, not a math one. A classic bloom's k probes hit k random positions in a
multi-megabyte bit array — up to k cache misses per lookup, and the lsm-tree
chapter's double-hashing loop is exactly that shape. RocksDB confines all k
probes for a key to **one 64-byte cache line** — one miss, maximum. The price is
paid in statistics, and the implementation states it to three decimal places:

```
// util/bloom_impl.h:105-108, for 10 bits/key and num_probes = 6

  theoretical best, cache-local, 512-bit bucket     0.9535%
  this implementation                               0.957%
  LegacyLocalityBloomImpl<false>                    1.138%
  1024-bit buckets (some ARM cache lines)           0.951%
```

Set that against the *non*-local optimum for the same budget — 0.844% at k = 6,
the number the lsm-tree chapter computed for fjall's filter at the same 10
bits/key — and the trade is exact: **0.844% → 0.957% false positives, about 13%
relatively worse, in exchange for 1 cache miss instead of up to 6.** Sizing is
in `millibits_per_key` (`filter_policy.cc:369`, asserted ≥ 1000 at `:376`),
because fleet-scale tuning wants sub-bit granularity.

**Ribbon filters** (`Standard128RibbonBitsBuilder`, `:658`) attack the space
instead. The construction solves a linear system over the key hashes rather than
setting independent bits, and the header states the trade:

```
// include/rocksdb/filter_policy.h:169-173

  "saves about 30% space compared to Bloom filters, with similar query times
   but roughly 3-4x CPU time and 3x temporary space usage during construction.
   For example, if you pass in 10 for bloom_equivalent_bits_per_key, you'll get
   the same 0.95% FP rate as Bloom filter but only using about 7 bits per key."
```

10 bits/key → 7 bits/key at an unchanged 0.95% false-positive rate: a pure
CPU-for-DRAM trade, spend build-time CPU during compaction and save filter
memory forever after. Two production details make it usable. Solving can fail,
so the builder re-seeds — 256 times, then gives up and builds a bloom instead:

```cpp
// table/block_based/filter_policy.cc — inside Standard128RibbonBitsBuilder::Finish, 751-762
   751      bool success = banding.ResetAndFindSeedToSolve(
   752          num_slots, hash_entries_info_.entries.begin(),
   753          hash_entries_info_.entries.end(),
   754          /*starting seed*/ entropy & 255, /*seed mask*/ 255);
   755      if (!success) {
   756        ROCKS_LOG_WARN(
   757            info_log_, "Too many re-seeds (256) for Ribbon filter, %llu / %llu",
   758            static_cast<unsigned long long>(hash_entries_info_.entries.size()),
   759            static_cast<unsigned long long>(num_slots));
   760        SwapEntriesWith(&bloom_fallback_);
   761        assert(hash_entries_info_.entries.empty());
   762        return bloom_fallback_.Finish(buf, status);
```

And the choice is made **per level**, not per database: `bloom_before_level = 0`
by default, meaning bloom for flushes (L0) and ribbon everywhere below
(`filter_policy.h:184-185`, with the rationale at `:175-181` — bloom's speed
where the data is hot and short-lived, ribbon's density where it is not). That
is the same "spend the budget unevenly across levels" instinct Monkey formalises
in the next chapter, applied to filter *construction* rather than filter *size*.

### Step 8 — the MANIFEST: MVCC for metadata

> **In:** the output files Step 3 produced and the input files it consumed.
> **Out:** a durably committed new `Version`, with readers still safely using
> the old one.

Compaction's final act is swapping files: outputs replace inputs. The vocabulary
is defined in one comment:

```cpp
// db/version_edit.h — the definition, 701-708
   701  // The state of a DB at any given time is referred to as a Version.
   702  // Any modification to the Version is considered a Version Edit. A Version is
   703  // constructed by joining a sequence of Version Edits. Version Edits are written
   704  // to the MANIFEST file.
   705  class VersionEdit {
   706   public:
   707    // Retrieve the table files added as well as their associated levels.
   708    using NewFiles = std::vector<std::pair<int, FileMetaData>>;
```

A `VersionEdit` really is just "add these files, delete those": the two tags are
`kDeletedFile = 6` and `kNewFile = 7` in the serialization enum
(`db/version_edit.h:37-47`, with `kNewFile4 = 103` as the current format,
`:52`). `VersionSet::LogAndApply` (`db/version_set.cc:6778`) is the commit
entry point, and the durable part is two lines inside `ProcessManifestWrites`:

```cpp
// db/version_set.cc — inside VersionSet::ProcessManifestWrites, 6500 and 6508-6513, 6527-6530
  6500          io_s = raw_desc_log_ptr->AddRecord(write_options, record);
   ...
  6508        if (s.ok()) {
  6509          io_s =
  6510              SyncManifest(db_options_, write_options, raw_desc_log_ptr->file());
  6511          manifest_io_status = io_s;
   // ... 6512-6513: sync point callback ...
   ...
  6527      if (s.ok() && new_descriptor_log) {
  6528        io_s = SetCurrentFile(
  6529            write_options, fs_.get(), dbname_, pending_manifest_file_number_,
  6530            file_options_.temperature, dir_contains_current_file);
```

Read the condition on **6527**: CURRENT is rewritten only when a *new* MANIFEST
file was started. The steady-state commit is one appended record (6500) plus one
fsync (6509) — CURRENT keeps pointing at the same log. That is the delta design
paying off: metadata cost is proportional to the change, not to the database.

In memory, the swap is refcounting:

```cpp
// db/version_set.cc — inside VersionSet::AppendVersion, 6093-6108
  6093    // Make "v" current
  6094    assert(v->refs_ == 0);
  6095    Version* current = column_family_data->current();
  6096    assert(v != current);
  6097    if (current != nullptr) {
  6098      assert(current->refs_ > 0);
  6099      current->Unref();
  6100    }
  6101    column_family_data->SetCurrent(v);
  6102    v->Ref();
  6103
  6104    // Append to linked list
  6105    v->prev_ = column_family_data->dummy_versions()->prev_;
  6106    v->next_ = column_family_data->dummy_versions();
  6107    v->prev_->next_ = v;
  6108    v->next_->prev_ = v;
```

The old version is *un*referenced, not deleted — `Version::Unref` frees only at
zero (`:4943-4951`) — and every live version stays on the doubly-linked list at
6104-6108. A reader mid-iteration holds a reference and keeps reading the file
layout it started with, while compaction publishes a new one beside it. That is
multi-version concurrency control applied to *metadata*: writers never disturb
readers, and crash recovery replays the edit log.

lsm-tree writes a whole new version file instead (`persist.rs:16-17`) — same
atomicity, different scale point: a delta log wins when you have 100K files, a
snapshot wins on simplicity when you have 100.

## Where each step lives in the code

Every line number is `7c80a5a`.

- **Steps 1-2 — scoring, `db/version_set.cc`**: `ComputeCompactionScore` at
  `:3983`; `kScoreScale = 10.0` at `:3996`; the L0 justification and
  `num_sorted_runs` at `:4002-4021`; the L0 formula at `:4077-4078`; the L1+
  formula and its in-flight exclusion at `:4125-4137`; the bubble sort at
  `:4173-4186`; `MaxBytesForLevel` at `:5354-5360`. Defaults:
  `include/rocksdb/options.h:255` (L0 trigger 4), `:303` (256 MB base),
  `include/rocksdb/advanced_options.h:671` (multiplier 10).
- **Step 2 — picking, `db/compaction/compaction_picker_level.cc`**:
  `SetupInitialFiles` `:207-260` (threshold at `:214`, the two score comments at
  `:229`/`:232`, the descending-order break at `:255-258`, the intra-L0 fallback
  at `:248-252`); `PickCompaction` `:531-558`; `SetupOtherInputsIfNeeded`
  `:481`; the score recompute after registering a job, `:590-597`.
- **Step 3 — merging, `db/compaction/`**: `compaction_job.cc:725-746`
  (sub-compaction fan-out), `:1904` (`ProcessKeyValueCompaction`), `:1920-1924`
  (compaction filter), `:1621-1632` (`CompactionIterator` with the snapshot
  list). Decisions: `compaction_iterator.cc:356` and `:600-630`
  (`kRemove` / `kChangeValue` / `kRemoveAndSkipUntil`); tombstone drops at
  `:1152-1187` (early, via `KeyNotExistsBeyondOutputLevel`) and `:1188-1191`
  (bottommost).
- **Step 4 — stalls, `db/column_family.cc:1010-1046`**:
  `GetWriteStallConditionAndCause`, all six branches. Defaults in
  `include/rocksdb/advanced_options.h`: `:271`, `:547`, `:554`, `:709`, `:717`.
  Contrast: `fjall src/keyspace/write_delay.rs:5-16` at `80cf6bc`.
- **Step 5 — building, `table/block_based/`**:
  `block_based_table_builder.cc:1096-1097` (restart interval + delta encoding),
  `:1126-1128` (flush policy), `:1901-1912` (index entry, with the "the r"
  example); `index_builder.cc:78-101` (`FindShortestInternalKeySeparator`);
  `util/comparator.cc:42-101` (the byte-level algorithm). Defaults:
  `include/rocksdb/table.h:400`, `:413`, `:416`, `:423`.
- **Step 6 — reading, `table/block_based/block_based_table_reader.cc`**: `Get`
  `:3010`; filter check `:3039-3042`; index iterator `:3044-3053`; the block
  loop `:3071-3096`; block cache `:2010` and `:2345`; two-level pinning
  `:1776-1780`. Partitioned index: `partitioned_index_reader.h:14-15`, `:27-28`.
- **Step 7 — filters, `table/block_based/filter_policy.cc`**:
  `FastLocalBloomBitsBuilder` `:365-377` (`millibits_per_key`);
  `Standard128RibbonBitsBuilder` `:658-672`; the 256-reseed fallback
  `:751-762`. The numbers: `util/bloom_impl.h:99-131`,
  `include/rocksdb/filter_policy.h:169-205`.
- **Step 8 — committing, `db/version_set.cc`**: `LogAndApply` `:6778`;
  `ProcessManifestWrites` `:6111`, with `AddRecord` `:6500`, `SyncManifest`
  `:6509`, `SetCurrentFile` guarded by `new_descriptor_log` `:6527-6530`;
  `AppendVersion` `:6082-6109`; `Version::Ref`/`Unref` `:4941-4951`. Format:
  `db/version_edit.h:37-78` (tags), `:701-708` (the definition).

## Questions to answer in notes.md

1. Why does leveled compaction pick by *score* rather than round-robin?
   Construct a workload where round-robin lets one level grow unboundedly, and
   check it against `SetupInitialFiles`' early break at `:255-258` — what does
   the descending sort guarantee that a round-robin scan cannot?
2. Partitioned index vs lsm-tree's per-block hash index — both attack "index
   too big for cache". Which helps point reads, which helps scans, why? Size
   both for the 256 MB / 4 KB-block SST in Step 6.
3. FastLocalBloom does k probes in one cache line — Step 7 puts the cost at
   0.844% → 0.957% at 10 bits/key. Using topic 0's price list, how many extra
   *nanoseconds* of avoided cache misses does that 0.113-point FPR increase buy
   back, and at what filter size does the trade stop paying?

## Done when

Answer each before unfolding it.

- [ ] You can compute both compaction scores from a level's state, and say which files are excluded from the numerator and why.

  <details><summary>Answer</summary>

  L0: `num_sorted_runs / level0_file_num_compaction_trigger`
  (`db/version_set.cc:4077-4078`, trigger defaults to 4,
  `include/rocksdb/options.h:255`). L1+:
  `level_bytes_no_compacting / MaxBytesForLevel(level)` (`:4136-4137`, with L1
  at `max_bytes_for_level_base` = 256 MB and each level 10× the last).

  Both numerators exclude files with `being_compacted` set — the `if
  (!f->being_compacted)` guards at `:4018` and `:4131`. The reason is that a
  score is a claim about *outstanding* work: a file already assigned to a running
  job will be paid for shortly, and counting it again would make the picker
  choose the same level repeatedly and double-book its files. This is also why
  `GetCompaction` recomputes every score immediately after registering a new job
  (`compaction_picker_level.cc:590-597`).

  Worked: 8 L0 files → 2.00; L1 holding 512 MB → 2.00; start one L1 job covering
  256 MB and L1 drops to (512−256)/256 = 1.00. Under
  `level_compaction_dynamic_level_bytes`, scores above 1.0 are additionally
  multiplied by `kScoreScale = 10.0` (`:3996`), so logs show 20.0 where this
  arithmetic gives 2.0.

  </details>

- [ ] You can list all six write-stall conditions with their defaults, and say what order they are tested in.

  <details><summary>Answer</summary>

  From `db/column_family.cc:1016-1045`, in the code's own order — stops first,
  then delays:

  1. unflushed memtables ≥ `max_write_buffer_number` (2) → **stop**
  2. L0 files ≥ `level0_stop_writes_trigger` (36) → **stop**
  3. pending compaction bytes ≥ `hard_pending_compaction_bytes_limit` (256 GB) →
     **stop**
  4. unflushed memtables ≥ `max_write_buffer_number − 1`, only when that option
     is > 3 → **delay**
  5. L0 files ≥ `level0_slowdown_writes_trigger` (20) → **delay**
  6. pending compaction bytes ≥ `soft_pending_compaction_bytes_limit` (64 GB) →
     **delay**

  Defaults at `include/rocksdb/advanced_options.h:271`, `:554`, `:717`, `:547`,
  `:709`. Because it is a single if-else chain, the order is the priority: a
  database at 40 L0 files matches condition 2 and reports `kStopped`, never
  `kDelayed`. Three causes (memtable limit, L0 file count, pending compaction
  bytes) × two severities, and the pending-bytes pair is the only one that
  measures debt in bytes rather than in objects.

  </details>

- [ ] You can explain `LogAndApply`'s refcounted-Version scheme as "MVCC for metadata", including what is *not* rewritten on a normal commit.

  <details><summary>Answer</summary>

  A `Version` is the immutable file layout; a `VersionEdit` is a delta against it
  (`db/version_edit.h:701-704`), serialized with `kDeletedFile = 6` and
  `kNewFile = 7` tags (`:43-44`). `LogAndApply` (`db/version_set.cc:6778`) routes
  into `ProcessManifestWrites`, which appends the record (`:6500`) and fsyncs the
  MANIFEST (`:6509`).

  The thing that is *not* rewritten is CURRENT: `SetCurrentFile` runs only under
  `if (s.ok() && new_descriptor_log)` (`:6527`), i.e. only when a fresh MANIFEST
  file was started. A steady-state compaction commit is one appended record plus
  one fsync.

  In memory, `AppendVersion` (`:6082-6109`) unrefs the outgoing current version,
  installs the new one and refs it (6097-6102), then links it into a
  doubly-linked list of live versions (6104-6108). `Version::Unref` frees only
  when the count hits zero (`:4943-4951`), so a reader that took a reference
  before the swap keeps reading its own snapshot of the file layout, and the
  files it names cannot be deleted underneath it. Writers never block readers,
  and every historical version is reconstructible by replaying edits — the same
  two properties MVCC gives to data.

  </details>

- [ ] You can walk `FindShortestSeparator` on a concrete key pair and say how many bytes it saves.

  <details><summary>Answer</summary>

  Using RocksDB's own example from `block_based_table_builder.cc:1904-1908` and
  the algorithm at `util/comparator.cc:42-101`: with
  `start = "the quick brown fox"` (the last key of one block) and
  `limit = "the who"` (the first key of the next), the common-prefix scan
  (`:47-50`) stops at `diff_index = 4`, where `'q'` (0x71) meets `'w'` (0x77).
  Since `start_byte < limit_byte` and `0x71 + 1 = 0x72` is still below 0x77
  (`:57`, `:64`), the code increments byte 4 to `'r'` and truncates to 5 bytes
  (`:65-66`). The index entry becomes `"the r"` — 19 bytes down to 5, a 74%
  saving on that entry, and it is still ≥ every key in the first block and <
  every key in the next.

  The branch at `:64` is the interesting one: when incrementing would land
  exactly on `limit`, no single-byte bump is legal, and the code walks forward to
  the first non-`0xFF` byte instead (`:67-80`). Separately, because this operates
  on user keys, the index builder must re-append a sequence number afterwards to
  keep internal-key ordering valid (`index_builder.cc:89-97`).

  The payoff is that a smaller index is a more cacheable index — which is the
  same lever Step 6 pulls again, one level up, by partitioning it.

  </details>

- [ ] You can state the price RocksDB pays for a cache-local bloom filter, in false-positive rate, and for a ribbon filter, in build cost.

  <details><summary>Answer</summary>

  Cache-local bloom: at 10 bits/key with 6 probes, `util/bloom_impl.h:105-108`
  records the theoretical best for a 512-bit (one cache line) bucket as 0.9535%,
  this implementation at about 0.957%, and the older
  `LegacyLocalityBloomImpl<false>` at 1.138%. The non-local textbook optimum for
  the same 10 bits/key and k = 6 is 0.844% — the figure the lsm-tree chapter
  computed for fjall's filter. So locality costs roughly 0.113 percentage points,
  about 13% relatively more false positives, and buys at most one cache miss per
  lookup instead of up to six.

  Ribbon: `include/rocksdb/filter_policy.h:169-173` claims about 30% space saving
  at equal false-positive rate — 10 bloom-equivalent bits/key becomes about 7
  actual bits/key at the same 0.95% — for "roughly 3-4x CPU time and 3x temporary
  space usage during construction", plus a 3 GB-vs-1 GB temporary-memory example
  for a 100 M-key filter (`:200-203`). Construction can also fail outright: the
  builder tries 256 seeds (`filter_policy.cc:751-754`) and falls back to bloom
  (`:760-762`). Hence the default `bloom_before_level = 0` (`filter_policy.h:184`):
  bloom for flushes, ribbon for everything below.

  </details>

## References

**Code**
- [facebook/rocksdb](https://github.com/facebook/rocksdb), pinned at `7c80a5a`.
  Read in the step order above; budget ~3 h and skim `compaction_job.cc` rather
  than reading it.
- [fjall-rs/fjall](https://github.com/fjall-rs/fjall) at `80cf6bc` —
  `src/keyspace/write_delay.rs` for the 100×-simpler stall valve.

| File | Lines | What |
|------|-------|------|
| `db/version_set.cc` | 3983-4186 | `ComputeCompactionScore`: both formulas, in-flight exclusion, `kScoreScale`, the bubble sort |
| `db/compaction/compaction_picker_level.cc` | 207-260, 531-558, 590-597 | pick highest score ≥ 1, expand inputs, then rescore |
| `db/compaction/compaction_job.cc` | 725-746, 1621-1632, 1904 | sub-compactions, snapshot list, the per-thread merge |
| `db/compaction/compaction_iterator.cc` | 1152-1191 | tombstones dropped early when no key exists below the output level |
| `db/column_family.cc` | 1010-1046 | six stall conditions, stops tested before delays |
| `table/block_based/block_based_table_builder.cc` | 1096-1097, 1901-1912 | restart interval 16, and the shortened index separator |
| `util/comparator.cc` | 42-101 | `FindShortestSeparator`, byte by byte |
| `table/block_based/block_based_table_reader.cc` | 3010, 3039-3096 | filter gate, index iterator, data-block loop |
| `table/block_based/partitioned_index_reader.h` | 14-15, 27-28 | two-level index, top level pinned |
| `table/block_based/filter_policy.cc` | 365-377, 658-672, 751-762 | cache-local bloom, ribbon, 256 re-seeds then fall back |
| `util/bloom_impl.h` | 99-131 | the exact FPR cost of confining k probes to one cache line |
| `include/rocksdb/filter_policy.h` | 169-205 | ribbon's 30% / 3-4× CPU trade, and `bloom_before_level` |
| `db/version_edit.h` | 37-78, 701-708 | `kNewFile`/`kDeletedFile`, and the definition of a Version |
