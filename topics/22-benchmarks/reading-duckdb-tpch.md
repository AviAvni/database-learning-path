# Reading guide — DuckDB's TPC-H extension (`extension/tpch/`)

How a modern engine ships a benchmark as a built-in — and the fastest
way to get real TPC-H numbers on this machine (no CLI install needed;
`pip install duckdb` or the Rust crate both carry the extension).

## Layout ([`~/repos/duckdb/extension/tpch/`](https://github.com/duckdb/duckdb))

| path | what |
|---|---|
| `tpch_extension.cpp:17-30` | `DBGenFunctionData` — dbgen exposed as a **table function**: `CALL dbgen(sf=1)` |
| `tpch_extension.cpp:49-95` | bind (parse `sf`, :63) → init → `DbgenFunction` (:99) streaming chunks — the generator IS an operator, so SF100 generation parallelizes and never materializes a .tbl file |
| `dbgen/` | the actual TPC-official dbgen C code, vendored (`bm_utils.cpp`, `build.cpp`, `permute.cpp` — 1990s C, seeded, spec-exact) |
| `dbgen/queries/q01.sql…q22.sql` | the 22 queries, parameter-substituted |
| `dbgen/answers/` | reference results per SF — correctness oracle, not just speed |
| `tpch_config.py` | generates the header embedding queries/answers |

The lesson for M22: **benchmark data generators belong inside the
engine as table functions** — deterministic, parallel, no file-format
drift, and answers ship next to queries so every run is also a
correctness test (topic 16's oracle habit).

## Run it (record numbers in notes.md)

```sql
-- python: import duckdb; con = duckdb.connect()
INSTALL tpch; LOAD tpch;
CALL dbgen(sf=1);
PRAGMA tpch(1);   -- Q1
PRAGMA tpch(6);   -- Q6
PRAGMA tpch(9);   -- Q9
-- .timer on / %timeit around them; compare against our
-- dbgen-lite oracle numbers (bench_suite) at matched row counts
```

Expected shape (verify): Q6 saturates memory bandwidth (topic 0's
30 GB/s baseline), Q1 is compute-bound in expression eval + fused
aggregation, Q9 is join-order sensitive (try
`SET disabled_optimizers='join_order'` for the horror version).

## Why our dbgen-lite is NOT dbgen

Real dbgen: correlated text fields (`comment` with pattern-planted
`%green%` for Q9), spec-exact value distributions, refresh streams,
and — crucially — the SAME seeds everyone else uses, so results are
comparable across papers. Ours: uniform, independent, three columns'
worth of fidelity — enough for Q1/Q6 choke-point work, useless for
Q9 or optimizer studies. Scope your generator to your question.

## Questions (answer in notes.md)

1. Measure DuckDB Q1 and Q6 at SF1 on this machine; compute effective
   GB/s and compare with our oracle lanes AND topic 0's streaming
   baseline. Where does the gap come from (vectorization? fewer
   passes? parallelism — check with `SET threads=1`)?
2. Why does shipping `answers/` matter more than shipping `queries/`?
   Relate to topic 16's oracle taxonomy.
3. `DbgenFunction` streams chunks instead of writing .tbl files —
   which topic-11 concept is that (operator vs materialization)?
4. Q9 with join order disabled: how much slower, and which topic-10
   lesson does the number reproduce?
5. Sketch M22's `CALL ldbc_datagen(sf=1)` equivalent for the
   capstone: what determinism/answer-shipping properties must it keep?
