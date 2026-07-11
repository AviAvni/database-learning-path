# Code reading — RocksDB `tools/db_bench_tool.cc`

Source: `~/repos/rocksdb/tools/db_bench_tool.cc` (~10,400 lines, shallow clone @
`7c80a5a`). **Do not read this linearly** — it's a flag-driven monolith. The study
guide's goal is the *vocabulary*: these workload names and flags are the lingua franca
of storage benchmarking (LevelDB inherited → RocksDB extended → every LSM paper since).

## Skim route (30–60 min)

| Lines | What |
|-------|------|
| 115–170 | `DEFINE_string(benchmarks, ...)` — the full workload menu; read the help text below it (172+), it's the best documentation |
| 275–458 | The knobs that define a workload: `num`, `threads`, `value_size`, `histogram`, `read_random_exp_range` (452) |
| 1708–1717 | `keyrange_dist_a..d` — the mixgraph skew model |
| 2436 | `class Stats` — per-thread stats, `HistogramImpl` per op type |
| 2564 | `Stats::FinishedOps` — where each op's micros get recorded |
| 3802 | `GenerateKeyFromInt` — int → fixed-width key; all key distributions reduce to picking the int |
| 4030–4140 | `Benchmark::Run()` dispatch: `name == "fillseq"` → method pointer — the map from workload name to implementation |
| 4583 | `RunBenchmark(n, name, method)` — spawns N threads, merges per-thread `Stats` (histogram merge at 2488, same lesson as Tene: merge histograms, never average percentiles) |
| 5869 | `enum WriteMode { RANDOM, SEQUENTIAL, UNIQUE_RANDOM }` |
| 6088 | `class KeyGenerator` — how UNIQUE_RANDOM permutes the key space |

## The vocabulary worth memorizing

- **`fillseq`** — sequential-order load. Fast path for an LSM (no compaction debt);
  papers use it to build the DB before the real test.
- **`fillrandom` / `overwrite`** — random inserts vs. random overwrites of existing
  keys (overwrite creates garbage → compaction pressure — different beast).
- **`fillsync`** — one fsync per write, N/1000 ops: measures durability cost, not
  throughput.
- **`readrandom` / `readseq` / `readreverse`** — point lookups vs. iterator scans.
- **`readwhilewriting`** — 1 writer + N readers: the "does compaction wreck my read
  tail?" test. The `*whilemerging`/`*whilescanning` variants isolate other interference.
- **`seekrandom`** — iterator Seek cost (touches every level; very different profile
  from Get).
- **`multireadrandom`** — MultiGet batching.
- **`mixgraph`** (4133) — the odd one out: models Facebook's *measured* production
  distributions (the "Characterizing, Modeling..." FAST'20 paper) with two-term-exponential
  key ranges (`keyrange_dist_a..d`) and Pareto value sizes. The industrial answer to
  "uniform random keys are the wrong distribution" — same motivation as the capstone's
  Zipfian `workload` crate.
- Standard invocation shape: `db_bench --benchmarks=fillseq,readrandom --num=10000000
  --value_size=100 --histogram` — comma list runs *in order against the same DB*, so
  earlier benchmarks create the state later ones measure. That ordering **is** the
  methodology.

## What to notice about measurement

- Per-op latency goes through `FinishedOps` (2564) into a plain `HistogramImpl` —
  reported only with `--histogram`. Default output is *throughput* (ops/s, MB/s).
- It's a **closed loop** like redis-benchmark: each thread issues the next op after the
  previous completes. There's a `--benchmark_write_rate_limit`/read-rate variant for
  paced writes, but no coordinated-omission correction — same critique applies.
- db_bench measures the *embedded* engine (no network), so "latency" here is service
  time by construction — legitimate for engine work, misleading if quoted as user latency.

## Takeaway

db_bench's value is the workload taxonomy, not the harness. When topic 4 (LSM) and M4
(backend shootout) arrive, name capstone benches in this vocabulary (`fillseq`,
`readrandom`, `readwhilewriting`) so numbers are comparable against published RocksDB
results.
