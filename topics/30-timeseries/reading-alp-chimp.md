# ALP and Chimp: two bets on what a float column is

Gorilla ([reading-gorilla.md](reading-gorilla.md)) compresses a double by
predicting its *bit pattern* from the previous one. Two later codecs make the
two opposite bets about where the redundancy in a float column lives. **Chimp**
(Liakos, Papakonstantinopoulou, Kotidis, *Chimp: Efficient Lossless Floating
Point Compression for Time Series Databases*, PVLDB 15(11), 2022) keeps
Gorilla's XOR and re-tunes its control bits to what real series look like.
**ALP** (Afroozeh, Kuffó, Boncz, *ALP: Adaptive Lossless floating-Point
Compression*, SIGMOD 2024, [doi](https://doi.org/10.1145/3626717), code at
[cwida/ALP](https://github.com/cwida/ALP)) drops XOR entirely: most doubles in
a database *started life as a decimal* — a price, a temperature, a CSV cell —
so it recovers that decimal, stores an integer, and hands the integers to
frame-of-reference and bit-packing, the same lightweight encodings topic 12
uses on integer columns. This chapter builds both from zero, reads them
against the code, and then measures the bet itself: the same million values
through a byte-aligned XOR codec, Gorilla, and an ALP-style codec on
decimal-origin, arithmetic-origin and truly random doubles (`alp.rs`,
`tsdb_bench` lane F).

It lives in topic 30 rather than topic 12 because its measurement needs this
topic's Gorilla stub and generators, and because the comparison *is* the
"XOR family vs. decimal family" fork a time-series engine has to choose at; topic
12's [reading-duckdb-compression.md](../12-columnar-analytics/reading-duckdb-compression.md)
covers the framework ALP plugs into.

Versions read: the Chimp paper (PVLDB 15, 2022, §3–§5), the ALP paper as posted
by CWI (SIGMOD 2024, §2–§4), **cwida/ALP at `31ca0ed`**, and **DuckDB at
`d54cbbe3df`** (the pin in `resources/codebases.md`). Every `file:line` below
was re-read at those revisions with `tools/pinned-source.py`
(`--ref 31ca0ed11c93c99d3f5b5c30e01a3e1c3832d3ce` for ALP, which is not in the
pin table).

## The problem in one sentence

A double is 64 bits whose *numeric* neighbourhood (12.34 and 12.35) is a
*bitwise* stranger (`0x4028ae147ae147ae` vs `0x4028b33333333333`), so
integer tricks (delta, frame-of-reference) fail on raw doubles and the cheap
float trick (XOR) still pays about 45 bits for a 0.01 change — and the open
question is whether to compress the bits as they are or the decimal they came
from.

## The concepts, step by step

### Step 1 — why a float is not an integer, and what XOR keeps

> **In:** nothing — the IEEE 754 layout and one pair of values, 12.34 and 12.35.
> **Out:** the vocabulary (sign, exponent, fraction, XOR, leading and trailing
> zeros) and the measured fact that numerically close doubles XOR to ~45
> meaningful bits, which is the number both codecs are trying to beat.

An **IEEE 754 double** is 64 bits: 1 **sign** bit, 11 **exponent** bits (the
power of two), and 52 **fraction** bits (also called the mantissa or
significand: the digits after the leading 1). The value is
`(−1)^sign × 2^(exponent−1023) × (1 + fraction/2^52)` (ALP §2.1). Two
consequences drive everything below. First, **doubles do not hold most decimal
fractions exactly**: 8.0605 is stored as
`8.0604999999999993320898283855058252811431884765625` (ALP §2.5 quotes the same
digits, which we re-derived in Python). Second, **adding two doubles rounds**,
so *delta* (store `v[i] − v[i−1]`) and *frame of reference* (**FOR**: store
`v[i] − min`, so the numbers shrink and need fewer bits) corrupt the value
unless the arithmetic is exact — which is why neither is applied to raw doubles
(ALP §1).

**XOR** (bitwise exclusive-or: 1 where two bits differ) is the workaround
every codec in the Gorilla family uses: `bits(v) ^ bits(prev)` is exactly
reversible (XOR again), never rounds, and is 0 wherever neighbours agree. Two
words describe the result: its **leading zeros** (how many high bits are 0 —
the shared sign, exponent and top of the fraction) and its **trailing zeros**
(how many low bits are 0). Only the span in between, the **meaningful bits**,
needs storing.

Worked on real values (computed from the bit patterns; the script is a four-line
`struct.pack`):

```
12.34 = 0x4028ae147ae147ae       12.35 = 0x4028b33333333333
XOR   = 0x00001d2749d2749d
        leading zeros 19, trailing zeros 0  ->  64 - 19 - 0 = 45 meaningful bits
12.34 vs 12.31:  XOR = 0x000030ac2b0ac2b1: leading 18, trailing 0 -> 46 bits
100.0 vs 101.0:  XOR = 0x0000400000000000: leading 17, trailing 46 -> 1 bit
12.34 vs 12.34:  XOR = 0
```

A change of 0.01 costs ~45 bits because 12.34 and 12.35 are *not* short in
binary — they are repeating fractions — so their low bits are unrelated. An
integer-valued pair (100.0, 101.0) costs one bit. That asymmetry is the whole
chapter: XOR rewards values whose *binary* expansion is short, and decimals are
short in base 10, not base 2.

### Step 2 — Chimp: Gorilla with the control bits re-tuned

> **In:** the XOR stream from Step 1, and Gorilla's three-way flag
> (`0` / `10` reuse window / `11` new window, reading-gorilla.md Step 4).
> **Out:** Chimp's four 2-bit flags, its coarse leading-zero code, the
> trailing-zero threshold, and the count — 58 bits vs 51 — for the Step 1 pair.

Chimp's authors measured how real series XOR (§3) and found three
places where Gorilla spends bits for nothing, then changed each.

1. **Identical values are not the common case, so give every case a 2-bit flag.**
   Gorilla gives "same as previous" the 1-bit flag `0`; Chimp found identical
   consecutive values not very frequent in float series (§4.1.1) and uses four
   2-bit flags instead, which buys cheaper codes for the cases that dominate.
2. **Round the leading-zero count to 8 buckets and store it in 3 bits, not 5.**
   The code stores one of `0, 8, 12, 16, 18, 20, 22, 24` (§4.1.2). The paper reports average gains of 0.51 bits per value for a 4-bit code
   at 2-bit granularity and 0.95 for the final 3-bit exponentially spaced code (§4.1.2). Rounding *down* costs the
   extra zeros back as payload (a lead of 19 is stored as 18 and one zero rides
   along), never correctness.
3. **Spend the 6-bit length only when trailing zeros make it pay.** Gorilla
   always writes a 6-bit "length of the meaningful bits" so it can drop trailing
   zeros. Chimp observed trailing zeros are usually few and sets a threshold of
   6: if the XOR has **6 or fewer** trailing zeros, write all bits after the
   leading zeros and skip the length field (§4.1.3); if it has **more**, write
   leading code, a 6-bit length and just the middle bits. A fourth refinement
   (§4.1.4): when the (rounded) leading count equals the previous value's, a
   single `0` replaces the 3-bit count.

The four cases of Chimp (§4.1.5, Figure 6), as the paper's Algorithm 2 writes
them:

| trailing zeros | other condition | bits written |
|---|---|---|
| > 6 | XOR = 0 | `00` |
| > 6 | XOR ≠ 0 | `01` + 3-bit leading code + 6-bit length + the middle bits |
| ≤ 6 | leading code = previous | `10` + all bits after the leading zeros |
| ≤ 6 | leading code ≠ previous | `11` + 3-bit leading code + all bits after the leading zeros |

**Worked on the Step 1 pair** (12.34 → 12.35: leading 19, trailing 0, 45
meaningful). Gorilla's new-window case writes `11` + 5-bit lead + 6-bit length
+ 45 bits = 2 + 5 + 6 + 45 = **58 bits**. Chimp: trailing 0 ≤ 6, lead 19 rounds
to 18, so it writes `11` + 3-bit code + (64 − 18) = 46 bits = 2 + 3 + 46 =
**51 bits** — the 8 bits of header it saved minus the 1 extra payload bit the
rounding costs. And when the previous lead was also 18, `10` + 46 = **48 bits**;
Gorilla's reuse case for the same value, with a window of lead 18 and trail 0,
is `10` + 46 = 48 bits, identical. So the gain is confined to the new-window
case — and Step 7 shows how rarely a Gorilla stream takes that branch once its
first window is set.

**Chimp128** adds the second bet: instead of XOR against the previous value only,
look back over the last 128 values and XOR against the one that yields the most
trailing zeros — a repeating value (a sensor oscillating between a few levels)
then costs almost nothing — at the price of a 7-bit index (§4.2). The reference
is found in O(1) with a 128-entry ring buffer and a 2^14-entry table keyed by
the low 14 bits of each value (§4.2). DuckDB's port is the code to read:

```cpp
// duckdb chimp128.hpp, the reference-selection prologue of CompressValue
 79   static constexpr uint8_t TRAILING_ZERO_THRESHOLD = SignificantBits<CHIMP_TYPE>::size + INDEX_BITS_SIZE;
        // ... 104-111: key = low bits of `in`; look the key up in the ring buffer ...
112   if (((int64_t)state.ring_buffer.Size() - (int64_t)reference_index) < (int64_t)ChimpConstants::BUFFER_SIZE) {
        // ... 113-118: reference value found, within the last 128 values ...
119       CHIMP_TYPE tempxor_result = (CHIMP_TYPE)in ^ reference_value;
120       trailing_zeros = CountZeros<CHIMP_TYPE>::Trailing(tempxor_result);
121       trailing_zeros_exceed_threshold = trailing_zeros > TRAILING_ZERO_THRESHOLD;
```

The line to focus on is 79: the threshold is `6 + 7 = 13`, not Chimp's 6 — a
reference 127 values back is only worth its 7-bit index if the XOR then has more
than 13 trailing zeros (6 bits of length + 7 bits of index must be recovered).
Line 112 is the "within the last 128" test; when it fails, or the threshold is
not exceeded, lines 126–127 fall back to the immediately previous value.

A fact about the code as pinned: **DuckDB can no longer write Chimp or Patas.**
`chimp_analyze.hpp:22-24` returns `nullptr` from `ChimpInitAnalyze` with the
comment "This compression type is deprecated" and lines 29 and 35 throw if the
analyze step is ever reached (`patas_analyze.hpp:22-24, 29, 35` is identical).
The functions are still registered (`compression_config.cpp:24-25`) so old files
read, but a new file's floating-point columns choose among ALP, ALPRD, and the
general-purpose methods (Step 5). The ALP paper is the reason: on its 30
datasets the average is 28.7 bits/value for Chimp128 and 35.5 for Patas against
21.7 for ALP (Table 4, "ALL AVG").

### Step 3 — ALP's bet: recover the decimal, store the integer

> **In:** the observation from Step 1 that 12.34 is a stranger to 12.35 in
> binary but a neighbour in decimal.
> **Out:** the two procedures `ALP_enc` and `ALP_dec`, why the naive version
> fails on 8.0605, and the exponent/factor pair `(e, f)` that fixes it —
> verified on the paper's own example.

Look at the data first. ALP's authors studied the 30 datasets the earlier
codecs were evaluated on and concluded that **28 of the 30 originate as
decimals** (§3.4: monetary values, coordinates, temperatures, discrete counts
stored as doubles), and that within one block of 1024 values the **decimal
precision barely varies**: in 25 of 30 datasets its standard deviation inside a
vector is below 1 (§2.4, Table 2 column 5). So a vector of values can share one
`(exponent, factor)` pair. That is the opportunity; the difficulty is making it
exactly lossless.

The naive version moves the decimal point right by `e` places:
`d = round(n × 10^e)` and decodes `n' = d × 10^−e`. For `n = 8.0605`, `e = 4`:

```
n × 10^4           = 80605.0            -> d = 80605           (encode is fine)
d × 10^-4          = 8.060500000000001108446667785756…         (decode)
n stored exactly   = 8.060499999999999332089828385505…
decode == n ?      NO   -> the naive scheme loses the exact bit pattern
```

(All digits computed with Python's `Decimal`; ALP §2.5 prints the same two
values, 8.06049999999999933209 and 8.0605000000000011084.) The error is not in
the encode — `10^e` is exact as a double for `e ≤ 21` — it is in the *decode*,
where `10^−4` is not exactly representable (it is
`0.0001000000000000000047921736…`) and its error lands in the result.

ALP's fix is a second power of ten, the **factor** `f`:

```
ALP_enc(n)  = round( n × 10^e × 10^-f )          (paper formula 1)
ALP_dec(d)  = d × 10^f × 10^-e                   (paper formula 2)
n   the double being encoded            d  the integer stored
e   exponent, how far the point moves   f  factor, how many trailing decimal zeros to cut back off
```

Why a factor: a *high* `e` (the authors found `e = 14` the single most
successful, Table 2 column 12, reaching ~95% of values on average) makes the
`10^−e` in the decode more accurate and covers a wider range of decimal
precisions, but produces enormous 64-bit integers. `f` then divides the
integer back down, so the stored `d` is small again. Run on the same 8.0605
with `e = 14, f = 10` (we ran it in double arithmetic; the digits match the paper):

```
n × 10^14        = 806049999999999.875          (exact value of the product, shown to 3 places)
× 10^-10         = 80604.999999999985448…      (stored as the double 80604.99999999999)
round            = d = 80605
d × 10^10 × 10^-14 = 8.0604999999999993320898…  == n   (bit for bit)  ✓
```

So the decimal point was moved 14 places and pulled back 10, net 4, but the
*route* through `(14, 10)` is exactly reversible where `(4, 0)` was not. We
confirmed both facts as a unit test in `alp.rs`
(`paper_example_8_0605_needs_e14_f10_or_any_working_pair`).

The code has this as two tiny functions. ALP's `encoder.hpp` and `decoder.hpp`:

```cpp
// encoder.hpp:82-89 — encode_value (SAFE = true path)
 82   static ST encode_value(const PT value, const factor_idx_t factor_idx, const exponent_idx_t exponent_idx) {
 83       PT tmp_encoded_value = value * Constants<PT>::EXP_ARR[exponent_idx] * Constants<PT>::FRAC_ARR[factor_idx];
 84       if constexpr (SAFE) {
 85           if (is_impossible_to_encode(tmp_encoded_value)) { return ENCODING_UPPER_LIMIT; }
 86       }
 87       tmp_encoded_value = tmp_encoded_value + Constants<PT>::MAGIC_NUMBER - Constants<PT>::MAGIC_NUMBER;
 88       return static_cast<ST>(tmp_encoded_value);
 89   }
```

```cpp
// decoder.hpp:128-131 — decode_value
128   static inline PT decode_value(const ST encoded_value, const uint8_t factor, const uint8_t exponent) {
129       const PT decoded_value = encoded_value * Constants<PT>::FACT_ARR[factor] * Constants<PT>::FRAC_ARR[exponent];
130       return decoded_value;
131   }
```

The line to focus on is 87: **the rounding is an add and a subtract**. The magic
number is `2^51 + 2^52` (`constants.hpp:70`, `0x0018000000000000`); adding it
pushes the double into the range where the fraction bits have run out, so the
hardware rounds to the nearest integer as a side effect, and subtracting it
restores the scale (ALP §3.1, "fast_double_round"). Two floating-point adds
instead of a `round()` call, which has no SIMD instruction. Note what the code
does *not* do: `FACT_ARR` (`10^f`) is an integer table, `FRAC_ARR` (`10^−e`) a
double table (`constants.hpp:85-107`), and the exponent range is **0–18**
(`constants.hpp:73`, `MAX_EXPONENT = 18`) — not the 21 the paper's analysis
uses (DuckDB's header comments the cap as "10^18 is the maximum int64",
`alp_constants.hpp:96`).

DuckDB's copy of the same two functions is `alp.hpp:134-150`. Its decode casts
the integer to the float type first ("to prevent a signed integer overflow",
line 145) where ALP's relies on promotion, and its `NumberToInt64` (lines
120–129) checks `IsImpossibleToEncode` a *second* time after the magic-number
round, where ALP checks once before.

### Step 4 — exceptions, then frame of reference and bit-packing

> **In:** the per-value `(e, f)` pair from Step 3 and the round-trip test
> `decode(encode(n)) == n`.
> **Out:** the exception mechanism (the values that fail the test), the
> frame-of-reference + bit-width step, and the byte-exact cost of a vector,
> worked on five values.

Choosing one `(e, f)` for 1024 values means some values will not survive the
round trip: a NaN, an infinity, `-0.0`, a value with more digits than the rest
of its vector, a value too large to be an int64. ALP stores each such value
raw as an **exception**, with its position, and patches it back in after
decoding. For the encoded-integer array, exceptions must not stretch the next
step, so ALP overwrites each exception's slot with the **first non-exception
integer** (ALP Algorithm 1, lines 15–25; `encoder.hpp:382-397`):

```cpp
// encoder.hpp:382-397 — placeholder substitution
382   ST a_non_exception_value = 0;
383   for (size_t i {0}; i < config::VECTOR_SIZE; i++) {
384       if (i != TMP_INDEX_ARR[i]) {
385           a_non_exception_value = encoded_integers[i];
386           break;
387       }
388   }
        // ... 389: blank ...
390   for (size_t j {0}; j < exceptions_idx; j++) {
391       size_t     i                                   = TMP_INDEX_ARR[j];
392       const auto actual_value                        = input_vector[i];
393       encoded_integers[i]                            = a_non_exception_value;
394       exceptions[current_exceptions_count]           = actual_value;
395       exceptions_positions[current_exceptions_count] = i;
396       current_exceptions_count                       = current_exceptions_count + 1;
```

Line 384 is a small trick: `TMP_INDEX_ARR[i]` is the list of exception
positions compacted to the front, so the first `i` where `i != TMP_INDEX_ARR[i]`
is the first index that is *not* an exception. The find-exceptions loop that
fills it (`encoder.hpp:373-379`) is **branch-free**: it writes the index
unconditionally and advances the write cursor by `is_exception` (0 or 1) — the
predication pattern from topic 17, which lets the compiler vectorize it.

With exceptions replaced, the integers are encoded by **frame of reference +
bit-packing**: subtract the vector's minimum (`base`, the *frame of reference*),
and store the differences in `w = ceil(log2(max − min + 1))` bits each — the
**bit width** — packed back to back. DuckDB does this at `alp.hpp:353-381`
(minimum/maximum at 354–360, subtract at 366–370, `MinimumBitWidth` at 372, packed
size at 373). ALP uses its FastLanes library to fuse the subtract into the pack
loop as one kernel (the paper's "FFOR"); **DuckDB does not** — `alp.hpp:353`
mentions FFOR only in a comment and the pack is `BitpackingPrimitives::PackBuffer`
(line 375), so DuckDB's ALP is "ALP encode, then DuckDB's own FOR + bitpack",
two passes.

**What one vector costs, exactly.** DuckDB's `RequiredSpace`
(`alp.hpp:93-100`) is the packed integers, plus 10 bytes per exception (an
8-byte raw double and a 2-byte position, `alp.hpp:95`), plus a fixed header:
exponent (1 B), factor (1 B), exception count (2 B), frame of reference (8 B),
bit width (1 B) — **13 bytes**, written at `alp_compress.hpp:126-140`. In symbols,
for `n` values at bit width `w` with `x` exceptions:

```
bytes(n, w, x) = ceil(n × w / 8) + 10 × x + 13
```

(DuckDB's `GetRequiredSize` first rounds the count up to the bit-packing
algorithm's group size, `bitpacking.hpp:100-103`; the formula above ignores
that, and `alp.rs` uses it as written.) Worked on a
five-value vector: 12.34, 12.35, 12.31, 12.40, 12.33 with `e = 2, f = 0`:

```
n × 100       = 1234.0  1235.0  1231.0  1240.0  1233.0   (each product is exactly an integer here)
d             = 1234    1235    1231    1240    1233     decode(d) == n for all five  -> x = 0
base          = min = 1231
d - base      = 3       4       0       9       2
max - min     = 9    -> w = ceil(log2(9 + 1)) = 4 bits   (9 = 0b1001)
packed        = 5 × 4 = 20 bits -> ceil(20/8) = 3 bytes   vs 5 × 8 = 40 bytes raw
bytes         = 3 + 10×0 + 13 = 16   (the 13-byte header dominates a 5-value vector;
                                       at n = 1024 it is 1.3% of the vector)
```

The same five values as XOR-with-previous in Step 1 cost 45–46 meaningful bits
each (the first pair we computed). That 20 bits versus ~225 is the entire
case for the decimal approach, *when the data is a decimal*.

And a break-even, because the codec must know when to give up. A full vector
(1024 values) stored raw is `1 + 8 × 1024 = 8193` bytes (a one-byte sentinel
plus the doubles; DuckDB's `uncompressed_size`, `alp_compress.hpp:91`). At bit
width 12 the compressed form is `128 × 12 + 13 = 1549` bytes before exceptions,
leaving `8193 − 1549 = 6644` bytes, so up to `⌊6644 / 10⌋ = 664` exceptions
(65% of the vector) before raw wins. At bit width 64 there is no room at all:
`13 + 8192 = 8205 > 8193`. DuckDB implements the decision as
`should_compress = compressed_size < uncompressed_size`
(`alp_compress.hpp:95-96`) and stores a raw vector under a sentinel exponent,
`UNCOMPRESSED_MODE_SENTINEL` (`alp_constants.hpp:37`, written at
`alp_compress.hpp:174-180`).

### Step 5 — choosing `(e, f)` cheaply: two-level sampling

> **In:** the cost formula from Step 4 and the 190 candidate pairs.
> **Out:** why a brute-force search per vector is too slow, how ALP samples twice
> (row group, then vector), and the three numbers (8, 32, 5) the code hard-codes.

For doubles `e` runs 0..18 and `f` 0..e, so there are `1 + 2 + … + 19 = 190`
candidate pairs (the paper counts 253 for `e ≤ 21`, §2.6). Trying all of them
on all 1024 values of every vector would make the encoder the bottleneck;
ALP's observation is that the *best pair* repeats: for most datasets the best
pair of every vector lies among the same 5 pairs (§2.6, Figure 3). So:

1. **First level, once per row group** (100 vectors = 102,400 values,
   `config.hpp:13-15`): sample 32 equidistant values from each of 8 equidistant
   vectors (`config.hpp:17, 21`), find the best pair for each of those 8
   samples by trying **all** pairs, and keep the `k = 5` pairs that won most
   often (`config.hpp:23`; ties broken toward the larger exponent, then the larger
   factor, `encoder.hpp:128-132`). The search loops are
   `encoder.hpp:159-160`; the per-pair cost estimate is the Step 4 formula
   applied to the sample, `encoder.hpp:184-188`; pairs with fewer than 2
   successful values are skipped (`encoder.hpp:182`).
2. **Second level, once per vector**: sample 32 values of the vector and try
   only those `k′ ≤ 5` pairs, **stopping early** after two consecutive
   candidates are no better than the incumbent (`encoder.hpp:289-296`, the
   threshold `SAMPLING_EARLY_EXIT_THRESHOLD = 2`, `constants.hpp:16`). If the
   first level kept a single pair, this level is skipped entirely
   (`encoder.hpp:409-415`).

The paper's numbers for what this buys: brute force over all pairs improves the
ratio by **less than 1%** over the two-level sample (§4.2), and the second level
costs about 6% of compression time when it runs; the first level runs on 256 of
every 102,400 values — `8 × 32 / (100 × 1024)` = **0.25%** of the data.

**DuckDB's version differs in three ways.** It samples per *segment* rather than
per 100-vector row group (`FindTopKCombinations` "is called once per segment",
`alp.hpp:212-215`), with `RG_SAMPLES = 8` vectors of `SAMPLES_PER_VECTOR = 32`
values (`alp_constants.hpp:29-30`; the jump between sampled vectors is derived
from DuckDB's 122,880-row row group, line 32 — the same 0.208% that
reading-duckdb-compression.md Step 4 computes). It ranks first-level ties by
estimated size before exponent (`alp.hpp:159-170`, the comparison has four
tiers where ALP's has three). And ALP's row-group-level decision to *abandon*
decimals (Step 6) is not a branch inside DuckDB's ALP at all: DuckDB registers
**ALP and ALPRD as two separate compression functions**
(`compression_config.cpp:26-27`) that the generic analyze → score → compress
machinery (topic 12 §2) pits against each other, and ALP's own analyze scores a
vector by `should_compress ? compressed_size : uncompressed_size`
(`alp_analyze.hpp:166-170`), so a poor ALP candidate simply loses.

### Step 6 — when the data is not a decimal: ALP_RD

> **In:** the failure case: a vector that is mostly exceptions.
> **Out:** the "cut the 64 bits in two" fallback, its constants, and the
> arithmetic for what it can and cannot save.

Two of the paper's 30 datasets (POI-lat and POI-lon, coordinates in radians
with up to 20 decimal digits) have no decimal to recover. When the first-level
sample's best estimate is no better than `48 bits × 32 samples`
(`RD_SIZE_THRESHOLD_LIMIT`, `constants.hpp:69`; `encoder.hpp:213-216`) ALP
switches the whole row group to **ALP_RD** ("real doubles", §3.4).

ALP_RD observes that even these doubles share their **front** bits — sign,
exponent, and the top few fraction bits — so it cuts each 64-bit pattern at a
position `p` into a *left* part (the top `64 − p` bits, at most 16) and a
*right* part (the low `p` bits). The right part is bit-packed at exactly `p`
bits. The left part is **dictionary-encoded** with at most 8 entries (3 bits),
and left parts missing from the dictionary become exceptions. The cut position
is chosen from `p ≥ 48` (`config.hpp:24`, `CUTTING_LIMIT = 16`;
`rd.hpp:94-100` tries `p = 63, 62, …, 48`), the dictionary size from
`config.hpp:25-26` (`MAX_RD_DICT_BIT_WIDTH = 3`).

```cpp
// rd.hpp:119-123 — the cut
119   for (size_t i {0}; i < config::VECTOR_SIZE; ++i) {
120       UT tmp         = in[i];
121       right_parts[i] = tmp & ((1ULL << stt.right_bit_width) - 1);
122       left_parts[i]  = (tmp >> stt.right_bit_width);
123   }
```

```cpp
// rd.hpp:165-169 — the glue (decode)
165   for (size_t i = 0; i < config::VECTOR_SIZE; i++) {
166       uint16_t left  = stt.left_parts_dict[left_parts[i]];
167       UT       right = right_parts[i];
168       out[i]         = (static_cast<UT>(left) << stt.right_bit_width) | right;
169   }
```

**What it can save, on paper:** with the smallest allowed cut `p = 48` and a
1–3 bit dictionary, a value costs `48 + 3 = 51` bits at best, about 6.4 bytes,
versus 8 raw: **at most ~20% off** (`1 − 51/64 = 0.203`). The paper's own
result for these two datasets is 55.5 and 56.4 bits per value (Table 4,
asterisked), 87–88% of raw — and the speed price is ~3× slower
compression and ~4× slower decompression than main ALP (§4.2). The limit is
the 52 incompressible fraction bits that no front-bit trick can touch.

DuckDB ships it as a separate function (`alprd.cpp`, `alprd/algorithm/alprd.hpp`):
cut loop `alprd.hpp:149-159`, dictionary at most `MAX_DICTIONARY_SIZE`
(lines 110–117), cut and pack `alprd.hpp:170-171, 196-204`, glue `alprd.hpp:236`.

### Step 7 — measuring the bet here (`tsdb_bench`, lane F)

> **In:** six 1,000,000-value series from `gen.rs` (seeded, so the byte counts
> reproduce exactly): a decimal-origin random walk at 2 and at 6 decimal places,
> an integer-valued counter, a constant, the raw random walk (arithmetic origin),
> and uniform random doubles; the three codecs of Steps 1, 2 and 4.
> **Out:** bytes per value for each pair, the exception rate, and where each
> family loses.

`cargo run --release --bin tsdb_bench`, lane F. Raw is 8.00 B/value. The XOR
codecs are the provided byte-aligned `xor_bytes_encode` (previous-value XOR,
leading zero *bytes* dropped, one 4-bit tag per value) and the Gorilla stub
(values only: its output minus the 1 bit/sample the perfectly regular
timestamps cost). The ALP column is `alp.rs`: one `(e, f)` per vector searched
over all 190 pairs on a 32-value sample (the single-level simplification the
module header states), exceptions patched, FOR + bit-pack, raw fallback per
vector, sizes counted with the formula of Step 4. **Gorilla's column is the
reference solution's** (the repo's stub prints `[stub]`; see `notes.md`).

```
            shape xor-bytes  gorilla    ALP |  exc%  bits raw-vecs modal e,f
      decimal 2dp      6.79     8.03   1.50 |  0.1%  11.8    0/977     14,12
      decimal 6dp      6.82     8.07   3.25 |  1.0%  25.1    0/977      14,8
          counter      6.07     3.01   1.95 |  0.3%  15.3    0/977     14,14
         constant      0.50     0.13   0.01 |  0.0%   0.0    0/977     18,18
 walk (f64 arith)      6.80     8.07   8.00 | 95.1%  57.9  926/977      16,0
   random doubles      7.48     7.37   8.00 | 100.0%  0.0  977/977         -
```

(`bits` is the mean bit width of the vectors that took the decimal path,
`raw-vecs` is how many of the 977 vectors fell back to raw doubles.) Read it as
five findings.

1. **The decimal family wins by 4–5× where the premise holds.** At two
   decimals, ALP is 1.50 B/value against 6.79 (byte-XOR) and 8.03 (Gorilla):
   the values are integers in a 1,024-value window about 2^11.8 wide, so 11.8
   bits plus 13 B / 1024 of header plus 0.1% exceptions. Check the arithmetic:
   `11.8 / 8 = 1.475`, `+ 13/1024 = 0.0127`, `+ 0.001 × 10 = 0.010` gives
   **1.50** ✓.
2. **Gorilla is *worse than raw* on decimal-origin values** (8.03 B vs 8.00),
   and worse than the byte-aligned XOR (6.79). 0.01 steps leave ~45 meaningful
   XOR bits (Step 1), so no codec can do well per value; the difference is in
   the *window*. Once Gorilla's first non-zero XOR opens a window
   (leading and trailing zero counts), every later XOR whose zeros nest inside
   it is written as `10` plus the window's full width — and the window never
   narrows by itself, so it stays as wide as the widest XOR that forced it.
   The reference solution's branch counts (notes.md) show this series opens a
   new window **10 times in 999,999 values**; the other 99.1% of values are
   written through the reuse branch, at 64.2 bits per value including the
   control bits. The byte-aligned codec re-decides per value (a tag nibble and
   whole bytes), so it drops the ~2 leading zero bytes each time and wins by
   1.2 B. This is the **negative result for the topic's headline codec**:
   Gorilla's premise is that neighbouring values share long runs of *binary*
   structure, and decimal fractions have none.
3. **A random walk computed in floating point is not a decimal.** The raw
   `gauge` shape has 95.1% exceptions and 926 of 977 vectors fall back to raw
   (8.00 B/value); every sample is the result of an accumulated `v + step`, so it
   has ~17 significant digits and no `(e, f)` reproduces it. The *same walk*
   rounded to two decimals compresses to 1.50. ALP's win is a property of how
   the data was *born*, not of how smooth it is — and a column of computed
   averages, sums or ratios looks like the walk, not like the rounded walk.
4. **Gorilla's one win is the integer-valued counter**: 3.01 B against ALP's
   1.95 and byte-XOR's 6.07. Integer doubles have ~40 trailing zero bits, which
   Gorilla's window drops and byte-XOR keeps as payload (byte-XOR sees only
   leading zeros). ALP turns the integers into a 15.3-bit frame of reference
   and still beats it.
5. **On true random doubles nothing helps, and the codecs disagree about how
   little.** Gorilla 7.37 (59 bits/value) and byte-XOR 7.48 are *below* raw only because
   successive samples share their sign and top exponent bits; ALP stores
   them raw (8.00), correctly, and is the only one that does not *pretend*. The
   saving left on the table is what ALP_RD recovers (Step 6, ≤ ~20%).

The two decimal rows also show a quirk worth knowing: the **modal pair is
`(14, 12)` for two-decimal values**, not `(2, 0)`. Both give the same integers
(effective shift `e − f = 2`), the cost ties, and the tie-break prefers the
larger exponent then the larger factor (`encoder.hpp:128-132`, and the strict `<`
in the loops of `alp.rs::search_ef`) because high exponents were the ones whose
decode is most accurate (Step 3). It is a reminder that `(e, f)` is a *search
result*, not a statement about the data.

**The speed claim, and why this repo cannot confirm it.** The paper's Table 5
(Intel Ice Lake, tuples per CPU cycle, averaged over its 30 datasets):
decompression ALP 2.609, Gorilla 0.047, Chimp 0.039, Chimp128 0.040
(ALP ≈ 55× Gorilla), compression ALP 0.487 vs Gorilla 0.052, Chimp 0.042,
Chimp128 0.040. The mechanism it gives: ALP's decode is a straight loop with no
data-dependent branches (a multiply, a multiply, a store, plus a patch loop for
the few exceptions), where Gorilla's decoder must decode each value's flag
before it knows where the next value starts. In `tsdb_bench` lane F prints decode throughput for both codecs
(Apple M3 Pro, one run on 2026-10-26, timings vary by ~20% between runs): on the
decimal-path rows ALP decodes at **590, 589 and 647 Mvalues/s** (decimal 2dp,
decimal 6dp, counter) against **164, 168 and 174** for the byte-aligned XOR —
**3.6×, 3.5× and 3.7×** (`590/164 = 3.6`). The last two rows (raw-fallback
vectors, 2614 and 2405 Mvalues/s) are a `memcpy` and say nothing about ALP's
decode. `alp.rs` unpacks with a generic scalar loop and does not fuse FOR into
the pack, so the figure shows the *shape* of the paper's claim — a decode with
no per-value branches beats one with them — at a small fraction of the paper's
absolute speed, and it is a measurement of this code, not of ALP.

### Step 8 — where this leaves a time-series engine

> **In:** Steps 1–7.
> **Out:** a decision rule for the value column of a TSDB or a columnar format,
> and the honest limits of the measurement.

- **If the column was born as decimals** (prices, sensor readings with a fixed
  display precision, anything that crossed a CSV, JSON or SQL `DECIMAL`), a
  decimal-to-integer codec wins by multiples and is *also* faster to decode.
- **If it is computed in floating point** (rates, ratios, sums of doubles, a
  random-walk simulator) it is not a decimal, ALP degenerates to raw or to ALP_RD
  (≤ ~20%), and XOR-family codecs get what the shared exponent gives them —
  about 0.5–1 B/value in Step 7.
- **If values repeat** (a sensor at a few levels), neither family is the
  right first tool: dictionary or RLE first, then ALP on the dictionary or the
  run values (ALP §4.1). The paper's "LWC+ALP" column averages 18.8
  bits/value against 21.7 for plain ALP and 20.6 for Zstd (Table 4, ALL AVG).
- **Time-series with regular steps** still wins big on the *timestamp* column
  with delta-of-delta (reading-gorilla.md Step 3); the value codec is a separate
  choice. Gorilla's 1.37 B/point headline is an average over
  Facebook's production series (reading-gorilla.md), a mix of repeats and
  small changes that this lane's synthetic shapes do not reproduce.

Limits of Step 7: the datasets are synthetic and seeded (a random walk, a
counter with 1-in-10,000 resets, uniform doubles ×1e9), not the paper's 30 real
datasets; ALP here is the single-level simplification; there is no Chimp
column (Chimp's codes are not implemented in the lane, so the Chimp numbers in
this chapter are the paper's and the 58-vs-51-bit arithmetic of Step 2);
there is no ALP_RD. The negative result in finding 2 — Gorilla at 8.03 B on
decimal data — holds for *this* generator, a 0.01-step random walk; a series with
many exact repeats would give Gorilla its zero-XOR branch.

## Where each step lives in the code

| Anchor | What | Step |
|---|---|---|
| ALP `include/alp/encoder.hpp:82-89` | `encode_value`: multiply, magic-number round, cast | 3 |
| ALP `include/alp/decoder.hpp:128-131` | `decode_value`: `d × 10^f × 10^−e` | 3 |
| ALP `include/alp/constants.hpp:70, 73, 69` | magic number `2^51+2^52`, `MAX_EXPONENT = 18`, `RD_SIZE_THRESHOLD_LIMIT` | 3, 6 |
| ALP `include/alp/encoder.hpp:382-397` | exception placeholder + exception arrays | 4 |
| ALP `include/alp/encoder.hpp:128-132, 159-206` | first-level search and tie-breaks | 5 |
| ALP `include/alp/encoder.hpp:241-305, 409-418` | second-level search, early exit, skip when `k′ = 1` | 5 |
| ALP `include/alp/config.hpp:11-26` | 1024 / 100 / 8 / 32 / 5 / 16 / 3 — every sampling constant | 5, 6 |
| ALP `include/alp/rd.hpp:94-100, 119-123, 165-169` | ALP_RD cut search, cut, glue | 6 |
| DuckDB `alp/algorithm/alp.hpp:93-100, 134-150` | `RequiredSpace`, `EncodeValue`, `DecodeValue` | 3, 4 |
| DuckDB `alp/algorithm/alp.hpp:216-303` | `FindTopKCombinations`, `FindBestFactorAndExponent` | 5 |
| DuckDB `alp/algorithm/alp.hpp:308-381` | `Compress`: encode, exceptions, FOR, bit width | 4 |
| DuckDB `alp/alp_compress.hpp:91-98, 125-146, 174-180` | raw fallback decision, vector header, sentinel | 4 |
| DuckDB `alp/alp_analyze.hpp:153-190` | `AlpFinalAnalyze`: the score the framework compares | 5 |
| DuckDB `function/compression_config.cpp:24-27` | CHIMP, PATAS, ALP, ALPRD registered as separate functions | 2, 5 |
| DuckDB `chimp/chimp_analyze.hpp:22-36`, `patas/patas_analyze.hpp:22-36` | "deprecated, can no longer be used to compress" | 2 |
| DuckDB `chimp/algorithm/chimp128.hpp:79, 103-168` | Chimp128 compress: reference choice, four flag cases | 2 |
| DuckDB `chimp/algorithm/chimp_utils.hpp:32-43` | `LEADING_ROUND`, 8-entry representation, `BUFFER_SIZE = 128` | 2 |
| Ours `experiments/src/alp.rs` | the ALP-style codec, lane F | 3–7 |
| Ours `experiments/src/baseline.rs` | `xor_bytes_encode`, the byte-aligned XOR | 7 |

## Questions to answer while reading

1. Chimp rounds a leading-zero count of 19 down to 18 and pays one extra payload
   bit. Show that rounding *down* is the only safe direction, and compute the
   cost of a worst-case miss (a count of 17 rounded to 16).
2. ALP's second level stops after two consecutive candidates that are no
   better. Construct a vector for which that early exit picks a pair that is
   worse than the best of the five, and say what it costs in bits per value.
3. The frame of reference makes `max − min` the cost, not `max`. Take a vector of
   2-decimal prices that are mostly near 20.00 but include one 2000.00; what
   happens to the bit width, and what would an exception-based design do
   differently?
4. In Step 7, why does the 6-decimal series cost 3.25 B while the 2-decimal one
   costs 1.50, when both are the same random walk? Derive the difference from
   the bit widths in the table.
5. DuckDB's ALP fuses nothing (Step 4); ALP's FFOR does. Predict which decode
   pass dominates for a 12-bit vector and what fusion saves, and say what the
   paper's own Figure 5 measurement says about it.
6. For FalkorDB: edge properties arrive as JSON/Cypher numerics. Which of the
   three columns of "where born" in Step 8 does a property like `weight: 0.37`
   fall into, and what in the ingest path would change the answer?

## Done when

Answer each before unfolding it.

- [ ] You can explain why XOR cannot compress a column of 2-decimal prices well,
      with a number.

  <details><summary>Answer</summary>

  A decimal such as 12.34 is a repeating fraction in binary, so its low fraction
  bits look random. 12.34 (`0x4028ae147ae147ae`) and 12.35
  (`0x4028b33333333333`) XOR to `0x00001d2749d2749d`: 19 leading zeros, 0
  trailing zeros, 45 meaningful bits. Every 0.01 step therefore costs ~45 bits
  of payload plus control bits (Gorilla's new-window case is 2 + 5 + 6 + 45 = 58
  bits), and in the measured lane Gorilla lands at 8.03 B/value on the
  two-decimal walk — slightly above raw — because most steps also miss the
  previous window and pay the 13-bit header again.

  </details>

- [ ] You can state `ALP_enc` and `ALP_dec`, say what `e` and `f` do, and run
      them on 8.0605.

  <details><summary>Answer</summary>

  `d = round(n × 10^e × 10^−f)` and `n′ = d × 10^f × 10^−e`; `e` moves the
  decimal point right `e` places, `f` cuts back off the trailing zeros that a
  high `e` creates, so a large `e` (accurate decode) does not force a large
  integer. For 8.0605: `(e, f) = (4, 0)` encodes to 80605 but decodes to
  8.060500000000001108… ≠ the stored 8.06049999999999933209…, so it fails;
  `(14, 10)`: 8.0605 × 10^14 = 806049999999999.875, × 10^−10 = 80604.9999999999854…,
  rounds to 80605, and 80605 × 10^10 × 10^−14 equals the original double bit for
  bit.

  </details>

- [ ] You can compute the stored size of an ALP vector and say when it is stored
      raw instead.

  <details><summary>Answer</summary>

  `bytes = ceil(n × w / 8) + 10 × x + 13` for `n` values, bit width `w` and `x`
  exceptions (a 13-byte header of exponent, factor, exception count, frame of
  reference and bit width; 10 bytes per exception: 8 raw plus a 2-byte
  position). A raw vector is `1 + 8n` bytes. For `n = 1024`: raw is 8193; at
  `w = 12` the compressed form is 1549 before exceptions, which tolerates
  `⌊(8193 − 1549)/10⌋ = 664` exceptions before raw wins; at `w = 64` it is
  8205 and never wins. The five-value example (12.34 … 12.33) costs 3 + 0 + 13 =
  16 bytes at `w = 4`.

  </details>

- [ ] You can name the three things ALP samples and why sampling is safe.

  <details><summary>Answer</summary>

  First level, per row group: 32 values from each of 8 vectors, trying all 190
  `(e, f)` pairs on each sample and keeping the 5 most frequent winners.
  Second level, per vector: 32 values, trying only those ≤ 5 pairs with an
  early exit after two non-improving candidates (skipped when only one pair
  survived). The third is the row-group *scheme* decision — ALP or ALP_RD —
  taken from the first-level estimate against 48 bits × 32 samples. It is safe
  because best pairs repeat across vectors (≤ 5 pairs cover almost every vector
  of a dataset, §2.6 Figure 3), and the paper measured that exhaustive
  search over all pairs improves the ratio by under 1%.

  </details>

- [ ] You can say what the measurement in Step 7 does and does not show.

  <details><summary>Answer</summary>

  It shows, on seeded synthetic series, that the decimal family is 4–5× smaller
  than XOR on values born as decimals (1.50 vs 6.79 / 8.03 B at two decimals),
  that the same random walk left as raw floating-point arithmetic is not a
  decimal (95.1% exceptions, 926/977 vectors raw, 8.00 B), that Gorilla's only
  win in the lane is the integer-valued counter (3.01 vs 6.07 byte-XOR, but ALP
  1.95 is better still), and that no codec compresses uniform random doubles far
  below 8 B (Gorilla 7.37, byte-XOR 7.48, ALP 8.00 raw). It does not show the
  paper's speed ratio (the loop here is scalar and unfused), a Chimp number (not
  implemented in the lane), ALP_RD, or behaviour on real data with many exact
  repeats, where Gorilla's zero-XOR branch would win back ground.

  </details>

## Takeaway

Float compression is two different bets. XOR codecs (Gorilla, Chimp) bet that
*neighbours share binary structure*; decimal-to-integer codecs (ALP) bet that
*values were born with short decimal expansions*. On the data this repo
measures, the second bet is worth 4–5× when it is true and nothing when it is
not, and it tells you honestly (exceptions, raw fallback) which case you are in.
The same lesson as Gorilla's, one level up: compression wins on knowing where
the data came from, and a codec that cannot say "I don't apply here" is worse
than one that can.

## References

**Papers**
- Afroozeh, Kuffó, Boncz — "ALP: Adaptive Lossless floating-Point
  Compression," SIGMOD 2024, [doi:10.1145/3626717](https://doi.org/10.1145/3626717).
  §2.1 IEEE layout; §2.4–2.6 data analysis (Table 2, Figure 3); §3.1 encoding,
  exceptions, FFOR, fast rounding; §3.2 sampling; §3.4 ALP_RD; §4.1 compression
  ratio (Table 4); §4.2 speed (Table 5).
- Liakos, Papakonstantinopoulou, Kotidis — "Chimp: Efficient Lossless Floating
  Point Compression for Time Series Databases," PVLDB 15(11), 2022. §3 the
  analysis of Gorilla; §4.1 Chimp (Algorithm 2, Figure 6); §4.2 Chimp128
  (Figure 8); §5.2 compression ratio.
- Pelkonen et al. — Gorilla, VLDB 2015 (reading-gorilla.md).

**Code**
- [cwida/ALP](https://github.com/cwida/ALP) at `31ca0ed`: `include/alp/`.
- [duckdb/duckdb](https://github.com/duckdb/duckdb) at the pin
  (`d54cbbe3df`): `src/include/duckdb/storage/compression/{alp,alprd,chimp,patas}/`,
  `src/function/compression_config.cpp`.
- Ours: `experiments/src/alp.rs`, `experiments/src/baseline.rs`,
  `experiments/src/bin/tsdb_bench.rs` lane F.
