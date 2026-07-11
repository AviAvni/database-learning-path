# Reading guide — Gorilla (VLDB '15) + prometheus chunkenc/xor.go

Paper: *Gorilla: A Fast, Scalable, In-Memory Time Series Database*,
Pelkonen et al., VLDB 2015. Code: [`~/repos/prometheus/tsdb/chunkenc/xor.go`](https://github.com/prometheus/prometheus)
— the most-deployed reimplementation of §4.1.

## Why it worked

Facebook's observation: 96% of their timestamps arrive at a fixed
interval, and consecutive metric values usually share sign, exponent, and
most of the mantissa. Both facts turn into *prediction*: encode only the
error against a trivial predictor.

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

Result on Facebook's production data: **1.37 bytes/sample** vs 16 raw.
Our bench measures the honest version per workload shape — including the
full-entropy series where XOR *must* fail (>8 B/sample), because the
codec exploits regularity, not information theory.

## The bit format (what your `gorilla.rs` stub implements)

| dod range | prefix | payload |
|---|---|---|
| 0 | `0` | — |
| [-63, 64] | `10` | 7 bits |
| [-255, 256] | `110` | 9 bits |
| [-2047, 2048] | `1110` | 12 bits |
| else | `1111` | 32 bits (paper; we use 64 for ms robustness) |

Values: `0` = identical; `10` = meaningful bits fit the previous
(leading, trailing) window, store only the middle; `11` = new window:
5-bit leading-zero count + 6-bit length + the bits. The 6-bit length
stores 64 as 0 — the classic off-by-one everyone reimplements.

## prometheus xor.go, line by line

1. `xorAppender.Append` (`xor.go:161`) — the whole timestamp path. Note
   prometheus's buckets differ: 14/17/20/64 bits (`:195-208`) because
   scrape intervals up to minutes with ms timestamps produce bigger dods
   than Gorilla's 60s-max regime. Same idea, retuned constants — bucket
   boundaries are a *workload parameter*, not a law.
2. `writeVDelta` (`:226`) — the XOR path, with the leading/trailing
   window reuse.
3. The iterator (`:357-396`) — decode is a mirror-image state machine;
   `it.tDelta = uint64(int64(it.tDelta) + dod)` (`:396`) is the entire
   "prediction + error" model in one line.
4. Note what's *absent*: no random access. A Gorilla chunk decodes
   front-to-back only — fine, because queries always scan time ranges,
   and chunks are capped (~120 samples) so seeking costs one chunk.

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
