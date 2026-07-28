# Topic 2 — notes

## Baseline (provided lane, Apple M3 Pro, measured 2026-07-28)

`cargo run --release --bin rehash_spike` — 10 M keys inserted one at a time,
every individual insert timed into an HdrHistogram (not criterion: the whole
point is the max, which averaging destroys).

| impl | p50 | p99 | p99.9 | p99.99 | max |
|---|---|---|---|---|---|
| hashbrown (doubling) | 42 ns | 291 ns | 1292 ns | 13.4 µs | **58.4 ms** |
| incremental (yours) | | | | | stub |

Per-decile max, in ns — the doubling sweeps are visible in the data:

```
[8110084, 13203125, 36417, 28320250, 85792, 46625, 51125, 58385375, 470917, 62083]
   8.1ms    13.2ms    36µs   28.3ms    86µs   47µs   51µs   58.4ms   471µs   62µs
```

**p50 is 42 ns and the max is 58.4 ms — a 1.4-millionfold spread inside one
operation type.** Four deciles carry a multi-millisecond spike and the rest are
in the tens of microseconds, because a rehash is not spread over anything: one
unlucky insert copies the whole table. That is the entire argument for redis's
incremental rehash, and it is why a p50 (or a mean, or a throughput figure)
cannot see this class of problem at all.

Note the spikes are NOT evenly spaced across deciles: they land where the table
crossed a power of two, and 10 M keys crosses 2²³ near the eighth decile — the
58.4 ms max. Your incremental map has to move that max to microseconds while
keeping p50 near 42 ns; the trade you are making is that *every* insert now
does a little migration work.

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
