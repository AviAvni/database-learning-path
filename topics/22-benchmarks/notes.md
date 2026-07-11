# Topic 22 notes — standard benchmarks

## Baseline (provided code, Apple M3 Pro, measured 2026-07-10)

### TPC-H choke points (bench_suite, dbgen-lite)

| SF | rows | Q1 oracle ms | Q1 GB/s | Q6 oracle ms | Q6 GB/s |
|---|---|---|---|---|---|
| 0.05 | 300K | 2.4 | 4.7 | 0.7 | 11.9 |
| 0.25 | 1.5M | 10.2 | 5.6 | 2.7 | 15.7 |

Q1 oracle: HashMap entry per row (even with only 6 groups) + 4 f64
FMAs — hashing dominates, exactly the CP1.2 story: the group domain
is tiny, so a real engine replaces the hash with an array and turns
Q1 into an expression benchmark. Q6 branchy oracle already at
15.7 GB/s — the 2% selectivity means the branch is nearly
always-false, i.e. perfectly predicted (topic 0's sorted case).

### YCSB A-F (1M keys, 500K ops, uniform, BTreeMap store)

| workload | Mops/s | p50 ns | p99 ns | p999 ns |
|---|---|---|---|---|
| A update-heavy | 2.88 | 292 | 792 | 2083 |
| B read-mostly | 4.15 | 208 | 500 | 667 |
| C read-only | 3.72 | 209 | 542 | 958 |
| D read-latest | 4.40 | 208 | 417 | 625 |
| E short-ranges | 1.11 | 917 | 1583 | 2041 |
| F read-mod-write | 2.85 | 292 | 708 | 1541 |

E is 4× slower than C — a 100-element range walk per op; updates add
allocation (the vec![1u8;100] per update). The mix table alone
predicts the ordering: D ≥ B ≥ C > A ≈ F > E. (C < B is real:
C's every op is a read against a fully-cold... no — same store;
likely noise + B's 5% updates keeping allocator warm. Re-measure
when implementing zipf.)

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| q1_flat speedup over HashMap oracle at SF 0.25 — ×? | | |
| q6_branchless vs branchy oracle at 2% selectivity — faster, same, SLOWER? | | |
| q6 at 50% selectivity (edit the predicate): branchy vs branchless ×? | | |
| zipf .99, n=1M: fraction of ops on top-100 keys | | |
| YCSB A uniform → zipf: throughput up or down, and p999? | | |
| YCSB E uniform → zipf: does it move at all? | | |

## Implementation log

- [ ] zipf.rs Zipfian + Scrambled — statistical tests green
- [ ] tpch.rs q1_flat + q6_branchless — match oracles, bench lanes fill
- [ ] prediction table reconciled
- [ ] run real TPC-H SF1 in DuckDB (python duckdb; PRAGMA tpch(1/6/9)),
      record Q1/Q6/Q9 ms + threads=1 numbers, compare with our lanes
- [ ] stretch: 50%-selectivity Q6 variant — reproduce topic 17's
      branchy crater inside a "TPC-H" query
- [ ] stretch: coordinated-omission demo — open-loop driver at 80%
      of closed-loop max rate, compare p999

Surprises / dead ends:

- Nearest-rank percentile off-by-one: round((n-1)·p) gave 501 for
  p50 of 1..=1000; switched to ceil(p·n)-1. Benchmark harness bugs
  are benchmark results bugs.

## Questions from the reading guides

### Boncz TPC-H (reading-boncz-tpch.md)

1. Q1/Q6/Q9 → Cypher choke-point analogues:
2. Which dbgen-lite columns need correlation to break q1_flat:
3. Q6 2% vs 50% selectivity branchy/branchless crossover:
4. Which of the 22 queries an IVM engine answers in O(1):
5. What TPC-C NewOrder tests that no TPC-H query can:

### YCSB (reading-ycsb.md)

1. P(rank 0) = 1/ζ(n,θ) derivation; top-100 mass at n=1M:
2. The two fast paths — what fraction of draws at θ=.99:
3. Per-workload uniform→zipf prediction (table above):
4. Coordinated-omission fix sketch; workload E p999:
5. Why plain zipfian on a growing keyspace is wrong (zetan staleness):

### OLTP-Bench + TPC-C (reading-oltpbench-tpcc.md)

1. D_NEXT_O_ID abort rate under OCC at 4WH×16T:
2. Why load-time vs run-time C_LAST constants must differ:
3. "TPC-C for graphs" — the supernode hot counter:
4. Open vs closed loop p999 on workload E:
5. Phased-rate config reproducing an eviction storm:

### DuckDB tpch extension (reading-duckdb-tpch.md)

1. DuckDB Q1/Q6 SF1 measured; gap analysis vs our lanes:
2. answers/ over queries/ — oracle taxonomy:
3. dbgen-as-table-function = which topic-11 concept:
4. Q9 with join order disabled — how much slower:
5. capstone ldbc_datagen table function requirements:

## Cross-topic threads

- Q6 branchy-at-2%-is-fine = topic 0's branch_misprediction sorted
  case; the 50% crater is topic 17's filter curve — selectivity
  decides the winner, benchmarks pick the selectivity.
- Q1's flat group array = topic 11's exec engine trick; TPC-H Q1 is
  the reason that trick exists.
- Zipfian generator = capstone workload crate's distribution (topic
  0) — now with the actual YCSB math and statistical contract.
- TPC-C's D_NEXT_O_ID = topic 8's write-hot-key MVCC abort story =
  topic 9's contended counter, institutionalized as a benchmark.
- dbgen uniform data = why topic 10's JOB exists; cardinality lies
  need correlated data to show up.
- Benchmark harness = topic 16's oracle discipline: answers ship
  with queries or you're measuring wrong fast.

## M22 log (capstone)

- [ ] standing suite: LDBC SNB interactive + graph micro + ann-bench
      recall/QPS, one command, results as committed data files
- [ ] regression tracking across milestones (M0 BASELINES.md is the
      floor)
- [ ] three-way shootout falkordb-scratch / falkordb-rs-next-gen /
      FalkorDB with the sins checklist applied

## Done when

- Both stub pairs green with lanes filled; prediction table
  reconciled; DuckDB SF1 numbers recorded and explained; guide
  questions answered; M22 suite design sketched.
