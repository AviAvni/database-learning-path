# tidesdb: the same LSM with nothing abstracted away

The value of this skim (1–2 h) is seeing the machinery you just traced in
fjall rendered in plain C, with *nothing* hidden — memory ordering, pointer
arithmetic, and disk offsets are all in your face. This chapter first
rebuilds the LSM lifecycle step by step, each time pointing at the concrete
C structure that fjall's Rust abstractions wrap. Read it as a contrast
exercise: match each fjall concept to its C twin and notice exactly what
Rust's abstractions buy you, and what they conceal.

## The problem in one sentence

Do fjall's job — absorb random-key writes as sequential IO, survive crashes,
answer reads in a handful of file probes — in ~40K lines of C where every
byte offset, atomic barrier, and malloc is spelled out by hand.

## The concepts, step by step

### Step 1 — the LSM recipe, restated in plain C terms

An LSM (log-structured merge) engine never updates data in place. It appends
every write to a log file for crash safety, buffers the same write in a
sorted in-memory structure (the **memtable**), periodically dumps the full
memtable to disk as an immutable sorted file (an **SSTable**), and merges
those files in the background (**compaction**) to keep reads cheap. (The
fjall chapter builds the *why* of each piece from sequential-vs-random IO;
this one shows each piece as bytes and structs.) In tidesdb every one of
those nouns is a file you can open: the memtable is `skip_list.c`, the log
and SSTables go through `block_manager.c`, the "maybe present?" filter is
`bloom_filter.c`, and the list of which SSTable belongs to which level is
`manifest.c`. Nothing else. That is the whole engine.

### Step 2 — the memtable is a skip list you can read

A **skip list** is a sorted linked list with "express lanes": each node gets
a random height, and higher lanes skip over many nodes, so search is
O(log n) like a balanced tree — but insertion never rebalances anything,
which makes it easy to run lock-free (concurrent threads use atomic
pointer swaps instead of locks).

```
 level 3:  head ──────────────────► k₄₀ ─────────────────► nil
 level 2:  head ────────► k₂₂ ────► k₄₀ ────────► k₇₈ ───► nil
 level 1:  head ─► k₀₇ ─► k₂₂ ────► k₄₀ ─► k₅₅ ─► k₇₈ ───► nil
 level 0:  head ─► k₀₇ ─► k₁₃ ─► k₂₂ ─► k₄₀ ─► k₅₅ ─► k₇₈ ─► nil
            search(k₅₅): drop down whenever the next key overshoots
            → ~log₂(n) hops instead of n
```

tidesdb's `skip_list.c` also shows the allocation strategy fjall's Rust
hides: an **arena bump allocator** — one big malloc'd slab, and each insert
just bumps a pointer forward. No per-node free; the whole arena dies when
the memtable is flushed. Cheap allocation, and it makes the "memtable size
limit" check a single pointer comparison.

### Step 3 — the write path: a WAL batch is just bytes at an offset

tidesdb groups writes into transactions: `tidesdb_txn_put` stages each
operation in a per-transaction ops array, and `tidesdb_txn_commit`
serializes the whole batch and hands it to `block_manager_write_raw` — a
raw append of length-prefixed bytes to the log file. Only after the log
append do the ops go into the skip list (`apply_ops_to_memtable`) — the
write-ahead rule, visible as two consecutive C calls.

One detail the C makes load-bearing and explicit: **key and value share one
malloc** (`tidesdb.c:26579`): `op->value = op->key + key_size` — the value
pointer is just the key pointer plus an offset. Layout as pointer
arithmetic. The Rust equivalent would be a single `Box<[u8]>` with split
indices; here you *see* that one allocation per op is a deliberate
throughput decision, not an accident.

Cost, same as fjall: every byte is written twice (log now, SSTable later),
and commit latency is the fsync policy on the log.

### Step 4 — the SSTable made explicit: build the bloom, write the offsets

When the memtable is over threshold, a worker (`tidesdb_flush_memtable`)
walks the skip list in key order and writes an SSTable: compressed blocks of
sorted key-value pairs, a **block index** (an array of "first key → byte
offset in this file" entries), and a **bloom filter** (a bit array set by k
hash functions; ~10 bits/key gives ~1% false positives on "is this key maybe
in this file?").

In fjall both helpers are inside the `lsm-tree` crate; here you can read
them end to end. `bloom_filter.c` is ~600 lines — the hash mixing, the bit
math, all of it. And the block index returns **raw file offsets**
(`tidesdb.c:9835`): the reader binary-searches a struct array and then
`seek()`s to a byte position. No cursor abstraction — the disk format *is*
the data structure. That is what "immutable sorted file" actually means at
the bottom: a byte layout you can compute offsets into.

### Step 5 — the read path: every potential miss, one function per stop

A read must check every place a newer version of the key could hide,
newest-first, and return the first hit. tidesdb performs each stop as a
separate, named function call — the read path *is* the topic README's §1 LSM
read diagram, one function per box. In pseudo-Rust:

```rust
fn get(&self, key: &[u8]) -> Option<Val> {
    if let Some(v) = self.txn_write_set.get(key) { return Some(v); } // own writes first
    if let Some(v) = self.active_memtable.get(key) { return Some(v); }
    for mt in self.immutable_memtables.newest_first() {              // refcount-pinned
        if let Some(v) = mt.get(key) { return Some(v); }
    }
    for level in &self.levels {
        for sst in level.newest_first() {
            if !sst.bloom.might_contain(key) { continue; }  // skips MOST absent-key IO
            let off = sst.block_index.binary_search(key)?;  // a raw file offset —
            if let Some(v) = sst.read_block_at(off).find(key) {  // the disk format IS
                return Some(v);                                  // the data structure
            }
        }
    }
    None    // read amp made concrete: every stop above was a potential miss
}
```

Count the stops: write set, active memtable, N immutable memtables, then
per level per SSTable a bloom check and maybe one block read. That count is
**read amplification** as a for-loop — and the bloom `continue` is the line
that keeps it affordable (1% false positives ⇒ ~0.2 block reads for an
absent key across 20 SSTables, instead of 20).

### Step 6 — rotation and compaction: the concurrency is hand-rolled

Two mutation streams run concurrently with reads: memtable **rotation**
(swap a full memtable for a fresh one, hand the full one to the flush
worker) and **compaction** (merge SSTables within/between levels to bound
read amplification and drop shadowed versions). Both need object-lifetime
guarantees — a reader mid-lookup must not have its memtable freed under it.

fjall gets this from `Arc` for free. tidesdb writes it out: memtables carry
atomic **refcounts**, and rotation uses a CAS (compare-and-swap) loop with
**memory ordering spelled out** (`tidesdb.c:29761`):
`memory_order_acq_rel` on the memtable refcount during rotation. Rust's
`Arc` hides exactly these barriers — topic 9 makes you write them yourself.

Compaction scheduling is equally visible:

- After a flush, if a level is over capacity, work is enqueued (`tidesdb.c:19910`).
- Queued work is deduplicated via a CAS `is_compacting` flag
  (`tidesdb_enqueue_compaction`, `tidesdb.c:25366`) — and the merge geometry
  is computed at *dequeue* time, not enqueue, so it reflects current state.
- The worker picks which L_i → L_{i+1} merge to run by SSTable counts
  (`tidesdb.c:20143`).

Cost, same trade as every LSM: background write amplification purchased to
keep the Step 5 for-loop short.

## Where each step lives in the code

| File | Role (steps) |
|------|------|
| `tidesdb.c` (~38K lines) | the whole engine: write/read/compaction orchestration (3, 5, 6) |
| `skip_list.c` | memtable — lock-free skip list, arena bump allocator (2) |
| `block_manager.c` | physical block IO (WAL + SSTs) (3, 4) |
| `bloom_filter.c` | ~600 lines, readable bloom filter (4) |
| `manifest.c` | level metadata: which SST is in which level (6) |

**Write path (steps 2–4), file:line**

```
tidesdb_txn_put            tidesdb.c:26535   stage in per-txn ops array
tidesdb_txn_commit         tidesdb.c:29780   serialize WAL batch → block_manager_write_raw
apply_ops_to_memtable      tidesdb.c:29837   skip-list inserts (atomic refcounts)
rotate check (CAS loop)    tidesdb.c:29850   memtable over threshold → rotate
tidesdb_flush_memtable     tidesdb.c:24887   worker serializes skip list → compressed SST
```

**Read path (step 5), file:line**

```
txn write-set check        tidesdb.c:26672   your own uncommitted writes first
active memtable            tidesdb.c:26808   skip_list_get_with_seq_ref
immutable memtables        tidesdb.c:26845   newest-first, refcount-protected
tidesdb_sstable_get        tidesdb.c:9756    per level: bloom (9810) → block index
                                             binary search (9832) → scan blocks
```

**Compaction (step 6)**: enqueue at `tidesdb.c:19910`, CAS dedup at
`tidesdb.c:25366`, level-pick at `tidesdb.c:20143`. The three
"C makes it visible" anchors from the steps, collected: one-malloc key+value
`tidesdb.c:26579` (step 3), `memory_order_acq_rel` refcount `tidesdb.c:29761`
(step 6), raw-offset block index `tidesdb.c:9835` (step 4).

## Done when

You've matched each fjall concept (journal, memtable, rotation, bloom, level) to its
C twin and noticed the abstractions Rust buys you — and what they hide.

## References

**Code**
- [tidesdb](https://github.com/tidesdb/tidesdb) — `tidesdb.c` (~38K
  lines, the whole engine), `skip_list.c`, `block_manager.c`,
  `bloom_filter.c` (~600 readable lines), `manifest.c` (shallow clone at
  `~/repos/tidesdb`; skim-read, 1–2 h)
