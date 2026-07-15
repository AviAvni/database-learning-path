# Gorilla: compress by predicting

The 8-byte f64 value dominates every naive metrics codec — Gorilla's XOR
trick is the attack on those 8 bytes, and it's the chunk format inside
essentially every modern TSDB. This chapter builds the codec step by
step — why the baseline stalls at 11 bytes, prediction as the engine of
compression, the timestamp and value halves, and the exact bit format —
then reads the VLDB '15 paper's §4.1 against prometheus's
`tsdb/chunkenc/xor.go`, the most-deployed reimplementation and the spec
for our `gorilla.rs` stub.

## The problem in one sentence

A metrics **sample** is 16 raw bytes — an 8-byte timestamp and an 8-byte
f64 value — and the obvious codec (delta+varint timestamps, raw values)
only gets to **11.00 B/sample** (our baseline.rs, measured) because the
untouched 8-byte value dominates; Gorilla lands at **1.37 B/sample** on
Facebook's production data, an 8× win that lives or dies on the values.

## The concepts, step by step

### Step 1 — the baseline, and where the bytes hide

A time series is one metric's stream of (timestamp, value) pairs. The
standard first move is **delta encoding** — store the difference from the
previous timestamp instead of the timestamp (10 vs 1,721,000,000,010) —
followed by a **varint** (a variable-length integer encoding that spends
1 byte on small numbers instead of 8). That crushes timestamps to ~2–3
bytes but leaves values at 8: our `baseline.rs` measures **11.00
B/sample regardless of value shape**. Any real progress must attack the
f64 — and f64 bit patterns don't respond to varints (a small *change* in
value does not produce a small *integer*).

### Step 2 — compression is prediction plus a cheap error encoding

The general trick behind Gorilla (and most codecs): pick a **predictor**
(a guess for the next datum computed from what came before), store only
the **prediction error**, and design the encoding so *zero error costs
almost zero bits*. Compression then equals regularity: the more
predictable the stream, the smaller the errors, the fewer the bits.
Facebook's observation is that metrics are extremely predictable in two
specific ways — 96% of timestamps arrive at a fixed interval, and
consecutive values usually share sign, exponent, and most mantissa bits:

```
 timestamps: predictor = "same delta as last time"
   t=1000, 1010, 1020, 1030, 1029, 1040 (10s scrape, 1ms jitter)
   deltas:      10, 10, 10, 9, 11
   delta-of-delta: 0,  0, -1,  2      <- mostly ZERO -> mostly 1 bit

 values: predictor = "same value as last time"
   v XOR prev: 0x0000000000000000            (unchanged -> 1 bit)
               0x0000000FE1000000            (close -> short run of
                ^^^^^^^    ^^^^^^             meaningful bits in the
                leading    trailing            middle -> store just those)
                zeros      zeros
```

The flip side is the honest one: on unpredictable data the scheme *must*
lose (errors as big as the data, plus control-bit overhead). Our bench
demands exactly that — full-entropy values must come out >8 B/sample —
because the codec exploits regularity, not information theory.

### Step 3 — timestamps: delta-of-delta

The timestamp predictor is "same delta as last time", so the stored error
is the **delta-of-delta (dod)**: `(t − t_prev) − delta_prev`. On a steady
10 s scrape the dod is 0 — one bit — and jitter of ±1 s stays in the
smallest bucket. Errors are encoded with a **prefix code** (shorter bit
patterns for likelier cases, like Morse code): a leading `0` bit means
"dod = 0", and progressively longer prefixes buy progressively wider
payload fields:

| dod range | prefix | payload |
|---|---|---|
| 0 | `0` | — |
| [-63, 64] | `10` | 7 bits |
| [-255, 256] | `110` | 9 bits |
| [-2047, 2048] | `1110` | 12 bits |
| else | `1111` | 32 bits (paper; we use 64 for ms robustness) |

At 96% zeros, timestamps cost ~1.1 bits/sample — effectively free. The
bucket boundaries are a *workload parameter*, not a law: prometheus
retunes them to 14/17/20/64 bits because minute-scale scrape intervals
with ms timestamps produce bigger dods than Gorilla's 60 s-max regime.

### Step 4 — values: XOR against the previous float

The value predictor is even simpler — "same value as last time" — but the
error is measured in *bits*, not arithmetic: `xor = v.to_bits() ^
v_prev.to_bits()`. An f64 is sign (1 bit), exponent (11), mantissa (52),
ordered high to low; two nearby values share the top bits and often the
bottom ones too, so the XOR is zeros except a short run of **meaningful
bits** in the middle — characterized by its count of **leading zeros**
and **trailing zeros**. Three cases, prefix-coded:

- `0` — XOR is zero (value unchanged): 1 bit total.
- `10` — the meaningful bits fit inside the *previous* (leading,
  trailing) window: store just the middle bits, reusing the stored
  window geometry (no new header).
- `11` — new window: 5 bits of leading-zero count + 6 bits of
  meaningful-bit length + the bits themselves. The 6-bit length field
  stores 64 as 0 — the classic off-by-one everyone reimplements.

The window-reuse branch is a bet that consecutive XORs look alike; it
saves the 11-bit header but pads with wasted bits when the true window is
tighter (Q2).

### Step 5 — the append path, end to end

Both halves are one state machine over `(t_prev, delta_prev, v_prev)` —
this is precisely what `gorilla.rs` implements:

```rust
fn append(&mut self, t: i64, v: f64) {
    let dod = (t - self.t_prev) - self.delta_prev;  // error vs "same delta as last time"
    match dod {                                     // smaller error => fewer bits
        0            => self.w.bits(0b0, 1),
        -63..=64     => { self.w.bits(0b10, 2);   self.w.bits(dod as u64, 7); }
        -255..=256   => { self.w.bits(0b110, 3);  self.w.bits(dod as u64, 9); }
        -2047..=2048 => { self.w.bits(0b1110, 4); self.w.bits(dod as u64, 12); }
        _            => { self.w.bits(0b1111, 4); self.w.bits(dod as u64, 64); }
    }
    let xor = v.to_bits() ^ self.v_prev.to_bits(); // error vs "same value as last time"
    if xor == 0 { self.w.bits(0b0, 1); }
    else { self.write_vdelta(xor); }   // '10': reuse prev (leading,trailing) window;
                                       // '11': 5-bit leading + 6-bit len + middle bits
    self.delta_prev = t - self.t_prev;
    self.t_prev = t; self.v_prev = v;
}
```

Steady scrape + slowly-moving gauge ⇒ ~1 bit (ts) + ~1–15 bits (value)
per sample ⇒ Facebook's measured **1.37 B/sample vs 16 raw**. Decode is
the mirror-image state machine: replay the same predictions, add the
stored errors.

### Step 6 — what the format refuses to do, on purpose

A Gorilla chunk has **no random access**: every field's width depends on
the previous state, so the only way in is decoding front-to-back from
sample one. That's fine *because of the workload* — queries always scan
time ranges (README §0) — and the cost is capped by capping the chunk:
prometheus cuts chunks at **~120 samples** (2 hours at 15 s scrape), so
seeking costs at most one small chunk decode, and each chunk's fixed
header is amortized over enough samples to stay negligible (the two
pressures of Q3). The other refusal: no attempt to compress the
incompressible — on full-entropy values the `11` branch fires every
sample and pays ~1.6 bytes of control overhead *over* raw (Q5 asks you to
count the bits).

## Where each step lives in the code

prometheus `tsdb/chunkenc/xor.go`, line by line:

1. `xorAppender.Append` (`xor.go:161`) — the whole timestamp path
   (Steps 3, 5). Note prometheus's buckets differ: 14/17/20/64 bits
   (`:195-208`) because scrape intervals up to minutes with ms timestamps
   produce bigger dods than Gorilla's 60s-max regime. Same idea, retuned
   constants — bucket boundaries are a *workload parameter*, not a law.
2. `writeVDelta` (`:226`) — the XOR path (Step 4), with the
   leading/trailing window reuse.
3. The iterator (`:357-396`) — decode is a mirror-image state machine
   (Step 5); `it.tDelta = uint64(int64(it.tDelta) + dod)` (`:396`) is the
   entire "prediction + error" model in one line.
4. Note what's *absent* (Step 6): no random access. A Gorilla chunk
   decodes front-to-back only — fine, because queries always scan time
   ranges, and chunks are capped (~120 samples) so seeking costs one
   chunk.

## Questions to answer while reading

1. Why does the timestamp scheme store delta-of-delta but the value
   scheme store plain XOR (delta-of-value, in a sense) — what property of
   each stream makes second-order prediction pay for one but not the other?
2. The `10` value branch reuses the previous (leading, trailing) window
   even when the current XOR would fit a *tighter* one. What does that
   trade, and why does the encoder still emit `11` sometimes on purpose?
3. Chunks are capped at ~120 samples in prometheus. Derive the two
   pressures that set that number (decode-on-read cost vs per-chunk
   header amortization).
4. Counters are monotone integers stored as f64. Why does XOR do worse on
   a fast counter than on a noisy gauge of similar magnitude — and what
   do delta-encoding-the-*value* schemes (VictoriaMetrics
   nearest_delta2) exploit that XOR can't?
5. Your `random_values_hit_the_entropy_floor` test demands >8 B/sample.
   Where exactly do the extra ~1.6 bytes over raw come from? Count the
   control bits.
6. M30 mapping: property history in FalkorDB is (entity, property, ts) →
   value where values are often strings/ids, not floats. Which half of
   Gorilla survives (dod timestamps) and what replaces XOR for
   non-numeric payloads?

## References

**Papers**
- Pelkonen et al. — "Gorilla: A Fast, Scalable, In-Memory Time Series
  Database" (VLDB 2015) — §4.1 is the codec and the reason to read it;
  §3 and §5 are the ops war stories

**Code**
- [prometheus](https://github.com/prometheus/prometheus)
  `tsdb/chunkenc/xor.go` — the most-deployed reimplementation of §4.1;
  note the retuned dod buckets (14/17/20/64 bits) vs the paper's
