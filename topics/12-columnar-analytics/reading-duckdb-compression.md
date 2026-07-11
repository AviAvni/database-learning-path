# DuckDB's encoding zoo: analyze, score, commit

Who picks the encoding? DuckDB's answer: nobody — race every candidate
encoder over the column and let the byte estimates decide, per column,
per row group. This chapter walks the framework contract that makes
that affordable, then two encoders end-to-end (RLE, bit-packing), then
the string stack and the zone maps that filter pushdown lands on.

The selection loop, condensed:

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

## 1. The framework: analyze → score → compress → scan

`src/include/duckdb/function/compression_function.hpp:130–141` — the
lifecycle, documented in the header:

```
 for each candidate encoder:            (per column, per row group)
   init_analyze (:139)
   analyze(vector) per vector — may return false = drop out early
   final_analyze (:141) -> SCORE (estimated bytes; lower wins)
 winner runs compress_data (:148) over the same data again
 scans use scan_vector (:159) / scan_partial (:162);
 point lookups use fetch_row (:172)   <- random access into encodings!
```

Two-pass design: DuckDB pays a full extra read of the data to CHOOSE the
encoding. Benchmark-before-committing, in production. `fetch_row` is
the constraint that shapes everything — every encoding must support
point access (or fake it); that's why heavyweight block codecs (zstd)
are a LAST resort (whole-block decode to fetch one row).

## 2. RLE (`rle.cpp`) — the simplest complete example

- `RLEAnalyzeState :86` / `RLEAnalyze :99` — counts runs;
  `RLEFinalAnalyze :113` returns bytes = runs × (value + count size).
- `RLECompressState :126` — writes two interleaved arrays (values,
  counts).
- The `CompressionFunction` registration `:570` bundles all the function
  pointers — grep this pattern in every other encoder.

## 3. Bit-packing (`bitpacking.cpp`) — four encodings in one

`BitpackingMode` (`:103`, decode `:42`): AUTO picks per GROUP of 2048
values (`:209–:264`):

```
 all equal            -> CONSTANT       (store 1 value)
 equal deltas         -> CONSTANT_DELTA (store base + delta)
 clustered            -> FOR: store min, bit-pack (value - min)
 sequential-ish       -> DELTA_FOR: delta-encode, then FOR the deltas
```

Note `:219–:237`: the mode decision arithmetic — it computes the width
each variant needs and picks the smallest. Per-2048-group modes mean ONE
column segment mixes encodings. `ForceBitpackingModeSetting :312` for
experiments.

## 4. The string stack (skim)

- `dictionary_compression.cpp:48` — dictionary; ids then bit-packed.
- `fsst.cpp:40–:47,:72` — FSST analyze/compress: train a symbol table
  (8-byte substrings → 1-byte codes) on a sample, encode all strings;
  random access preserved (decode one string without its neighbors —
  unlike zstd).
- `dict_fsst/` — both at once: dictionary of FSST-compressed strings.
- `zstd.cpp` — the heavyweight fallback for what nothing else catches.

## 5. Zone maps

`src/storage/table/column_data.cpp:423` — `ColumnData::CheckZonemap`:
consults per-segment stats (numeric_stats.cpp / string_stats.cpp — note
strings keep min/max PREFIXES) and returns a `FilterPropagateResult`:
always-true (drop the filter too!), always-false (skip the segment), or
no-pruning. Filter pushdown from topic 10 lands HERE — the plan-level
rewrite becomes a storage-level skip.

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
