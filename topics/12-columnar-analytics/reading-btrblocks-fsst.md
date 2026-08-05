# FSST & BtrBlocks: compress harder, stay random-access

Two papers from the same research line, read together because the second one uses the
first as a component:

- **FSST** — Peter Boncz, Thomas Neumann, Viktor Leis, *FSST: Fast Random Access String
  Compression*, PVLDB 13(11), 2020. Code: <https://github.com/cwida/fsst>.
- **BtrBlocks** — Maximilian Kuschewski, David Sauerwein, Adnan Alhomssi, Viktor Leis,
  *BtrBlocks: Efficient Columnar Compression for Data Lakes*, SIGMOD 2023. Code:
  <https://github.com/maxi-k/btrblocks>.

Read FSST first (13 pages, short) and BtrBlocks second (14 pages). Every number below
carries the section, table or figure it came from; if you find one that does not, treat it
as unverified and delete it.

Two terms, since the papers use them interchangeably and this guide will too.
**Compression factor** (or ratio) is `uncompressed bytes / compressed bytes`, so bigger is
better and a factor below 1.0 means the "compressor" made the data *larger*. **Random
access** means fetching value *i* without touching values 0…*i*−1.

---

## The problem in one sentence

A general-purpose byte compressor such as LZ4 or zstd needs a large window of surrounding
bytes to find redundancy, so it only pays off when you compress thousands of values as one
opaque block — and then reading one value costs decompressing the whole block; FSST buys
back per-value random access by replacing the back-reference with a *static* 255-entry
symbol table, and BtrBlocks asks what a whole file format looks like when every scheme in
it has that property.

The FSST paper measures exactly how badly the alternatives fail. On its `urls` column,
compressing each string *individually* with LZ4 gives a compression factor **below 1.0** —
the output is bigger than the input (§6.2, Figure 4), because a single URL is too short to
contain the repetition LZ4 needs. Chopping the same column into fixed-size blocks and
compressing each block recovers the ratio, but only once the blocks are big:

| LZ4 block size (bytes) | 16 | 64 | 256 | 1 K | 4 K | 16 K | 64 K |
| ---------------------- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| compression factor on `urls` | 0.46 | 0.78 | 1.14 | 1.59 | 2.03 | 2.45 | 2.73 |

*(FSST §6.1.)* Below 256-byte blocks LZ4 inflates the data. To beat 2× you need blocks of
several KB — which is hundreds of URLs, all of which must be decompressed to read one.

---

## The concepts, step by step

### Step 1 — Why a byte compressor cannot give you per-value access

> **In:** a string column, and the wish to read value *i* without reading its neighbours.
> **Out:** the reason LZ4/zstd cannot serve that wish, and the shape of a scheme that can.

LZ4 and zstd are **block compressors**: they encode the input as a stream of literals and
*back-references* ("copy 12 bytes from 300 bytes ago"). Two consequences follow, and both
are structural, not implementation details.

1. **The dictionary is the data itself.** A back-reference is only meaningful relative to
   the bytes already decompressed, so decoding is inherently sequential: to produce byte
   *n* you must have produced bytes 0…*n*−1. The FSST paper puts it exactly: LZ4
   decompression mutates internal state, "which precludes cheap point access" (§3).
2. **Short inputs have nothing to point at.** A 60-byte URL contains almost no internal
   repetition. The redundancy lives *across* URLs, and a per-string compressor cannot see
   it. Hence the sub-1.0 factor in the table above.

The way out is to move the shared redundancy into a structure that lives *outside* the
compressed bytes and *does not change* while you decode: a symbol table.

```
LZ4 (block):   [ ....... 64 KB of literals+backrefs ....... ]
                 ^ to read value 900 you decode from here

FSST:          symbol table (fixed, shared)   +   [c][c][c] [c][c] [c][c][c][c] ...
                                                   val 0     val 1   val 2
                 ^ to read value 900, jump to it and decode 1-byte codes
```

### Step 2 — The symbol table: 255 symbols, 1-byte codes

> **In:** a corpus of strings, and a symbol table already trained (Step 4 builds it).
> **Out:** how a string is represented, and the exact size of the decoder state.

FSST replaces *substrings of 1 to 8 bytes* — the **symbols** — with **1-byte codes**
(§3). Fixing the code width at one byte is the design decision everything else follows
from:

- A code is 1 byte, so there can be at most **256** codes. One is reserved as the
  **escape**, leaving **255** real symbols (§3, §3.2).
- Symbols are 1–8 bytes and sit on byte boundaries — no bit-level packing, no alignment
  work at decode time.
- The **escape** (code 255) means "the next input byte is a literal, emit it as-is"
  (§3.2). So an unrepresentable byte costs **2 bytes** — escape + literal.

Decoding is an array lookup and an unconditional store. The paper is precise about the
state that has to stay hot: symbols are held as 8-byte words in a **2048-byte** array
(256 × 8) with a separate **256-byte** length array, and both fit in L1 (§3.1). The
decoder writes a full 8-byte word unconditionally and then advances the output pointer by
the symbol's real length — branch-free, at the cost of writing up to 7 bytes it will
overwrite.

```
// ILLUSTRATION — not quoted from cwida/fsst; this is FSST §3.1 in Rust-shaped
// pseudocode. The real decoder is fsst.h `fsst_decompress` in cwida/fsst, and the
// production integration this repo pins is duckdb/duckdb@6c0c1a68
// src/storage/compression/fsst.cpp:470 (`duckdb_fsst_decompress`).
let mut out = 0;
for &code in codes {
    if code == 255 {              // escape: next input byte is a literal
        out_buf[out] = next_literal_byte();
        out += 1;
    } else {
        // unconditional 8-byte store, then advance by the symbol's true length
        out_buf[out..out + 8].copy_from_slice(&symbols[code as usize]);
        out += lengths[code as usize] as usize;
    }
}
```

Two numbers worth holding onto. The symbol table's worst-case serialised size is
`8 × 255 + 255` = **2295 bytes** (§3.4: 8 bytes per symbol plus one length byte each);
typical tables are a few hundred bytes because the average symbol length is about 2
(§3.4). And a table is per-block, so it is amortised over the whole block — DuckDB's
integration sizes this explicitly at `src/storage/compression/fsst.cpp:198-199`, dividing
the estimated payload by the block size to count how many symbol tables it will pay for.

### Step 3 — "Static" is the whole trick

> **In:** the symbol table from Step 2.
> **Out:** the three capabilities that follow from it never changing, and their limits.

The table is built once per block and then **immutable** — it never adapts while
compressing, unlike LZ4's implicit sliding window. Three things fall out.

**Random access.** Decoding value *i* needs only value *i*'s codes and the shared table.
No state carries over between values, so `fetch_row(i)` is `O(len(i))`, not
`O(bytes before i)`.

**Selectivity-proportional work.** FSST §6.2 (Figure 5) measures this directly: FSST's
output rate is unaffected by how selective the query is, while block-LZ4 must decompress
an entire block regardless of how few rows survive the predicate. A 1-in-10,000 lookup on
LZ4 blocks of 64 K values does 64,000 values' worth of decode work; FSST does one.

**Comparison on compressed data.** Because the mapping from string to codes is
deterministic given a table, two strings compressed with the *same* table are equal iff
their code sequences are equal (§3.4). So an equality predicate can compress the constant
once and compare bytes — no decompression at all. This is the payoff for **late
materialization** (deferring the conversion back to user-visible values until after the
filters have run).

The limits are stated just as plainly, and they are what the exercises should check:

- The equality trick holds only "as long as both operands are compressed with the same
  symbol table" (§3.4). Two blocks trained separately have different tables, so
  cross-block equality needs decompression — this is exactly what bit the paper's own
  TPC-H join experiment, where "the two join predicate columns use different dictionaries"
  and had to be decompressed (§6.6).
- Range comparisons, `LIKE`, and sorting are *not* supported on compressed form; §3.4
  leaves automata-based `LIKE` to future work.

### Step 4 — Training the table: gain, iterations, sampling

> **In:** a raw corpus (or a sample of it).
> **Out:** a 255-symbol table, and the cost of producing it.

Choosing the best 255 symbols is circular: a symbol's worth depends on which *other*
symbols exist, because a longer symbol steals occurrences from its own prefixes (§4.1).
FSST sidesteps this by measuring worth empirically (§4.2):

1. Start with an **empty** table. Compressing with it escapes every byte, so the first
   pass produces output exactly **twice** the input size (§4.3) — this is also the
   algorithm's worst case, and the honest answer to "what if my data is incompressible?"
2. Compress the corpus with the current table, counting how often each *code* occurs and
   how often each *pair* of successive codes occurs.
3. Build the next generation from the top 255 candidates by **apparent gain**
   = `frequency × length`, where candidates are the surviving symbols, all concatenations
   of observed pairs, and every single byte plus single-byte extensions (§4.2).
4. Repeat. At least 3 iterations are needed to reach the 8-byte maximum symbol length,
   and **5 iterations** converge in practice (§4.4).

Sampling makes this cheap: the shipped utility trains on a **16 KB sample per 4 MB
chunk**, growing the sample from 6% to 100% of that sample linearly across the 5
iterations (§4.4). The reasoning is a nice piece of statistics-free intuition — a symbol
frequent in the whole corpus is very unlikely to be absent from the sample.

The escape code earns its keep here too: because unseen bytes are always representable,
a table trained on a sample is *valid* for data it never saw, which is what makes sampling
sound in the first place (§3.2).

### Step 5 — What FSST actually buys, measured

> **In:** the mechanism from Steps 2–4.
> **Out:** the numbers, and the two places the popular summary of this paper is wrong.

Setup for everything below (§6): the "dbtext" corpus of 23 real string columns, 8 MB per
file, Intel i9-7900X (10 cores, 3.3 GHz), 32 GB RAM, LZ4 1.8.1, g++ 8.3.1 `-O3
-march=native`, single-threaded.

**Table 1** is the headline:

| | LZ4 | FSST |
| --- | --- | --- |
| compression factor, average over 23 columns | **1.70×** | **2.28×** |
| compression speed, average | 608 MB/s | 977 MB/s |
| decompression speed, average | 1857 MB/s | 1942 MB/s |

Per-column, FSST's factor ranges from **1.63×** (`yago`, a column of Wikipedia entity
names) to **3.84×** (`c_name`, TPC-H customer names). LZ4's range on the same columns is
1.14× to 3.08×.

Two corrections to the way this paper is usually summarised, both from §6.1:

- **"FSST gets LZ4-class ratios" understates it.** FSST is **34% better** on average
  (2.28 / 1.70 = 1.34). It is not a tie; it wins.
- **"FSST decompresses faster than LZ4" is not what was measured.** The paper's own
  wording: "FSST is faster on some data sets and LZ4 is on others – with the average being
  almost identical" (1942 vs 1857 MB/s is 4.6%, inside the noise of a column-to-column
  swing). The measured wins are **34% on ratio** and **60% on compression speed**. The
  decompression story is *equal throughput plus random access* — which is the better claim
  anyway, because random access is the thing LZ4 cannot do at any speed.

Where FSST loses, also measured, also worth stating (§6.3, Silesia corpus): FSST is about
**10% better than LZ4 on text files** but **25% worse on binaries**, and on large XML/JSON
files its factor is **2–2.5× worse** than LZ4's. The premise FSST needs is *many short
strings with shared substrings*. Give it one huge document and the block compressor's
long-range matching wins.

End-to-end, in the Umbra prototype on TPC-H SF10 with 20 threads (§6.6, Table 4): the
string pool is 4.1 GB uncompressed, 1.5 GB with LZ4, **0.69 GB with FSST**; Q19 (which
filters heavily on string columns) gets **30% faster** (99 ms → 69 ms) because compression
saves scan bandwidth *and* lets the filter push down; Q13's `LIKE` on `o_comment`, which
must decompress, slows by only **3%** (228 ms → 235 ms).

### Step 6 — BtrBlocks, part 1: encoder output is just another column

> **In:** a 64,000-value block and a pool of encoding schemes.
> **Out:** a cascade of schemes, and the rule that stops it.

BtrBlocks splits each column into fixed-size blocks of **64,000 values** (§2.2) and
compresses each block independently, so the scheme can follow a changing data
distribution. Its pool is seven existing schemes plus one new one (§1): **RLE**
(run-length encoding — store `(value, run length)` instead of a repeated value),
**One Value** (the degenerate case: a whole block of one value), **Dictionary** (replace
each distinct value with a small integer code into a lookup table), **Frequency**
(BtrBlocks' variant stores the single dominant value, a bitmap of where it occurs, and the
exceptions), **FOR** (frame of reference — subtract a base so the residuals are small) with
**bit-packing** (store each residual in exactly as many bits as the widest one needs),
SIMD-FastPFOR / SIMD-FastBP128 for patched, SIMD-friendly versions of the same, **FSST**
for strings, **Roaring bitmaps** for NULLs and exception positions, and the paper's new
**Pseudodecimal** encoding for doubles.

The structural insight is that most of these emit *more columns*. RLE on
`[3.5, 3.5, 18, 18, 3.5, 3.5]` produces a value array `[3.5, 18, 3.5]` and a run-length
array `[2, 2, 2]` (§3.2). Both are columns. Both can be compressed again — the run-length
array by One Value, the value array by Dictionary, and the resulting code array by
FastBP128. That is **cascading compression**, and BtrBlocks applies it recursively with a
default **maximum depth of 3**; when the depth is exhausted, the remaining data is stored
**uncompressed** (§3.2).

That last clause is important and easy to skim past: the cascade terminates in raw bytes,
*not* in zstd. Step 8 is about why.

### Step 7 — BtrBlocks, part 2: choose by sampling, not by trying everything

> **In:** a 64,000-value block and the scheme pool from Step 6.
> **Out:** one chosen scheme per cascade level, at 1.2% of compression CPU.

Picking the best cascade exactly would mean compressing the block with every scheme and
every combination — exponential in the cascade depth (§3). BtrBlocks instead runs, at each
recursion level (§3):

1. Collect statistics in one pass: min, max, unique count, average run length.
2. Filter non-viable schemes by heuristic — exclude RLE if average run length < 2, exclude
   Frequency if ≥ 50% of values are unique (§3.1).
3. Compress a **sample** with each surviving scheme and keep the best observed ratio.
4. Compress the whole block with the winner.
5. If the output is itself compressible, recurse from step 1.

The sample's *shape* matters as much as its size. Random individual tuples destroy runs,
so RLE looks useless; a single contiguous range is badly biased. BtrBlocks takes
**10 runs of 64 values** from random positions in non-overlapping parts of the block —
640 values, **1% of 64,000** (§3.1, Figure 2). §6.3 scores strategies by how often they
pick the optimal scheme (or one within 2% of it) and finds that "sampling multiple small
chunks across the entire block improves accuracy compared to other strategies, though
there is little difference between strategies that choose chunks of ≥ 16 tuples".

The measured cost/benefit of that choice (§6.3): scheme selection consumes **1.2%** of
compression CPU time, picks the correct scheme **77%** of the time, and the resulting
files are only **3.3% larger** than the best cascade achievable by exhaustive search.
Paying 1.2% to get within 3.3% of optimal is the trade the whole design rests on.

### Step 8 — BtrBlocks, part 3: no block compressor on top, and why

> **In:** a fully cascaded block.
> **Out:** the bet BtrBlocks is making, and the two different "GB/s" it forces you to
> distinguish.

Parquet and ORC lean on a general-purpose compressor — Snappy or zstd — layered over their
encodings (§1). BtrBlocks does not: the cascade bottoms out uncompressed (§3.2). The
trade, measured on the Public BI Benchmark:

| | compression factor | decompression, vs BtrBlocks |
| --- | --- | --- |
| BtrBlocks | **7.06×** | 1.0× (baseline) |
| Parquet + Snappy | 6.88× | BtrBlocks is **3.6×** faster |
| Parquet + Zstd | **8.24×** | BtrBlocks is **3.8×** faster |
| Parquet (encodings only) | — | BtrBlocks is **2.6×** faster |

*(Factors from §6.4; decompression speedups from §6.6, averaged over Public BI. On TPC-H
the speedups are 2.6× / 3.9× / 4.2× respectively, §6.6.)*

So BtrBlocks gives up about **14%** of Parquet+Zstd's ratio (7.06 vs 8.24) to decompress
**3.8×** faster. Whether that is a good trade depends on a metric §6.7 defines carefully,
and this is the part of the paper to read twice:

- **T_u = uncompressed size / decompression time.** The rate at which *logical* data
  appears. This is what Figure 8 plots and what a data consumer feels.
- **T_c = compressed size / decompression time** — i.e. `T_u / compression factor`. The
  rate at which the decompressor can *consume bytes off the wire*.

The distinction decides the design. Every Parquet variant reaches over 50 GB/s of T_u,
which looks comfortably above the 12.5 GB/s of a 100 Gbit link — the paper calls that "a
false conclusion stemming from the definition of decompression throughput" (§6.7). What
must exceed the network rate is **T_c**, and Table 5 shows only BtrBlocks gets close:

| Format | T_u [GB/s] | T_c [Gbit/s] | scan cost [$] | normalized |
| --- | --- | --- | --- | --- |
| BtrBlocks | 174.6 | **86.2** | 0.97 | 1.00× |
| Parquet | 56.1 | 52.6 | 2.47 | 2.61× |
| Parquet + Snappy | 77.6 | 33.2 | 1.74 | 1.84× |
| Parquet + Zstd | 78.6 | **24.8** | 1.70 | 1.77× |

The S3 client saturates at 91 Gbit/s on uncompressed data, so BtrBlocks' 86.2 Gbit/s uses
**95%** of the available link while Parquet+Zstd's 24.8 Gbit/s uses **27%** — the CPU, not
the network, is the bottleneck for zstd, and you pay for the idle network in instance
hours. Note the units differ between the columns: dividing T_u by T_c in the same units
recovers the aggregate compression factor those five workbooks achieved, e.g.
174.6 ÷ (86.2 / 8) = **16.2×** for BtrBlocks and 78.6 ÷ (24.8 / 8) = **25.4×** for
Parquet+Zstd — which is exactly the point, zstd compresses harder and still costs more.

One more control from §6.8, because it is the obvious objection: is BtrBlocks fast only
because it uses SIMD? They reimplemented every decompression routine in scalar form. That
slowed in-memory decompression by **17%** — and the scalar version was still **2.3×**
faster than the fastest Parquet variant. The win is the format, not the intrinsics.

---

## How to read the papers

Read **FSST** in this order:

1. §3 (the format) and §3.1 (decompression). Ten minutes, and it is the whole idea.
2. §3.4 "Useful Properties" — the shortest, highest-value section in the paper. Every
   claim about pushing predicates into compressed data traces back to it.
3. §4.2's Figure 2 — four iterations on the toy corpus `tumcwitumvldb`, with the symbol
   table after each. Work through it by hand; it is the fastest way to internalise
   "apparent gain".
4. §6.1's Table 1, then §6.3. Skip §5 (AVX512 encoding) on a first pass unless SIMD is why
   you came.

Read **BtrBlocks** in this order:

1. §2.2 (the scheme pool) and Figure 3 (the per-type decision trees).
2. §3, §3.1, §3.2 — selection, sampling, cascading. Listing 1's `pickScheme` is 10 lines
   and is the entire selection algorithm.
3. §6.7 — the T_u / T_c argument. Read it even if you skip the rest of the evaluation.
4. §4 (Pseudodecimal) only if you care about floats; it is self-contained.

For the code, this repo pins **duckdb/duckdb@6c0c1a68**, which contains a production FSST
integration you can read against the paper:

| Idea from the papers | Where to look |
| --- | --- |
| Train a symbol table on a sample | `src/storage/compression/fsst.cpp:170` (`duckdb_fsst_create`) — the sample rate is `ANALYSIS_SAMPLE_SIZE = 0.25` at `:38`, applied at `:119` |
| Estimate the compressed size before committing | `src/storage/compression/fsst.cpp:149-203` (`StringFinalAnalyze`) |
| Refuse the scheme unless it clearly wins | `src/storage/compression/fsst.cpp:37` — `MINIMUM_COMPRESSION_RATIO = 1.2`, applied to the returned score at `:202` |
| Pay for one symbol table per block | `src/storage/compression/fsst.cpp:198-199` |
| Decode | `src/storage/compression/fsst.cpp:470` (`duckdb_fsst_decompress`) |
| FSST layered on a dictionary (BtrBlocks' `Dict+FSST`) | `src/storage/compression/dict_fsst/compression.cpp` |

Confirm the pin before you read: `python3 tools/pinned-source.py ref duckdb`.

---

## Work the numbers yourself

Do this before reading §6 of either paper. It takes five minutes and makes the results
legible.

**The escape's worst case.** FSST codes are 1 byte, and an escape is 2 bytes (escape +
literal). So for input where *nothing* matches a symbol, the output is 2 bytes per input
byte: a compression factor of **0.5×**, i.e. 2× inflation. §4.3 confirms this from the
other direction — the first training iteration uses the empty table and "the result will
be twice the input size". Compare with Parquet's RLE-hybrid worst case of about
1.0055× overhead (see `reading-arrow-parquet.md`, which works the same arithmetic on a
concrete column): FSST bets much more aggressively, and its safety net is *selection* —
DuckDB simply refuses to use FSST unless the sample shows at least a 1.2× win
(`fsst.cpp:37`).

**The symbol table's fixed cost.** Serialised worst case `8 × 255 + 255` = **2295 bytes**
(§3.4); in-memory decode state `256 × 8 + 256` = **2304 bytes** (§3.1). On a 64 KB block
that is a 3.5% overhead; on a 4 MB chunk, 0.05%. This is why block size and symbol table
size are the same conversation.

**The same concrete column as the rest of this topic.** 1,000,000 values, 200 distinct,
average run 8. As *strings* averaging 12 bytes, the raw column is
`1,000,000 × 12` = **12,000,000 bytes**. Three ways to shrink it:

| Scheme | Arithmetic | Bytes | Factor |
| --- | --- | --- | --- |
| Dictionary alone | `1,000,000 × 1 B` codes + `200 × 12 B` dict = 1,000,000 + 2,400 | 1,002,400 | **11.97×** |
| Dictionary → RLE on the codes | `125,000 runs × 2 B` + 2,400 | 252,400 | **47.54×** |
| FSST on the raw strings (at the paper's 2.28× average) | `12,000,000 / 2.28` | 5,263,158 | **2.28×** |

The code width is `ceil(log2(200))` = **8 bits** = 1 byte exactly (2⁷ = 128 < 200 ≤ 256 =
2⁸), so the dictionary codes need no bit-packing at all here.

Now read the table again, because it explains BtrBlocks' Table 4 better than any prose. On
a *low-cardinality* column, dictionary encoding beats FSST by 5×, and dictionary→RLE beats
it by 20×. FSST is not a competitor to dictionary encoding; it is what you reach for when
the dictionary itself is huge — which is precisely why BtrBlocks' most common string
scheme in Table 4 is `Dict+FSST`: dictionary-encode the column, then FSST the *dictionary*.
Read Table 4's `NYC/Community Board` row (`Dict+FSST`, ratio 8.0×, 15.0 GB/s) next to
`Motos/Medio` (`OneValue`, ratio 5048.8×, 30.8 GB/s) and the point lands: scheme selection
is worth orders of magnitude, and no single scheme is the answer.

**Which GB/s?** FINDINGS row 12 records this topic's measured scan floor of **24–57 GB/s
on a machine with ~150 GB/s of memory bandwidth**. That figure is *logical* bytes per
second: the lane folds 800 MB of decoded `u64`s. Compress the column 7.96× and the same
scan reads only ~100 MB of real traffic, so the same 57 GB/s of logical rate corresponds to
about 7.2 GB/s of memory traffic — comfortably under the bus. Quote the number the other
way round and you get an "effective bandwidth" of 454 GB/s, three times what the hardware
can deliver. Both numbers are arithmetically correct; only one of them is a memory rate.
This is the same ambiguity BtrBlocks names as T_u versus T_c (§6.7), and it is the same
class of error as this topic's own famous bug — FINDINGS row 12 also records the
**19,047,619 GB/s** a hoisted timing loop once printed here, which is about **127,000×**
the machine's peak and therefore impossible on its face. When you write down a bandwidth,
write down which bytes you counted.

---

## Questions for notes.md

1. FSST versus dictionary encoding on (a) 1 M distinct URLs sharing 20 prefixes,
   (b) country codes with 200 distinct values, (c) UUIDs. Pick the winner per case and say
   why — the arithmetic table above gives you (b) directly, and FSST Table 1's `urls` row
   (FSST 2.16×, LZ4 2.77×) is the calibration for (a). Which cascade would BtrBlocks build
   for (a), and which for (c)?
2. Why must FSST's table be *static* — immutable after training — for random access and
   vectorized decode? Name what an adaptive, LZ78-style code would break, using the
   argument in §3 ("precludes cheap point access") and the branch-free decode loop of
   §3.1.
3. BtrBlocks samples (§3.1: 640 values, 1.2% of CPU, 77% correct), DuckDB analyses a
   fraction of everything (`fsst.cpp:38`, `:119`), and ClickHouse makes you declare the
   codec in DDL. Place the three on an ingest-cost / ratio-quality / operator-burden
   triangle, and say which one you would want on a column whose distribution changes
   monthly.
4. The escape byte: what is FSST's worst-case inflation on incompressible input, and where
   does §4.3 confirm it? Compare with the Parquet RLE-hybrid's worst case from
   `reading-arrow-parquet.md`. Then say which mechanism — not which scheme — protects a
   production system from each.
5. **M12**: property values in FalkorDB are often short, similar strings (emails, category
   names). Sketch the cascade for a string property column and mark which stages allow
   predicate-on-encoded execution. `= 'x'` on dictionary codes is easy; on FSST codes it is
   trickier because codes have unequal lengths — yet §3.4 says equality *can* compare
   encoded bytes. Under exactly what condition, and what does §6.6's join experiment show
   happens when that condition fails?

---

## Takeaway

Both papers make the same move: they give up some compression ratio to keep the decoder's
state small, fixed and independent per value. FSST gives up the sliding window and gets
random access, predicate evaluation on compressed bytes, and — measured, not asserted —
34% better ratios than LZ4 at roughly equal decompression speed. BtrBlocks gives up zstd
and gets 3.8× faster decompression for 14% worse ratios, which turns out to be the better
end of the trade once you measure throughput in the units the network actually charges
you for. The general lesson for this topic: a compression scheme's *access pattern* is
usually worth more than its ratio, and the only way to know is to be specific about which
bytes you are counting per second.

---

## Done when

Answer each before unfolding it.

- [ ] An FSST-compressed column and an LZ4-compressed column have the same size on disk.
      Name two things you can do with the FSST one that you cannot do with the LZ4 one.

<details><summary>Answer</summary>

**Fetch a single value** without decompressing its neighbours: FSST decoding is a lookup
in an immutable table, so value *i* is independent of values 0…*i*−1, while LZ4's
back-references make decoding sequential — "which precludes cheap point access" (FSST §3).
Figure 5 (§6.2) measures the consequence: FSST's output rate is independent of query
selectivity, block-LZ4's is not.

**Evaluate an equality predicate without decompressing at all**: compress the constant
with the same symbol table and compare code sequences (§3.4). §6.6 uses exactly this in
Umbra, which is why Q19 gets *faster* (99 ms → 69 ms) when the column is compressed.

The caveat on the second one: it requires both operands to share a symbol table (§3.4).
Across blocks with independently trained tables, you are back to decompressing — which is
what happened in the §6.6 join experiment.
</details>

- [ ] Compress 8 MB of strings that share no substrings whatsoever. What does FSST produce,
      and what stops a real system from shipping that result?

<details><summary>Answer</summary>

Every byte escapes, and an escape is 2 bytes (escape marker + literal), so the output is
**16 MB — a compression factor of 0.5×**, i.e. 2× inflation. FSST §4.3 states the same
thing from the training side: the first iteration compresses with the empty table, and
"the result will be twice the input size".

What stops it shipping is *scheme selection*, not the scheme. DuckDB analyses a 25% sample
(`src/storage/compression/fsst.cpp:38`, applied at `:119`), estimates the compressed size
(`:149-203`), and multiplies the estimate by `MINIMUM_COMPRESSION_RATIO = 1.2` (`:37`,
applied at `:202`) so FSST only wins the comparison when it is clearly better. BtrBlocks
does the equivalent by testing every viable scheme on a 640-value sample and keeping the
best observed ratio (§3.1).
</details>

- [ ] Where does the popular one-line summary "FSST is like LZ4 but with random access"
      get the measurements wrong?

<details><summary>Answer</summary>

In two places, both from FSST §6.1 / Table 1.

It **understates the ratio**: FSST averages **2.28×** against LZ4's **1.70×** over the 23
dbtext columns — 34% better, not equal.

It **overstates decompression**: the averages are 1942 MB/s (FSST) vs 1857 MB/s (LZ4), and
the paper's own summary is "FSST is faster on some data sets and LZ4 is on others – with
the average being almost identical". The clean measured wins are 34% on ratio and 60% on
compression speed.

It also hides the failure case (§6.3): on the Silesia binaries FSST is 25% *worse* than
LZ4, and on large XML/JSON files 2–2.5× worse. FSST's premise is many short strings with
shared substrings.
</details>

- [ ] BtrBlocks spends 1.2% of its compression time deciding which scheme to use. Why is
      that a bargain, and what is the measured cost of the shortcut?

<details><summary>Answer</summary>

The alternative is exhaustive search: compressing each block with every scheme *and* every
cascade combination, which §3 calls "prohibitively slow" and which grows exponentially in
the cascade depth. The bargain is measured in §6.3 — sampling 10 runs of 64 values (640
values, 1% of a 64,000-value block) costs **1.2%** of compression CPU, picks the optimal
scheme (or one within 2% of it) **77%** of the time, and yields files only **3.3% larger**
than the best achievable cascade.

The shortcut's cost is that 3.3%, plus the risk of a mis-estimate on data whose local
structure differs from the sample — which is why the sample is 10 *runs* rather than 640
scattered values: a run-length estimate needs locality to be meaningful (§3.1, Figure 2).
</details>

- [ ] Parquet+Zstd compresses better than BtrBlocks (8.24× vs 7.06×) yet costs 1.77× more
      to scan from S3. Explain, using the paper's own two throughput metrics.

<details><summary>Answer</summary>

Because compression ratio and *scan cost* are connected through the metric §6.7 calls
**T_c = compressed size / decompression time = T_u / compression factor**, not through the
ratio alone.

Parquet+Zstd's T_u of 78.6 GB/s looks far above a 100 Gbit link's 12.5 GB/s, but that
counts *uncompressed* bytes. Per byte arriving off the wire it manages only **24.8
Gbit/s** (Table 5), against a client that can pull **91 Gbit/s** — so the link sits ~73%
idle while the CPU works, and you rent the instance for the whole time. BtrBlocks reaches
**86.2 Gbit/s** of T_c, ~95% of the link, and finishes the scan for **$0.97 against
$1.70** (Table 5).

The general form: when data arrives over a channel, the decompressor must keep up
*measured in channel bytes*. Better ratios make that harder, not easier, because each
channel byte expands into more work.
</details>

---

## References

- Peter Boncz, Thomas Neumann, Viktor Leis. *FSST: Fast Random Access String Compression*.
  PVLDB 13(11): 2649–2661, 2020. <https://www.vldb.org/pvldb/vol13/p2649-boncz.pdf>
- Maximilian Kuschewski, David Sauerwein, Adnan Alhomssi, Viktor Leis. *BtrBlocks:
  Efficient Columnar Compression for Data Lakes*. SIGMOD 2023.
  <https://doi.org/10.1145/3589263>
- FSST reference implementation: <https://github.com/cwida/fsst>
- BtrBlocks implementation: <https://github.com/maxi-k/btrblocks>
- DuckDB's FSST integration, pinned at `duckdb/duckdb@6c0c1a68`:
  `src/storage/compression/fsst.cpp`, `src/storage/compression/dict_fsst/`
- `FINDINGS.md` row 12 — this topic's measured scan floor (24–57 GB/s on a ~150 GB/s
  machine) and the 19,047,619 GB/s hoisted-loop bug.
- `reading-arrow-parquet.md` in this topic — the Parquet RLE-hybrid worst case that the
  escape's 2× inflation is compared against.
