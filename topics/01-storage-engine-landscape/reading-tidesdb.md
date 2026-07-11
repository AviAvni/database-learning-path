# tidesdb: the same LSM with nothing abstracted away

The value of this skim (1–2 h) is seeing the machinery you just traced in
fjall rendered in plain C, with *nothing* hidden — memory ordering, pointer
arithmetic, and disk offsets are all in your face. Read it as a contrast
exercise: match each fjall concept to its C twin and notice exactly what
Rust's abstractions buy you, and what they conceal.

## Layout

| File | Role |
|------|------|
| `tidesdb.c` (~38K lines) | the whole engine: write/read/compaction orchestration |
| `skip_list.c` | memtable — lock-free skip list, arena bump allocator |
| `block_manager.c` | physical block IO (WAL + SSTs) |
| `bloom_filter.c` | ~600 lines, readable bloom filter |
| `manifest.c` | level metadata: which SST is in which level |

## Write path (file:line)

```
tidesdb_txn_put            tidesdb.c:26535   stage in per-txn ops array
tidesdb_txn_commit         tidesdb.c:29780   serialize WAL batch → block_manager_write_raw
apply_ops_to_memtable      tidesdb.c:29837   skip-list inserts (atomic refcounts)
rotate check (CAS loop)    tidesdb.c:29850   memtable over threshold → rotate
tidesdb_flush_memtable     tidesdb.c:24887   worker serializes skip list → compressed SST
```

## Read path (file:line)

```
txn write-set check        tidesdb.c:26672   your own uncommitted writes first
active memtable            tidesdb.c:26808   skip_list_get_with_seq_ref
immutable memtables        tidesdb.c:26845   newest-first, refcount-protected
tidesdb_sstable_get        tidesdb.c:9756    per level: bloom (9810) → block index
                                             binary search (9832) → scan blocks
```

Exactly the README §1 LSM read diagram, one function per box. Which is to
say, in code:

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

## Compaction

- Enqueue after flush when level over capacity: `tidesdb.c:19910`.
- Dedup queued work via CAS `is_compacting` flag: `tidesdb_enqueue_compaction`,
  `tidesdb.c:25366` — geometry computed at *dequeue* time, not enqueue.
- Worker picks L_i → L_{i+1} by SSTable counts: `tidesdb.c:20143`.

## What the C makes visible

1. **Key+value in one malloc** (`tidesdb.c:26579`): `op->value = op->key + key_size`
   — layout as pointer arithmetic. Rust equivalent would be a single `Box<[u8]>` with
   split indices; here the trick is load-bearing and explicit.
2. **Memory ordering spelled out** (`tidesdb.c:29761`): `memory_order_acq_rel` on the
   memtable refcount during rotation. Rust's `Arc` hides exactly these barriers —
   topic 9 makes you write them yourself.
3. **Block index returns raw file offsets** (`tidesdb.c:9835`): the reader seeks to a
   byte position from a struct array. No cursor abstraction — the disk format *is*
   the data structure.

## Done when

You've matched each fjall concept (journal, memtable, rotation, bloom, level) to its
C twin and noticed the abstractions Rust buys you — and what they hide.

## References

**Code**
- [tidesdb](https://github.com/tidesdb/tidesdb) — `tidesdb.c` (~38K
  lines, the whole engine), `skip_list.c`, `block_manager.c`,
  `bloom_filter.c` (~600 readable lines), `manifest.c` (shallow clone at
  `~/repos/tidesdb`; skim-read, 1–2 h)
