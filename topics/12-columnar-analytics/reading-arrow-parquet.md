# Arrow & Parquet: the layout compute wants, the bytes disk wants

Two open formats split the columnar world: Arrow is "the layout
kernels compute on" (in memory, O(1) random access, almost no
encoding), Parquet is "the layout bytes rest in" (on disk, encoded
then block-compressed, stats for pruning). This chapter reads both
from one Rust repo — arrow-rs ships both crates — and then the
boundary between them, which is where engines actually differ.

## 1. Arrow: layout as contract

- `arrow-data/src/data.rs:208` — `ArrayData`: data type + length +
  null count + `buffers` + child data. Every array type is a recipe of
  buffers:

```
 Int64Array      [validity bitmap][values i64 * n]
 StringArray     [validity][offsets i32 * (n+1)][utf8 bytes]
 DictionaryArray [keys array][values array]        <- topic 11's
 ListArray       [validity][offsets][child array]     DICTIONARY vector
```

- Validity is a BITMAP, not tombstones: null slots still occupy value
  space (fixed-width) — that's what makes kernels branch-free (compute
  everything, mask nulls; polars float_sum's masked variant, topic 11).
- Offsets-based strings: no per-string allocations, one contiguous
  bytes buffer. Compare redis SDS (topic 2) — same "length-prefixed,
  cache-friendly" instinct, different scale.
- Zero-copy slicing: `offset` + `len` over shared buffers (Arc'd) —
  the same buffer serves many arrays. IPC (`arrow-ipc/`) ships these
  buffers as-is: serialization = memcpy, the whole point of a standard.

## 2. Parquet: the on-disk hierarchy

```
 file
 └─ row group (~1M rows)                 RowGroupMetaData (metadata/mod.rs:630)
    └─ column chunk (1 col × 1 rg)       ColumnChunkMetaData (:808)
       └─ pages (~1MB)                   encoding per page
 footer: thrift metadata + min/max stats (:1458 min_values/max_values)
```

- Encodings (`parquet/src/basic.rs:397+`): PLAIN, RLE (:408 — actually
  an RLE/bit-packing HYBRID: runs when repetitive, bit-packed groups
  when not), RLE_DICTIONARY, DELTA_BINARY_PACKED (:429),
  BYTE_STREAM_SPLIT (floats: transpose bytes so compressors see
  similar bytes together).
- `parquet/src/encodings/rle.rs:55/:342` — the hybrid encoder/decoder;
  `util/bit_util.rs:696` `get_batch` — unpack a batch of bit-packed
  values (the tight loop under everything).

The hybrid's shape, decoded — each group's header low bit picks one of
two worlds:

```rust
// parquet "RLE" is really RLE + bit-packing, alternating per group:
// runs when the data repeats, packed literals when it doesn't
fn decode_hybrid(r: &mut BitReader, width: u32, out: &mut Vec<u32>) {
    while let Some(header) = r.read_uleb128() {
        if header & 1 == 0 {
            let count = header >> 1;                 // RLE group:
            let value = r.read_le_bytes(width);      //   one value,
            out.extend(repeat(value).take(count));   //   count copies
        } else {
            let literals = (header >> 1) * 8;        // bit-packed group:
            for _ in 0..literals {                   //   8-value multiples,
                out.push(r.read_bits(width));        //   width bits each
            }
        }
    }
}
```

- Two compression layers: encoding (semantic, scannable) THEN optional
  block compression (zstd/snappy over the encoded page). DuckDB skips
  the second layer for its own storage — random access again.
- Stats at chunk and page level = Parquet's zone maps; readers prune
  row groups by footer stats BEFORE reading data pages (predicate
  pushdown across a file boundary).

## 3. The boundary (where engines differ)

Reading Parquet into Arrow: dictionary pages can map DIRECTLY to Arrow
DictionaryArrays (no decode!), RLE levels decode to validity bitmaps.
The choice of when to decode is the late-materialization decision:

| system | strategy |
|---|---|
| DuckDB | own format; scans execute over encodings, decode per-vector |
| polars/DataFusion | Parquet → Arrow at scan, engine sees Arrow only |
| ClickHouse | own format; decompress granules, engine sees flat columns |

## Questions for notes.md

1. Why does Arrow have almost NO encodings (just dictionary + REE)
   while Parquet has many? What would delta-encoded values break for
   an O(1)-random-access compute kernel?
2. Parquet's RLE hybrid: why alternate runs with bit-packed groups
   instead of pure RLE? (What input kills pure RLE — and what's the
   worst-case size vs PLAIN?)
3. BYTE_STREAM_SPLIT: why does splitting f64s into 8 byte-planes help
   zstd? Connect to why columns compress better than rows — it's the
   same argument one level down.
4. min/max stats on a string column: why do engines store truncated
   prefixes, and what bug lurks if truncation isn't handled on the max
   side? (Hint: "abc\xff…" — increment-the-prefix.)
5. M12: property columns for FalkorDB — Arrow-style validity bitmaps
   for optional properties, or a separate presence structure
   (roaring bitmap keyed by node id)? What does each cost when 1% vs
   99% of nodes have the property?

## Done when

You can draw both hierarchies (buffers / file→rg→chunk→page), explain
the two compression layers and why only one is scannable, and name
where the Parquet→Arrow decode happens in polars vs DuckDB.

## References

**Papers**
- Melnik et al. — "Dremel: Interactive Analysis of Web-Scale Datasets"
  (VLDB 2010) — optional; the repetition/definition-level encoding for
  nested data that Parquet adopted wholesale (skipped here — graphs
  are flat)

**Code**
- [arrow-rs](https://github.com/apache/arrow-rs) — one repo, both
  crates: `arrow-data/src/data.rs` (ArrayData, the layout contract),
  `arrow-ipc/` (zero-copy shipping), `parquet/src/basic.rs`
  (encodings), `parquet/src/encodings/rle.rs` + `util/bit_util.rs`
  (the hybrid), `parquet/src/file/metadata/mod.rs` (footer stats); a
  fresh shallow clone is enough
