# DuckDB's encoding zoo: analyze, score, commit

Who picks the encoding? DuckDB's answer: nobody — race every candidate
encoder over the column and let the byte estimates decide, per column,
per row group. Before you open the C++, this chapter builds the ideas
one at a time: what a lightweight encoding is, what unit the choice is
made for, the two-pass lifecycle that makes racing affordable, the
random-access constraint that shapes the whole menu, two encoders
end-to-end (RLE, bit-packing), the string stack, and the zone maps
that filter pushdown lands on. Then it hands you the file and line
anchors to watch each piece in the source.

## The problem in one sentence

Analytics scans are memory-bound, so bytes moved ≈ time — an encoding
that shrinks a column 4× makes the scan up to 4× faster — but the
right encoding differs per column and per chunk of rows, and a wrong
guess can *inflate* the data; DuckDB refuses to guess.

## The concepts, step by step

### Step 1 — lightweight encoding: compression the scan can execute over

An **encoding** here is not gzip. It is a reversible rewrite of a
column's values that exploits a *pattern in the data* — repetition,
small ranges, few distinct values — and whose decode is a handful of
arithmetic instructions, cheap enough to run inside the scan loop.
Contrast a **block compressor** (gzip/zstd class): it treats bytes as
opaque, achieves great ratios, but must decompress a whole block
before you can read anything.

Concrete: a sorted column of 1M timestamps where each value repeats
~1000 times stores as ~1000 `(value, run_length)` pairs — 16 KB
instead of 8 MB, a 500× reduction — and decoding a run is one branch.
Why it matters: since scans move bytes, a lightweight encoding is not
a space feature, it *is* the performance feature. The topic's thesis
— compression IS performance — starts here.

### Step 2 — the unit of choice: row group, column segment, vector

DuckDB doesn't pick one encoding for a whole table, or even a whole
column. Tables are stored as **row groups** (horizontal slices of
122,880 rows); within a row group each column is its own sequence of
**segments** (contiguous encoded blocks); and all execution moves data
in **vectors** of 2,048 values (topic 11's unit). The encoding
decision is made **per column, per row group**.

```
 table
 └─ row group (122,880 rows)          ← the decision unit
    ├─ column "ts"   → segments encoded as DELTA_FOR
    ├─ column "city" → segments encoded as DICTIONARY
    └─ column "id"   → segments encoded as BITPACKING
```

Why it matters: data shape drifts within a table (early rows sorted,
late rows random; one column low-cardinality, another unique), so a
global choice is always wrong somewhere. Per-row-group choice bounds
the damage of any one bad fit to 122,880 rows.

### Step 3 — analyze → score → compress: race the encoders, cheapest estimate wins

For each column of each row group, DuckDB runs *every* candidate
encoder over the data in a dry-run **analyze** pass that only counts
what the encoded size *would* be, picks the smallest estimate, and
only then lets the winner actually **compress**. The selection loop,
condensed:

```rust
// per column, per row group: race every encoder, cheapest estimate wins
fn choose(col: &RowGroupColumn, candidates: &[&dyn Encoder]) -> &dyn Encoder {
    let mut best = (f64::INFINITY, candidates[0]);
    for enc in candidates {
        let mut st = enc.init_analyze();
        if !col.vectors().all(|v| enc.analyze(&mut st, v)) {
            continue;                          // encoder drops out early
        }
        let score = enc.final_analyze(st);     // ESTIMATED bytes — no compressing yet
        if score < best.0 { best = (score, *enc); }
    }
    best.1        // winner re-reads the whole column in compress_data
}
```

The lifecycle, as the framework header documents it:

```
 for each candidate encoder:            (per column, per row group)
   init_analyze
   analyze(vector) per vector — may return false = drop out early
   final_analyze -> SCORE (estimated bytes; lower wins)
 winner runs compress_data over the same data again
 scans use scan_vector / scan_partial;
 point lookups use fetch_row   <- random access into encodings!
```

The cost: this is a **two-pass design** — DuckDB pays a full extra
read of the data at ingest just to CHOOSE the encoding. That is
benchmark-before-committing, built into a production storage engine.
(You can override it with `PRAGMA force_compression` for experiments.)

### Step 4 — fetch_row: the random-access constraint that shapes the menu

Every encoding must also answer a point request: "give me row 1907 of
this segment" (`fetch_row` in the framework contract), because
operators like index joins and late-materialized fetches ask for
single rows, not whole vectors. An encoding qualifies for the menu
only if it can decode one value without decoding everything before it
— or fake it acceptably.

This single constraint explains the menu's shape: RLE, dictionary,
bit-packing, and FSST can all jump to (or near) a single value;
a heavyweight block codec (zstd) cannot — fetching one row means
decompressing the whole block. That is why zstd is the **last
resort** fallback, not a default. Why it matters: the storage format
is negotiated with the *executor*, not chosen for ratio alone.

### Step 5 — RLE: the simplest complete encoder

RLE (**run-length encoding** — store each maximal run of equal values
as one `(value, count)` pair) is the smallest example that exercises
the entire framework contract. Its analyze pass just counts runs; its
score is `runs × (value_size + count_size)`; its compress pass writes
two interleaved arrays (values, counts).

```
 input:   7 7 7 7 7 7 9 9 9 2 2 2 2 ...     (1M rows, ~1000 runs)
 encoded: values [7, 9, 2, ...]  counts [6, 3, 4, ...]
 score:   1000 × (8 B + 2 B) = 10 KB   vs   8 MB raw
```

RLE wins on sorted or low-cardinality data and loses catastrophically
on random data (1M runs of length 1 = *bigger* than raw — which is
exactly what the score detects before any bytes are written). Read
this encoder first in the source; every other encoder repeats its
registration pattern.

### Step 6 — bit-packing: four encodings in one function

Bit-packing stores integers in `ceil(log2(max - min + 1))` bits
instead of 64 — but DuckDB's `bitpacking.cpp` is really four encodings
picked *per group of 2,048 values* by trying each and computing the
width it would need:

```
 all equal            -> CONSTANT       (store 1 value)
 equal deltas         -> CONSTANT_DELTA (store base + delta)
 clustered            -> FOR: store min, bit-pack (value - min)
 sequential-ish       -> DELTA_FOR: delta-encode, then FOR the deltas
```

FOR (**frame of reference** — store the group's minimum once, then
only each value's offset from it) turns values like
1,000,000,007…1,000,000,900 into 10-bit offsets: 64 bits → 10 bits,
a 6.4× cut. DELTA_FOR (delta-encode first — store differences from
the previous value — then FOR the deltas) catches timestamps and
sequences. Per-2048-group modes mean **one column segment mixes
encodings** — the decision granularity is even finer than the
row-group race of Step 3. Why it matters: the arithmetic that picks
the mode is the same score-then-commit discipline, recursed one level
down.

### Step 7 — the string stack: dictionary, FSST, both, then give up

Strings get a cascade of increasingly aggressive encodings. **Dictionary
encoding** (store each distinct string once in a dictionary; the
column becomes integer ids into it) wins when there are few distinct
values — 1M rows of 200 country names become 1M small ints (which then
get bit-packed, Step 6) plus a 200-entry dictionary; string
comparisons become int comparisons. **FSST** (fast static symbol table
— a 255-entry table mapping 1–8-byte substrings to 1-byte codes)
catches columns where strings are *distinct but similar* (URLs,
emails) that dictionary can't dedup, while keeping every string
individually decodable — the Step 4 constraint again. `dict_fsst/`
stacks both: a dictionary whose entries are FSST-compressed. And
`zstd.cpp` is the heavyweight fallback for whatever nothing else
catches — accepted only because sometimes ratio beats access. (FSST
gets its own chapter: [reading-btrblocks-fsst.md](reading-btrblocks-fsst.md).)

### Step 8 — zone maps: skip the segment instead of decoding it

A **zone map** is a per-segment min/max statistic kept alongside the
data; before scanning a segment, the engine checks the filter against
the min/max and can skip the whole segment — no read, no decode:

```
 WHERE ts BETWEEN '2026-01-01' AND '2026-01-02'
 seg 0 [ts: 2025-11-01 .. 2025-12-04]  -> skip (no read, no decode)
 seg 1 [ts: 2025-12-04 .. 2026-01-05]  -> scan
 seg 2 [ts: 2026-01-05 .. 2026-02-11]  -> skip
```

DuckDB's check returns three-valued answers: **always-false** (skip
the segment), **always-true** (scan it *and drop the filter* — every
row passes, so why test each one), or no-pruning. String columns keep
min/max *prefixes*, not full values. The catch: zone maps only prune
if the data is clustered on the filter column — on random data every
zone spans the whole domain and nothing skips. Why it matters: this is
where topic 10's filter pushdown physically lands — a plan-level
rewrite becomes a storage-level skip.

## Where each step lives in the code

Read the framework header first — the lifecycle contract is documented
in it — then the encoders, then zone maps.

| File | Role (steps) |
|------|------|
| `src/include/duckdb/function/compression_function.hpp` | the lifecycle contract (3, 4) |
| `src/storage/compression/rle.cpp` | the simplest complete encoder (5) |
| `src/storage/compression/bitpacking.cpp` | four modes in one (6) |
| `src/storage/compression/dictionary_compression.cpp`, `fsst.cpp`, `dict_fsst/`, `zstd.cpp` | the string stack (7) |
| `src/storage/table/column_data.cpp` | zone maps (8) |

- **Step 3** — `compression_function.hpp:130–141`: `init_analyze`
  (`:139`), `analyze` per vector, `final_analyze` (`:141`) returning
  the score; the winner's `compress_data` (`:148`). Force a choice via
  `PRAGMA force_compression` for your experiments.
- **Step 4** — scans via `scan_vector` (`:159`) / `scan_partial`
  (`:162`); point lookups via `fetch_row` (`:172`) — the constraint
  that keeps zstd a last resort.
- **Step 5** — `rle.cpp`: `RLEAnalyzeState :86` / `RLEAnalyze :99`
  count runs; `RLEFinalAnalyze :113` returns bytes = runs × (value +
  count size); `RLECompressState :126` writes the two interleaved
  arrays. The `CompressionFunction` registration at `:570` bundles all
  the function pointers — grep this pattern in every other encoder.
- **Step 6** — `bitpacking.cpp`: `BitpackingMode` (`:103`, decode
  `:42`); AUTO picks per group of 2048 values (`:209–:264`); the mode
  decision arithmetic at `:219–:237` computes each variant's width and
  picks the smallest. `ForceBitpackingModeSetting :312` for
  experiments.
- **Step 7** — `dictionary_compression.cpp:48` (ids then bit-packed);
  `fsst.cpp:40–:47,:72` (train a symbol table on a sample, encode all
  strings); `dict_fsst/` (both at once); `zstd.cpp` (the fallback).
- **Step 8** — `column_data.cpp:423` `ColumnData::CheckZonemap`:
  consults per-segment stats (`numeric_stats.cpp` /
  `string_stats.cpp` — note strings keep min/max PREFIXES) and returns
  a `FilterPropagateResult`: always-true (drop the filter too!),
  always-false (skip the segment), or no-pruning.

## Questions for notes.md

1. The analyze pass doubles ingest cost. What does BtrBlocks do instead
   (sampling) and what does it risk?
2. `fetch_row` on DELTA_FOR: decoding row 1907 of a 2048 group requires
   what? Why is this fine for OLAP (how often does fetch_row run —
   think late materialization: fetch AFTER filter).
3. RLE score vs dictionary score on a column of 50% NULLs: which wins
   and why does validity (empty_validity.cpp) change the answer?
4. Zone map always-true result removes the FILTER — when does that
   matter more than segment skipping? (Selectivity ~100% — filter cost
   itself.)
5. M12: which of the four bitpacking modes fits node-id columns in a
   graph adjacency payload? (Ids are dense-ish and clustered by
   creation time.)

## Done when

You can recite the analyze→score→compress lifecycle, the four
bitpacking modes with their triggers, and explain why fetch_row shapes
the whole encoder menu.

## References

**Code**
- [duckdb](https://github.com/duckdb/duckdb) — read
  `src/include/duckdb/function/compression_function.hpp` first (the
  lifecycle contract is documented in the header), then the encoders in
  `src/storage/compression/` (`rle.cpp`, `bitpacking.cpp`,
  `dictionary_compression.cpp`, `fsst.cpp`, `dict_fsst/`, `zstd.cpp`);
  zone maps in `src/storage/table/column_data.cpp`
