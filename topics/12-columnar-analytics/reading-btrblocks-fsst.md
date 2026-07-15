# FSST & BtrBlocks: compress harder, stay random-access

Dictionary encoding dedups whole strings; LZ catches partial overlap
but kills random access. FSST closes that gap — LZ4-class ratios on
similar-but-distinct strings with every single string decodable alone —
and BtrBlocks (same group, three years later) shows what happens when
you cascade such encodings recursively and pick per block by sampling.
This chapter builds both ideas step by step — the gap, the symbol
table, why static tables are the trick, the cascade, and the sampling
argument — then routes you through the two papers (FSST first: it's a
component; then BtrBlocks: the composition).

## The problem in one sentence

A column of 1M distinct URLs defeats dictionary encoding (nothing
repeats *whole*) and zstd would shrink it ~4× but forces you to
decompress a whole block to read ONE string — the gap is a string
encoding with LZ-class ratios where any single value decodes alone.

## The concepts, step by step

### Step 1 — the gap: whole-string dedup vs block compression

Dictionary encoding (topic 12's staple: store each distinct string
once, reference it by integer id) dedups WHOLE strings — useless when
strings are distinct but SIMILAR: URLs, emails, file paths, where the
redundancy is shared *substrings* ("http://www.", "@gmail.com").
LZ-family compressors (LZ4, zstd) catch exactly that redundancy — they
replace repeated substrings with back-references into a sliding
history window — but the back-references are the poison: to decode
string 5,000 you must first decode everything its references point
into, i.e. the whole block. That breaks `fetch_row`-style random
access and rules them out as a scan-path vector format. Why it
matters: real analytics columns are full of medium-cardinality similar
strings, and until 2020 the menu offered no encoding that was both
compact and randomly accessible for them.

### Step 2 — the symbol table: 255 substrings, 1-byte codes

FSST (**fast static symbol table**) compresses strings with a small
fixed table of at most 255 **symbols** — each a 1–8 byte substring —
and encodes each string by greedily replacing matched substrings with
the 1-byte code of the symbol; code 255 is an escape marker meaning
"the next byte is a literal that matched no symbol":

```
 "http://www.example.com/index.html"
   [http://www.] [example] [.com/] [index] [.html]
        3           17        9      42      51      -> 5 bytes + table
```

34 bytes → 5 bytes, ~7×, and the table itself is tiny: 255 × 8 B ≈
2 KB, shared by the whole block. Why it matters: the compression unit
dropped from "whole string" (dictionary) to "substring" (LZ) *without*
adopting LZ's history window — the table IS the entire shared state.

### Step 3 — static is the trick: random access and vectorized decode

Because the symbol table is trained once and then immutable
(**static**), decoding is a pure per-code table lookup with no
history: any single string decodes alone, in isolation, and the 2 KB
table sits in L1 cache the whole time:

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

Contrast an adaptive (LZ78-style) scheme, where the table *evolves* as
you decode — every code's meaning depends on all prior codes, and
random access dies. This single property — decode one string without
its neighbors — is why DuckDB ships FSST both as a storage encoding
and as a VECTOR TYPE (FSST_VECTOR, topic 11): compressed strings flow
through the executor itself. Why it matters: the design constraint
(random access) dictated the mechanism (static table), not the other
way around.

### Step 4 — training the table: greedy search on a sample

The table is built by an iterative search over a small SAMPLE of the
data: start with single-byte symbols, repeatedly extend and merge
symbols, scoring each candidate by its estimated gain (frequency ×
length), for a bounded number of iterations — greedy with restarts,
not an optimal search. The claims to verify in the paper: ~LZ4-class
ratios on string data, *faster* decompression than LZ4, and random
access on top; check their table for where FSST loses (long-range
redundancy, already-compressed data). The cost side: training is a
few passes over a sample — cheap, but nonzero, and a bad sample means
a bad table for the whole block. Why it matters: this is the topic's
recurring pattern — spend bounded work at write time choosing a
representation, harvest it on every scan.

### Step 5 — BtrBlocks: encoder outputs are columns too, so recurse

BtrBlocks starts from an observation about open formats: Parquet picks
conservative, one-shot encodings; you can do much better if the format
may choose AGGRESSIVELY per block. Its scheme: for each 64K-value
block, try every applicable encoder, pick the best — then **cascade**:
the *outputs* of one encoding (dictionary codes are an int column; FOR
residuals are an int column; dictionary entries are a string column)
get the same treatment recursively, up to depth 3:

```
 strings ─ dictionary ─┬─ codes (ints)  ─ FOR ─ bit-pack
                       └─ dict entries  ─ FSST
 doubles ─ pseudodecimal ─ (mantissa ints) ─ ...   <- their new float trick
```

So FSST slots in as one component of a larger composition — exactly
how DuckDB's `dict_fsst/` uses it. Why it matters: no single encoding
is the answer; the win compounds — dictionary might give 5×, then
bit-packed codes another 4× — and the cascade finds the composition
per block instead of per format revision.

### Step 6 — sampling, not full analysis

To choose among cascades without reading each block many times,
BtrBlocks estimates each candidate's ratio on small random SAMPLES —
and shows that a handful of small slices drawn from *different*
positions (not one contiguous slice!) predicts the full-block ratio
well. Compare the three answers to "who picks the encoding": DuckDB
analyzes everything (full extra pass at ingest), ClickHouse makes you
declare, BtrBlocks samples — near-optimal choice at a fraction of the
ingest cost, risking only an unrepresentative sample. (The topic 0
sampling lesson: representative beats exhaustive.) Why it matters:
choice quality vs ingest cost is a dial, and sampling sits at its
sweet spot for data lakes where ingest volume is huge.

### Step 7 — no block compressor on top: the CPU-vs-network bet

BtrBlocks deliberately puts NO general-purpose byte compressor over
its cascade — everything on disk stays scannable and SIMD-decodable —
and still reaches Parquet+zstd-class ratios with ~4× faster
decompression. The bet: in the object-storage era (topic 28), network
bandwidth to S3 is plentiful and CPU is the scarce resource at scan
time, so trading a few percent of ratio for 4× cheaper decode wins.
This is Parquet's two-layer design (semantic + block) with the second
layer amputated on purpose. Why it matters: it closes the arc this
topic opened — compression IS performance, and the last block codec
standing gets cut when it stops paying for its CPU.

## How to read the papers (with the concepts in hand)

**FSST (VLDB '20)** — read first; it's a component.

1. The scheme itself is Steps 2–3; the paper's contribution beyond
   them is the table-construction search (Step 4) and the engineering
   for vectorized decode — read both carefully.
2. Check the evaluation table for where FSST *loses* (long-range
   redundancy, already-compressed data) — the honest boundary of the
   technique.

**BtrBlocks (SIGMOD '23)** — read second; it's the composition.

1. The cascade (Step 5) and the sampling argument (Step 6) are the
   core — the sampling section is the part to work through slowly
   (why multiple small slices beat one contiguous slice).
2. The evaluation's Parquet+zstd comparison is Step 7's bet
   quantified — note it's the same group as the VLDB '15 / LeanStore
   papers, and the hardware-conscious style shows.

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
