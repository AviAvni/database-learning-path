# An LSM you can read whole: the lsm-tree crate

Every LSM concept in this topic — restart-point block encoding, bloom-gated
point reads, versioned level metadata, pluggable compaction — exists as a few
hundred readable lines in fjall's `lsm-tree` crate. Topic 1 read fjall's
keyspace layer; everything LSM-shaped delegates here, and the crate is small
enough to read completely. Before you open it, this chapter builds the whole
machine one layer at a time — block, segment, filter, level, compaction, read
path — then hands you the file and line anchors to watch each layer in code.

## The problem in one sentence

Absorb writes at sequential-disk speed by only ever appending sorted files —
and then keep a point read from having to search *dozens* of those files: a
naive pile of 100 flushed files means up to 100 disk probes per `get`, and
every mechanism in this crate exists to push that back toward 1.

## The concepts, step by step

### Step 1 — the shape of the machine: buffer, flush, merge

An LSM engine never modifies data on disk; it buffers writes in a sorted
in-memory structure (the **memtable** — your topic 2 skiplist) and, when that
fills (say 8 MB), writes its contents out as one immutable sorted file called
a **segment** (RocksDB calls it an SST, "sorted string table"). Deletes are
writes too: a **tombstone** (a key marked "deleted") is inserted like any
other entry, because you can't erase from files you never modify. Background
**compaction** merges accumulated segments into fewer, bigger ones so reads
stay bounded. Everything below is one of those three verbs — buffer, flush,
merge — made concrete. The cost baked into the shape: a key can now exist in
several places at once (memtable and multiple segments), so every read must
consult them *newest first* and take the first hit.

### Step 2 — the block: prefix truncation with restart points

A segment's data is cut into **blocks** — ~4 KB chunks that are the unit of
IO, checksum, and compression — and inside a block, sorted neighbors share
prefixes, so each entry stores only `shared_prefix_len + rest` relative to
its predecessor. That breaks random access (decoding entry N needs entry
N−1), so every 16th entry is written in FULL as a **restart point**: binary
search jumps between restart points, then linear-decodes at most 15 entries.

```
 inside one 4 KB block (restart interval 16):
 [FULL key ∥ v][shared=5,rest ∥ v][shared=7,rest ∥ v]…[FULL key]…[restart offsets]
  ▲ binary search over restart points, linear decode between them
```

Why this is safe here and not in a B-tree page: blocks are **immutable** —
written once, never edited — so no in-place update can ever break the delta
chain. The payoff is real: keys like `user:1042:profile` shrink to a few
bytes each, so more entries fit per 4 KB block, so fewer blocks per lookup.
Each block also carries an **xxh3 checksum** (a fast non-cryptographic hash
that detects corruption) and optional LZ4 compression — both trivial to add
when nothing mutates. The crate even embeds an optional per-block hash index,
a tiny SwissTable-ish shortcut inside each block; the topic 2 pattern at yet
another scale.

### Step 3 — the segment: an SST written in one forward pass

A segment is blocks plus a table of contents, and because it is immutable it
can be written **append-only in a single pass**: buffer key-value pairs,
flush a data block whenever ~4 KB accumulate, remember each block's last key
and file offset for the **index block** (which maps key ranges to block
positions), feed every key's hash to the filter builder (Step 4), and write
index + filter + trailer/metadata last:

```
 ┌─────────────┬─────────────┬──────┬──────────────┬────────┬─────────┐
 │ data block  │ data block  │  …   │ filter block │ index  │ trailer │
 │ (~4KB, LZ4) │             │      │ (bloom)      │ block  │ /meta   │
 └─────────────┴─────────────┴──────┴──────────────┴────────┴─────────┘
```

A point read inside one segment costs: index lookup (usually cached) → one
data block read → binary search over restart points. One segment ≈ one disk
IO. The problem is *how many segments* — Steps 4 and 5.

### Step 4 — the bloom filter: paying DRAM to skip IO

A **bloom filter** is a probabilistic set summary: a bit array plus k hash
functions that can answer "definitely not present" or "maybe present" —
never a false negative, occasionally a false positive. Each segment carries
one over all its keys, so a read checks the filter (a few DRAM probes, ~100
ns) before paying a disk IO (~100 µs) for a segment that probably doesn't
have the key. The classic sizing: 10 bits per key ⇒ ~1% false positive rate
with k≈7 probes (`m,k` from `−n·ln(fpr)/ln²2`).

Seven hash computations per key would be expensive, so the crate uses
**double hashing**: compute two real hashes and derive all k probe positions
via `h1 += h2; h2 *= i` — k memory probes but only ONE real hash
computation. Compare RocksDB's cache-local bloom, which keeps all k bits in
one cache line (topic 0 priced why: k random probes into a big bit array is
k potential cache misses). The trade to hold: ~1.25 bytes of DRAM per key
buys skipping ~99% of pointless segment reads — Monkey (this topic's paper)
optimizes exactly this budget.

### Step 5 — runs, levels, and the version: keeping "newest first" cheap

A **run** is a set of segments whose key ranges are *disjoint* (no overlap),
so finding which segment might hold a key is a binary search over ranges —
**one segment probed per run**. Levels organize runs by age and size:
**L0** is special — every memtable flush lands there as its own tiny run,
and flushes overlap arbitrarily, so a read must probe *every* L0 run; L1 and
deeper are each one disjoint run, each level ~10× larger than the one above.

```
 L0:  [run][run][run][run]     ← one run per flush, overlapping: probe ALL
 L1:  [────── one disjoint run ──────]           ← binary search: probe 1
 L2:  [───────────── one run, 10× bigger ─────────────]        ← probe 1
```

The metadata saying "these segments, in these runs, at these levels" is the
**version** — an immutable snapshot of the tree's file layout. Compaction
never mutates a version; it writes a *new* version file, checksums it, and
atomically renames it into place (`rewrite_atomic` + fsync). This is
RocksDB's MANIFEST in miniature: **compaction commits by publishing a new
version, never by mutating the old one** — copy-on-write again, at the
metadata level. Cost of L0's laxness: at 20+ L0 runs a point read does 20+
filter checks, which is why every engine eventually stalls writers (topic
README §3).

### Step 6 — compaction: a k-way merge plus one deferred rule

Compaction picks some input segments, merges them (a **k-way merge**: pop
the smallest key across k sorted iterators, using an interval heap —
double-ended, so reverse scans work too), writes new segments, and publishes
a new version. The crate makes the *policy* a trait — `choose(version,
config, state)` returns `Merge | Move | Drop | DoNothing` — so leveled
(L0 trigger 4 runs, size ratio 10) and any other strategy plug in. Note
`Move`: a segment that doesn't overlap the next level is *relinked* into it,
zero bytes of IO.

The one subtle rule: **tombstones are evicted only when the compaction's
output is the last level** (`evict_tombstones(is_last_level)`). Drop a
tombstone at L1 while an older version of its key still sits in L3, and the
old value is *resurrected* — the delete silently undone. So deleted keys
physically survive, level by level, until a merge finally carries the
tombstone to the bottom. That's space amplification with a purpose, and the
same reasoning your M4 capstone will need.

### Step 7 — the read path, end to end

A `get` is now just Steps 1–6 executed newest-first: active memtable →
sealed memtables (newest first) → each run of the version, gated by filters.
Two production touches worth noticing: the key's filter hash is computed
**once** and shared across all segment filter checks (the SipHash-cost
lesson from topic 0 applied), and **seqno filtering** happens at every step
— each entry carries a sequence number (a global write counter), and a read
under a snapshot picks the newest version ≤ its snapshot seqno. That's MVCC
(multi-version concurrency control — readers see a frozen point in time)
falling out of the LSM's "never overwrite" design for free.

The whole path, compressed to its shape:

```rust
fn get(&self, key: &[u8], snapshot: SeqNo) -> Option<Value> {
    if let Some(v) = self.active.get(key, snapshot) { return live(v); }
    for mt in self.sealed.iter().rev() {              // newest sealed first
        if let Some(v) = mt.get(key, snapshot) { return live(v); }
    }
    let h = hash(key);                                // hashed ONCE for all filters
    for run in self.version.runs() {                  // L0: run per flush; L1+: one
        let Some(seg) = run.get_for_key(key) else { continue };  // disjoint ⇒ binary search
        if !seg.filter_maybe_contains(h) { continue; }            // bloom: skip the IO
        if let Some(v) = seg.point_read(key, snapshot) { return live(v); }
    }
    None                                              // live(): tombstone ⇒ None
}
```

Count the cost: memtable probes are free; each L0 run and each deeper level
is one filter check; actual disk IO only where a filter says "maybe". That
is read amplification tamed — the number this whole crate exists to bound.

## Where each step lives in the code

Read the directories in step order — each layer lands before the one that
uses it.

- **Step 2 — block encoding, `src/table/block/`**: restart intervals +
  prefix truncation in `encoder.rs:61–151` — a FULL item every
  `restart_interval` items; between restarts, items store
  `shared_prefix_len + rest` (`longest_shared_prefix_length`, :142);
  optional per-block hash index (:148–154). Per-block header with xxh3 u128
  checksum + sizes in `header.rs:49–60` (the header itself gets a u32
  checksum, :109). LZ4 per block: `mod.rs:60–70, 111–120`. Index blocks:
  `index_block/block_handle.rs:20–43` — varint offset+size handles.
- **Step 3 — segment writer/reader, `src/table/`**: `writer/mod.rs:40–95` —
  buffer KVs, flush block at size threshold, feed the filter + index
  writers, trailer/metadata last (single forward pass — an SST is written
  append-only, like everything else in an LSM). Read path with filter:
  `mod.rs:245–290` — filter loaded lazily; if `maybe_contains_hash` says no,
  the point read never touches a data block (:281–288).
- **Step 4 — bloom filter, `src/table/filter/standard_bloom/`**:
  `builder.rs:55–127` — `with_fp_rate` computes m,k from `−n·ln(fpr)/ln²2`
  (:58); `with_bpk` direct (:93). Double hashing: `builder.rs:10–13` +
  `mod.rs:102–129`.
- **Step 5 — version + levels, `src/version/`**: `mod.rs:42–114` +
  `run.rs:51–103` — levels are runs; `get_for_key` (:99–103)
  binary-searches segment ranges. Persistence: `persist.rs:9–45` — new
  version file written, checksummed, `rewrite_atomic` on
  CURRENT_VERSION_FILE + fsync. Recovery: `recovery.rs:34–95`.
- **Step 6 — compaction, `src/compaction/`**: the trait in `mod.rs:87–98` —
  `choose(version, config, state) → Merge | Move | Drop | DoNothing`; find
  where leveled uses `Move` (`leveled/mod.rs:19` pick_minimal_compaction).
  Leveled policy: `leveled/mod.rs:113–143` — L0 trigger 4 runs, ratio 10.
  Worker: `worker.rs:382–389` — `evict_tombstones(is_last_level)`. Merge:
  `merge.rs:35–99` — k-way merge on an interval heap.
- **Step 7 — read path, `src/tree/mod.rs`**: `get` :639–643 →
  `get_internal_entry` :696–750: active memtable → sealed (newest first) →
  levels; seqno filtering at every step (:701/707/730); hash computed once
  and shared across all segment filter checks (:721–723). Tombstones:
  `value_type.rs:8–27`; hidden at read time (`tree/mod.rs:67–72`), dropped
  at bottom-level compaction.

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

## References

**Code**
- [fjall-rs/lsm-tree](https://github.com/fjall-rs/lsm-tree) — the engine under
  fjall; read it all (~3 h): `src/table/block/`, `src/table/`,
  `src/table/filter/standard_bloom/`, `src/version/`, `src/compaction/`,
  `src/tree/mod.rs`. Local shallow clone at `~/repos/lsm-tree`.
