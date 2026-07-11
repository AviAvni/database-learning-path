# Topic 4 — notes

## Predictions (fill BEFORE running write_amp)

At 10M ops, 3.3M distinct keys, 100B values, 1MB memtable, ratio 10 / K=4:
predicted levels = ___, so:

| Metric | leveled (predict) | leveled (measured) | tiered (predict) | tiered (measured) |
|---|---|---|---|---|
| write amp | ~ratio/2 × levels = | | ~levels = | |
| read amp (segs/get) | | | | |
| space amp | ~1.1 | | ~K | |
| load Kops/s | | | | |

## Reading answers

### lsm-tree crate
1. Why L0 can't be disjoint / what it costs:
2. Restart interval 16 trade; why B-tree pages don't:
3. Whole-version rewrite vs MANIFEST log breakdown point:

### RocksDB
1. Score vs round-robin — adversarial workload:
2. Partitioned index vs per-block hash index:
3. Blocked bloom FPR cost:

### Monkey
1. Uniform vs Monkey expected false probes (computed, then measured):
2. What breaks for range scans:
3. Zero-result-heavy workloads outside LSMs:

### Dostoevsky
1. Lazy-leveling score on MY measured numbers:
2. Why ranges don't benefit:
3. Universal-compaction knobs ≈ K and Z:

### TODS '21
1. CPU costs an LSM adds over a hash table:
2. Checksums-at-every-layer; FalkorDB/redis story:
3. The §4 lesson I'm applying to M4:

### Design space (VLDB '21)
- 5-system × 4-axis table:
- Prediction to test (granularity vs p99.9):

## Experiment findings

- Measured RUM table above; explain each gap between prediction and measurement:
- Bloom saved __% of probes; at what level did misses concentrate:
- Tombstone test: what compaction bug did the tests catch first try (log it):

## M4 log

- [ ] LSM backend behind the M1 trait; snapshot-as-SST bulk load uses trivial move:
- [ ] B+tree (M3) vs LSM bench on mutation stream + bulk load; p99 discipline:
- [ ] Where redis RDB/AOF sits vs both (seeds topic 5):
