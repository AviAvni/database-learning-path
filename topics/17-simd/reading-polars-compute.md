# Reading guide — polars-compute kernels

Clone: [`~/repos/polars`](https://github.com/pola-rs/polars) (`crates/polars-compute/src/`). The
production-Rust answer to "how do I ship SIMD without a nightly
compiler or per-CPU binaries": autovec-friendly scalar bodies,
explicit `std::simd` where it pays, raw intrinsics only for the one
instruction Rust can't reach (vpcompress).

## Anchor map

| anchor | what it is |
|---|---|
| float_sum.rs:13-14 | `STRIPE = 16`, `PAIRWISE_RECURSION_LIMIT = 128` |
| float_sum.rs:44 | `vector_horizontal_sum` — reduce lanes at the END only |
| float_sum.rs:67-90 | `SumBlock`: sum 128 elems as 16-lane chunks (chunks_exact) |
| filter/scalar.rs:12 | mask-bit loop: `while m > 0` + trailing_zeros (simdjson's flatten!) |
| filter/scalar.rs:90 | 64-element blocks — process a whole mask word |
| filter/avx512.rs:7 | `simd_filter!` macro — the shared loop skeleton |
| filter/avx512.rs:50-60 | `filter_u8_avx512vbmi2`: `_mm512_maskz_compress_epi8` |
| filter/avx512.rs:87-95 | u32 via `_mm512_maskz_compress_epi32` (AVX-512F) |
| filter/mod.rs | dispatch: runtime feature detect → avx512 or scalar |
| min_max/ | same pattern for min/max kernels |

## 1. float_sum: the reduction playbook

Two problems, two fixes:

- **throughput**: one accumulator = one dependency chain. Fix:
  sum `[T; 128]` blocks as `Simd<T, 16>` lanes — 16 chains, reduce
  to scalar only at block end (`vector_horizontal_sum`).
- **accuracy**: naive left-to-right float sum accumulates O(n)
  error. Fix: pairwise recursion above 128 elements — O(log n)
  error, and it's the same tree shape the SIMD blocking already
  built. One design, both wins.

Question: why is the null-masked variant (`_with_mask`) just
`select(mask, x, 0)` + the same sum, and what does that say about
null handling in vectorized engines generally (topic 11's
validity-mask philosophy)?

## 2. filter: three rungs on one skeleton

`simd_filter!` (avx512.rs:7) fixes the loop: load 64 mask bits,
loop vectors of the value type, compress-store, advance out-ptr by
popcount. The compress instruction is the ONLY per-ISA part:

```
 u8  → vbmi2  _mm512_maskz_compress_epi8   (needs Ice Lake+)
 u32 → avx512f _mm512_maskz_compress_epi32
 scalar fallback → while m > 0 { tz = m.trailing_zeros(); ... }
```

The scalar fallback (scalar.rs:12) is itself branch-light: iterate
SET BITS with trailing_zeros instead of testing every element —
selectivity-adaptive for free (low selectivity = few iterations).
Question: at 99% selectivity, which wins — bit-iteration or
copy-everything-then-truncate? What does polars do for the
mostly-true case (look for the `is_simple` / all-set fast path)?

## 3. What NEON gets instead

No vpcompress on ARM. Options polars doesn't need but you do (M17):
simdjson's LUT-shuffle compress (8 lanes max per `vqtbl1q`), or
branchless scalar append (often wins — measure!). This is the
experiments' `filter.rs` stub.

## 4. The dispatch pattern

Runtime `is_x86_feature_detected!` at the kernel boundary — one
branch per 64+ elements, not per element. Compare hashbrown
(compile-time cfg per Group backend) and SimSIMD (function-pointer
tables at init). Question: when is each of the three binding times
right (compile / init / call)?

## Questions for notes.md

1. STRIPE=16 for f32 = 512 bits = 4 NEON registers. Why does a
   WIDER stripe than the vector width still help on ARM (ports ×
   latency)?
2. Pairwise limit 128: derive the error bound difference vs
   left-to-right for n = 10⁸ (hint: ~ε·log₂(n/128) vs ~ε·n).
3. The `simd_filter!` skeleton advances `out` by popcount without
   zeroing skipped lanes. Why is the trailing garbage safe (who
   truncates)?
4. Filter returns (values, validity) — how does the validity BITMAP
   itself get filtered (bit-level compress — the harder problem)?
5. For M17: polars chose NOT to use `std::simd` for filter, only
   intrinsics + scalar. Why does compress specifically defeat
   portable SIMD abstractions?
