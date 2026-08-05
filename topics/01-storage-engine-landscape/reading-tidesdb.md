# tidesdb: the same LSM with nothing abstracted away

The value of this skim (1–2 h) is seeing the machinery you just traced in fjall
rendered in plain C, with *nothing* hidden — memory ordering, pointer arithmetic
and disk offsets are all in your face. This chapter first rebuilds the LSM
lifecycle step by step, each time pointing at the concrete C structure that
fjall's Rust abstractions wrap. Read it as a contrast exercise: match each fjall
concept to its C twin and notice exactly what Rust's abstractions buy you, and
what they conceal.

**tidesdb *is* in this repo's pin table**, at `tidesdb/tidesdb@810507a` — confirm
with `python3 tools/pinned-source.py ref tidesdb`, list the tree with
`python3 tools/pinned-source.py list tidesdb`, and read any file at that commit
with `python3 tools/pinned-source.py show tidesdb <path> -r A:B`. Every line
number below was checked against that commit; several anchors in the previous
version of this guide were off and are corrected inline. Note the paths all
carry a `src/` prefix — it is `src/tidesdb.c`, not `tidesdb.c`.

## The problem in one sentence

Do fjall's job — absorb random-key writes as sequential IO, survive crashes,
answer reads in a handful of file probes — in a 37,702-line C file where every
byte offset, atomic barrier and `malloc` is spelled out by hand.

## The concepts, step by step

### Step 1 — the LSM recipe, restated in plain C terms

> **In:** the LSM lifecycle from the fjall chapter, as concepts.
> **Out:** the five files those concepts live in, with real line counts, so you
> know the size of what you are about to skim.

An LSM (log-structured merge) engine never updates data in place. It appends
every write to a log file for crash safety, buffers the same write in a sorted
in-memory structure (the **memtable**), periodically dumps the full memtable to
disk as an immutable sorted file (an **SSTable**), and merges those files in the
background (**compaction**) to keep reads cheap. The fjall chapter builds the
*why* of each piece from sequential-vs-random IO; this one shows each piece as
bytes and structs.

In tidesdb every one of those nouns is a file you can open, and the sizes are
the first surprise:

```text
src/tidesdb.c        37,702 lines   write path, read path, compaction, workers
src/skip_list.c       2,929 lines   the memtable — skip list + arena allocator
src/block_manager.c   2,004 lines   physical block IO: WAL frames and SST blocks
src/tidesdb.h         2,152 lines   every public type and every tunable
src/manifest.c          923 lines   which SSTable belongs to which level
src/bloom_filter.c      624 lines   the whole filter, hash mixing included
```

Nothing else. That is the whole engine — plus `src/btree.c` (tidesdb can build a
B-tree-shaped SSTable), `src/clock_cache.c`, `src/compress.c` and the object-store
backends, none of which you need for this pass. Compare with fjall, where
`src/keyspace/mod.rs` is 1,113 lines and *everything below the lifecycle* is
behind the `lsm-tree` dependency: the ratio is not that C is more verbose, it is
that tidesdb has no crate boundary to hide behind.

### Step 2 — the memtable is a skip list you can read

> **In:** the need for a sorted, concurrently-writable in-memory buffer.
> **Out:** why a skip list rather than a tree, and the allocation strategy
> fjall's Rust hides.

A **skip list** is a sorted linked list with "express lanes": each node gets a
random height, and higher lanes skip over many nodes, so search is O(log n) like
a balanced tree — but insertion never rebalances anything, which is what makes
it easy to run lock-free (concurrent threads use atomic pointer swaps instead of
locks).

```text
 level 3:  head ──────────────────► k₄₀ ─────────────────► nil
 level 2:  head ────────► k₂₂ ────► k₄₀ ────────► k₇₈ ───► nil
 level 1:  head ─► k₀₇ ─► k₂₂ ────► k₄₀ ─► k₅₅ ─► k₇₈ ───► nil
 level 0:  head ─► k₀₇ ─► k₁₃ ─► k₂₂ ─► k₄₀ ─► k₅₅ ─► k₇₈ ─► nil
            search(k₅₅): drop down whenever the next key overshoots
            → ~log₂(n) hops instead of n
```

At this topic's scale that matters concretely: a 64 MiB memtable of 100-byte
records holds ~671,000 entries, so a skip-list probe is log₂(671,000) ≈ **20
hops** against 671,000 for a linear list.

`src/skip_list.c` also shows the allocation strategy fjall's Rust hides: an
**arena allocator** — big slabs are allocated up front and each insert bumps a
pointer forward, with no per-node `free`; the whole arena dies when the memtable
is flushed. tidesdb goes one step further and shards the arenas per thread,
caching slot assignments in thread-local storage (`src/skip_list.c:22–42`,
including the comment explaining that a small *set* of cached slots beats a
single one when a thread interleaves writes across arenas). Cheap allocation,
and it makes the memtable-size check a single comparison —
`skip_list_get_size(umt->skip_list)` at `src/tidesdb.c:29846`.

### Step 3 — the write path: a WAL batch is just bytes at an offset

> **In:** a transaction with staged operations, and the write-ahead rule.
> **Out:** the three consecutive calls that implement it, with line numbers, and
> the one memory-layout decision C forces into the open.

tidesdb groups writes into transactions: `tidesdb_txn_put`
(`src/tidesdb.c:26535`) stages each operation in a per-transaction ops array,
and `tidesdb_txn_commit` (`src/tidesdb.c:29697`) serialises the whole batch and
hands it to `block_manager_write_raw` — a raw append of length-prefixed bytes to
the log file. The write-ahead rule then appears as three calls, forty lines
apart, in commit order:

```text
src/tidesdb.c:29796   block_manager_write_raw(umt->wal, uwal_batch, uwal_size)
                      ↑ the WAL append. Comment at :29792 says it uses a raw
                        write "to avoid malloc/memcpy/free per commit"

src/tidesdb.c:29814   if (config.unified_memtable_sync_mode == TDB_SYNC_FULL)
                        tidesdb_unified_wal_group_sync(...)
                      ↑ the fsync, and only under one of three sync modes.
                        Comment at :29811: "group-commit durability -- one
                        fdatasync per batch of concurrent committers"

src/tidesdb.c:29837   tidesdb_txn_apply_ops_to_unified_memtable(txn, umt->skip_list)
                      ↑ ONLY NOW does the write become visible in RAM
```

That ordering is the entire durability contract, and unlike fjall — where it is
implied by holding a `MutexGuard` across two method calls — here you can point at
the three lines. Note the three sync modes named on line 29814's comment:
`TDB_SYNC_FULL` (fdatasync per commit batch), `TDB_SYNC_INTERVAL` (a background
sync worker), `TDB_SYNC_NONE` (skip). That is the same knob as fjall's
`PersistMode`, and the same knob you must equalise before comparing engines.

One detail the C makes load-bearing and explicit: **key and value share one
malloc**, and the source says why:

```c
// src/tidesdb.c at tidesdb/tidesdb@810507a — inside tidesdb_txn_put,
// lines 26579-26590. The comment is the source's own.
26579      /*** we coalesce key+value into a single allocation to halve malloc pressure
26580       **  op->value points into the same buffer at offset key_size
26581       *   only op->key should be freed (it owns the entire buffer) */
26582      const size_t kv_alloc_size = key_size + (value_size > 0 ? value_size : 0);
26583      op->key = malloc(kv_alloc_size);
26584      if (!op->key) return TDB_ERR_MEMORY;
26585      memcpy(op->key, key, key_size);
26586      op->key_size = key_size;
26587
26588      if (value_size > 0)
26589      {
26590          op->value = op->key + key_size;
```

Line 26590 is layout as pointer arithmetic: the value pointer is the key pointer
plus an offset. The Rust equivalent would be a single `Box<[u8]>` with split
indices; here you *see* that one allocation per op — instead of two — is a
deliberate throughput decision, and that the ownership rule it creates ("only
`op->key` should be freed") has to be maintained by comment rather than by the
type system.

Cost, same as fjall: every byte is written twice (log now, SSTable later), and
commit latency is the sync mode on the log.

### Step 4 — the SSTable made explicit: build the bloom, write the offsets

> **In:** a full memtable and a file to write it into.
> **Out:** what an SSTable is at byte level, and the bloom sizing formula
> evaluated on this topic's numbers — from the source's own code, not a
> remembered rule of thumb.

When the memtable is over threshold, a worker (`tidesdb_flush_memtable`,
`src/tidesdb.c:24887`) walks the skip list in key order and writes an SSTable:
compressed blocks of sorted key-value pairs, a **block index** (an array of
"first key → byte offset in this file" entries), and a **bloom filter**.

In fjall both helpers are inside the `lsm-tree` crate. Here `src/bloom_filter.c`
is 624 lines and you can read all of it — the hash mixing (murmur-family prime
at `:37`, a v2 hash that "appends a murmur3 fmix32 finalizer so short keys fully
avalanche", `:39–43`), the packed 64-bit bitset macros (`:29–33`), the
serialisation format with its version sentinel (`:46–59`), and the sizing:

```c
// src/bloom_filter.c at tidesdb/tidesdb@810507a — bloom_filter_new, lines
// 203-223. p is the target false-positive rate, n the expected key count.
203      /**** we calculate the size of the bitset (m) using the formula
204       ***  m = -n * ln(p) / (ln(2)^2)
205       **
206       */
207      const double m_double = ceil(-((double)n) * log(p) / (M_LN2 * M_LN2));
217      (*bf)->m = (unsigned int)m_double;
219      /* we calculate the number of hash functions (h) using the formula
220       * h = (m / n) * ln(2)
221       *
222       */
223      const double h_double = ceil(((double)(*bf)->m) / n * M_LN2);
```

Work it for one flushed memtable at this topic's shape — n = 671,000 records
(64 MiB / 100 B), target p = 1%:

```text
  m = ceil(-671000 · ln(0.01) / (ln 2)²)
    = ceil(671000 · 4.6052 / 0.4805)
    = 6,431,575 bits = 804 KB of filter    ⇒ 9.59 bits per key
  h = ceil((m/n) · ln 2) = ceil(9.59 × 0.6931) = ceil(6.65) = 7 hash functions
```

So the folk figure "about 10 bits per key for 1%" is *derived*, not assumed —
and 7 is exactly the range the source's own comment at `:225` calls typical
("typical real-world values are 7-15").

The block index is the other half, and it returns **raw file offsets**:

```c
// src/tidesdb.c at tidesdb/tidesdb@810507a — inside tidesdb_sstable_get,
// lines 9832-9837. There is no cursor abstraction; the lookup produces a
// byte position that a seek() consumes directly.
9832      if (sst->block_indexes && sst->block_indexes->count > 0)
9833      {
9834          int64_t start_slot = 0;
9835          if (compact_block_index_find_slot(sst->block_indexes, key, key_size, &start_slot) == 0)
9836          {
9837              start_file_position = sst->block_indexes->file_positions[start_slot];
```

That is what "immutable sorted file" actually means at the bottom: a byte layout
you can compute offsets into. Note also the honesty in the surrounding comment
(`:9838–9840`): the prefix index is *lossy*, so keys sharing a long prefix span
several blocks with identical min/max prefixes and the lookup must walk a run —
a real complication that a `BTreeMap` API would have hidden from you entirely.

### Step 5 — the read path: every potential miss, one function per stop

> **In:** a key that could be hiding in any of five kinds of place.
> **Out:** read amplification as a literal for-loop, with the line number of
> each stop and the arithmetic of what the bloom `continue` saves.

A read must check every place a newer version of the key could hide,
newest-first, and return the first hit. tidesdb performs each stop as a separate,
named call — the read path *is* the topic README's LSM read diagram, one
function per box:

```rust
// ILLUSTRATION — pseudo-Rust for tidesdb's C read path. Each line names the
// real anchor; read them in order at tidesdb/tidesdb@810507a.
1  fn get(&self, key: &[u8]) -> Option<Val> {
2      // src/tidesdb.c:26672 — your own uncommitted writes first, via a hash
3      // table for large transactions (linear reverse scan for small ones)
4      if let Some(v) = self.txn_write_set.get(key) { return Some(v); }
5
6      // src/tidesdb.c:26808 — skip_list_get_with_seq_ref on the ACTIVE memtable,
7      // taken under tidesdb_active_memtable_try_ref (:26804) so a rotation
8      // cannot swap it out mid-probe
9      if let Some(v) = self.active_memtable.get(key) { return Some(v); }
10
11     // src/tidesdb.c:26845 — immutable memtables, newest first. The comment
12     // there spells out the invariant: pointers snapshotted under one rwlock,
13     // each immutable pinned by refcount "so a concurrent flush-worker
14     // eviction cannot free one out from under the scan"
15     for mt in self.immutable_memtables.newest_first() {
16         if let Some(v) = mt.get(key) { return Some(v); }
17     }
18
19     // src/tidesdb.c:9756 — tidesdb_sstable_get, once per SSTable per level
20     for level in &self.levels {
21         for sst in level.newest_first() {
22             // src/tidesdb.c:9810 — bloom check; skips MOST absent-key IO.
23             // Note skip_bloom at :9808: redundant when an L1+ boundary
24             // search already identified this file
25             if !sst.bloom.might_contain(key) { continue; }
26             // src/tidesdb.c:9835 — block index → a raw file offset (:9837)
27             let off = sst.block_index.find_slot(key)?;
28             if let Some(v) = sst.read_block_at(off).find(key) { return Some(v); }
29         }
30     }
31     None   // read amp made concrete: every stop above was a potential miss
32 }
```

Count the stops: write set, active memtable, N immutable memtables, then per
level per SSTable a bloom check and maybe one block read. That count *is* **read
amplification** — the number of places consulted per lookup, against the one
that holds the answer. The bloom `continue` on line 25 is what keeps it
affordable: with 20 SSTables and the 1% filter sized in Step 4, a lookup for an
absent key does 20 × 0.01 = **0.2 expected block reads** instead of 20.

### Step 6 — rotation and compaction: the concurrency is hand-rolled

> **In:** two background mutation streams — rotation and compaction — running
> against live readers.
> **Out:** the exact atomics that make that safe, and what `Arc` was doing for
> you in fjall.

Two mutation streams run concurrently with reads: memtable **rotation** (swap a
full memtable for a fresh one, hand the full one to the flush worker) and
**compaction** (merge SSTables within and between levels to bound read
amplification and drop shadowed versions). Both need object-lifetime guarantees
— a reader mid-lookup must not have its memtable freed underneath it.

fjall gets this from `Arc` for free. tidesdb writes it out, with the memory
ordering visible on every line:

```c
// src/tidesdb.c at tidesdb/tidesdb@810507a — the tail of tidesdb_txn_commit,
// lines 29846-29856. Read the ordering arguments, not just the calls.
29846          const size_t umt_size = (size_t)skip_list_get_size(umt->skip_list);
29847          atomic_fetch_sub_explicit(&umt->writers, 1, memory_order_release);
29848          atomic_fetch_sub_explicit(&umt->refcount, 1, memory_order_release);
29850          if (umt_size >= txn->db->unified_mt.write_buffer_size)
29851          {
29852              /** CAS-based admission, only one thread enters rotation at a time
29853               *  same lock-free pattern as per-CF flush in tidesdb_flush_memtable_internal */
29854              int expected = 0;
29855              if (atomic_compare_exchange_strong_explicit(&txn->db->unified_mt.is_flushing, &expected,
29856                                                          1, memory_order_acquire,
```

Three things are explicit here that Rust would have made invisible. The
refcount and writer-count decrements on lines 29847–29848 are
`memory_order_release`, which publishes this committer's skip-list writes to
whoever later acquires. The rotation admission on line 29855 is a
compare-and-swap with `memory_order_acquire`, so exactly one thread wins and it
sees everything the releasing writers did. And the *acquire* side of the reader
path is `tidesdb_active_memtable_try_ref` (`src/tidesdb.c:29761`, and again at
`:26804` on the read path), which loops with a bounded attempt count —
`TDB_ACTIVE_REF_MAX_ATTEMPTS` — rather than blocking. Rust's `Arc` hides exactly
these barriers; topic 9 makes you write them yourself.

Compaction scheduling is equally visible:

- After a flush, if the level geometry demands it, work is enqueued —
  `tidesdb_enqueue_compaction(cf, 0)` at `src/tidesdb.c:19918`, under a comment
  calling it an "auto-compaction trigger -- geometry-driven, not a full merge".
  The sibling branch at `:19910` steers a key range straight to the bottom level
  instead.
- The enqueue itself (`src/tidesdb.c:25366`) deduplicates via an `is_compacting`
  flag, and the blocking variant at `:25403` falls through to it. The merge
  geometry is computed at *dequeue* time, not enqueue, so it reflects current
  state.
- `tidesdb_compaction_worker_thread` (`src/tidesdb.c:20143`) is the worker
  entry point; its header comment (`:20139–20141`) states the concurrency rule:
  "the `is_compacting` flag ensures only one compaction per CF at a time, but
  multiple workers can compact different CFs concurrently."

Cost, the same trade as every LSM: background write amplification purchased to
keep the Step 5 for-loop short. [FINDINGS.md](../../FINDINGS.md) row 1 is what
that trade is worth on this topic's workload — an LSM at 0.45× space
amplification against a copy-on-write B-tree at 63.28×, a 140× spread on the
same 108 MB of records.

## Where each step lives in the code

| File | Lines | Role (steps) |
|------|-------|------|
| `src/tidesdb.c` | 37,702 | the whole engine: write/read/compaction orchestration (3, 5, 6) |
| `src/skip_list.c` | 2,929 | memtable — skip list, per-thread arena allocator (2) |
| `src/tidesdb.h` | 2,152 | every public type and tunable |
| `src/block_manager.c` | 2,004 | physical block IO (WAL frames + SST blocks) (3, 4) |
| `src/manifest.c` | 923 | level metadata: which SST is in which level (6) |
| `src/bloom_filter.c` | 624 | the whole filter, sizing math included (4) |

**Write path (steps 2–4)** — all in `src/tidesdb.c`:

```text
tidesdb_txn_put                          26535   stage in per-txn ops array
  coalesced key+value malloc             26579   one allocation, value at +key_size
tidesdb_txn_commit                       29697   serialize the batch
  block_manager_write_raw (WAL)          29796   raw framed append
  group fdatasync (TDB_SYNC_FULL only)   29814   one sync per committer batch
  apply_ops_to_unified_memtable          29837   skip-list inserts
  refcount/writers release               29847   memory_order_release
  rotation check + CAS admission         29850   size >= write_buffer_size
tidesdb_flush_memtable                   24887   worker: skip list → compressed SST
```

**Read path (step 5)** — `src/tidesdb.c`:

```text
txn write-set check                      26672   your own uncommitted writes first
active memtable try_ref                  26804   pin it before probing
  skip_list_get_with_seq_ref             26808   the probe itself
immutable memtables                      26845   newest-first, refcount-protected
tidesdb_sstable_get                       9756   per level, per SSTable
  bloom check (skippable)                 9810   the line that bounds read amp
  block index find_slot                   9835   → raw file offset at 9837
```

**Compaction (step 6)** — `src/tidesdb.c`: trigger at `19918` (steer-to-bottom
branch at `19910`), enqueue + dedup at `25366`, worker thread at `20143`.

**The three "C makes it visible" anchors**, collected: one-malloc key+value at
`src/tidesdb.c:26579–26590` (step 3), the release/acquire pair around rotation at
`src/tidesdb.c:29847–29856` (step 6), and the raw-offset block index at
`src/tidesdb.c:9835–9837` (step 4).

## Questions to answer in notes.md

Each needs the source open. `python3 tools/pinned-source.py show tidesdb
src/tidesdb.c -r A:B` is the fastest way in.

1. Read `src/tidesdb.c:29792–29837` and identify the exact window during which an
   acknowledged write exists in the WAL but not in the memtable. What does a
   concurrent reader at `:26808` see during that window, and is that a bug? Name
   the field that decides.
2. `src/tidesdb.c:29814` only calls `tidesdb_unified_wal_group_sync` when the
   sync mode is `TDB_SYNC_FULL`. Find the other two modes in `src/tidesdb.h`,
   and say for each one exactly what is lost on power failure versus process
   crash. Then say which mode you would have to select to make a fair comparison
   against fjall's `PersistMode::SyncAll`.
3. `src/bloom_filter.c:207` computes `m = ceil(-n·ln(p)/(ln 2)²)` and `:223`
   computes `h = ceil((m/n)·ln 2)`. Evaluate both for p = 0.001 at
   n = 671,000, compare the filter size against the p = 0.01 case, and say what
   that extra memory buys you in expected block reads across a 20-SSTable level.
4. The block-index comment at `src/tidesdb.c:9838–9840` says the prefix index is
   *lossy* and a lookup may have to walk a run of blocks. Construct a key
   distribution that makes that run long, and say which of this repo's
   generators would produce it. What does that do to the Step 5 read-amp count?
5. Compare `src/tidesdb.c:29847–29856` with fjall's
   `src/keyspace/mod.rs:940–947`. Both rotate a full memtable. List every
   guarantee tidesdb states with an explicit `memory_order_*` argument that
   fjall gets from `Arc` and `MutexGuard` — and name one guarantee that is
   *harder* to see in the Rust version because of that.

## Done when

Answer each before unfolding it.

- [ ] You can match each fjall concept — journal, memtable, rotation, bloom, level metadata — to its tidesdb twin, with a file for each.

<details>
<summary>Answer</summary>

| fjall | tidesdb |
|---|---|
| journal (`src/journal/writer.rs`) | WAL frames via `src/block_manager.c`, appended at `src/tidesdb.c:29796` |
| `PersistMode::{Buffer,SyncData,SyncAll}` | `TDB_SYNC_{NONE,INTERVAL,FULL}`, branched at `src/tidesdb.c:29814` |
| memtable (skip list inside `lsm-tree`) | `src/skip_list.c`, with a per-thread arena allocator (`:22–42`) |
| `Keyspace` | column family (`tidesdb_column_family_t`) |
| rotation (`inner_rotate_memtable`, `mod.rs:727`) | CAS admission at `src/tidesdb.c:29850–29856` |
| bloom policy (`options.rs:108`) | `bloom_filter_new(bf, p, n)`, `src/bloom_filter.c:188` |
| segment / SST | SSTable written by `tidesdb_flush_memtable`, `src/tidesdb.c:24887` |
| level metadata (inside `lsm-tree`) | `src/manifest.c` |
| `snapshot_tracker` seqno watermark | per-memtable atomic refcounts + `try_ref`, `src/tidesdb.c:26804` |

</details>

- [ ] You can point at the three consecutive lines that implement the write-ahead rule, and say what would break if two of them swapped.

<details>
<summary>Answer</summary>

`src/tidesdb.c:29796` (WAL append), `:29814` (group fdatasync, `TDB_SYNC_FULL`
only), `:29837` (apply to the skip list). If the memtable apply moved *before*
the WAL append, a crash between them would leave a write that was visible to
readers — possibly read and acted on — but absent from the log, so replay would
silently lose it. That is the whole content of "write-ahead": the log must be
the superset. The `fdatasync` on `:29814` is the separate question of whether
the log's bytes have actually reached the platter; moving it after `:29837`
would not break correctness of replay, only shrink the durability window.

</details>

- [ ] You can derive a bloom filter's size and hash count from a target false-positive rate, using the source's formulas, and say what it buys in read amplification.

<details>
<summary>Answer</summary>

`src/bloom_filter.c:207`: `m = ceil(-n·ln(p) / (ln 2)²)`; `:223`:
`h = ceil((m/n)·ln 2)`.

For one 64 MiB memtable of 100-byte records — n = 671,000 — at p = 1%:
`m = ceil(671000 × 4.6052 / 0.4805)` = 6,431,575 bits = **804 KB**, i.e. 9.59
bits per key, and `h = ceil(9.59 × 0.6931)` = **7 hash functions** (inside the
source's own "typical real-world values are 7-15" range at `:225`).

What it buys: at Step 5's `continue` on line 25 of the read-path sketch, a
lookup for an absent key across 20 SSTables costs 20 × 0.01 = **0.2 expected
block reads** instead of 20 — a 100× reduction in read amplification for 804 KB
per file.

</details>

- [ ] You can name at least three things Rust's abstractions were doing for you that this codebase does by hand — and one thing the C makes clearer.

<details>
<summary>Answer</summary>

Hidden by Rust: (1) **lifetime pinning** — `Arc` versus tidesdb's explicit atomic
refcounts and `tidesdb_active_memtable_try_ref` with a bounded retry count
(`src/tidesdb.c:26804`, `:29761`); (2) **memory ordering** — every
`memory_order_release`/`acquire` at `src/tidesdb.c:29847–29856` is implicit in
`Arc`'s and `Mutex`'s internals; (3) **allocation and ownership** — the
coalesced key+value buffer at `:26579–26590`, where "only `op->key` should be
freed" is enforced by a comment rather than by `Box`.

Clearer in C: the *ordering* of the durability contract. In fjall the
write-ahead rule is implied by holding a `MutexGuard` across two method calls
(`src/keyspace/mod.rs:919–944`); in tidesdb it is three numbered lines you can
point at, and the fsync mode is a visible branch rather than an enum argument
threaded through a config struct.

</details>

- [ ] You can explain how a reader mid-lookup is protected from a concurrent flush or compaction, and where the mechanism is written.

<details>
<summary>Answer</summary>

By refcount pinning, stated in the source's own comment at
`src/tidesdb.c:26845–26848`: immutable-memtable pointers are snapshotted under a
single rwlock acquisition and each is pinned by refcount "so a concurrent
flush-worker eviction cannot free one out from under the scan". The active
memtable gets the same treatment via `tidesdb_active_memtable_try_ref`
(`:26804`), and the writer side releases with `memory_order_release`
(`:29847–29848`) so the reader's acquire sees a consistent skip list.

Rotation admission is a separate CAS on `is_flushing` (`:29855`) so exactly one
thread rotates; compaction uses the same trick with `is_compacting`, one per
column family, which is why `src/tidesdb.c:20139–20141` can promise that
"multiple workers can compact different CFs concurrently".

</details>

## References

**Code** (all at `tidesdb/tidesdb@810507a` — this repo's pin table entry;
confirm with `python3 tools/pinned-source.py ref tidesdb`)
- [tidesdb](https://github.com/tidesdb/tidesdb) — `src/tidesdb.c` (37,702 lines,
  the whole engine: `tidesdb_txn_put:26535`, `tidesdb_txn_commit:29697`,
  WAL append `:29796`, sync `:29814`, memtable apply `:29837`, rotation
  `:29850`, `tidesdb_flush_memtable:24887`, `tidesdb_sstable_get:9756`,
  compaction enqueue `:25366` and worker `:20143`), `src/skip_list.c` (2,929),
  `src/block_manager.c` (2,004), `src/manifest.c` (923), `src/bloom_filter.c`
  (624 — `bloom_filter_new:188`, sizing at `:207` and `:223`). Skim-read, 1–2 h

**This repo**
- [reading-fjall.md](reading-fjall.md) — the same lifecycle in Rust, with the
  concepts built from sequential-vs-random IO; read it first
- [reading-rocksdb-layout.md](reading-rocksdb-layout.md) — the same lifecycle
  again, industrialised, where each of these single files becomes a directory
- [FINDINGS.md](../../FINDINGS.md) row 1 — the measured LSM-vs-B-tree space
  amplification (0.45× vs 63.28×) that all of this machinery exists to move;
  `./verify.sh 01`
