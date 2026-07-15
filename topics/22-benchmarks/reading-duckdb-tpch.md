# dbgen as a table function: shipping a benchmark inside the engine

How a modern engine ships TPC-H as a built-in: DuckDB vendors the
official dbgen, wraps it in a table function, and stores the
reference answers next to the queries — so every benchmark run is
also a correctness test. It's also the fastest way to get real
TPC-H numbers on this machine (no CLI install needed;
`pip install duckdb` or the Rust crate both carry the extension).
Before pointing at the code, this chapter builds the four design
ideas in order — table functions, vendored determinism, streaming
generation, and answers-as-oracle — then hands you the file anchors
and the exact SQL to run.

## The problem in one sentence

The classic TPC-H workflow — download dbgen, fight its 1990s
Makefile, generate multi-GB `.tbl` flat files, write a loader, hope
your columns parse the same as everyone else's — takes hours and
introduces silent divergence at every step; DuckDB collapses it to
`CALL dbgen(sf=1)` with byte-identical data and shipped reference
answers.

## The concepts, step by step

### Step 1 — the table function: a generator that pretends to be a table

A **table function** is a function the query engine treats as a
table: instead of scanning stored pages, the engine repeatedly asks
the function "give me the next batch of rows". Anything that can
produce rows on demand — a CSV reader, a range generator, a
benchmark data generator — plugs into the query machinery this way.
DuckDB exposes dbgen exactly so: `CALL dbgen(sf=1)` invokes a table
function that *generates* TPC-H data and feeds it straight into
table creation. The plumbing is small: `tpch_extension.cpp:17-30`
declares `DBGenFunctionData`; :49-95 is the standard bind (parse
the `sf` argument, :63) → init → execute lifecycle every DuckDB
operator follows.

Why it matters: once the generator is an operator, it inherits the
engine's whole execution stack — parallelism, batching, pipelining —
for free.

### Step 2 — vendoring the real dbgen: determinism is the product

DuckDB does not reimplement the generator — it **vendors** (copies
into its own tree) the TPC-official dbgen C code (`dbgen/` —
`bm_utils.cpp`, `build.cpp`, `permute.cpp`: 1990s C, seeded,
spec-exact). The seeds and value distributions are the ones every
published TPC-H result used, so DuckDB's SF1 `lineitem` is
row-for-row the same data as everyone else's SF1 `lineitem`.

That's the non-negotiable property: a benchmark generator's output
must be *deterministic and shared*, or cross-paper comparison dies.
Rewriting dbgen "cleanly" and drifting by one distribution would be
worse than the ugly C.

### Step 3 — streaming chunks: the generator never touches disk

Classic dbgen writes `.tbl` flat files that you then parse and load
— materializing (writing out in full) the entire dataset once on
disk and again in the database. DuckDB's `DbgenFunction` (:99)
instead **streams**: it produces data one vectorized chunk (~2K
rows) at a time, directly into the engine's ingest path. Because
each chunk is independent, SF100 generation parallelizes across
threads and never creates a 100 GB intermediate file — no file
format to version, no parser to disagree.

This is topic 11's operator-vs-materialization lesson wearing a
benchmark costume: expose work as an iterator over chunks, and
composition plus parallelism come for free.

### Step 4 — shipping answers: every benchmark run is a correctness test

Next to the 22 parameter-substituted queries
(`dbgen/queries/q01.sql…q22.sql`), DuckDB ships the **reference
answers** per scale factor (`dbgen/answers/`) — the exact result
rows a correct engine must produce (deterministic data ⇒
deterministic answers). `tpch_config.py` embeds both into a
generated header. So `PRAGMA tpch(1)` can be *diffed*, not just
timed.

This closes the loop on Fair Benchmarking's pitfall 3.8 (topic 0:
"incorrect code wins" — a fast wrong answer beats every correct
system unless someone checks): the correctness oracle rides along
with the benchmark, and a speed regression and a wrongness
regression are caught by the same run. Topic 16's oracle habit,
institutionalized.

### Step 5 — scoping your own generator: why dbgen-lite is NOT dbgen

Our dbgen-lite (`lineitem.rs`) generates uniform, independent
values for three columns' worth of fidelity — enough for Q1/Q6
choke-point work, and deliberately nothing more. Real dbgen adds
what those queries don't need: correlated text fields (`comment`
with pattern-planted `%green%` for Q9's LIKE), spec-exact value
distributions, refresh streams, and the shared seeds of Step 2.
Consequence: our numbers are comparable only to ourselves, and Q9
or any optimizer study is out of scope by construction. The
principle: **scope your generator to your question**, and say out
loud which questions it cannot answer.

## Where each step lives in the code

Layout of [`~/repos/duckdb/extension/tpch/`](https://github.com/duckdb/duckdb):

| path | what (step) |
|---|---|
| `tpch_extension.cpp:17-30` | `DBGenFunctionData` — dbgen exposed as a **table function**: `CALL dbgen(sf=1)` (1) |
| `tpch_extension.cpp:49-95` | bind (parse `sf`, :63) → init → `DbgenFunction` (:99) streaming chunks — the generator IS an operator, so SF100 generation parallelizes and never materializes a .tbl file (1, 3) |
| `dbgen/` | the actual TPC-official dbgen C code, vendored (`bm_utils.cpp`, `build.cpp`, `permute.cpp` — 1990s C, seeded, spec-exact) (2) |
| `dbgen/queries/q01.sql…q22.sql` | the 22 queries, parameter-substituted (4) |
| `dbgen/answers/` | reference results per SF — correctness oracle, not just speed (4) |
| `tpch_config.py` | generates the header embedding queries/answers (4) |

Reading order: `tpch_extension.cpp` top-to-bottom (it's short),
then skim one file of the vendored C (`build.cpp`) just to see the
seeded generation, then open `queries/q06.sql` and its answer file
side by side.

The lesson for M22: **benchmark data generators belong inside the
engine as table functions** — deterministic, parallel, no
file-format drift, and answers ship next to queries so every run is
also a correctness test.

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

## References

**Code**
- [duckdb](https://github.com/duckdb/duckdb) `extension/tpch/` —
  `tpch_extension.cpp` (the table-function plumbing), `dbgen/` (the
  vendored TPC-official C code), `dbgen/queries/`,
  `dbgen/answers/`, `tpch_config.py`
