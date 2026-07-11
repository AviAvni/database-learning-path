# Reference baselines — falkordb-rs-next-gen

Numbers to chase. Recorded at M0; re-measure at every milestone that claims a win.

## Provenance (Fair Benchmarking §3.1 — reproducibility)

- Reference: [`~/repos/falkordb-rs-next-gen`](https://github.com/FalkorDB/falkordb-rs-next-gen) @ `e8a44d25` (2026-07-02), **release**
  build `target/release/libfalkordb.dylib` — note: working tree had uncommitted
  changes to `graph/src/graph/graphblas/*` at build time, so treat these as
  "e8a44d25-ish"; rebuild from a clean commit before quoting anywhere serious.
- Harness: the reference's own `tests/test_bench.py` (pytest-benchmark), local
  redis-server loading the module, Python `falkordb` client.
- Machine: Apple Silicon macOS (same box as all topic benchmarks), idle.
- Run: 2026-07-10, `venv/bin/pytest tests/test_bench.py::<id> --benchmark-json=...`
  (subset: n ∈ {1000, 100000}; add scales by extending the id list).

## Methodology caveats (topic 0 lens — read before comparing)

- **Closed loop** (pytest-benchmark): these are *service times* through a Python
  client, not latency under load. No coordinated-omission story here because there is
  no target rate — fine for engine-throughput comparisons, useless for tail claims.
- `test_return` (~100 µs median) is the **client + RESP + dispatch floor**: every
  other number includes it. Engine-side cost ≈ measured − floor for small queries.
- `match_*` results stream all rows back through the Python client — at 100K rows the
  client-side parse dominates. Compare capstone numbers with the *same* client, or not
  at all (apples vs oranges, §3.3).
- create/delete benches: 5 rounds, fresh graph per round (`benchmark.pedantic`).

## Results (median, per full query)

| Benchmark | n=1,000 | n=100,000 | derived (100K) |
|---|--:|--:|--|
| `RETURN 1` | 100 µs | — | round-trip floor |
| unwind range | 1.79 ms | 177 ms | 565 K rows/s produced+returned |
| create_node | 555 µs | 34.1 ms | 2.9 M nodes/s |
| create_relationship (2 nodes + 1 edge each) | 1.01 ms | 94.4 ms | 1.06 M edges/s |
| match_node (return all) | 5.73 ms | 667 ms | 150 K rows/s end-to-end |
| match_relationship (return n,r,m) | 14.8 ms | 1.78 s | 56 K rows/s end-to-end |
| delete_node | 518 µs | 28.1 ms | 3.6 M deletes/s |
| delete_relationship | 861 µs | 46.3 ms | 2.2 M deletes/s |

## What M-milestones should chase

- **M13 (naive graph core):** create_node / create_relationship / delete_* engine-side
  throughput — beat the numbers above *minus the RESP+Python floor*.
- **M7 (RESP server):** reproduce `test_return`'s ~100 µs floor with falkordb-py
  against the capstone server; then measure properly (open-loop + HdrHistogram).
- **M20 (sparse core):** match_* traversal-side; use LDBC (M22), not these micro
  matches, for the headline comparison.
