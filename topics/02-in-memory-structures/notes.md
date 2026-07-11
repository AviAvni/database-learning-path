# Topic 2 — notes

## Predictions (fill BEFORE running benches)

| Bench | hashbrown | BTreeMap | crossbeam SkipMap | my skiplist | my inc. map |
|---|---|---|---|---|---|
| point lookup, 1e7, Zipf (ns/op) | | | | | |
| insert 1e6 (M ops/s) | | | | | |
| ordered scan 1e6 (M elems/s) | | | | | |
| rehash_spike max: hashbrown vs incremental | | — | — | — | |

## Reading answers

### redis dict (reading-redis-dict.md)
1. Insert into ht[0] during rehash — why a bug:
2. pauserehash exists for:
3. empty_visits=10n tail guarantee:

### redis skiplist (reading-redis-skiplist.md)
1. Why skiplist + dict both:
2. Expected search cost at p=0.25, priced vs measured:

### hashbrown (reading-hashbrown.md)
1. 7/8 vs 1.0 load factor:
2. Hash policy paragraph (for M2 decision):
3. DELETED churn ↔ LSM tombstones:

### RocksDB memtable (reading-rocksdb-memtable.md)
1. spans/backward under concurrent CAS:
2. acquire/release vs SeqCst at line 383:
3. Miss estimate vs hashbrown number:

### rax / ART / SwissTable talk
- (questions in each guide)

## Experiment findings

- rehash_spike table + per-decile max:
- Where my skiplist loses to hashbrown and by how much (RUM terms):
- Implementation trade I chose for skiplist node layout, and why:

## M2 log

- [ ] attribute-store design written BEFORE peeking at reference
- [ ] comparison vs reference attribute_store.rs / string_pool.rs:
- [ ] hash policy decision + bench evidence:
