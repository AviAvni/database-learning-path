# Reference baselines — C FalkorDB 4.x and Rust FalkorDB 6.x

Numbers to chase. Recorded at M0; re-measure at every milestone that claims a win.
The bar is the C engine: a capstone number counts as a win only if its median, p99
and memory match or beat C FalkorDB on the same harness and machine. Rust FalkorDB 6.x
(the falkordb-rs-next-gen engine, now shipping as FalkorDB 6.0 on `FalkorDB/FalkorDB`
`main`) is the design reference the capstone rebuilds.

## Provenance (Fair Benchmarking §3.1 — reproducibility)

Both engines were built from clean, tagged commits in throwaway `git worktree`s of
`~/repos/FalkorDB`, and each identified itself by version at load (`MODULE LIST`).

| | C FalkorDB | Rust FalkorDB 6.x |
|---|---|---|
| Tag | `v4.22.2` (branch `4.22`) | `v6.0.3` (branch `6.0`) |
| Commit | `7b61f511d` (2026-10-19) | `c937aa4fa` (2026-10-22) |
| `MODULE LIST` ver | `42202` | `60003` |
| Build | `./build.sh` → `bin/macos-arm64v8-release/falkordb.so` (CMake `RelWithDebInfo`, Homebrew LLVM clang + libomp) | `cargo build --release` → `target/release/libfalkordb.dylib` (`lto = true`, `codegen-units = 1`; GraphBLAS/LAGraph/RediSearch from the `native-deps` cache, keyed on the submodule revisions) |

- Tracked sources were clean in both worktrees after the build. The one exception is
  C's `deps/libcsv`, whose autotools files the build itself regenerates.
- The C pin the guides cite, `ccb449a9a` (2026-07-15), is older than `v4.22.2` and was
  not measured. `v4.22.2` was chosen because it is the C engine that currently ships.
- The prebuilt `~/repos/FalkorDB/bin/macos-arm64v8-release/falkordb.so` (2026-07-24) was
  not used: it reports `ver 999999`, a dev build that cannot be traced to a commit.
- Machine: Apple M3 Pro, 36 GB, macOS 26.6.2, redis-server 8.6.2 (jemalloc 5.3.0).
- Client: Python 3.11.11, `FalkorDB==1.8.1`, `redis==8.1.0`, `pytest==9.1.1`,
  `pytest-benchmark==5.3.0`.
- Run: 2026-10-26. The engines ran one after the other, never concurrently. Load average
  was 2.9–3.4 on 11 cores during both runs (`baselines/load.log`): quiet, but not
  perfectly idle.

**Harness:** [`baselines/test_bench.py`](baselines/test_bench.py) runs the workload of
the reference's old `tests/test_bench.py` (removed from FalkorDB at `3e7690dc1`; read it
with `git show 3e7690dc1^:tests/test_bench.py`). It keeps the same queries, the same
graph and the same fresh-graph-per-round setup. Two things changed. It starts its own
redis-server from `$FALKORDB_MODULE`, so one file drives both engines. And it collects
more samples: pedantic benches run 100 rounds (was 5) and the others
`--benchmark-min-rounds=100`. Memory comes from [`baselines/mem.py`](baselines/mem.py).
Reproduce everything with [`baselines/run.sh`](baselines/run.sh), which ends by calling
[`baselines/summarize.py`](baselines/summarize.py). The raw per-round timings are in
`baselines/bench-*.json` (`stats.data`), and the memory rows are in
`baselines/mem-*.json`.

## Methodology caveats (topic 0 lens — read before comparing)

- **Closed loop** (pytest-benchmark): these are *service times* through a Python client,
  not latency under load. There is no target rate, so coordinated omission does not
  arise. That is fine for comparing engine throughput, but these numbers cannot back a
  claim about tails under load.
- **p99 is nearest-rank over N raw samples.** It is the ⌈0.99·N⌉-th smallest sample.
  With N = 100 that is the second-largest sample, so one stall moves it. Treat a p99
  gap under ~10% as noise. N is listed per row. Every sample is a single query
  (`iterations == 1`, which `summarize.py` asserts).
- `test_return` is the **client + RESP + dispatch floor** (86–109 µs median here), and
  every other number includes it. For small queries, engine-side cost ≈ measured −
  floor.
- `match_*` results stream every row back through the Python client. At 100K rows,
  parsing on the client dominates, which is why C and Rust land within 3% of each
  other there. Compare capstone numbers using the *same* client, or not at all (apples
  vs oranges, §3.3).
- create/delete benches use `benchmark.pedantic`: a fresh graph each round, 1 warmup
  round, and setup time excluded.

## Results (per full query; N = samples)

Median and p99, with the ratio **Rust ÷ C** on the median (< 1 means Rust is faster).

| Benchmark | n | N (C / Rust) | C median | C p99 | Rust median | Rust p99 | Rust÷C |
|---|--:|--:|--:|--:|--:|--:|--:|
| `RETURN 1` | — | 284 / 156 | 109 µs | 251 µs | 86 µs | 188 µs | 0.79 |
| unwind range | 1,000 | 308 / 328 | 2.92 ms | 9.89 ms | 2.71 ms | 6.89 ms | 0.93 |
| unwind range | 100,000 | 100 / 100 | 296 ms | 314 ms | 287 ms | 301 ms | 0.97 |
| create_node | 1,000 | 100 / 100 | 318 µs | 383 µs | 375 µs | 466 µs | 1.18 |
| create_node | 100,000 | 100 / 100 | 18.4 ms | 20.6 ms | 29.5 ms | 35.6 ms | **1.60** |
| create_relationship (2 nodes + 1 edge each) | 1,000 | 100 / 100 | 701 µs | 875 µs | 959 µs | 1.14 ms | 1.37 |
| create_relationship | 100,000 | 100 / 100 | 60.3 ms | 65.1 ms | 78.3 ms | 85.3 ms | **1.30** |
| match_node (return all) | 1,000 | 102 / 100 | 8.8 ms | 13.8 ms | 8.74 ms | 14.7 ms | 0.99 |
| match_node | 100,000 | 100 / 100 | 983 ms | 1.02 s | 971 ms | 1.00 s | 0.99 |
| match_relationship (return n,r,m) | 1,000 | 100 / 100 | 22.7 ms | 28.2 ms | 21.4 ms | 26.4 ms | 0.94 |
| match_relationship | 100,000 | 100 / 100 | 2.50 s | 2.61 s | 2.43 s | 2.47 s | 0.97 |
| delete_node | 1,000 | 100 / 100 | 468 µs | 592 µs | 292 µs | 341 µs | 0.62 |
| delete_node | 100,000 | 100 / 100 | 30.7 ms | 33.3 ms | 16.2 ms | 17.4 ms | 0.53 |
| delete_relationship | 1,000 | 100 / 100 | 1.42 ms | 1.54 ms | 608 µs | 676 µs | 0.43 |
| delete_relationship | 100,000 | 100 / 100 | 88.4 ms | 118 ms | 30.2 ms | 33.2 ms | 0.34 |

Rust 6.0.3 does **not** meet the C bar on bulk creation. Its create_node(100K) median is
1.60× C's and its create_relationship(100K) median is 1.30×, and its p99 is worse in
both. It beats C by 2–3× on deletes. On unwind, match and the floor the two engines are
within a few percent, except `RETURN 1`, where Rust is about 20% faster.

### Memory (MiB; fresh server per row)

`mem.py` starts a fresh redis-server for each row, loads the graph with the bench's own
CREATE query, waits 1 s, then reads `INFO memory`. It then runs the bench's MATCH query
once (returning all n rows) and shuts down. Peak RSS is the "maximum resident set size"
that `/usr/bin/time -l` reports for the whole server lifetime. In both engines
`used_memory_peak` was already at its final value right after the load, so the peak
comes from the CREATE, not from the MATCH.

| Graph | C `used_memory` after load | C `used_memory_peak` | C peak RSS | Rust `used_memory` after load | Rust `used_memory_peak` | Rust peak RSS |
|---|--:|--:|--:|--:|--:|--:|
| empty server | 2.8 | 2.8 | 19.8 | 2.9 | 2.9 | 22.5 |
| 100K nodes | 9.5 | 46.5 | 63.1 | 7.2 | 26.9 | 61.3 |
| 1M nodes | 60.1 | 378.7 | 388.1 | 44.1 | 297.5 | 304.3 |
| 100K × (2 nodes + edge) | 19.9 | 103.9 | 111.0 | 18.5 | 61.9 | 93.4 |
| 1M × (2 nodes + edge) | 150.4 | 886.1 | 878.5 | 153.9 | 705.9 | 693.0 |

- Steady-state size is close between the engines. Rust is smaller on node-only graphs
  (44.1 vs 60.1 MiB at 1M nodes) and about the same on the edge graph (153.9 vs
  150.4 MiB). Both engines allocate through the Redis allocator: Rust's `used_memory`
  agrees with its own `GRAPH.MEMORY USAGE` total (141 MB at 1M edges), so the two
  `used_memory` columns are comparable. The raw JSON also records `used_memory_rss` and
  the per-component `GRAPH.MEMORY USAGE` breakdown.
- **The load peak, not the resting graph, sets peak memory.** A single
  `UNWIND … CREATE` of 1M edges peaks at 5.9× the resting size in C (886 vs 150 MiB)
  and at 4.6× in Rust (706 vs 154 MiB). Rust's peak RSS is lower in every loaded row,
  by 3–22%. A capstone that matches resting size but buffers the whole CREATE has not
  met the bar.

## Superseded: July 2026 numbers (Rust only)

These medians were measured on 2026-07-10 against `falkordb-rs-next-gen` @ `e8a44d25`
(release `libfalkordb.dylib`). They are superseded for three reasons. The working tree
had uncommitted changes to `graph/src/graph/graphblas/*`, so the build is not
reproducible. They give medians only, with 5 rounds per create/delete bench, and no
memory. And there was no C number at all, so they could not answer whether the
reference met the C bar. The client stack is also undocumented (that venv is gone), so
the gap between these numbers and today's is not attributable to the engine. Do not
compare against them.

| Benchmark | n=1,000 | n=100,000 |
|---|--:|--:|
| `RETURN 1` | 100 µs | — |
| unwind range | 1.79 ms | 177 ms |
| create_node | 555 µs | 34.1 ms |
| create_relationship | 1.01 ms | 94.4 ms |
| match_node | 5.73 ms | 667 ms |
| match_relationship | 14.8 ms | 1.78 s |
| delete_node | 518 µs | 28.1 ms |
| delete_relationship | 861 µs | 46.3 ms |

## What M-milestones should chase

The target for each row is the better of the C and Rust columns, at both median and
p99. Memory is held to the same rule.

- **M13 (naive graph core):** create_node / create_relationship / delete_* engine-side
  throughput. Beat C on creation (C is the faster engine there) and Rust on deletion,
  after subtracting the ~0.1 ms RESP + Python floor. Keep the 1M-edge load peak under
  C's 886 MiB `used_memory_peak`.
- **M7 (RESP server):** reproduce `test_return`'s 86–109 µs floor with falkordb-py
  against the capstone server, then measure properly (open loop + HdrHistogram).
- **M20 (sparse core):** match_* on the traversal side. Use LDBC (M22), not these micro
  matches, for the headline comparison, because at 100K rows the client parse hides
  the engine.
