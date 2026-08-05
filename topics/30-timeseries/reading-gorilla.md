# Gorilla: compress by predicting

The 8-byte f64 value dominates every naive metrics codec — Gorilla's XOR
trick is the attack on those 8 bytes, and it's the chunk format inside
essentially every modern TSDB. This chapter builds the codec from zero —
why the baseline stalls at 11 bytes, prediction as the engine of
compression, the timestamp and value halves, and the exact bit format —
then reads the VLDB '15 paper's §4.1 against prometheus's
`tsdb/chunkenc/xor.go`, the most-deployed reimplementation and the spec
for our `gorilla.rs` stub. Two questions drive the read: *what does each
half of a sample cost in bits, and why can only one half be compressed at
all on unpredictable data?*

The paper anchors below are Pelkonen et al., **"Gorilla: A Fast, Scalable,
In-Memory Time Series Database," VLDB 2015** — §4.1.1 for timestamps,
§4.1.2 for values, Figure 2 for the worked example. The code anchors are
prometheus at `f282b5c` (the commit this repo pins), quoted with the line
numbers they occupy in `tsdb/chunkenc/xor.go` at that commit.

## The problem in one sentence

A metrics **sample** — one metric's (timestamp, value) reading — is 16 raw
bytes (an 8-byte integer timestamp and an 8-byte f64 value), and the obvious
codec (delta+varint timestamps, raw values) only gets to **11.00 B/sample**
(our `baseline.rs`, [measured](../../FINDINGS.md) — topic 30) because the
untouched 8-byte value dominates; Gorilla lands at **1.37 bytes per point**
on Facebook's production data, which the value half either compresses to a
single bit or barely touches, depending entirely on how predictable the
values are.

## The concepts, step by step

### Step 1 — the baseline, and where the bytes hide

> **In:** nothing yet — this step fixes the 16-byte sample and the 11.00
> B/sample floor the rest of the chapter is trying to break.
> **Out:** the finding that timestamps are already cheap and the f64 value is
> the whole remaining cost — the target Steps 3–4 split and attack.

A **time series** is one metric's stream of (timestamp, value) pairs. The
standard first move on the timestamps is **delta encoding** — store the
difference from the previous timestamp instead of the absolute value (`10`
instead of `1721000000010`) — followed by a **varint** (a variable-length
integer that spends one byte on small numbers instead of a fixed eight).
That crushes the timestamp column to two or three bytes but leaves each
value at eight: our `baseline.rs` measures a flat **11.00 B/sample for all
four value shapes** — constant, gauge, counter, and random alike.

That flatness is the whole point. A series whose value never changes
compresses exactly as well as uniform random noise, because delta+varint
only ever touched the *timestamp*; the f64 was copied through untouched. An
f64 does not respond to a varint — a small *change* in value (42.0 → 42.1)
is not a small *integer*, it is a different 64-bit pattern with different
low mantissa bits. Any real progress has to attack the value bits directly,
and it has to do so in a way that pays almost nothing when the value is
predictable and cannot help when it is not. That asymmetry is the codec.

### Step 2 — compression is prediction plus a cheap error encoding

> **In:** the 11.00 B/sample baseline from Step 1, and the fact that the
> value bytes are the target.
> **Out:** the two predictors Steps 3 and 4 each turn into a bit format —
> "same delta as last time" for timestamps, "same value as last time" for
> values — and the rule that zero prediction error must cost near-zero bits.

The general engine behind Gorilla, and behind most codecs, is three moves:
pick a **predictor** (a guess for the next datum computed only from what came
before), store only the **prediction error** (how far the real datum fell
from the guess), and design the encoding so that *zero error costs almost
zero bits*. Compression then equals regularity — the more predictable the
stream, the smaller the errors, the fewer the bits. The honest flip side,
which the bench enforces, is that on unpredictable data the scheme *must*
lose: the error is as large as the datum, plus the control bits it takes to
say so.

Facebook's paper (§4.1) observes that metrics are predictable in two
specific, separable ways, so Gorilla uses two predictors:

```
 timestamps: predictor = "same delta as last time"
   t     = 1000, 1010, 1020, 1030, 1039, 1050   (10s scrape, 1s jitter)
   delta =       10,   10,   10,    9,   11
   dod   =              0,    0,   -1,    2      <- mostly ZERO -> mostly 1 bit

 values: predictor = "same value as last time"
   v XOR prev = 0x0000000000000000              (unchanged -> 1 bit)
                0x0000000FE1000000              (close -> a short run of
                 ^^^^^^^      ^^^^^^             meaningful bits in the
                 leading      trailing           middle -> store just those)
                 zeros        zeros
```

The two predictors are independent — a series can be steady in time and wild
in value, or vice versa — so the timestamp half (Step 3) and the value half
(Step 4) get separate formats and are analysed separately. Everything below
is one of these two predictors turned into bits.

### Step 3 — timestamps: delta-of-delta

> **In:** the "same delta as last time" predictor from Step 2, applied to the
> timestamp column.
> **Out:** a prefix-coded bit field per timestamp — one bit in the common
> case — that Step 5 interleaves with the value field.

The timestamp predictor is "same delta as last time," so the stored error is
the **delta-of-delta (dod)** — the change in the gap between successive
samples: `D = (t_n − t_{n-1}) − (t_{n-1} − t_{n-2})`. Here `t_n` is the
current timestamp and the two subtractions are this gap minus the previous
gap. On a steady scrape the gap is constant, so `D = 0`.

The paper's own worked example (§4.1.1): if four successive gaps are 60, 60,
59, 61 seconds, the deltas-of-deltas are `0, −1, 2` — subtract each gap from
the one before. A single dropped sample is absorbed almost as cheaply:
gaps of 60, 60, 121, 59 give dods `0, 61, −62`, and both 61 and −62 still
fall in the smallest non-zero bucket.

The dods are stored with a **prefix code** — shorter bit patterns for the
likelier cases, like Morse code — where a leading `0` means "dod = 0" and
each longer prefix buys a wider payload field. The exact ranges are
§4.1.1's encoding algorithm, step 2:

| dod range | prefix | payload | total bits |
|---|---|---|---|
| `0` | `0` | — | 1 |
| [−63, 64] | `10` | 7 bits | 9 |
| [−255, 256] | `110` | 9 bits | 12 |
| [−2047, 2048] | `1110` | 12 bits | 16 |
| else | `1111` | 32 bits (paper; our stub uses 64 for ms robustness) | 36 (or 68) |

The block also opens with a header: the paper stores an aligned start
timestamp and the first delta in **14 bits** (§4.1.1 footnote 1: 14 bits
spans a bit over four hours). Our stub simplifies the header to a raw 64-bit
`t0` and 64-bit `v0` (`gorilla.rs`, layout doc at the top of the file), which
is why a short series looks header-heavy and a long one does not.

Worked example — the paper reports (§4.1.1, Figure 3, from 440,000 real
timestamps) that **about 96% of all timestamps compress to a single bit**. On
a 1000-sample steady chunk that is `0.96 × 1 bit + 0.04 × (say 9 bits) ≈
0.96 + 0.36 = 1.32 bits/sample` — call it ~1.1–1.3 bits, effectively free,
which is why the timestamp column stopped being the problem back in Step 1.
The bucket boundaries are a *workload parameter*, not a law: prometheus
retunes them (Step 5) because millisecond timestamps at minute-scale scrapes
produce larger dods than Gorilla's second-resolution regime.

### Step 4 — values: XOR against the previous float

> **In:** the "same value as last time" predictor from Step 2, applied to the
> f64 value column.
> **Out:** a control-bit-coded bit field per value — one bit when the value
> repeats — that Step 5 interleaves with the timestamp field from Step 3.

The value predictor is simpler still — "same value as last time" — but the
error is measured in *bits*, not arithmetic: `xor = v.to_bits() ^
v_prev.to_bits()`. An **f64** lays out as **sign** (1 bit), **exponent** (11
bits) and **mantissa** (52 bits, the significant digits), ordered high to
low. Two nearby values share the sign, the exponent, and the top mantissa
bits, so their XOR is zeros except for a short run of **meaningful bits** in
the middle — the run is described by its count of **leading zeros** (zero
bits above the run) and **trailing zeros** (zero bits below it). The paper's
§4.1.2 scheme, using **control bits** (the small tag that selects which case
applies):

- `0` — XOR is zero, the value is unchanged: **1 bit total**.
- `10` — control bit `0`: the meaningful bits fall *within* the previous
  value's (leading, trailing) window (at least as many leading zeros and at
  least as many trailing zeros), so reuse that window and store just the
  middle bits — no new header.
- `11` — control bit `1`: a new window. Store the **leading-zero count in 5
  bits**, the **meaningful-bit length in 6 bits**, then the meaningful bits
  themselves.

The 6-bit length field is where everyone trips: 64 meaningful bits will not
fit in 6 bits (which max out at 63), but a length of 0 can never occur (that
is the `0`, XOR-is-zero case), so **64 is stored as 0 and adjusted back on
read** — prometheus documents this exactly at `xor.go:442-448`. The 5-bit
leading count has the same ceiling: it caps at 31, so prometheus *clamps*
any leading count ≥ 32 down to 31 (`xor.go:425-427`) rather than overflow
the field.

Worked example — the paper's Figure 2, hand-encoded bit by bit. The values
are 12.0 then 24.0:

```
 12.0 = 0x4028000000000000       (the first value: stored raw, 64 bits)
 24.0 = 0x4038000000000000
 XOR  = 0x0010000000000000       -> 11 leading zeros, 1 meaningful bit,
                                     52 trailing zeros

 encode 24.0 after 12.0, control bits '11' (new window):
   '1'   nonzero            1 bit
   '1'   new-window control 1 bit
   11    leading count      5 bits   (binary 01011)
   1     length             6 bits   (binary 000001)
   1     the meaningful bit 1 bit
   ------------------------------- 14 bits total  (matches Figure 2's "2+5+6+1")
```

Figure 5 (§4.1.2, from 1.6 million real values) gives the population
breakdown: **51% of values compress to a single bit** (`0`, unchanged),
**~30% take the `10` reuse branch** at an average of 26.6 bits, and the
remaining **19% take `11`** at 36.9 bits — the extra ~13 bits being the two
control bits plus the 5+6 header. Those are the numbers that turn "predictor
+ error" into 1.37 bytes.

### Step 5 — the append path, end to end

> **In:** the timestamp field (Step 3) and the value field (Step 4).
> **Out:** one interleaved bitstream, driven by a single state machine over
> `(t_prev, delta_prev, v_prev, leading, trailing)` — the exact shape
> `gorilla.rs` implements and `xor.go` ships.

Each sample writes its dod field then its XOR field, updating the same five
pieces of carried state. This is the pseudocode our stub fills in — it is
*not* quoted, because `gorilla.rs::append` is a `todo!()` you implement, but
its spec is fixed by the layout doc at the top of that file:

```rust
// ILLUSTRATION — not quoted; the spec for experiments/src/gorilla.rs:63
// (append), whose reference implementation is prometheus tsdb/chunkenc/xor.go.
fn append(&mut self, t: i64, v: f64) {
    let dod = (t - self.t_prev) - self.delta_prev;  // Step 3: timestamp error
    match dod {                                      // smaller error => fewer bits
        0            => self.w.bits(0b0, 1),
        -63..=64     => { self.w.bits(0b10, 2);   self.w.bits(dod as u64, 7); }
        -255..=256   => { self.w.bits(0b110, 3);  self.w.bits(dod as u64, 9); }
        -2047..=2048 => { self.w.bits(0b1110, 4); self.w.bits(dod as u64, 12); }
        _            => { self.w.bits(0b1111, 4); self.w.bits(dod as u64, 64); }
    }
    let xor = v.to_bits() ^ self.v_prev.to_bits();   // Step 4: value error
    if xor == 0 { self.w.bits(0b0, 1); }
    else { self.write_vdelta(xor); }   // '10' reuse window, or '11' new window
    self.delta_prev = t - self.t_prev;
    self.t_prev = t; self.v_prev = v;
}
```

The real, shipped version of the timestamp half is `xorAppender.Append`, and
the load-bearing lines are the four `case` arms of the dod switch — note the
widths are prometheus's retuned **14/17/20/64** bits, not the paper's
7/9/12/32, but the *prefixes* (`10`/`110`/`1110`/`1111`) are identical:

```go
// prometheus tsdb/chunkenc/xor.go — inside Append, the dod switch, 194-209
   194  		switch {
   195  		case dod == 0:
   196  			a.b.writeBit(zero)
   197  		case bitRange(dod, 14):
   198  			a.b.writeByte(0b10<<6 | (uint8(dod>>8) & (1<<6 - 1)))
   199  			a.b.writeByte(uint8(dod))
   200  		case bitRange(dod, 17):
   201  			a.b.writeBits(0b110, 3)
   202  			a.b.writeBits(uint64(dod), 17)
   203  		case bitRange(dod, 20):
   204  			a.b.writeBits(0b1110, 4)
   205  			a.b.writeBits(uint64(dod), 20)
   206  		default:
   207  			a.b.writeBits(0b1111, 4)
   208  			a.b.writeBits(uint64(dod), 64)
   209  		}
```

The value half lives in `xorWrite`, which `writeVDelta` (`xor.go:226`) calls
for every sample. The three branches of Step 4 are lines 415, 429–433 and
439–449:

```go
// prometheus tsdb/chunkenc/xor.go — xorWrite, the value path, 412-449
   412  func xorWrite(b *bstream, newValue, currentValue float64, leading, trailing *uint8) {
   413  	delta := math.Float64bits(newValue) ^ math.Float64bits(currentValue)
   414
   415  	if delta == 0 {
   416  		b.writeBit(zero)                       // '0' — unchanged, 1 bit
   417  		return
   418  	}
   419  	b.writeBit(one)                            // '1' — nonzero
   // ... 421-427: count leading/trailing zeros; clamp leading >= 32 down to 31 ...
   429  	if *leading != 0xff && newLeading >= *leading && newTrailing >= *trailing {
   430  		// In this case, we stick with the current leading/trailing.
   431  		b.writeBit(zero)                       // control '0' -> the '10' branch
   432  		b.writeBits(delta>>*trailing, 64-int(*leading)-int(*trailing))
   433  		return
   434  	}
   // ... 436-437: this is a new window; update *leading, *trailing ...
   439  	b.writeBit(one)                            // control '1' -> the '11' branch
   440  	b.writeBits(uint64(newLeading), 5)         // 5-bit leading count
   // ... 442-446: sigbits == 64 doesn't fit 6 bits, so write 0 and fix on read ...
   447  	sigbits := 64 - newLeading - newTrailing
   448  	b.writeBits(uint64(sigbits), 6)            // 6-bit length
   449  	b.writeBits(delta>>newTrailing, int(sigbits))
   450  }
```

Line 432 is the reuse branch (`10`); line 439 opens the new-window branch
(`11`). Decode is the mirror-image state machine: `xorIterator.Next`
(`xor.go:305`) replays the timestamp prediction and adds the stored dod on
one line — `it.tDelta = uint64(int64(it.tDelta) + dod)` at **`xor.go:396`** —
then `readValue` (`:402`) calls `xorRead` (`:452`) to undo the XOR.

Worked example — a constant series, steady 10 s scrape, at the stub's layout.
After the 128-bit header (`t0`, `v0`), every subsequent sample is `dod = 0`
(1 bit) plus `xor = 0` (1 bit) = **2 bits/sample**. Over a long chunk that is
**0.25 B/sample**, against 16 raw and the 11.00 baseline — the header
amortises to nothing. That is the constant-series prediction in `notes.md`
(~0.3 B/sample), and Facebook's all-shapes production average is the
paper's headline **1.37 bytes per point, a 12× reduction from 16 raw**
(§4 opening; Figure 6, the two-hour block). Measured against *this repo's*
11.00 B/sample baseline that is `11.00 / 1.37 = 8.0×` — the win the value
half adds on top of what delta+varint already did to the timestamps.

### Step 6 — what the format refuses to do, on purpose

> **In:** the interleaved bitstream from Step 5.
> **Out:** the two deliberate non-features — no random access, no compression
> of the incompressible — and the chunk cap that makes the first one cheap.

A Gorilla chunk has **no random access**: every field's width depends on the
decoded state before it (the dod bucket, the reuse window), so the only way
to sample 500 is to decode samples 1 through 499 first. That is acceptable
*because of the workload* — queries always scan time ranges (README §0), never
seek to a lone point — and the cost is capped by capping the chunk.
Prometheus caps chunks at a **default** of `DefaultSamplesPerChunk = 120`
(`tsdb/head.go:236`), reached not as a hard count but via a time prediction:
a chunk is cut when its predicted end time arrives or, as a hard ceiling, at
`2 × samplesPerChunk = 240` samples (`tsdb/head_append.go:2041`). At a 15 s
scrape, ~120 samples is about 30 minutes of data — note this is *not* the
2-hour figure, which is the immutable *block* boundary
(`DefaultBlockDuration`, `tsdb/db.go:56`), a different level of the hierarchy
(reading-prometheus-tsdb.md). Seeking therefore costs at most one small chunk
decode, and the fixed header amortises over ~120 samples.

The second refusal is not compressing the incompressible. On full-entropy
values the `11` branch fires every sample, and the *control* overhead is
exactly `1 (nonzero) + 1 (new window) + 5 (leading) + 6 (length) = 13 bits`
= **~1.6 bytes over the 8 raw value bytes**, before the ~64 meaningful bits
that just reproduce the value. Plus ~1 bit for the steady timestamp, that is
~9.6 B/sample — above 8, which is precisely what our
`random_values_hit_the_entropy_floor` test demands. A codec that wins on
regularity must lose on noise; pretending otherwise would be the bug.

## Where each step lives in the code

prometheus `tsdb/chunkenc/xor.go` at `f282b5c`, line by line:

| Lines | What | Step |
|-------|------|------|
| `161` | `xorAppender.Append` — the whole timestamp path | 3, 5 |
| `194-209` | the dod bucket switch — prefixes `10/110/1110/1111`, widths 14/17/20/64 | 3, 5 |
| `220-224` | `bitRange` — how a bucket's fit is tested | 3 |
| `226` | `writeVDelta` → delegates to `xorWrite` | 4, 5 |
| `412-450` | `xorWrite` — the XOR value path, all three branches | 4, 5 |
| `425-427` | the leading-zero clamp to 31 (5-bit field) | 4 |
| `442-448` | the "store 64 as 0" length wrinkle (6-bit field) | 4 |
| `305-400` | `xorIterator.Next` — decode, the mirror state machine | 5 |
| `396` | `it.tDelta = uint64(int64(it.tDelta) + dod)` — prediction + error, one line | 5 |
| `402`, `452` | `readValue` → `xorRead` — undo the XOR | 5 |

Note what is *absent* (Step 6): there is no seek-by-index anywhere in the
file. A chunk decodes front-to-back only, which is fine because queries scan
ranges and chunks are capped at ~120 samples.

## Questions to answer while reading

1. Why does the timestamp scheme store delta-of-delta but the value scheme
   store plain XOR (delta-of-value, in a sense) — what property of each
   stream makes second-order prediction pay for one but not the other?
2. The `10` value branch reuses the previous (leading, trailing) window even
   when the current XOR would fit a *tighter* one. What does that trade, and
   why does the encoder still emit `11` sometimes on purpose?
3. Chunks are capped at ~120 samples in prometheus. Derive the two pressures
   that set that number (decode-on-read cost vs per-chunk header
   amortization).
4. Counters are monotone integers stored as f64. Why does XOR do worse on a
   fast counter than on a noisy gauge of similar magnitude — and what do
   delta-encoding-the-*value* schemes (VictoriaMetrics `nearest_delta2`,
   reading-victoriametrics-influx.md) exploit that XOR can't?
5. Your `random_values_hit_the_entropy_floor` test demands >8 B/sample.
   Where exactly do the extra ~1.6 bytes over raw come from? Count the
   control bits.
6. M30 mapping: property history in FalkorDB is (entity, property, ts) →
   value where values are often strings/ids, not floats. Which half of
   Gorilla survives (dod timestamps) and what replaces XOR for non-numeric
   payloads?

## Done when

Answer each before unfolding it.

- [ ] You can explain delta-of-delta on timestamps and why regular scrapes make it nearly free.

  <details><summary>Answer</summary>

  The stored quantity is `D = (t_n − t_{n-1}) − (t_{n-1} − t_{n-2})`, the
  change in the gap between successive samples (§4.1.1). A scraper that polls
  every 10 s produces a constant gap, so every `D` is 0 and encodes as the
  single prefix bit `0` (the top row of Step 3's table). Even jitter and a
  dropped sample stay cheap: the paper's own example turns gaps of 60, 60,
  59, 61 into dods 0, −1, 2, all inside the [−63, 64] bucket. The paper
  measured (Figure 3, 440,000 timestamps) that **96% of timestamps compress
  to one bit**, so a steady chunk costs ~1.1–1.3 bits per timestamp — which
  is why Step 1's baseline had already made the timestamp column a rounding
  error and left the value column as the whole problem.

  </details>

- [ ] You can explain XOR plus leading/trailing zero counts on values, and why that is the half delta+varint cannot compress.

  <details><summary>Answer</summary>

  `xor = v.to_bits() ^ v_prev.to_bits()` (§4.1.2, `xor.go:413`). Two nearby
  f64s share sign, exponent and high mantissa bits, so the XOR is zeros
  except a run of meaningful bits bounded by a leading-zero count and a
  trailing-zero count. Unchanged values XOR to 0 and cost 1 bit; a value
  inside the previous window costs the middle bits with no header (`10`); a
  new window costs 5+6 header bits plus the run (`11`). Figure 5 measured
  51% single-bit, 30% at 26.6 bits, 19% at 36.9 bits.

  delta+varint cannot touch this because a varint compresses an integer by
  dropping *leading zero bytes*, and a float value that changes slightly
  (42.0 → 42.1) is a completely different bit pattern with populated low
  mantissa bits — no small integer to shrink. XOR works precisely because it
  looks at *bit-level* similarity between neighbours, not numeric magnitude.

  </details>

- [ ] You can predict bits per sample for a constant series and for a random one — this topic measures the baseline at a flat 11.00 B/sample for both, which is the gap you are closing.

  <details><summary>Answer</summary>

  Constant series, steady scrape: every sample is `dod = 0` (1 bit) + `xor =
  0` (1 bit) = 2 bits, so after the header amortises, **~0.25 B/sample** —
  three orders below the 11.00 baseline. Random values, steady scrape: the
  timestamp is still ~1 bit, but the value takes the `11` branch every time —
  `1 + 1 + 5 + 6 = 13` control bits plus ~64 meaningful bits ≈ 77 bits ≈
  **~9.6 B/sample**, *above* the 8-byte raw value.

  The baseline reads 11.00 for both because delta+varint only ever
  compressed the timestamp; the value shape was invisible to it. Gorilla
  makes the value shape decide everything — the spread from constant to
  random goes from zero (baseline) to enormous (~0.25 vs ~9.6 B/sample). If
  your encoder does not show that spread, it is not exploiting the shape.

  </details>

- [ ] You can say what makes the encoding block-oriented and what a partial block costs.

  <details><summary>Answer</summary>

  Every field's width depends on state decoded before it — the dod bucket
  prefix, the reuse-window geometry — so there is no way to index into a
  chunk; you decode from sample one. That makes a chunk a block you enter only
  at its head. Prometheus bounds the cost by capping the chunk: a default of
  120 samples (`head.go:236`), cut by time prediction with a hard ceiling of
  240 (`head_append.go:2041`). A partial block therefore costs at most one
  short decode — ~120 samples — and the per-chunk header (the raw first
  timestamp and value) amortises over those samples. Push the cap too low and
  the header dominates; push it too high and every point query decodes a long
  run. Those are the two pressures of Q3.

  </details>

## Takeaway

Gorilla splits a sample into two independently-predictable halves and gives
each the cheapest possible encoding of "no change": one bit. On regular
metrics that is 1.37 bytes per point, a 12× win over raw and ~8× over this
repo's delta+varint baseline. On noise it is honestly worse than raw. For the
capstone (M30): the dod timestamp half generalises to any monotone-ish
column; the XOR value half is float-specific and gives way to
dictionary/RLE for the string and id payloads a graph's property history
carries (Q6).

## References

**Papers**
- Pelkonen et al. — "Gorilla: A Fast, Scalable, In-Memory Time Series
  Database" (VLDB 2015). §4.1.1 is the timestamp codec (dod buckets, the 96%
  figure, Figure 3); §4.1.2 is the value codec (XOR, control bits, Figure 5);
  Figure 2 is the worked 12.0→24.0 example; §4 opening and Figure 6 give the
  1.37 bytes/point headline. §3 and §5 are the ops war stories.

**Code**
- [prometheus](https://github.com/prometheus/prometheus) `tsdb/chunkenc/xor.go`
  (pinned at `f282b5c`) — the most-deployed reimplementation of §4.1. Note
  the retuned dod buckets (14/17/20/64 bits, `:194-209`) versus the paper's
  7/9/12/32, and the two field-width wrinkles at `:425-427` and `:442-448`.
- Our `experiments/src/gorilla.rs` — the stub whose layout doc fixes the exact
  bit format this chapter's Step 5 pseudocode targets.
