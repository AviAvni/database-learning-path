# dbgen as a table function: shipping a benchmark inside the engine

How a modern engine ships TPC-H as a built-in: DuckDB vendors the
official dbgen, wraps it in a table function, and stores the
reference answers next to the queries — so every benchmark run is
also a correctness test. It's also the fastest way to get real
TPC-H numbers on this machine (no CLI install needed;
`pip install duckdb` or the Rust crate both carry the extension).
Before pointing at the code, this chapter builds the four design
ideas in order — table functions, vendored determinism, chunked
generation, and answers-as-oracle — then hands you the file anchors
and the exact SQL to run.

Every C++ line number below belongs to **duckdb/duckdb@6c0c1a68**
(`python3 tools/pinned-source.py show duckdb extension/tpch/…` prints
the same gutters). Rust line numbers are this topic's `experiments/`.
Spec clause numbers are TPC-H revision 3.0.1. Where the file layout
differs from what a summary of it would say — and in two places it
does — the code wins.

## The problem in one sentence

The classic TPC-H workflow — download dbgen, fight its 1990s
Makefile, generate multi-GB `.tbl` flat files, write a loader, hope
your columns parse the same as everyone else's — takes hours and
introduces silent divergence at every step; DuckDB collapses it to
`CALL dbgen(sf=1)` with byte-identical data and shipped reference
answers.

## The concepts, step by step

### Step 1 — the table function: a generator wearing an operator's interface

> **In:** SQL, and the idea that a query scans stored pages.
> **Out:** what `CALL dbgen(sf=1)` actually is — the bind/init/execute
> lifecycle, the seven named parameters, and the surprise that this table
> function returns no table.

A **table function** is a function the query engine treats as a
table: instead of scanning stored pages, the engine repeatedly asks
the function "give me the next chunk of rows". Anything that can
produce rows on demand — a CSV reader, a range generator, a benchmark
data generator — plugs into the query machinery this way. DuckDB
registers three of them plus a pragma for TPC-H:

```cpp
// duckdb extension/tpch/tpch_extension.cpp — LoadInternal, 244-267 (elided at 246-252)
   244  static void LoadInternal(ExtensionLoader &loader) {
   245  	TableFunction dbgen_func("dbgen", {}, DbgenFunction, DbgenBind, DbgenInit);
   // ... 246-252: named_parameters sf, overwrite, catalog, schema, suffix, children, step ...
   253  	dbgen_func.call_return_type = StatementReturnType::NOTHING;
   254  	dbgen_func.table_scan_progress = DbgenProgress;
   255  	loader.RegisterFunction(dbgen_func);
   256
   257  	// create the TPCH pragma that allows us to run the query
   258  	auto tpch_func = PragmaFunction::PragmaCall("tpch", PragmaTpchQuery, {LogicalType::BIGINT});
   259  	loader.RegisterFunction(tpch_func);
   260
   261  	// create the TPCH_QUERIES function that returns the query
   262  	TableFunction tpch_query_func("tpch_queries", {}, TPCHQueryFunction, TPCHQueryBind, TPCHInit);
   263  	loader.RegisterFunction(tpch_query_func);
   264
   265  	// create the TPCH_ANSWERS that returns the query result
   266  	TableFunction tpch_query_answer_func("tpch_answers", {}, TPCHQueryAnswerFunction, TPCHQueryAnswerBind, TPCHInit);
   267  	loader.RegisterFunction(tpch_query_answer_func);
   268  }
```

Line 245 is the three-callback shape every DuckDB table function has:

- **bind** — `DbgenBind` (49-93) runs once at plan time. It parses the
  named parameters (59-78), defaults catalog and schema from the
  session (54-57), rejects `children` without `step` (79-81),
  registers that the statement modifies the database (82-89), and
  declares the output columns (90-91).
- **init** — `DbgenInit` (95-97) creates the per-execution state, here
  a `DBGenGlobalState` (30-35) holding the generator behind a mutex.
- **execute** — `DbgenFunction` (99-133) is called repeatedly until it
  reports finished.

The struct bind fills in is small enough to read whole:

```cpp
// duckdb extension/tpch/tpch_extension.cpp — DBGenFunctionData, 17-28
    17  struct DBGenFunctionData : public TableFunctionData {
    18  	DBGenFunctionData() {
    19  	}
    20
    21  	double sf = 0;
    22  	Identifier catalog = INVALID_CATALOG;
    23  	Identifier schema = DEFAULT_SCHEMA;
    24  	string suffix;
    25  	bool overwrite = false;
    26  	uint32_t children = 1;
    27  	int step = -1;
    28  };
```

`sf` is a `double` (21), so `CALL dbgen(sf=0.01)` is legal even though
Clause 4.1.3.1 admits only ten scale factors — DuckDB's small SFs are
useful and are not TPC-H. `suffix` (24) lets you generate a second
copy of the schema alongside the first; `children`/`step` (26-27) are
Step 3's explicit partitioning.

**The surprise.** Lines 90-91 of `DbgenBind` declare a single BOOLEAN
column named `Success`, and line 253 sets
`call_return_type = StatementReturnType::NOTHING`. `dbgen` is a table
function that **returns no rows of TPC-H data at all**: it *creates
and populates the eight tables as a side effect*
(`CreateTPCHSchema` at 107) and reports only that it finished. So the
accurate sentence is "the generator is an operator, so it inherits
the engine's scheduling, progress reporting and interrupt handling"
(`DbgenProgress` at 135-148, `context.InterruptCheck()` at 1127 of
`dbgen.cpp`) — not "the generated rows flow through the query as this
function's output". They do not.

`tpch_queries` (262) and `tpch_answers` (266) *are* ordinary
row-returning table functions, and `PRAGMA tpch(6)` is neither: line
258 registers it as a **query-rewrite pragma**, whose handler simply
returns the query's text:

```cpp
// duckdb extension/tpch/tpch_extension.cpp — PragmaTpchQuery, 239-242
   239  static string PragmaTpchQuery(ClientContext &context, const FunctionParameters &parameters) {
   240  	auto index = parameters.values[0].GetValue<int32_t>();
   241  	return tpch::DBGenWrapper::GetQuery(index);
   242  }
```

`PRAGMA tpch(6)` does not "run Q6 specially" — it expands to the text
of `q06.sql` and DuckDB then plans and executes that SQL like any
other. Which is why timing `PRAGMA tpch(6)` is timing Q6.

### Step 2 — vendoring the real dbgen: determinism is the product

> **In:** Step 1's registration.
> **Out:** which files are the TPC-official C, which file is a build manifest
> (not, as is often claimed, a code generator), and where the queries and
> answers physically live once compiled.

DuckDB does not reimplement the generator — it **vendors** (copies
into its own tree) the TPC-official dbgen C code. The build manifest
names it exactly:

```python
# duckdb extension/tpch/tpch_config.py — the whole file is 22 lines, 8-22
     8  source_files = [
     9      os.path.sep.join(x.split('/'))
    10      for x in [
    11          'extension/tpch/tpch_extension.cpp',
    12          'extension/tpch/dbgen/bm_utils.cpp',
    13          'extension/tpch/dbgen/build.cpp',
    14          'extension/tpch/dbgen/dbgen.cpp',
    15          'extension/tpch/dbgen/dbgen_gunk.cpp',
    16          'extension/tpch/dbgen/permute.cpp',
    17          'extension/tpch/dbgen/rnd.cpp',
    18          'extension/tpch/dbgen/rng64.cpp',
    19          'extension/tpch/dbgen/speed_seed.cpp',
    20          'extension/tpch/dbgen/text.cpp',
    21      ]
    22  ]
```

Nine vendored translation units (12-20) plus the DuckDB glue (11).
`rnd.cpp`, `rng64.cpp` and `speed_seed.cpp` are the seeded random
number machinery; `permute.cpp` is dbgen's deterministic shuffle;
`text.cpp` builds the comment strings that Q9's `%green%` and Q13's
`l_comment` search. The seeds and value distributions are the ones
every published TPC-H result used, so DuckDB's SF1 `lineitem` is
row-for-row the same data as everyone else's SF1 `lineitem`.

That is the non-negotiable property: a benchmark generator's output
must be *deterministic and shared*, or cross-paper comparison dies.
Rewriting dbgen "cleanly" and drifting by one distribution would be
worse than the ugly C.

**`tpch_config.py` is a build manifest, not a code generator.** It is
22 lines and contains only two Python lists — include directories
(4-6) and the source files above. The queries and answers are baked
in by a different script entirely, and the generated file says so on
its first line:

```
  extension/tpch/dbgen/include/tpch_constants.hpp — 189 lines
     1  /* THIS FILE WAS AUTOMATICALLY GENERATED BY generate_csv_header.py */
     5  const int TPCH_QUERIES_COUNT = 22;
    28  const char *TPCH_QUERIES[] = {
    74  const char *TPCH_ANSWERS_SF0_01[] = {
   120  const char *TPCH_ANSWERS_SF0_1[] = {
   166  const char *TPCH_ANSWERS_SF1[] = {
```

The generator is `scripts/generate_csv_header.py` at the DuckDB repo
root; it encodes each `.sql` and `.csv` file as a `uint8_t` array so
the extension is a single self-contained binary with no data files to
lose. That is the same determinism argument one level up: if the
queries were read from disk at runtime, two installs could disagree.

### Step 3 — chunked generation: 2,048 rows at a time, and never a .tbl file

> **In:** Step 1's execute callback, Step 2's vendored C.
> **Out:** the constant that sets the chunk size, how many chunks an SF-1
> LINEITEM takes, and the difference between "parallel because chunks are
> independent" and "parallel because you asked for it".

Classic dbgen writes `.tbl` flat files that you then parse and load —
**materializing** (writing out in full) the entire dataset once on
disk and again in the database. DuckDB's generator instead appends
into the engine's own chunk format and flushes a chunk whenever it
fills:

```cpp
// duckdb extension/tpch/dbgen/dbgen.cpp — append_begin_row, 184-192
   184  static void append_begin_row(tpch_append_information &info) {
   185  	D_ASSERT(info.appender || info.optimistic_collection);
   186  	D_ASSERT(info.active_row == DConstants::INVALID_INDEX);
   187  	if (info.row >= STANDARD_VECTOR_SIZE) {
   188  		info.FlushChunk();
   189  	}
   190  	info.active_row = info.row;
   191  	info.active_col = 0;
   192  }
```

Line 187's `STANDARD_VECTOR_SIZE` is not folklore — it is
`DEFAULT_STANDARD_VECTOR_SIZE`, defined as `2048U`:

```cpp
// duckdb src/include/duckdb/common/vector_size.hpp — the vector size, 15-21
    15  //! The default standard vector size
    16  #define DEFAULT_STANDARD_VECTOR_SIZE 2048U
    17
    18  //! The vector size used in the execution engine
    19  #ifndef STANDARD_VECTOR_SIZE
    20  #define STANDARD_VECTOR_SIZE DEFAULT_STANDARD_VECTOR_SIZE
    21  #endif
```

So the arithmetic, using Step 1's link to Clause 4.2.5.1's SF-1
cardinality:

```
  SF-1 LINEITEM rows         = 6,001,215                (Clause 4.2.5.1)
  rows per chunk             = 2,048                    (vector_size.hpp:16)
  chunks for LINEITEM        = 6,001,215 / 2,048 = 2,930.7 → 2,931
  all eight tables, SF 1     = 8,661,245 / 2,048 = 4,229.1 → ~4,230 chunks
  peak intermediate on disk  = 0 bytes
```

Nothing in there is a 641 MB `.tbl` file, no file format to version,
no parser to disagree.

Parallelism is a separate decision, not a consequence. `GenerateNext`
dispatches to one of two modes:

```cpp
// duckdb extension/tpch/dbgen/dbgen.cpp — GenerateNext, 1126-1143
  1126  	bool GenerateNext() override {
  1127  		context.InterruptCheck();
  1128  		if (finished.load()) {
  1129  			return true;
  1130  		}
  1131  		if (total_work == 0) {
  1132  			Finish();
  1133  			return true;
  1134  		}
  1135  		switch (mode) {
  1136  		case DBGenMode::PARALLEL:
  1137  			return GenerateParallel();
  1138  		case DBGenMode::SEQUENTIAL:
  1139  			return GenerateSequential();
  1140  		default:
  1141  			throw InternalException("Unexpected TPC-H dbgen mode");
  1142  		}
  1143  	}
```

and `DbgenBind` exposes dbgen's own partitioning scheme so you can
generate one slice per process:

```cpp
// duckdb extension/tpch/tpch_extension.cpp — the children/step parameters, 73-81
    73  		} else if (kv.first == "children") {
    74  			result->children = UIntegerValue::Get(kv.second);
    75  		} else if (kv.first == "step") {
    76  			result->step = UIntegerValue::Get(kv.second);
    77  		}
    78  	}
    79  	if (result->children != 1 && result->step == -1) {
    80  		throw InvalidInputException("Step must be defined when children are defined");
    81  	}
```

`CALL dbgen(sf=100, children=8, step=3)` generates the fourth eighth
of SF-100 — the same `-C`/`-S` flags the original dbgen has, because
the vendored C is doing the work. Line 1127's `InterruptCheck` is the
other half of "the generator is an operator": Ctrl-C works during a
20-minute SF-100 generation, and `DbgenProgress` (135-148) drives the
progress bar, because the generator lives inside the engine's task
machinery rather than beside it.

This is topic 11's operator-vs-materialization lesson wearing a
benchmark costume: expose work as an iterator over chunks, and
scheduling, cancellation and progress come for free.

### Step 4 — shipping answers: every benchmark run is a correctness test

> **In:** Step 2's generated header.
> **Out:** the exact set of scale factors whose answers are usable, the
> shape of the `tpch_answers` table, and the row-count arithmetic that
> tells you the answers stop where the header stops.

Next to the 22 parameter-substituted queries
(`dbgen/queries/q01.sql…q22.sql`) DuckDB ships the **reference
answers** — the exact result rows a correct engine must produce.
Deterministic data plus fixed substitution parameters means
deterministic answers, so `PRAGMA tpch(1)` can be *diffed*, not just
timed.

But only up to a point, and the point is in the code:

```cpp
// duckdb extension/tpch/dbgen/dbgen.cpp — GetAnswer, 1451-1466
  1451  string DBGenWrapper::GetAnswer(double sf, int query) {
  1452  	if (query <= 0 || query > TPCH_QUERIES_COUNT) {
  1453  		throw SyntaxException("Out of range TPC-H query number %d", query);
  1454  	}
  1455  	const char *answer;
  1456  	if (sf == 0.01) {
  1457  		answer = TPCH_ANSWERS_SF0_01[query - 1];
  1458  	} else if (sf == 0.1) {
  1459  		answer = TPCH_ANSWERS_SF0_1[query - 1];
  1460  	} else if (sf == 1) {
  1461  		answer = TPCH_ANSWERS_SF1[query - 1];
  1462  	} else {
  1463  		throw NotImplementedException("Don't have TPC-H answers for SF %llf!", sf);
  1464  	}
  1465  	return answer;
  1466  }
```

Three scale factors: **0.01, 0.1 and 1** (1456-1461), and anything
else throws (1462-1464). The repository contains
`dbgen/answers/sf10/` and `dbgen/answers/sf100/` directories too, but
Step 2's generated header only carries three arrays
(`TPCH_ANSWERS_SF0_01`, `_SF0_1`, `_SF1`), so those two SFs are
present as files and absent from the binary. Verifying an SF-10 run
against shipped answers is not something this extension can do.

The `tpch_answers` table function makes the same statement in SQL:

```cpp
// duckdb extension/tpch/tpch_extension.cpp — TPCHQueryAnswerFunction, 209-218
   209  static void TPCHQueryAnswerFunction(ClientContext &context, TableFunctionInput &data_p, DataChunk &output) {
   210  	auto &data = data_p.global_state->Cast<TPCHData>();
   211  	idx_t tpch_queries = 22;
   212  	vector<double> scale_factors {0.01, 0.1, 1};
   213  	idx_t total_answers = tpch_queries * scale_factors.size();
   214  	if (data.offset >= total_answers) {
   215  		// finished returning values
   216  		return;
   217  	}
```

```
  SELECT count(*) FROM tpch_answers();
    = tpch_queries × scale_factors.size()          (line 213)
    = 22 × 3
    = 66 rows, with columns (query_nr, scale_factor, answer)   (195-207)
```

So the correctness loop you can actually close is:

```sql
-- run every query and diff it against the shipped answer, at SF 1
CALL dbgen(sf=1);
SELECT query_nr, answer FROM tpch_answers() WHERE scale_factor = 1;
-- then, per query_nr, execute PRAGMA tpch(query_nr) and compare
```

This closes the loop on Fair Benchmarking's "incorrect code wins"
pitfall (topic 0: a fast wrong answer beats every correct system
unless someone checks): the correctness oracle rides along with the
benchmark, and a speed regression and a wrongness regression are
caught by the same run. Topic 16's oracle habit, institutionalized —
and bounded, at SF ≤ 1.

### Step 5 — scoping your own generator: why dbgen-lite is not dbgen, in bytes

> **In:** Steps 1-4's full-fidelity machinery.
> **Out:** the exact byte accounting behind this topic's GB/s headline,
> reproduced by hand from the spec's own sizing convention — and the list of
> questions our generator cannot answer.

Our dbgen-lite (`lineitem.rs`) generates seven columns of uniform,
independent values — enough for Q1/Q6 choke-point work, and
deliberately nothing more:

```rust
// experiments/src/lineitem.rs — the seven columns Q1 and Q6 touch, 8-16
     8  pub struct LineItem {
     9      pub quantity: Vec<f64>,       // 1..=50
    10      pub extendedprice: Vec<f64>,  // ~ 900..=105000
    11      pub discount: Vec<f64>,       // 0.00..=0.10
    12      pub tax: Vec<f64>,            // 0.00..=0.08
    13      pub returnflag: Vec<u8>,      // 'A' | 'N' | 'R'
    14      pub linestatus: Vec<u8>,      // 'O' | 'F'
    15      pub shipdate: Vec<u32>,       // days since 1992-01-01, 0..=2526
    16  }
```

That layout is where this topic's headline GB/s comes from, and the
bench states its byte accounting inline rather than asking you to
trust it:

```rust
// experiments/src/bin/bench_suite.rs — the effective-bandwidth report, 74-79
    74          let bytes = t.len() * (8 * 4 + 2 + 4); // cols Q1 touches
    75          println!(
    76              "        Q1 effective {:.1} GB/s | Q6 scans {:.1} GB/s (oracle lanes)",
    77              bytes as f64 / q1 / 1e6,
    78              (t.len() * (8 * 3 + 4)) as f64 / q6 / 1e6
    79          );
```

Work line 74 out, and note that it agrees with the TPC-H spec's own
sizing convention — the Comment under Clause 4.2.5.1's Table 3 says
"4-byte integers, 8-byte decimals, 4-byte dates":

```
  Q1 touches (line 74):  4 decimals × 8 B  = 32   quantity, extendedprice, discount, tax
                       + 2 chars    × 1 B  =  2   returnflag, linestatus
                       + 1 date     × 4 B  =  4   shipdate
                                             ───
                                              38 B per row

  Q6 touches (line 78):  3 decimals × 8 B  = 24   quantity, extendedprice, discount
                       + 1 date     × 4 B  =  4   shipdate
                                             ───
                                              28 B per row
```

Then the GB/s, using notes.md's baseline table (M3 Pro, measured
2026-07-10) at SF 0.25 = 1,500,000 rows. `q1` and `q6` are
milliseconds, so `bytes / ms / 1e6` is GB/s:

```
  Q1: 1,500,000 × 38 B = 57,000,000 B over 10.2 ms
      57,000,000 / 10.2 / 1e6 = 5.59 GB/s      (notes.md prints 5.6)
  Q6: 1,500,000 × 28 B = 42,000,000 B over  2.7 ms
      42,000,000 /  2.7 / 1e6 = 15.6 GB/s      (notes.md prints 15.7)
```

The canonical headline is FINDINGS.md row 22, from the later
2026-07-28 run: **Q1 at 5.2–5.7 GB/s and Q6 at 9.0–14.4 GB/s
effective**. Both are "effective" bandwidth — bytes the query
logically consumed divided by wall time — not DRAM traffic; at these
working-set sizes much of it is served from cache.

For calibration on the same machine, FINDINGS.md row 17 measures a
tuned eight-accumulator sum at **26.32 GB/s** and a branchless filter
flat at **~10 GB/s** while the branchy version collapses to
**0.95 GB/s** at 50% selectivity. Q6's 1.9% selectivity (see
[reading-boncz-tpch.md](reading-boncz-tpch.md) Step 4) sits on the
safe side of that crater, which is why the branchy oracle is already
fast.

What real dbgen adds that ours does not: correlated text fields
(`text.cpp`, whose comments carry the `%green%` Q9 searches for),
spec-exact value distributions and the functional dependency between
`returnflag` and `linestatus`, the correlated date columns of CP3.3,
the other seven tables, refresh streams RF1/RF2, and Step 2's shared
seeds. Consequence: our numbers are comparable only to ourselves, and
Q9, cardinality estimation, join order and anything requiring
Power@Size are out of scope by construction. The principle: **scope
your generator to your question**, and say out loud which questions
it cannot answer.

## Where each step lives in the code

Layout of [`duckdb/extension/tpch/`](https://github.com/duckdb/duckdb)
at `6c0c1a68` — 159 files, of which 110 are answer CSVs and 22 are
query `.sql` files, leaving 27 files of actual code and build glue:

| path | lines | what (step) |
|---|---|---|
| `tpch_extension.cpp` | 17-28 | `DBGenFunctionData` — the seven bind parameters (1) |
| `tpch_extension.cpp` | 49-93 | `DbgenBind` — parse (59-78), validate `children`/`step` (79-81), declare one BOOLEAN column (90-91) (1) |
| `tpch_extension.cpp` | 95-97 | `DbgenInit` — per-execution `DBGenGlobalState` (1) |
| `tpch_extension.cpp` | 99-133 | `DbgenFunction` — create schema (107), loop `GenerateNext` (117-132) (3) |
| `tpch_extension.cpp` | 135-148 | `DbgenProgress` — why the progress bar works (3) |
| `tpch_extension.cpp` | 209-237 | `TPCHQueryAnswerFunction` — 22 × 3 = 66 answer rows (4) |
| `tpch_extension.cpp` | 239-242 | `PragmaTpchQuery` — `PRAGMA tpch(n)` is a query rewrite (1) |
| `tpch_extension.cpp` | 244-268 | `LoadInternal` — the four registrations; `NOTHING` return type at 253 (1) |
| `dbgen/dbgen.cpp` | 184-192 | `append_begin_row` — flush at `STANDARD_VECTOR_SIZE` (3) |
| `dbgen/dbgen.cpp` | 1126-1143 | `GenerateNext` — PARALLEL/SEQUENTIAL dispatch, interrupt check (3) |
| `dbgen/dbgen.cpp` | 1451-1466 | `GetAnswer` — answers exist for SF 0.01/0.1/1 only (4) |
| `src/include/duckdb/common/vector_size.hpp` | 15-21 | `2048` — the chunk size Step 3 divides by (3) |
| `dbgen/bm_utils.cpp`, `build.cpp`, `permute.cpp`, `rnd.cpp`, `rng64.cpp`, `speed_seed.cpp`, `text.cpp`, `dbgen_gunk.cpp` | — | the vendored TPC-official C, listed at `tpch_config.py:12-20` (2) |
| `dbgen/queries/q01.sql … q22.sql` | — | the 22 queries with validation parameters substituted (4) |
| `dbgen/answers/sf{0.01,0.1,1,10,100}/qNN.csv` | — | 110 answer files on disk; only the first three SFs are compiled in (4) |
| `dbgen/include/tpch_constants.hpp` | 1, 5, 28, 74, 120, 166 | the generated header: `TPCH_QUERIES_COUNT = 22`, one query array, three answer arrays (2, 4) |
| `tpch_config.py` | 4-22 | build manifest: include dirs and the ten source files (2) |

Reading order: `tpch_extension.cpp` top to bottom (301 lines), then
`dbgen.cpp:184-192` and `1126-1143` for the chunking, then open
`queries/q06.sql` and `answers/sf1/q06.csv` side by side — the second
is one number, and [reading-boncz-tpch.md](reading-boncz-tpch.md)
Step 4 derives it.

The lesson for M22: **benchmark data generators belong inside the
engine as table functions** — deterministic, chunked, cancellable, no
file-format drift, and answers ship next to queries so every run is
also a correctness test.

## Run it (record numbers in notes.md)

```sql
-- python: import duckdb; con = duckdb.connect()
INSTALL tpch; LOAD tpch;
CALL dbgen(sf=1);          -- creates and fills the 8 tables; returns "Success"
PRAGMA tpch(1);            -- expands to q01.sql and runs it
PRAGMA tpch(6);            -- Q6
PRAGMA tpch(9);            -- Q9
SELECT * FROM tpch_answers() WHERE scale_factor = 1 AND query_nr IN (1, 6);
```

Then, for the comparison that matters:

```sql
SET threads = 1;                      -- match our single-threaded lanes
SET disabled_optimizers = 'join_order';  -- Q9's horror version
```

Expected shape, to check rather than assume: Q6 should be the fastest
of the three per byte read and should scale close to linearly with
threads; Q1 should be compute-bound in expression evaluation and
fused aggregation over 98.6% of LINEITEM; Q9 should be the one that
moves by a large factor when the join-order optimizer is disabled.
Compute effective GB/s the way Step 5 does — Q1 reads 38 B/row and Q6
28 B/row of the *spec's* column widths, so SF 1 is 228 MB and 168 MB
respectively — and compare against FINDINGS.md row 22's 5.2–5.7 and
9.0–14.4 GB/s.

## Questions (answer in notes.md)

1. Measure DuckDB Q1 and Q6 at SF 1 on this machine, with
   `SET threads = 1` and without. Compute effective GB/s using Step 5's
   38 and 28 bytes per row, and compare against our oracle lanes and
   topic 17's 26.32 GB/s eight-accumulator ceiling. Where does the gap
   come from — vectorization, fewer passes, or parallelism?
2. Why does shipping `answers/` matter more than shipping `queries/`?
   Relate to topic 16's oracle taxonomy — and say what the SF ≤ 1
   limit in `GetAnswer` (dbgen.cpp:1462-1464) costs you at SF 10.
3. `dbgen` is registered with `call_return_type = NOTHING`
   (tpch_extension.cpp:253) and generates into tables rather than into
   its own output chunk. Which topic-11 concept is the chunked
   `append_begin_row`/`FlushChunk` loop, and which one is it *not*?
4. Q9 with `disabled_optimizers = 'join_order'`: how much slower, and
   which topic-10 lesson does the number reproduce?
5. Sketch M22's `CALL ldbc_datagen(sf=1)` equivalent for the capstone.
   Which of Steps 1-4's properties must it keep — and what is the
   graph analogue of shipping `answers/`, given that SNB's answers
   depend on the substitution parameters?

## Done when

Answer each before unfolding it.

- [ ] You can explain what a table function is, name the three callbacks, and say what `CALL dbgen(sf=1)` actually returns.

  <details><summary>Answer</summary>

  A table function is a function the engine treats as a table: it plugs into
  the scan interface and is asked for chunks. DuckDB's shape is
  **bind** (`DbgenBind`, 49-93 — plan time: parse named parameters, declare
  output columns), **init** (`DbgenInit`, 95-97 — per-execution state), and
  **execute** (`DbgenFunction`, 99-133 — called until finished).

  `CALL dbgen(sf=1)` returns a single BOOLEAN column called `Success`
  (declared at 90-91), and line 253 sets
  `call_return_type = StatementReturnType::NOTHING`. The TPC-H rows are a
  *side effect*: `CreateTPCHSchema` at 107 creates the eight tables and the
  generator fills them. The payoff of being an operator is scheduling,
  progress (`DbgenProgress`, 135-148) and cancellation
  (`InterruptCheck`, dbgen.cpp:1127) — not that the data flows through the
  query.

  </details>

- [ ] You can say which files are the vendored TPC code, and which file generates the compiled-in queries and answers.

  <details><summary>Answer</summary>

  `tpch_config.py:12-20` lists the nine vendored translation units:
  `bm_utils.cpp`, `build.cpp`, `dbgen.cpp`, `dbgen_gunk.cpp`, `permute.cpp`,
  `rnd.cpp`, `rng64.cpp`, `speed_seed.cpp`, `text.cpp`. That file is a
  **build manifest** — 22 lines, two Python lists — and generates nothing.

  The queries and answers are baked into
  `dbgen/include/tpch_constants.hpp`, whose line 1 reads "THIS FILE WAS
  AUTOMATICALLY GENERATED BY generate_csv_header.py". It holds
  `TPCH_QUERIES_COUNT = 22` (5), one `TPCH_QUERIES[]` array (28), and three
  answer arrays (74, 120, 166). Encoding them as byte arrays means the
  extension is one self-contained binary with no data files to lose or
  diverge.

  </details>

- [ ] You can state the chunk size, where it is defined, and how many chunks an SF-1 LINEITEM takes.

  <details><summary>Answer</summary>

  `append_begin_row` (dbgen.cpp:184-192) flushes when `info.row >=
  STANDARD_VECTOR_SIZE` (187-189), and `STANDARD_VECTOR_SIZE` is
  `DEFAULT_STANDARD_VECTOR_SIZE = 2048U` in
  `src/include/duckdb/common/vector_size.hpp:16`.

  6,001,215 / 2,048 = 2,930.7, so 2,931 chunks for LINEITEM and about 4,230
  for all 8,661,245 SF-1 rows. Peak intermediate storage: zero — there is no
  `.tbl` file, no file format to version and no parser to disagree.

  Parallelism is separate: `GenerateNext` (1126-1143) dispatches to
  `GenerateParallel` or `GenerateSequential`, and `children`/`step`
  (tpch_extension.cpp:73-81) expose dbgen's own `-C`/`-S` partitioning for
  generating one slice per process.

  </details>

- [ ] You can say which scale factors DuckDB can actually verify against, and how many rows `tpch_answers()` returns.

  <details><summary>Answer</summary>

  `GetAnswer` (dbgen.cpp:1451-1466) handles `sf == 0.01`, `0.1` and `1`
  (1456-1461) and throws `NotImplementedException` for anything else
  (1462-1464). The repo contains `answers/sf10/` and `answers/sf100/`
  directories, but the generated header carries only three answer arrays, so
  those two are files on disk and absent from the binary — an SF-10 run
  cannot be diffed against a shipped answer.

  `tpch_answers()` returns `tpch_queries × scale_factors.size()` =
  22 × 3 = **66 rows** (tpch_extension.cpp:211-213), with columns
  `query_nr`, `scale_factor`, `answer` (195-207).

  </details>

- [ ] You can reproduce this topic's effective-GB/s figure by hand from the byte accounting, and say what "effective" excludes.

  <details><summary>Answer</summary>

  `bench_suite.rs:74` charges Q1 `8*4 + 2 + 4 = 38` bytes per row — four
  8-byte decimals, two 1-byte chars, one 4-byte date — which is exactly the
  sizing convention in the Comment under TPC-H Clause 4.2.5.1's Table 3.
  Line 78 charges Q6 `8*3 + 4 = 28` bytes.

  At SF 0.25 (1,500,000 rows) with notes.md's 2026-07-10 baseline:
  1,500,000 × 38 = 57,000,000 B over 10.2 ms = 5.59 GB/s for Q1, and
  1,500,000 × 28 = 42,000,000 B over 2.7 ms = 15.6 GB/s for Q6. FINDINGS.md
  row 22's canonical figures from the later 2026-07-28 run are 5.2–5.7 and
  9.0–14.4 GB/s.

  "Effective" means bytes the query logically consumed ÷ wall time. It is
  **not** DRAM traffic: at these working-set sizes much of the data is served
  from cache, and it excludes the write side, the allocator, and everything
  the HashMap does. Comparing it to a DRAM bandwidth figure is a category
  error; comparing it to topic 17's 26.32 GB/s accumulate lane, measured the
  same way on the same machine, is not.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including your `CALL ldbc_datagen` sketch for M22.

  <details><summary>Answer</summary>

  Self-check — the answers belong in `notes.md`. The interesting half of
  question 5 is that SNB's queries take substitution parameters drawn from the
  generated graph, so "ship the answers" cannot mean a static CSV per query:
  it has to be answers *per parameter set*, generated alongside the data and
  pinned by the same seed. That is a stronger determinism requirement than
  TPC-H's, and it is why the generator and the answer generator have to be the
  same program.

  </details>

## References

**Code**
- [duckdb](https://github.com/duckdb/duckdb) `extension/tpch/` —
  pinned at `6c0c1a68`. `tpch_extension.cpp` is 301 lines and reads
  top to bottom in one sitting; the vendored `dbgen/` C is 1990s
  TPC-official code and is worth skimming, not reading.

| File | Lines | What |
|---|---|---|
| `extension/tpch/tpch_extension.cpp` | 17-28 | `DBGenFunctionData` — sf, catalog, schema, suffix, overwrite, children, step |
| `extension/tpch/tpch_extension.cpp` | 49-93 | `DbgenBind`; named-parameter parse at 59-78, output column at 90-91 |
| `extension/tpch/tpch_extension.cpp` | 63-64 | where `sf` is read out of the named parameters |
| `extension/tpch/tpch_extension.cpp` | 73-81 | `children`/`step`, and the check that they come as a pair |
| `extension/tpch/tpch_extension.cpp` | 95-97 | `DbgenInit` |
| `extension/tpch/tpch_extension.cpp` | 99-133 | `DbgenFunction`; `CreateTPCHSchema` at 107, generate loop at 117-132 |
| `extension/tpch/tpch_extension.cpp` | 135-148 | `DbgenProgress` |
| `extension/tpch/tpch_extension.cpp` | 172-193 | `TPCHQueryFunction` — the 22 query texts as a table |
| `extension/tpch/tpch_extension.cpp` | 209-237 | `TPCHQueryAnswerFunction` — 66 rows, three scale factors |
| `extension/tpch/tpch_extension.cpp` | 239-242 | `PragmaTpchQuery` — `PRAGMA tpch(n)` returns SQL text |
| `extension/tpch/tpch_extension.cpp` | 244-268 | `LoadInternal`; `StatementReturnType::NOTHING` at 253 |
| `extension/tpch/dbgen/dbgen.cpp` | 184-192 | `append_begin_row` — the chunk flush |
| `extension/tpch/dbgen/dbgen.cpp` | 1126-1143 | `GenerateNext` — parallel/sequential dispatch |
| `extension/tpch/dbgen/dbgen.cpp` | 1444-1449 | `GetQuery` — bounds-checked lookup into `TPCH_QUERIES` |
| `extension/tpch/dbgen/dbgen.cpp` | 1451-1466 | `GetAnswer` — SF 0.01 / 0.1 / 1 only |
| `extension/tpch/dbgen/include/tpch_constants.hpp` | 1, 5, 28, 74, 120, 166 | generated by `scripts/generate_csv_header.py` |
| `extension/tpch/tpch_config.py` | 4-22 | build manifest |
| `src/include/duckdb/common/vector_size.hpp` | 15-21 | `STANDARD_VECTOR_SIZE = 2048` |
| `experiments/src/lineitem.rs` | 8-16 | dbgen-lite's seven columns |
| `experiments/src/bin/bench_suite.rs` | 74-79 | the 38 B / 28 B per row accounting behind the GB/s headline |

Pinned revisions: duckdb/duckdb@6c0c1a68 (regenerate the pin table
with `python3 tools/pin-table.py`).

**Specification**
- TPC-H revision 3.0.1, Clause 4.1.3.1 (the ten legal scale factors —
  DuckDB's `sf` is a `double` and accepts many more), Clause 4.2.5.1
  Table 3 and its Comment (SF-1 cardinalities; "4-byte integers,
  8-byte decimals, 4-byte dates", which Step 5's byte accounting
  follows).

**Cross-topic**
- topic 11 — operator-vs-materialization; Step 3's chunk loop.
- topic 16 — oracle taxonomy; Step 4's shipped answers.
- topic 17 — 26.32 GB/s accumulate and the branchless filter floor,
  the calibration for Step 5's effective bandwidth.
- topic 0 `reading-fair-benchmarking.md` — "incorrect code wins".
