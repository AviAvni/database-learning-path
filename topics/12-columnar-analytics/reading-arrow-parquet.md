# Arrow & Parquet: the layout compute wants, the bytes disk wants

Two open formats split the columnar world: Arrow is "the layout
kernels compute on" (in memory, O(1) random access, almost no
encoding), Parquet is "the layout bytes rest in" (on disk, encoded
then block-compressed, stats for pruning). Before you open arrow-rs —
one Rust repo, both crates — this chapter builds each format's design
one concept at a time, then the boundary between them, which is where
engines actually differ.

## The problem in one sentence

Compute kernels want every value reachable in O(1) with zero decode,
while disks and networks want the fewest possible bytes — one layout
cannot be both (a delta-encoded value can't be read without its
predecessors), so the ecosystem standardized TWO layouts and one
question: where do you decode?

## The concepts, step by step

### Step 1 — two jobs, two formats

A **memory format** is a contract about where bytes sit in RAM so that
independently written code (a Rust kernel, a Python library, a JDBC
driver) can compute over the same buffers with no conversion; a **file
format** is a contract about bytes at rest so that data survives,
ships, and can be read selectively. Arrow is the first, Parquet the
second, and the design pressures are opposite: Arrow forbids anything
that breaks O(1) random access (a kernel must jump to value 173,205
directly); Parquet embraces any encoding that shrinks bytes, because
disk reads are the cost. Why it matters: every "why does Arrow/Parquet
do X" question in this chapter resolves to which side of this split X
lives on.

### Step 2 — an Arrow array is a recipe of buffers

Arrow represents a column ("array") as a small descriptor — data type,
length, null count — plus a fixed list of raw, contiguous **buffers**;
there are no per-value objects and no pointers between values. Every
array type is just a different recipe:

```
 Int64Array      [validity bitmap][values i64 * n]
 StringArray     [validity][offsets i32 * (n+1)][utf8 bytes]
 DictionaryArray [keys array][values array]        <- topic 11's
 ListArray       [validity][offsets][child array]     DICTIONARY vector
```

That descriptor is `ArrayData` in arrow-rs: data type + length + null
count + `buffers` + child data. A 1M-row `Int64Array` is exactly two
allocations: a 125 KB bitmap and an 8 MB values buffer. Why it
matters: "layout as contract" is the whole product — kernels (topic
11's polars-compute) run on these buffers directly, from any language,
with zero conversion.

### Step 3 — validity bitmaps: nulls without branches or holes

Arrow marks NULLs with a separate **validity bitmap** (one bit per
row: 1 = value present) rather than sentinel values or by omitting the
slot — null slots still occupy their full width in the values buffer.
For 1M rows that's 125 KB of bitmap regardless of how many nulls there
are, and value *i* is always at offset `i × 8` no matter what precedes
it. That's what makes kernels branch-free: compute everything, mask
nulls afterwards (polars `float_sum`'s masked variant, topic 11). Why
it matters: the "wasted" bytes for null slots buy unconditional O(1)
addressing — Step 1's memory-side priority, chosen explicitly over
compactness.

### Step 4 — offset-based strings, zero-copy slices, and IPC

Variable-length data avoids per-value allocations by concatenating all
bytes into ONE buffer and adding an **offsets** buffer of n+1 integers
— string *i* is `bytes[offsets[i] .. offsets[i+1]]`:

```
 values  "ab", "", "xyz":
 offsets [0, 2, 2, 5]
 bytes   [a b x y z]        1M strings = 2 allocations, not 1M
```

Compare redis SDS (topic 2) — same "length-prefixed, cache-friendly"
instinct, different scale. Two consequences of the everything-is-plain-
buffers rule:

- **Zero-copy slicing**: an array is `offset` + `len` over shared,
  reference-counted (Arc'd) buffers — the same buffer serves many
  arrays; slicing allocates nothing.
- **IPC** (inter-process communication — Arrow's wire format, in
  `arrow-ipc/`): ship the buffers as-is; serialization = memcpy. The
  whole point of a standard memory layout is that it's *already* the
  wire format.

### Step 5 — Parquet: a hierarchy built for selective reading

A Parquet file splits data twice before storing anything — first
horizontally into **row groups** (~1M rows each), then per column into
**column chunks**, whose bytes are stored as **pages** (~1 MB units of
encoding/compression) — with a **footer** at the end of the file
holding all metadata plus min/max statistics:

```
 file
 └─ row group (~1M rows)                 RowGroupMetaData
    └─ column chunk (1 col × 1 rg)       ColumnChunkMetaData
       └─ pages (~1MB)                   encoding per page
 footer: thrift metadata + min/max stats
```

The hierarchy exists so a reader can grab *pieces*: want 3 columns of
2 row groups out of a 500-column, 1000-row-group file? Read the
footer, then exactly 6 column chunks — a few MB from a multi-GB file.
Why it matters: on disk (or S3), the unit of cost is bytes fetched,
and the layout is organized so most bytes never get fetched.

### Step 6 — page encodings, and the RLE/bit-packing hybrid

Each page's values are encoded with a scheme chosen from a fixed menu:
PLAIN (raw), RLE_DICTIONARY (dictionary ids, run-length encoded),
DELTA_BINARY_PACKED (deltas, bit-packed), BYTE_STREAM_SPLIT (floats:
transpose the bytes so byte 0 of every value sits together — similar
bytes adjacent compress better; the columns-beat-rows argument, one
level down). The workhorse called "RLE" is actually a **hybrid** that
alternates per group between run-length runs and bit-packed literals —
runs when the data repeats, packed groups when it doesn't, so
non-repetitive stretches don't explode into length-1 runs:

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

Each group's header low bit picks one of the two worlds. Why it
matters: this decode loop (and the `get_batch` bit-unpacker under it)
is the tight loop under every Parquet scan you'll ever profile.

### Step 7 — two compression layers, and stats as cross-file zone maps

Parquet compresses twice: first the **semantic** layer (Step 6's
encodings — the scan can still make sense of the bytes), then an
optional **block** layer (zstd/snappy over the whole encoded page —
opaque bytes, whole-page decompress to read anything). Only the first
layer is scannable; the second buys ratio at rest. DuckDB skips the
second layer for its own storage — the `fetch_row` random-access
constraint from the DuckDB chapter, again.

On top, the footer keeps min/max statistics per column chunk and per
page — Parquet's zone maps. A reader evaluates predicates against
footer stats and prunes whole row groups BEFORE reading any data
pages: predicate pushdown across a file (even an S3) boundary. A
`WHERE ts >= '2026-01-01'` on a date-sorted file can skip 95% of row
groups for the cost of reading a footer measured in KB.

### Step 8 — the boundary: where do you decode?

Reading Parquet into Arrow is a decode from the disk layout to the
compute layout — and *when* to do it is the late-materialization
decision every engine answers differently. Two shortcuts exist:
Parquet dictionary pages can map DIRECTLY to Arrow DictionaryArrays
(no decode!), and RLE-encoded null levels decode straight into
validity bitmaps. Beyond that:

| system | strategy |
|---|---|
| DuckDB | own format; scans execute over encodings, decode per-vector |
| polars/DataFusion | Parquet → Arrow at scan, engine sees Arrow only |
| ClickHouse | own format; decompress granules, engine sees flat columns |

Why it matters: the formats are standardized; the boundary is where
engines still compete. Decode too early and you move decoded bytes
through the whole plan; decode too late and every operator must
understand every encoding.

## Where each step lives in the code

One repo — [arrow-rs](https://github.com/apache/arrow-rs) — both
crates; a fresh shallow clone is enough.

- **Step 2** — `arrow-data/src/data.rs:208` — `ArrayData`: data type +
  length + null count + `buffers` + child data. The buffer recipes
  above are its interpretation rules per type.
- **Steps 3–4** — validity and offsets are `ArrayData` buffers (same
  file); zero-copy shipping in `arrow-ipc/`.
- **Step 5** — `parquet/src/file/metadata/mod.rs`:
  `RowGroupMetaData` (`:630`), `ColumnChunkMetaData` (`:808`),
  min/max stats at `:1458` (`min_values`/`max_values`).
- **Step 6** — encodings enum: `parquet/src/basic.rs:397+` — PLAIN,
  RLE (`:408` — the hybrid), RLE_DICTIONARY, DELTA_BINARY_PACKED
  (`:429`), BYTE_STREAM_SPLIT. The hybrid encoder/decoder:
  `parquet/src/encodings/rle.rs:55/:342`; the batch bit-unpacker:
  `util/bit_util.rs:696` `get_batch` — the tight loop under
  everything.
- **Step 7** — stats: same metadata anchors as Step 5; block
  compression wraps the encoded page in the page writer/reader paths
  next to `rle.rs`.

Read order: `data.rs` first (the memory contract is one struct), then
`basic.rs` for the encoding menu, then `rle.rs` until the hybrid
decode loop is obvious, then the metadata module for the stats.

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
