# Topic 26 — Notes & measurements

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin filter_bench` — 10M sorted random u64 keys, 1M
queries per lane:

```
10000000 sorted u64 keys, 1000000 queries each lane
binary search (miss): 186 ns/lookup  (0 hits)
BTreeMap (miss):      240 ns/lookup  (0 hits)
HashSet (miss):       20 ns/lookup  (0 hits)  [224 MB]
binary search (hit):  177 ns/lookup  (1000000 found)
blocked bloom: [stub — implement bloom::BlockedBloom]
cuckoo: [stub — implement cuckoo::CuckooFilter]
hll: [stub — implement hll::Hll]
learned index: [stub — implement pgm::LearnedIndex]
```

The counts are seeded and exact (every miss lane finds 0, the hit lane finds
all 1000000; the HashSet is 224 MB, as first recorded); the timings are this
run's. Recorded 2026-07-10: 167 / 218 / 24 / 169 ns; this run: 186 / 240 /
20 / 177 ns — within ~20% on every lane.

> [FINDINGS.md](../../FINDINGS.md) row 26 quotes a third run (**246 / 299 /
> 28 ns** for the first three lanes). FINDINGS is canonical where they
> disagree; cite one run by name and never average them.

| lane | ns/lookup | note |
|---|---|---|
| binary search (miss) | 186 | ~23 dependent cache misses over 76 MB |
| BTreeMap (miss) | 240 | pointer-chasing tax over binary search |
| HashSet (miss) | 20 | the speed target — at 224 MB, the memory anti-target |
| binary search (hit) | 177 | hit ≈ miss: the cost is the walk, not the compare |

The whole topic in these four numbers: a filter should say "definitely
absent" at ~HashSet speed with ~5% of its memory (12 MB at 10 bpk), and a
learned index should collapse most of binary search's 23-miss walk.

## Predictions (fill BEFORE implementing the stubs)

| stub lane | predicted | measured |
|---|---|---|
| blocked bloom 10 bpk — ns/lookup and FPR | | |
| blocked bloom 8→16 bpk — FPR ratio | | |
| cuckoo 12-bit fp, 0.9 load — ns/lookup and FPR | | |
| hll 10M adds — relative error | | |
| learned ε=64 — ns/lookup | | |
| learned ε=256 — faster or slower than ε=64? | | |
| learned segments, 1M uniform | | |

(Fill the predicted column before implementing and the measured column after;
keep wrong predictions — they're the record.)

<details>
<summary>Author's prediction</summary>

| stub lane | prediction | reasoning |
|---|---|---|
| blocked bloom 10 bpk | ~15-25 ns, FPR 1.2-1.8% | 1 line fetch + 6 probes; theory 0.83% × 1.5-2 blocked tax |
| blocked bloom 8→16 bpk | FPR ~4× apart | each bit/key ≈ 2× FPR; test only demands halving |
| cuckoo 12-bit fp, 0.9 load | ~30-50 ns, FPR ~0.2% | 2 buckets = 2 possible misses; 8 slots × 2^−12 × load |
| hll 10M adds | err < 1.5% | σ = 0.81% at P=14; one seed, so anywhere within ~2σ |
| learned ε=64 | ~90-120 ns | segment descent cached + ~7-step window search, still 2-3 data misses |
| learned ε=256 | faster or slower than ε=64? | fewer segments (better cached) vs wider window — my bet: within 10% |
| learned segments, 1M uniform | ~500-1500 | cone ≥ optimal; optimal for uniform ≈ n/(ε²-ish scaling) is far under n/500 |

</details>

## Reference numbers for the exercise lanes

No reference run of the stub lanes is recorded for this topic. The stub tests
are the specification: BlockedBloom FPR < 2.5% at 10 bpk, under 4× theory, and
halving from 8 → 16 bpk; CuckooFilter FPR < 1% at 12-bit fingerprints and 90%
load; Hll error < 3% at 1K / 100K / 5M with exact-merge registers;
LearnedIndex window ≤ 2ε+2 and fewer than 2,000 segments (n/500) on 1M
uniform keys at ε=64.

## Questions to answer while reading (from the guides)

- [ ] Bloom Q1: why optimal k ⇒ half the bits set?
- [ ] Bloom Q4: build-can-fail (ribbon/cuckoo) vs monotone (bloom) — cost where?
- [ ] Cuckoo Q1: why `i1 XOR hash(fp)` and not `i1 XOR fp`?
- [ ] Cuckoo Q2: how does deleting a never-inserted key corrupt the filter?
- [ ] HLL Q2: show sigma() term ⇒ linear counting for n ≪ m.
- [ ] HLL Q4: ZERO/XZERO/VAL vs roaring containers — the density metric each switches on.
- [ ] Learned Q1: 4 points where the cone splits but optimal PLA doesn't.
- [ ] Learned Q4: ALEX under adversarial (clustered) inserts — predict, then paper §6.2.6.
- [ ] Roaring Q1: workload where per-chunk adaptivity beats per-matrix (topic 20).
- [ ] Postgres Q3: BRIN pruning condition; place timestamp / UUIDv4 / monotone ID.

## Cross-topic threads

- **Topic 4 (LSM)**: blooms exist because a point-miss touches every level.
  RocksDB's ribbon-below/bloom-above split (`bloom_before_level`) is a
  space-vs-CPU knob per level — cold levels are big (space matters) and
  rarely probed (CPU doesn't).
- **Topic 12 / BRIN**: zone maps are one-sided filters at range granularity;
  bloom is the same one-sidedness at key granularity, ~10,000× the bits.
- **Topic 20 / roaring**: array↔bitmap density crossover measured a third
  time (GraphBLAS per-matrix, roaring per-chunk, HLL sparse per-stream).
- **Topic 23**: postings stub already holds the array/bitmap containers;
  galloping intersect = MAXSCORE's skip = ALEX's exponential search — one
  primitive, three topics.
- **Topic 9**: HLL's exact-merge semilattice is what lets approximate
  count(DISTINCT) push below shuffles/shards.

## Capstone M-log (M26, per PLAN)

Target: secondary range indexes under MVCC + bloom filters in the LSM
backend + roaring bitmaps for label/type filtering + HLL fast path for
approximate `count(DISTINCT)`.

- Blocked bloom (this topic's stub, made SIMD later) per SST; 10 bpk to
  start, verify the ~1% FPR × per-level miss cost against topic 4's
  measured point-miss lane before spending 16 bpk.
- Label filter = one roaring bitmap per label over node IDs. Bulk loader
  allocates IDs monotonically ⇒ expect run containers; measure runs/label
  after load (reading-roaring-internals Q on ID allocator).
- HLL per (label, property) maintained on the write path — O(1) per insert
  (one register max), merged across shards at query time. Exact-merge
  register equality is the test.
- Learned index: NOT in M26. The ε window is elegant but our keys (node
  IDs) are already dense integers — a plain array *is* the perfect model.
  Revisit if/when property range indexes over timestamps show smooth CDFs.

## Infra notes

- Stub lanes wrapped in `catch_unwind` so filter_bench degrades to
  `[stub — implement …]` lines until each structure lands.
- Tests: 2 provided pass (hash avalanche/fastrange), 15 fail as `todo!()`
  panics — the contract to implement against.
- HLL stub spends a byte per register (16 KB vs redis's 12 KB packed) to
  skip the 6-bit shift dance; the estimator recipe in `hll.rs` doc comments
  is transcribed line-by-line from redis `hllCount`/`hllSigma`/`hllTau`.

## Done when

- [ ] All 17 tests green (`cargo test --release`).
- [ ] filter_bench stub lanes measured; predictions table graded honestly.
- [ ] Blocked bloom FPR ratio vs theory explained via Poisson crowding
  (compare against `CacheLocalFpRate`'s expectation).
- [ ] Can derive bloom FPR + optimal k on paper, and state cuckoo's
  partial-key involution from memory.
- [ ] One paragraph: which of bloom/cuckoo/xor/ribbon for (a) memtable,
  (b) immutable SST, (c) routing table with churn — with the space and
  build-failure trade for each.
