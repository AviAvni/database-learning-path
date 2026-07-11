# FSST & BtrBlocks: compress harder, stay random-access

Dictionary encoding dedups whole strings; LZ catches partial overlap
but kills random access. FSST closes that gap — LZ4-class ratios on
similar-but-distinct strings with every single string decodable alone —
and BtrBlocks (same group, three years later) shows what happens when
you cascade such encodings recursively and pick per block by sampling.
Read FSST first (it's a component), then BtrBlocks (the composition).

## FSST: Fast Static Symbol Table (VLDB '20)

The gap it fills: dictionary encoding dedups WHOLE strings — useless
when strings are distinct but SIMILAR (URLs, emails, paths).
LZ4/zstd catch that redundancy but kill random access (must decode a
whole block to read one string).

- The scheme: a static table of ≤255 symbols, each a 1–8 byte
  substring; encoding replaces substrings with 1-byte codes; code 255 =
  escape byte for literals.

```
 "http://www.example.com/index.html"
   [http://www.] [example] [.com/] [index] [.html]
        3           17        9      42      51      -> 5 bytes + table
```

- **Random access preserved**: any single string decodes alone —
  decode is a per-code table lookup (fits in L1: 255 × 8 B), no
  history window like LZ. This single property is why DuckDB ships it
  as a storage encoding and a VECTOR TYPE (FSST_VECTOR, topic 11) —
  compressed strings flow through the executor.

```rust
// FSST decode: a table lookup per code, NO history window —
// which is exactly why one string decodes without its neighbors
fn decode(codes: &[u8], sym: &[[u8; 8]; 255], len: &[u8; 255]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            255 => { out.push(codes[i + 1]); i += 2; }   // escape: literal byte
            c => {
                let n = len[c as usize] as usize;        // symbol = 1..8 bytes
                out.extend_from_slice(&sym[c as usize][..n]);
                i += 1;
            }
        }
    }
    out
}
```

- Symbol table construction: iterative — start with single bytes,
  repeatedly extend/merge symbols scoring by (frequency × length) gain,
  on a SAMPLE. A greedy-with-restarts search, bounded iterations.
- Claims: ~LZ4-class ratios on string data, faster decompression, AND
  random access. Check their table for where it loses (long-range
  redundancy, already-compressed data).

## BtrBlocks: cascaded encodings, chosen by sampling (SIGMOD '23)

The setting: open formats (Parquet) pick conservative encodings; you
can do much better if the format may choose AGGRESSIVELY per block.

- The scheme: for each 64K-value block, take small SAMPLES, try every
  applicable encoder ON the sample, pick the best ratio — then RECURSE:
  the outputs of one encoding (e.g. dictionary codes, FOR residuals)
  are themselves columns that get the same treatment, up to depth 3.

```
 strings ─ dictionary ─┬─ codes (ints)  ─ FOR ─ bit-pack
                       └─ dict entries  ─ FSST
 doubles ─ pseudodecimal ─ (mantissa ints) ─ ...   <- their new float trick
```

- Sampling vs DuckDB's full analyze pass: they show a handful of small
  random slices (not one contiguous slice!) estimates ratios well —
  ingest stays fast, choice stays near-optimal. (The topic 0 sampling
  lesson: representative beats exhaustive.)
- No general-purpose byte compressor on top — everything stays
  scannable + SIMD-decodable; they hit Parquet+zstd-class ratios with
  ~4× faster decompression, on the cheap-CPU side of the
  network-vs-CPU tradeoff (object storage era — topic 28).

## Questions for notes.md

1. FSST vs dictionary on: (a) 1M distinct URLs sharing 20 prefixes,
   (b) country codes with NDV 200, (c) UUIDs. Pick the winner per case
   and say why (BtrBlocks would cascade — which cascade for (a)?).
2. Why must FSST's table be STATIC (immutable after training) for
   random access + vectorized decode? What would adaptive (LZ78-style)
   codes break?
3. BtrBlocks samples; DuckDB analyzes everything; ClickHouse makes you
   declare. Place the three on an ingest-cost / ratio-quality /
   operator-burden triangle.
4. The escape byte: worst-case FSST inflation on incompressible input?
   Compare with Parquet RLE-hybrid's worst case from the
   arrow-parquet guide.
5. M12: property values in FalkorDB are often short similar strings
   (emails, category names). Sketch the cascade for a string property
   column and mark which stages allow predicate-on-encoded execution
   (`= 'x'` on dict codes: yes; on FSST codes: trickier — why? unequal
   code lengths, but equality CAN compare encoded bytes if the table
   is shared — when is it?).

## Done when

You can explain FSST in three sentences (symbol table, 1-byte codes,
random access), BtrBlocks in two (sample per block, cascade), and
argue when each beats plain dictionary + zstd.

## References

**Papers**
- Boncz, Neumann, Leis — "FSST: Fast Random Access String Compression"
  (VLDB 2020) — the scheme, the table-construction search, and the
  table of where it loses
- Kuschewski, Sauerwein, Alhomssi, Leis — "BtrBlocks: Efficient
  Columnar Compression for Data Lakes" (SIGMOD 2023) — the sampling
  argument and the cascade; same group as the VLDB '15 / LeanStore
  papers

**Code**
- [fsst](https://github.com/cwida/fsst) — the authors' reference
  implementation; [btrblocks](https://github.com/maxi-k/btrblocks) —
  the paper's artifact (both optional — DuckDB's `fsst.cpp` in
  [reading-duckdb-compression.md](reading-duckdb-compression.md) is the
  production integration)
