# polars-compute: shipping SIMD in stable Rust

The production-Rust answer to "how do I ship SIMD without a nightly
compiler or per-CPU binaries": autovec-friendly scalar bodies,
explicit `std::simd` where it pays, raw intrinsics only for the one
instruction Rust can't reach (vpcompress). Before the anchors, this
chapter builds the two kernels every engine needs — the reduction
and the filter — concept by concept, as polars actually ships them.

## The problem in one sentence

`xs.iter().sum::<f32>()` on Apple Silicon leaves ~15/16 of the FPU
idle — one accumulator is one serial dependency chain — and the
naive filter loop mispredicts a branch on every unpredictable
element; polars-compute fixes both in stable Rust with two files you
can read in an afternoon.

## The concepts, step by step

### Step 1 — the reduction problem: one accumulator, one chain

A reduction (folding n values into one, like a sum) has a hidden
serial bottleneck: `acc += x[i]` can't start until the previous
`acc += x[i-1]` finished, because each add needs the last add's
result. That's a **dependency chain** — with ~3-cycle add/FMA
latency and 4 vector ports on M-series, a single chain uses 1/12th
of the machine (README §1). And the compiler can't fix it: float
addition isn't associative (reordering changes rounding), so LLVM
won't reassociate `a+b+c+d` into `(a+b)+(c+d)` without
`-ffast-math`. Fast float sums must *explicitly* restructure the
order of additions.

### Step 2 — STRIPE=16: many accumulators, reduced once at the end

polars' `float_sum.rs` sums blocks of 128 elements as 16 parallel
lanes (`STRIPE = 16`, float_sum.rs:13): lane j accumulates elements
j, j+16, j+32, … — sixteen independent chains instead of one, kept
in `Simd<T, 16>` via `chunks_exact` (float_sum.rs:67-90). Lanes are
combined into a single scalar only at block end
(`vector_horizontal_sum`, float_sum.rs:44) — a horizontal reduce is
slow, so you do it once per 128 elements, not once per element.
STRIPE=16 for f32 is 512 bits = 4 NEON registers: *wider than the
vector* on purpose, because the accumulator count is set by
latency × ports, not by vector width.

### Step 3 — pairwise recursion: the same tree fixes accuracy

Naive left-to-right float summation accumulates O(n) rounding error
— each add rounds, and errors compound linearly. Pairwise summation
(add halves recursively, `PAIRWISE_RECURSION_LIMIT = 128`) gets
O(log n) error — and it's the same tree shape the SIMD blocking
already built. One design, both wins: below 128 elements, the
striped block; above, recursive halving. Question: why is the
null-masked variant (`_with_mask`) just `select(mask, x, 0)` + the
same sum, and what does that say about null handling in vectorized
engines generally (topic 11's validity-mask philosophy)?

### Step 4 — the filter problem, and the bit-iteration fallback

A filter (keep elements where a boolean mask is set, packed densely
into the output) is the other universal kernel. The naive
`if keep { out.push(x) }` mispredicts at mid selectivity (the
fraction of elements kept — at 50%, the branch is a coin flip, ~15
cycles per miss). polars' scalar fallback (filter/scalar.rs:12)
sidesteps prediction entirely: process 64 elements per iteration
using their mask *word*, and iterate only the SET bits with
`trailing_zeros` (index of the lowest 1-bit):

```rust
// one 64-element block per mask word; cost ∝ popcount, not 64
fn filter_block(vals: &[T; 64], mut m: u64, out: &mut Vec<T>) {
    while m > 0 {
        let i = m.trailing_zeros() as usize;   // next surviving element
        out.push(vals[i]);
        m &= m - 1;                            // clear lowest set bit
    }
}
```

Cost is proportional to survivors (popcount — the number of set
bits), not to 64: selectivity-adaptive for free, and the loop branch
(`while m > 0`) is highly predictable. This is simdjson's
flatten-bits idiom, value edition.

### Step 5 — the compress instruction: the one thing that needs intrinsics

AVX-512 (x86's 512-bit SIMD extension) has **compress-store**
instructions that do the entire filter in hardware: take a vector
and a mask, write only the selected lanes, packed left. polars wraps
them behind one macro, `simd_filter!` (avx512.rs:7), which fixes the
loop skeleton — load 64 mask bits, loop vectors of the value type,
compress-store, advance the output pointer by popcount:

```
 u8  → vbmi2  _mm512_maskz_compress_epi8   (needs Ice Lake+)
 u32 → avx512f _mm512_maskz_compress_epi32
 scalar fallback → while m > 0 { tz = m.trailing_zeros(); ... }
```

```rust
// AVX-512 replaces the whole scalar loop with one compress-store per vector:
// _mm512_maskz_compress_epi32(mask, v); out_ptr += mask.count_ones();
```

This is the only place polars drops to raw intrinsics — compress is
data-dependent lane *movement*, which no portable abstraction (and
no autovectorizer) can express. Question: at 99% selectivity, which
wins — bit-iteration or copy-everything-then-truncate? What does
polars do for the mostly-true case (look for the `is_simple` /
all-set fast path)?

### Step 6 — what NEON gets instead

No vpcompress on ARM. Options polars doesn't need but you do (M17):
simdjson's LUT-shuffle compress (a table of shuffle patterns indexed
by the mask, 8 lanes max per `vqtbl1q` — reading-simdjson.md step 7),
or the branchless scalar append (`out[k] = x; k += keep as usize` —
often wins; measure!). This is the experiments' `filter.rs` stub.

### Step 7 — dispatch: pay for the feature test once per block

The AVX-512 path exists only on some CPUs, so filter/mod.rs does
runtime dispatch: `is_x86_feature_detected!` at the kernel boundary
— one check per 64+ elements, not per element. Compare the two other
binding times in this topic's codebases: hashbrown binds at COMPILE
time (`cfg_if!` per Group backend), SimSIMD at INIT time
(function-pointer tables filled once). Question: when is each of the
three binding times right (compile / init / call)?

## Where each step lives in the code

| anchor | step | what it is |
|---|---|---|
| float_sum.rs:13-14 | 2–3 | `STRIPE = 16`, `PAIRWISE_RECURSION_LIMIT = 128` |
| float_sum.rs:44 | 2 | `vector_horizontal_sum` — reduce lanes at the END only |
| float_sum.rs:67-90 | 2 | `SumBlock`: sum 128 elems as 16-lane chunks (chunks_exact) |
| filter/scalar.rs:12 | 4 | mask-bit loop: `while m > 0` + trailing_zeros (simdjson's flatten!) |
| filter/scalar.rs:90 | 4 | 64-element blocks — process a whole mask word |
| filter/avx512.rs:7 | 5 | `simd_filter!` macro — the shared loop skeleton |
| filter/avx512.rs:50-60 | 5 | `filter_u8_avx512vbmi2`: `_mm512_maskz_compress_epi8` |
| filter/avx512.rs:87-95 | 5 | u32 via `_mm512_maskz_compress_epi32` (AVX-512F) |
| filter/mod.rs | 7 | dispatch: runtime feature detect → avx512 or scalar |
| min_max/ | — | same pattern for min/max kernels (a second lap, optional) |

Reading order: `float_sum.rs` top to bottom (steps 1–3 in ~100
lines), then `filter/scalar.rs`, then `avx512.rs` with the macro
expanded in your head, then `mod.rs` for the dispatch.

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

## References

**Code**
- [polars](https://github.com/pola-rs/polars) —
  `crates/polars-compute/src/` — start with `float_sum.rs` and
  `filter/` (scalar.rs, avx512.rs, mod.rs); `min_max/` repeats the
  same pattern if you want a second lap
