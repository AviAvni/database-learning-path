# An LSM you can read whole: the lsm-tree crate

Every LSM concept in this topic — restart-point block encoding, bloom-gated
point reads, versioned level metadata, pluggable compaction — exists as a few
hundred readable lines in fjall's `lsm-tree` crate. Topic 1 read fjall's
keyspace layer; everything LSM-shaped delegates here, and the crate is small
enough to read completely. Before you open it, this chapter builds the whole
machine one layer at a time — block, table, filter, level, compaction, read
path — then hands you the file and line anchors to watch each layer in code.

Every anchor below is **fjall-rs/lsm-tree at `8526dd3`**, the commit this repo
pins (`tools/pinned-source.py ref lsm-tree`), quoted with the line numbers the
code occupies in that revision. One naming note before you start: the crate
calls a flushed file a **table** (`src/table/`, `TableId`), not a *segment* —
that is the same object RocksDB calls an SST, and the topic README's "segment"
is the older vocabulary. This chapter uses the crate's word.

## The problem in one sentence

Absorb writes at sequential-disk speed by only ever appending sorted files —
and then keep a point read from having to search *dozens* of those files: a
naive pile of 100 flushed tables means up to 100 disk probes per `get`, and
every mechanism in this crate exists to push that back toward 1.

## The concepts, step by step

### Step 1 — the shape of the machine: buffer, flush, merge

> **In:** nothing yet — this step fixes the three verbs and the four words
> (memtable, table, tombstone, amplification) every later step leans on.
> **Out:** a stream of sorted key-value pairs arriving at a file writer, which
> is exactly what Step 2 encodes.

An LSM engine never modifies data on disk. It buffers writes in a sorted
in-memory structure — the **memtable**, your topic 2 skiplist — and when that
fills (fjall's default is 64 MiB, `fjall src/keyspace/options.rs:91`) it writes
the contents out as one immutable sorted file, the **table**. Deletes are
writes too: a **tombstone** is a key stored with the marker "this key is
deleted" instead of a value, because you cannot erase from files you never
modify. The crate's `ValueType` enum has four variants, two of which are
tombstones:

```rust
// src/value_type.rs — the on-disk marker byte, 5-22 (the `is_tombstone` test is 27-29)
     5  /// Value type (regular value or tombstone)
     6  #[derive(Copy, Clone, Debug, Eq, PartialEq)]
     7  #[cfg_attr(test, derive(strum::EnumIter))]
     8  pub enum ValueType {
     9      /// Existing value
    10      Value,
    11
    12      /// Deleted value
    13      Tombstone,
    14
    15      /// "Weak" deletion (a.k.a. `SingleDelete` in `RocksDB`)
    16      WeakTombstone,
    17
    18      /// Value pointer
    19      ///
    20      /// Points to a blob in a blob file.
    21      Indirection = 4,
    22  }
```

Background **compaction** then merges accumulated tables into fewer, bigger
ones so reads stay bounded. Everything below is one of those three verbs —
buffer, flush, merge — made concrete.

Three cost words, because the rest of the topic argues in them. **Write
amplification** is bytes physically written to disk per byte of user data.
**Read amplification** is places consulted per lookup. **Space amplification**
is bytes on disk per byte of live data. They trade against each other, and the
trade is the topic: this crate's leveled strategy documents itself as "high
write amplification, decent read amplification and great space amplification
(~1.1x)" at `src/compaction/leveled/mod.rs:119`.

The measured version of that claim, from this repo rather than from the crate's
doc comment: topic 1's lane writes 1.08 M records of 100 B each — 108,000,000 B
logical — through fjall and through redb, and reports **48,429,915 B on disk
for fjall against 6,833,917,952 B for redb**, space amp **0.45× vs 63.28×**
([FINDINGS.md](../../FINDINGS.md) row 1). The LSM lands *below* 1.0 because
Step 2's block compression is on; the copy-on-write B-tree lands at 63× because
1080 random-key commits each copy a root-to-leaf path. That is the machine this
chapter takes apart.

The cost baked into the shape: a key can now exist in several places at once
(memtable and several tables), so every read must consult them *newest first*
and take the first hit. Steps 4–7 are all about making "several places" cheap.

### Step 2 — the block: prefix truncation against a restart head

> **In:** the sorted key-value stream from Step 1, as the table writer receives
> it.
> **Out:** one ~4 KB compressed, checksummed block, plus the offset of each
> restart head — the unit Step 3 assembles into a table.

A table's data is cut into **blocks**: ~4 KB chunks that are the unit of IO,
checksum and compression (`BlockSizePolicy::all(4_096)`,
`src/config/mod.rs:261`). Inside a block, sorted neighbours share prefixes, so
most entries store `shared_prefix_len + rest` instead of the whole key. That
would break random access if every entry were relative to its predecessor, so
every 16th entry is written in FULL as a **restart head** — a self-contained
entry a search can start from. The **restart interval** is how many entries one
head covers: 16 for data blocks, 1 for index blocks
(`src/config/mod.rs:256-257`). A **binary index** over the restart heads' file
offsets is what makes the block searchable: binary-search the heads, then
linear-scan at most 15 entries.

The whole encoder is 38 lines, and one of them is the detail the textbook
account gets wrong:

```rust
// src/table/block/encoder.rs — Encoder::write, 122-159
   122      pub fn write(&mut self, item: &'a Item) -> crate::Result<()> {
   123          // NOTE: Check if we are a restart marker
   124          if self
   125              .item_count
   126              .is_multiple_of(usize::from(self.restart_interval))
   127          {
   128              self.restart_count += 1;
   129
   130              if self.restart_interval > 0 {
   // ... 131-134: a clippy allow for the u32 cast ...
   135                  self.binary_index_builder.insert(self.writer.len() as u32);
   136              }
   137
   138              item.encode_full_into(&mut *self.writer, &mut self.state)?;
   139
   140              self.base_key = item.key();
   141          } else {
   142              let shared_prefix_len = longest_shared_prefix_length(self.base_key, item.key());
   143              item.encode_truncated_into(&mut *self.writer, &mut self.state, shared_prefix_len)?;
   144          }
   // ... 145-158: hash-index bookkeeping (see below) and item_count += 1 ...
   159      }
```

The line to look at is **142**, together with **140**. `self.base_key` is
assigned only inside the restart branch, so the shared prefix is computed
against the **restart head**, not against the immediately preceding entry.
That is not the LevelDB scheme this is usually described as: it truncates less
(entry 15 shares only what it shares with entry 0, not with entry 14), and in
exchange decoding any entry needs *only* the restart head, never a chain of
15 predecessors. `parse_truncated` takes exactly that one extra argument,
`base_key_offset` (`src/table/data_block/mod.rs:120-124`).

Worked example, counting key bytes only. Take 16 consecutive keys
`user:0000001040:profile` … `user:0000001055:profile`, each 23 bytes, one
restart interval's worth. A full entry writes one varint key length plus the
key; a truncated entry writes two varints (shared length, rest length) plus the
rest:

```
full:       16 × (1 + 23)                                        = 384 bytes
truncated:  (1 + 23)                       head, written in full =  24
          +  9 × (2 +  9)   keys …1041-1049 share 14 of 23 bytes =  99
          +  6 × (2 + 10)   keys …1050-1055 share 13 of 23 bytes =  72
                                                                 ----
                                                                   195 bytes

saved 384 − 195 = 189 bytes, 49.2% of the key region
average entry:  24.0 bytes full  →  12.19 bytes truncated
```

Nearly twice as many entries per 4 KB block, and therefore about half as many
blocks per lookup — paid for with a linear scan of up to 15 variable-length
records after the binary search lands.

Why this is safe here and not in a B-tree page: blocks are **immutable**, so no
in-place update can break the truncation. The same immutability buys the rest of
the block header for free. Each block carries an **xxh3-128 checksum** — a fast
non-cryptographic hash whose only job is detecting corruption
(`src/hash.rs:7-9`) — computed over the *compressed* bytes at
`src/table/block/mod.rs:70` and verified before decompression at `:94-102`, so a
corrupt block is caught without ever being handed to LZ4. Compression itself is
per level, not global: the default policy is `[None, Lz4]`
(`src/config/mod.rs:275-283`), i.e. L0 uncompressed for flush speed and LZ4
everywhere below. Read the `#[cfg]` on 276-280 before you quote that: lsm-tree's
own `default = []` (`Cargo.toml:20`) leaves the policy at `[None]`, and it is
fjall — `default = ["lz4"]`, `fjall Cargo.toml:20-21` — that turns the second
entry on. The 0.45× space amplification in Step 1 was measured through fjall, so
it includes LZ4.

One honest correction to the folklore: the crate does embed an optional
per-block **hash index** (a tiny in-block hash table from key to restart index,
`encoder.rs:148-154`), but the default `HashRatioPolicy::all(0.0)`
(`src/config/mod.rs:286`) leaves it switched off, so the shipped read path is
binary search over restart heads.

### Step 3 — the table: one forward pass, three outputs

> **In:** the block stream from Step 2, plus every distinct user key as it goes
> past.
> **Out:** the fork this chapter turns on — data blocks (consumed by Step 7's
> disk read), index entries (consumed by Steps 5 and 7 to find the right block),
> and filter bits (consumed by Step 4). One pass, three artifacts, three
> different downstream readers.

Because a table is immutable it can be written **append-only in a single
forward pass**, and the writer is where the three artifacts diverge:

```rust
// src/table/writer/mod.rs — inside Writer::write, 266-296
   266          // NOTE: Check if we visit a new key
   267          if Some(&user_key) != self.current_key.as_ref() {
   268              self.meta.key_count += 1;
   269              self.current_key = Some(user_key.clone());
   // ... 270-274: comment — do not buffer every item's key, there may be
   //              multiple versions of the same one ...
   275              if self.bloom_policy.is_active() {
   276                  self.filter_writer.register_key(&user_key)?;
   277              }
   278          }
   // ... 279-287: first_key bookkeeping, chunk push ...
   288          if self.chunk_size >= self.data_block_size as usize {
   289              self.spill_block()?;
   290          }
   // ... 291-295: seqno range bookkeeping ...
   296      }
```

Line **276** is the filter fork and line **288** is the block fork. Note the
guard on 267: `register_key` fires once per *distinct user key*, so ten versions
of one key cost the filter one entry, not ten — a filter sized by live keys, not
by writes. `spill_block` (303-366) then encodes the buffered chunk, writes it
through `Block::write_into`, and registers the block's last key and file offset
with the index writer at **332-337**. That is the third artifact.

The order the pieces land in the file is decided by `finish`:

```rust
// src/table/writer/mod.rs — inside Writer::finish, 374-388 and 414, 518-528
   374          self.spill_block()?;
   // ... 376-380: delete the file and return None if nothing was written ...
   382          // Write index
   383          log::trace!("Finishing index writer");
   384          let index_block_count = self.index_writer.finish(&mut self.file_writer)?;
   385
   386          // Write filter
   387          log::trace!("Finishing filter writer");
   388          let filter_block_count = self.filter_writer.finish(&mut self.file_writer)?;
   // ... 390-411: linked blob files, table_version byte ...
   414          self.file_writer.start("meta")?;
   // ... 416-514: the meta block — key_count, restart intervals, seqno range … ...
   518          let mut checksum = self.file_writer.into_inner()?;
   519          checksum.inner_mut().get_mut().sync_all()?;
   // ... 520-527: take the file's checksum, then a clippy allow for the fsync ...
   528          fsync_directory(self.path.parent().expect("should have folder"))?;
```

So the on-disk order at `8526dd3` is data blocks → **index** (384) → **filter**
(388) → meta (414), then `sync_all` on the file (519) and an fsync of the
containing directory (528) so the new file's *name* is durable too:

```
 ┌─────────────┬─────────────┬──────┬────────┬──────────────┬─────────┐
 │ data block  │ data block  │  …   │ index  │ filter block │ meta    │
 │ (~4KB, LZ4) │             │      │ block  │ (bloom)      │ /trailer│
 └─────────────┴─────────────┴──────┴────────┴──────────────┴─────────┘
   written first, streaming        written at finish(), 384 → 388 → 414
```

That is index-before-filter — the reverse of the ASCII diagram in the topic
README, which draws the RocksDB-flavoured layout. Neither order matters to a
reader, because both are found through the meta block at the end; it matters
only if you are checking the guide against the code, which is the point.

A point read inside one table then costs: index lookup (usually cached) → one
data block read → binary search over restart heads → up to 15 entries of linear
scan. One table ≈ one disk IO. The problem is *how many tables* — Steps 4 and 5.

### Step 4 — the bloom filter: paying DRAM to skip IO

> **In:** the distinct-key stream forked off at Step 3's line 276.
> **Out:** one filter block per table, sized `m` bits with `k` probes — the
> gate Step 7 checks before it is allowed to spend an IO.

A **bloom filter** is a probabilistic set summary: a bit array plus k hash
functions, answering "definitely not present" or "maybe present", never a false
negative. A **false positive** is a "maybe" for a key that is not there, and the
**false-positive rate (FPR)** is how often that happens. Each table carries one
filter over all its keys, so a read checks DRAM (tens of nanoseconds) before
paying an IO for a table that probably does not have the key.

The sizing formula is the standard one, and the crate computes it in
`calculate_m`:

```
m = ceil_to_byte( −n · ln(fpr) / ln²2 )        src/table/filter/standard_bloom/builder.rs:129-150
k = floor( bits_per_key · ln2 ),  minimum 1                                        :79 and :111

 n   = number of distinct keys the filter will hold
 fpr = the target false-positive rate, e.g. 0.01
 m   = bits in the array, rounded up to a whole byte (:148)
 k   = how many bits each key sets and each probe tests
 ln2 = 0.6931…, and ln²2 = 0.4805…
```

Work it on the crate's own unit test, which asserts
`calculate_m(1_000, 0.01) == 9_592` (`builder.rs:184`):

```
n = 1000 keys, fpr = 0.01
m = −1000 × ln(0.01) / 0.4805 = −1000 × (−4.6052) / 0.4805 = 9584.6 → 9592 bits (byte-aligned)
bits per key = 9592 / 1000 = 9.592

k, as the textbook computes it:  round(9.592 × 0.6931) = round(6.648) = 7
k, as the crate computes it:     bpk is `(m / n) as f32` at :72 — usize division,
                                 so 9592 / 1000 = 9, and k = (9 × 0.6931) as usize = 6

actual FPR = (1 − e^(−k/bpk))^k        at bpk = 9.592
   k = 6:  (1 − e^(−0.6255))^6 = 0.46500^6 = 1.011%
   k = 7:  (1 − e^(−0.7298))^7 = 0.51806^7 = 1.000%
```

So the crate ships **k = 6, not the optimal 7**, because two truncating casts
(`(m / n) as f32` at :72, `as usize` at :79) round the bits-per-key down before
the multiply — and it costs 0.011 percentage points of FPR, which is why nobody
has noticed. The same arithmetic at the crate's *default* filter policy,
`BloomConstructionPolicy::BitsPerKey(10.0)` (`src/config/mod.rs:288-290`):

```
bpk = 10  →  k = (10 × 0.6931) as usize = 6      (optimal would be 7)
   k = 6:  0.844% false positives
   k = 7:  0.819% false positives
```

Ten bits per key is 1.25 bytes of DRAM per key, and it buys skipping ~99.2% of
the pointless table reads. That is the budget Monkey (this topic's next chapter)
spends differently.

Seven hash computations per key would be expensive, so the crate uses **double
hashing**: compute one real hash and derive all k probe positions from it
arithmetically.

```rust
// src/table/filter/standard_bloom/mod.rs — StandardBloomFilterReader::contains_hash, 102-121
   102      pub fn contains_hash(&self, mut h1: u64) -> bool {
   103          let mut h2 = secondary_hash(h1);
   104
   105          for i in 1..=(self.k as u64) {
   106              let idx = h1 % (self.m as u64);
   // ... 108-111: a clippy allow for the usize cast ...
   112              if !self.has_bit(idx as usize) {
   113                  return false;
   114              }
   115
   116              h1 = h1.wrapping_add(h2);
   117              h2 = h2.wrapping_mul(i);
   118          }
   119
   120          true
   121      }
```

Lines **116-117** are the trick: `h1 += h2; h2 *= i` walks k positions with two
integer operations each. The only real hash is `xxh3_64` (`src/hash.rs:2-4`, via
`Builder::get_hash` at `builder.rs:172-174`), and `secondary_hash`
(`builder.rs:10-13`) derives `h2` from `h1` with a shift and a multiply. Line
113 is the early exit: the *first* zero bit ends the probe, so a true negative
usually costs fewer than k memory touches. The build side is the identical loop
with `enable_bit` instead of `has_bit` (`builder.rs:153-168`).

Cost note the crate makes explicit: filter blocks are pinned in memory by
policy, and the default is `PinningPolicy::new([true, false])`
(`src/config/mod.rs:264`), which by `PinningPolicy::get`'s "index by level, last
entry repeats" rule (`src/config/pinning.rs:18-24`) means **pinned for L0 tables
only**. At deeper levels the filter block itself is fetched through the cache
(`src/table/mod.rs:267-275`), so a filter check down there is not unconditionally
free.

### Step 5 — runs, levels and the version: keeping "newest first" cheap

> **In:** the tables written by Step 3, now many of them.
> **Out:** the *version* — the immutable list of which tables are in which run
> at which level. Step 6 rewrites it; Step 7 walks it.

A **run** is a set of tables whose key ranges are *disjoint*, so finding which
table might hold a key is a binary search over ranges — **one table probed per
run**:

```rust
// src/version/run.rs — Run::get_for_key, 98-103
    98      /// Returns the table that may possibly contains the given key.
    99      pub fn get_for_key(&self, key: &[u8]) -> Option<&T> {
   100          let idx = self.partition_point(|x| x.key_range().max() < &key);
   101
   102          self.0.get(idx).filter(|x| x.key_range().min() <= &key)
   103      }
```

Line **100** is the binary search (`partition_point` is Rust's), and line 102 is
the "or nobody" case: the run may simply have no table covering that key, which
costs zero IO. A **level** is a list of runs, and it is disjoint exactly when it
holds one:

```rust
// src/version/mod.rs — GenericLevel::is_disjoint, and the level-count default, 31 and 67-69
    31  pub const DEFAULT_LEVEL_COUNT: u8 = 7;
   ...
    67      pub fn is_disjoint(&self) -> bool {
    68          self.run_count() == 1
    69      }
```

**L0** is the exception by construction: every memtable flush lands there as its
own run, and flushes overlap arbitrarily, so a read must probe *every* L0 run.
L1 and deeper are one run each, each level targeted about 10× larger than the
one above. The leveled strategy's defaults make that concrete —
`l0_threshold: 4`, `target_size: 64 MiB`, `level_ratio_policy: vec![10.0]`
(`src/compaction/leveled/mod.rs:135-143`) — with `level_base_size = target_size
× l0_threshold` (`:183-185`) and each deeper level multiplied by the ratio
(`:196-230`):

```
 L1 target = 64 MiB × 4        =   256 MiB
 L2 target = 256 MiB × 10      = 2.50 GiB
 L3 target = 2.5 GiB × 10      =  25.0 GiB
 L4 target = 25 GiB × 10       =   250 GiB

 L0:  [run][run][run][run]     ← one run per flush, overlapping: probe ALL 4
 L1:  [────── one disjoint run, 256 MiB ──────]           ← binary search: probe 1
 L2:  [───────── one run, 2.5 GiB ──────────────────]     ← probe 1
```

Read amplification, worked at those defaults: 4 L0 runs at the compaction
trigger plus 6 deeper levels (`DEFAULT_LEVEL_COUNT = 7`, and `choose` asserts
exactly that at `leveled/mod.rs:278`) is **10 runs**, so a `get` for an absent
key does at most 10 filter checks. At Step 4's measured default FPR of 0.844%
per filter, the expected number of *wasted* data-block reads is
`10 × 0.00844 = 0.084` per zero-result lookup — one in twelve. Fill L0 to 20
runs instead of 4 and it is `26 × 0.00844 = 0.22`. That single multiplication is
the reason write stalls exist (the RocksDB chapter, Step 4) and the reason
Monkey argues about how those bits are split.

The metadata saying "these tables, in these runs, at these levels" is the
**version**, and compaction never mutates one. It writes a whole new one:

```rust
// src/version/persist.rs — persist_version, 16-17 and 35-42 (the file is 45 lines)
    16      let path = folder.join(format!("v{}", version.id()));
    17      let file = std::fs::File::create_new(path)?;
   ...
    35      let checksum = writer.checksum();
    36
    37      let mut current_file_content = vec![];
    38      current_file_content.write_u64::<LittleEndian>(version.id())?;
    39      current_file_content.write_u128::<LittleEndian>(checksum.into_u128())?;
    40      current_file_content.write_u8(0)?; // 0 = xxh3
    41
    42      rewrite_atomic(&folder.join(CURRENT_VERSION_FILE), &current_file_content)?;
```

Read lines 16-17 and 42 together, because that is the commit protocol and it is
worth being precise about: each version is its **own new file** `v{id}`, written
with `create_new` so it can never clobber an existing one, and fsynced along
with its directory at `:32` *before* anything points at it; the only thing
rewritten in place is the 25-byte `current` file (`src/file.rs:12`) holding the
version id and its checksum, and `rewrite_atomic` does that as temp-file →
fsync → rename → fsync → fsync-directory (`src/file.rs:62-90`). Recovery reads
`current`, opens `v{id}`, and decodes levels → runs → tables
(`src/version/recovery.rs:34-94`). This is RocksDB's MANIFEST+CURRENT pair in
miniature with one difference that matters at scale: RocksDB appends *deltas*,
lsm-tree writes a *full snapshot* of the file layout every time.

### Step 6 — compaction: a k-way merge plus one deferred rule

> **In:** the version from Step 5, plus a policy object.
> **Out:** a `Choice`, and if it is `Merge` or `Move`, a new version — which is
> the input Step 7 reads against.

Compaction picks some input tables, merges them, writes new tables and publishes
a new version. The crate makes the *policy* a trait with a four-way answer:

```rust
// src/compaction/mod.rs — Choice and the strategy trait, 63-80 and 87-97
    63  /// Describes what to do (compact or not)
    64  #[derive(Debug, Eq, PartialEq)]
    65  pub enum Choice {
    66      /// Just do nothing.
    67      DoNothing,
    68
    69      /// Moves tables into another level without rewriting.
    70      Move(Input),
    71
    72      /// Compacts some tables into a new level.
    73      Merge(Input),
    // ... 75-79: Drop — delete tables without compacting, used by the FIFO strategy ...
    80  }
   ...
    87  pub trait CompactionStrategy {
    // ... 88-95: get_name and get_config ...
    96      /// Decides on what to do based on the current state of the LSM-tree's levels
    97      fn choose(&self, version: &Version, config: &Config, state: &CompactionState) -> Choice;
```

Line **70** is the one worth stealing. A **trivial move** is a compaction that
rewrites nothing: if the input does not overlap anything at the destination
level, the engine relinks the file into it, zero bytes of IO. Leveled returns it
in two places, and both guard it the same way:

```rust
// src/compaction/leveled/mod.rs — the two trivial-move sites, 524-527 and 574-577
   524          if target_level_overlapping_table_ids.is_empty() && first_level.is_disjoint() {
   525              return Choice::Move(choice);
   526          }
   527          return Choice::Merge(choice);
   ...
   574          if can_trivial_move && level.is_disjoint() {
   575              return Choice::Move(choice);
   576          }
   577          Choice::Merge(choice)
```

524 is the L0→L1 case ("nothing in L1 overlaps, and L0 happens to be one
disjoint run"); 574 is the L1+ case, where `pick_minimal_compaction`
(`:19`, called at `:553`) reports whether the tables it chose can be relinked.
Both require `is_disjoint()` — Step 5's `run_count() == 1`.

The merge itself is a **k-way merge**: pop the smallest key across k sorted
iterators. The crate's is double-ended, on an interval heap:

```rust
// src/merge.rs — the heap type and Merger::next, 6 and 85-99 (next_back is 102-117)
     6  use interval_heap::IntervalHeap as Heap;
   ...
    85      fn next(&mut self) -> Option<Self::Item> {
    86          if !self.initialized_lo {
    87              fail_iter!(self.initialize_lo());
    88          }
    89
    90          let min_item = self.heap.pop_min()?;
    91
    // ... 92: a clippy allow for the index ...
    93          if let Some(next_item) = self.iterators[min_item.0].next() {
    94              let next_item = fail_iter!(next_item);
    95              self.heap.push(HeapItem(min_item.0, next_item));
    96          }
    97
    98          Some(Ok(min_item.1))
    99      }
```

Line 90 pops the minimum and line 93 refills *only* the iterator it came from —
the standard k-way merge, O(log k) per item. An **interval heap** stores both
ends, so `pop_max` at `:108` gives reverse iteration from the same structure,
which is why a range scan can be run backwards without a second merge path.

The one subtle rule is when a tombstone may finally be discarded:

```rust
// src/compaction/worker.rs — the tombstone rule, 381-390
   381      let dst_lvl = payload.canonical_level.into();
   382      let last_level = opts.config.level_count - 1;
   383
   384      // NOTE: Only evict tombstones when reaching the last level,
   385      // That way we don't resurrect data beneath the tombstone
   386      let is_last_level = payload.dest_level == last_level;
   387
   388      merge_iter = merge_iter
   389          .evict_tombstones(is_last_level)
   390          .zero_seqnos(false);
```

Line **386** is the entire rule, and the comment on 384-385 is the reason. Drop a
tombstone at L1 while an older version of its key still sits at L3, and the old
value is *resurrected* — the delete silently undone. So deleted keys physically
survive, level by level, until a merge finally carries the tombstone to level 6
(`level_count - 1`, with the default 7). That is space amplification with a
purpose, and the same reasoning your M4 capstone will need.

### Step 7 — the read path, end to end

> **In:** everything: the memtables of Step 1, the tables of Step 3, the filters
> of Step 4, the version of Step 5 as Step 6 last published it.
> **Out:** one `Option<UserValue>`, and an IO count you can now predict.

A `get` is Steps 1-6 executed newest-first. `Tree::get` (`src/tree/mod.rs:639`)
delegates to `get_internal_entry` (`:157`), which is this:

```rust
// src/tree/mod.rs — Tree::get_internal_entry_from_version, 696-714
   696      pub(crate) fn get_internal_entry_from_version(
   697          super_version: &SuperVersion,
   698          key: &[u8],
   699          seqno: SeqNo,
   700      ) -> crate::Result<Option<InternalValue>> {
   701          if let Some(entry) = super_version.active_memtable.get(key, seqno) {
   702              return Ok(ignore_tombstone_value(entry));
   703          }
   704
   705          // Now look in sealed memtables
   706          if let Some(entry) =
   707              Self::get_internal_entry_from_sealed_memtables(super_version, key, seqno)
   708          {
   709              return Ok(ignore_tombstone_value(entry));
   710          }
   711
   712          // Now look in tables... this may involve disk I/O
   713          Self::get_internal_entry_from_tables(&super_version.version, key, seqno)
   714      }
```

Three probes in strict recency order — active memtable (701), sealed memtables
newest-first (707, and the `.rev()` that makes it newest-first is at `:743`),
then disk (713). Each carries `seqno`, the **sequence number**: a global write
counter stamped on every entry, so a read under a snapshot takes the newest
version at or below its own seqno. That is MVCC — multi-version concurrency
control, readers seeing a frozen point in time — falling out of "never
overwrite" for free. `ignore_tombstone_value` (`:67-73`) is what turns a found
tombstone into `None` rather than a value.

The disk half is nine lines, and contains the production touch:

```rust
// src/tree/mod.rs — Tree::get_internal_entry_from_tables, 716-736
   716      fn get_internal_entry_from_tables(
   717          version: &Version,
   718          key: &[u8],
   719          seqno: SeqNo,
   720      ) -> crate::Result<Option<InternalValue>> {
   721          // NOTE: Create key hash for hash sharing
   722          // https://fjall-rs.github.io/post/bloom-filter-hash-sharing/
   723          let key_hash = crate::table::filter::standard_bloom::Builder::get_hash(key);
   724
   725          for table in version
   726              .iter_levels()
   727              .flat_map(|lvl| lvl.iter())
   728              .filter_map(|run| run.get_for_key(key))
   729          {
   730              if let Some(item) = table.get(key, seqno, key_hash)? {
   731                  return Ok(ignore_tombstone_value(item));
   732              }
   733          }
   734
   735          Ok(None)
   736      }
```

Line **723** is the touch: the key is hashed **once**, outside the loop, and the
same `u64` is handed to every filter — the SipHash lesson from topic 0 applied,
since with 10 runs the naive version would hash the key 10 times. Lines 725-728
are Step 5's structure walked in order: levels, runs within a level, and
`get_for_key`'s binary search reducing each run to at most one candidate table.
`filter_map` drops the runs that cover no such key range before any IO is
considered.

The filter gate is inside `Table::get`:

```rust
// src/table/mod.rs — inside Table::get, 280-292
   280          if let Some(filter_block) = &filter_block {
   281              if !filter_block.maybe_contains_hash(key_hash)? {
   // ... 282-287: metrics — filter_queries += 1, io_skipped_by_filter += 1 ...
   288                  return Ok(None);
   289              }
   290          }
   291
   292          let item = self.point_read(key, seqno);
```

Line **288** is the whole value proposition of Step 4: the function returns
before `point_read` (292, defined at `:317`) can touch a data block. The metric
next to it is even named `io_skipped_by_filter`.

Count the cost of one `get` at the defaults: memtable probes are pure DRAM;
at most 10 runs survive `get_for_key`; each survivor costs one filter check;
and only a "maybe" (0.844% of absent keys, from Step 4) pays for a data block.
That is read amplification tamed — the number this whole crate exists to bound.

## Where each step lives in the code

Read the directories in step order — each layer lands before the one that uses
it. Every line number is `8526dd3`.

- **Step 1 — vocabulary, `src/value_type.rs`**: the four `ValueType` variants
  (`:5-22`), `is_tombstone` (`:27-29`). Amplification claim in the crate's own
  words: `src/compaction/leveled/mod.rs:119`.
- **Step 2 — block encoding, `src/table/block/`**: `encoder.rs:122-159` — full
  entry at 138, truncation against `base_key` at 140/142, hash index at
  148-154. Decode side, showing the restart head is the only dependency:
  `src/table/data_block/mod.rs:120-124` (`parse_truncated`'s `base_key_offset`).
  Block header with the xxh3-128 checksum: `header.rs:47-60`, its own u32
  checksum at `:109`. Compress on write and verify-then-decompress on read:
  `block/mod.rs:60-65`, `:70`, `:94-102`, `:104-118`. Index handles are varints:
  `src/table/index_block/block_handle.rs:20-26` (the struct) and `:45-50` (the
  encoding). Defaults: `src/config/mod.rs:256-257` (restart interval 16 data,
  1 index), `:261` (4 KB), `:275-283` (None at L0, LZ4 below), `:286` (hash
  index off).
- **Step 3 — table writer, `src/table/writer/mod.rs`**: `write` (243-296) —
  filter fork at 275-277, block fork at 288-290; `spill_block` (303-366) —
  index registration at 332-337; `finish` (371-539) — index 384, filter 388,
  meta 414, `sync_all` 519, directory fsync 528.
- **Step 4 — bloom filter, `src/table/filter/standard_bloom/`**:
  `builder.rs:129-150` (`calculate_m`), `:58-86` (`with_fp_rate`, the truncating
  `bpk` at :72 and `k` at :79), `:93-127` (`with_bpk`), `:153-168` (set),
  `:10-13` (`secondary_hash`), `:184` (the unit test whose numbers Step 4 works
  through). Probe side: `mod.rs:102-121`. Defaults and pinning:
  `src/config/mod.rs:288-290`, `:264`, `src/config/pinning.rs:18-24`.
- **Step 5 — version and levels, `src/version/`**: `mod.rs:31`
  (`DEFAULT_LEVEL_COUNT = 7`), `:42-78` (`GenericLevel`, `is_disjoint` at
  67-69), `run.rs:51-61` and `:98-103` (`get_for_key`). Level sizing:
  `src/compaction/leveled/mod.rs:135-143`, `:183-185`, `:196-230`. Persistence:
  `version/persist.rs:16-17`, `:35-42`; `src/file.rs:12`, `:62-90`. Recovery:
  `version/recovery.rs:34-94`.
- **Step 6 — compaction, `src/compaction/`**: `mod.rs:63-80` (`Choice`),
  `:87-97` (the trait). Trivial moves: `leveled/mod.rs:524-527`, `:574-577`,
  with `pick_minimal_compaction` at `:19` (called at `:553`). Tombstones:
  `worker.rs:381-390`. The k-way merge is **not** under `compaction/` — it is
  `src/merge.rs:6` (interval heap), `:85-99` (`next`), `:102-117` (`next_back`).
- **Step 7 — read path, `src/tree/mod.rs`**: `get` (`:639-643`) →
  `get_internal_entry` (`:157`) → `get_internal_entry_from_version`
  (`:696-714`) → `get_internal_entry_from_tables` (`:716-736`), with the shared
  hash at `:723`; sealed memtables newest-first at `:738-750`. Tombstones hidden
  at read time: `:67-73`. Filter gate: `src/table/mod.rs:245-278` (acquire),
  `:280-290` (skip), `:292`/`:317` (`point_read`).

## Questions to answer in notes.md

1. Why can L0 not be a disjoint run, and what does that cost a point read?
   (Flushes overlap arbitrarily ⇒ probe every L0 run. Put a number on it with
   Step 5's arithmetic: 4 runs at the trigger versus 20, at 0.844% FPR each.)
2. Restart interval 16: derive the trade — Step 2 measured 49.2% of the key
   region saved on 23-byte keys, against a scan of up to 15 variable-length
   records per lookup. At what key length and what block-cache hit rate does
   that stop being worth it? Why don't B-tree pages (topic 3) do this?
3. The version is written as a whole new `v{id}` file on every compaction
   (`persist.rs:16-17`) while RocksDB appends VersionEdits to a MANIFEST log.
   When does the simpler choice break down? (Count the bytes: a version file
   lists every table id, checksum and seqno per run per level —
   `recovery.rs:60-94` shows the exact record — so cost scales with *total*
   table count, not with the size of the change.)

## Done when

Answer each before unfolding it.

- [ ] You can trace one `get` from `src/tree/mod.rs:639` to a data-block binary search, naming every filter and index consulted.

  <details><summary>Answer</summary>

  `Tree::get` (`:639-643`) calls `get_internal_entry` (`:157`), which reaches
  `get_internal_entry_from_version` (`:696-714`): active memtable at 701, then
  sealed memtables newest-first at 707 (the `.rev()` is at `:743`), then
  `get_internal_entry_from_tables` at 713. That function hashes the key once at
  `:723` and walks `iter_levels().flat_map(|lvl| lvl.iter())` at 725-727,
  reducing each run to at most one candidate table with `Run::get_for_key`
  (`src/version/run.rs:98-103`, a `partition_point` binary search over key
  ranges at :100).

  Inside `Table::get` (`src/table/mod.rs:229`) the filter block is acquired
  (pinned, or loaded through the cache, 245-278) and probed at 281; a "no"
  returns at 288 having touched no data block. A "maybe" falls through to
  `point_read` at 292/`:317`, which uses the index block to find the one data
  block that can hold the key, then binary-searches that block's restart heads
  and linear-scans at most 15 entries (restart interval 16,
  `src/config/mod.rs:256`).

  At the leveled defaults that is at most 10 runs — 4 L0 runs at the trigger
  plus 6 deeper levels — so at most 10 filter checks and, for an absent key,
  `10 × 0.844% = 0.084` expected data-block reads.

  </details>

- [ ] You can explain why tombstones die only at the bottom level, and name the line that decides it.

  <details><summary>Answer</summary>

  `src/compaction/worker.rs:386`: `let is_last_level = payload.dest_level ==
  last_level;`, where `last_level = opts.config.level_count - 1` (:382, so level
  6 at the default `DEFAULT_LEVEL_COUNT = 7`, `src/version/mod.rs:31`). It is
  passed straight to `evict_tombstones(is_last_level)` at :389.

  The reason is in the comment at 384-385: a tombstone is only a *marker* that a
  key was deleted, and older versions of that key can still exist in any level
  below. Drop the marker during an L1→L2 merge while an L3 table still holds the
  old value, and the next read finds the old value and returns it — the delete is
  silently undone. Only a merge whose output *is* the deepest level can prove
  nothing older survives.

  The price is space amplification with a purpose: deleted keys occupy disk until
  compaction walks them all the way down, which under leveled compaction means
  once per level. This is also why a delete-heavy workload can look like it is
  not reclaiming anything.

  </details>

- [ ] You can compute the filter's real false-positive rate from `bits_per_key`, and say why the crate's `k` is not the textbook one.

  <details><summary>Answer</summary>

  `m = ceil_to_byte(−n · ln(fpr) / ln²2)` (`builder.rs:129-150`) and
  `k = floor(bits_per_key · ln2)` (`:79`, `:111`), then
  `FPR = (1 − e^(−k/bpk))^k`. At the crate's own test point,
  `calculate_m(1000, 0.01) = 9592` (`:184`), bits per key is 9.592 and the
  textbook `k` is `round(9.592 × 0.6931) = 7`, giving 1.000%.

  The crate computes `k = 6` instead, because line :72 writes
  `let bpk = (m / n) as f32` where both are `usize` — integer division turns
  9.592 into 9 before the multiply, and `as usize` at :79 truncates
  `9 × 0.6931 = 6.24` to 6. The real FPR at bpk 9.592 with k = 6 is 1.011%
  against 1.000% at k = 7: an 0.011-point tax nobody would ever see in a
  benchmark. At the default `BitsPerKey(10.0)` (`src/config/mod.rs:289`) the
  same truncation gives k = 6 and 0.844% where k = 7 would give 0.819%.

  The honest summary is that the sizing is textbook and the probe count is one
  short of optimal, for a cost of about 3% more false positives — which is far
  smaller than the effect Monkey is about, namely how the bits are split between
  levels in the first place.

  </details>

- [ ] You can say what a "version" is, how a compaction commits one, and how that differs from RocksDB.

  <details><summary>Answer</summary>

  A version is the immutable list of which tables sit in which run at which
  level. Compaction never edits one. `persist_version`
  (`src/version/persist.rs:9-45`) creates a brand-new file named `v{id}` with
  `File::create_new` (16-17) — so it cannot clobber an existing version — writes
  the whole layout into it, takes an xxh3 checksum (35), and then rewrites the
  25-byte `current` file with `{version id, checksum, checksum type}` (37-42).
  `rewrite_atomic` (`src/file.rs:62-90`) does that as temp file → `sync_all` →
  rename → `sync_all` → fsync of the directory, so `current` never points at a
  half-written version. Recovery is the same sequence backwards
  (`recovery.rs:34-94`): read `current`, open `v{id}`, decode level count, run
  counts, then each table's id, checksum and global seqno.

  The difference from RocksDB is what gets written, not whether it is atomic.
  RocksDB appends a `VersionEdit` — a delta, "add these files, delete those" — to
  an append-only MANIFEST log, so a compaction's metadata cost is proportional to
  the *change*. lsm-tree writes a full snapshot, so its metadata cost is
  proportional to the *total* number of tables. At a hundred tables the snapshot
  is simpler and free; at a hundred thousand it is a rewrite of the whole
  catalogue on every compaction.

  </details>

- [ ] You can explain what the shared key hash at `src/tree/mod.rs:723` saves, and why it is safe.

  <details><summary>Answer</summary>

  It saves k−1 … well, it saves *hashes*, not probes: the key is hashed once with
  `xxh3_64` (`src/hash.rs:2-4`) outside the loop at :723, and the resulting `u64`
  is passed into every `Table::get(key, seqno, key_hash)` at :730. Without it, a
  lookup crossing 10 runs would hash the key 10 times. Topic 0's finding — 21% of
  a HashMap lookup being SipHash ([FINDINGS.md](../../FINDINGS.md) row 0) — is
  the reason that is worth a line of code.

  It is safe because every filter in the tree uses the same hash function and
  derives its k probe positions arithmetically from that one value: `h2 =
  secondary_hash(h1)` (`builder.rs:10-13`), then `h1 += h2; h2 *= i`
  (`standard_bloom/mod.rs:116-117`). The per-filter variation comes from `self.m`
  in `h1 % (self.m as u64)` at :106, not from the hash, so two filters of
  different sizes get different bit positions out of the same input hash.

  The one thing it does *not* save is the memory probes: each filter still walks
  its own k bits, up to k cache misses per table, which is exactly the cost
  RocksDB's cache-local bloom attacks (see the compaction chapter, Step 7).

  </details>

## References

**Code**
- [fjall-rs/lsm-tree](https://github.com/fjall-rs/lsm-tree) — the engine under
  fjall, pinned at `8526dd3`; read it all (~3 h). Verify any anchor below with
  `tools/pinned-source.py show lsm-tree <path> -r <range>`.
- [fjall-rs/fjall](https://github.com/fjall-rs/fjall) at `80cf6bc` —
  `src/keyspace/options.rs:91` for the 64 MiB memtable default that decides how
  often Step 3 runs.

| File | Lines | What |
|------|-------|------|
| `src/value_type.rs` | 5-22, 27-29 | the tombstone markers, and the test that hides them |
| `src/table/block/encoder.rs` | 122-159 | restart heads (138), truncation against `base_key` (140/142) |
| `src/table/block/header.rs` | 47-60, 109 | per-block xxh3-128 checksum, plus a u32 checksum of the header |
| `src/table/block/mod.rs` | 60-65, 70, 94-118 | LZ4 on write, checksum verified *before* decompression |
| `src/table/writer/mod.rs` | 275-277, 288-290, 384-388 | the one-pass fork: filter, data block, then index-before-filter at `finish` |
| `src/table/filter/standard_bloom/builder.rs` | 72, 79, 129-150, 184 | m and k, the truncating casts, and the unit test Step 4 works through |
| `src/table/filter/standard_bloom/mod.rs` | 102-121 | double hashing, and the early exit at 113 |
| `src/version/mod.rs` | 31, 67-69 | 7 levels by default; "disjoint" means exactly one run |
| `src/version/run.rs` | 98-103 | one binary search per run, or no candidate at all |
| `src/version/persist.rs` | 16-17, 35-42 | new `v{id}` file, then an atomic rewrite of `current` |
| `src/compaction/mod.rs` | 63-80, 87-97 | `DoNothing`, `Move`, `Merge`, `Drop`, and the one-method trait |
| `src/compaction/leveled/mod.rs` | 135-143, 183-185, 524-527, 574-577 | ratio 10 / 64 MiB / L0 trigger 4; both trivial-move sites |
| `src/compaction/worker.rs` | 381-390 | tombstones evicted only when the output is the last level |
| `src/merge.rs` | 6, 85-99, 102-117 | interval-heap k-way merge, forwards and backwards |
| `src/tree/mod.rs` | 67-73, 696-714, 716-736 | the read path, newest-first, with the hash computed once at 723 |
