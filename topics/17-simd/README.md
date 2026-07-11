# Topic 17 — SIMD & Hardware-Conscious Data Processing

The last 10× on a single core. Topic 11 bought vectorization at the
OPERATOR level; this topic goes down to the LANES. You're on ARM
(Apple Silicon): NEON's 128-bit vectors are the home ISA, AVX2/
AVX-512's 256/512-bit the contrast to know.

## 1. The mental model

```
 scalar:   f32 + f32            → 1 result / instr
 NEON:     f32x4 + f32x4        → 4 results / instr  (128-bit)
 AVX2:     f32x8 + f32x8        → 8                  (256-bit)
 AVX-512:  f32x16 + f32x16      → 16 + per-lane MASKS

 but the real currency is PORTS × LATENCY:
 M-series: 4 FMA ports, ~3cy latency ⇒ need ≥12 independent
 FMA chains in flight to saturate — ONE accumulator uses 1/12
 of the machine. (SimSIMD's headers document exactly this.)
```

Data-parallel loops don't make you fast; *independent dependency
chains* do. SIMD width × ports × latency = the accumulator count
every fast kernel hardcodes.

## 2. Why autovectorization fails (the four classics)

1. **Float reductions**: `xs.iter().sum()` is a serial dependency
   chain — reassociation changes the answer, so LLVM won't (without
   `-ffast-math`). Fix: N explicit accumulators (polars float_sum.rs
   STRIPE=16 blocks + pairwise recursion above 128).
2. **Data-dependent control flow**: `if x > t { out.push(x) }` —
   branches don't vectorize. Fix: branchless append
   (`out[k]=x; k += (x>t) as usize`) or real compress instructions.
3. **Gather/indirection**: `vals[idx[i]]` — hardware gathers exist
   but cost ~1 load/lane anyway (topic 13's pointer-chasing tax).
4. **Aliasing doubt**: the optimizer can't prove in/out don't
   overlap — slices + iterators (not raw pointers) give LLVM the
   guarantee.

## 3. Branchless selection: the filter kernel

The DB kernel (SIGMOD '15's centerpiece). Three shapes:

```
 branchy:     if v[i] < t { out[k++] = v[i] }        ← mispredicts at 50% sel
 branchless:  out[k] = v[i]; k += (v[i] < t) as usize ← always-store, no branch
 compress:    mask = v .< t
   AVX-512:   vpcompressd (polars filter/avx512.rs:59 — hardware)
   NEON:      no compress! 4-bit mask → LUT of shuffle masks →
              vqtbl1q (simdjson arm64/simd.h:267-276 does exactly
              this for 8-byte compaction)
```

Selectivity decides the winner: branchy wins at ~0%/100% (predicted
perfectly), branchless/compress win in the middle. Measure it —
that's the experiments' centerpiece curve.

## 4. Masks and movemask on ARM

x86 `movemask` (bitmask from lanes) has no NEON equivalent —
the idiom is `vshrn` (shift-right-narrow) folding 16 lanes into a
64-bit "4 bits per lane" mask (hashbrown group/neon.rs, memchr's
Vector::movemask). SwissTable = SIMD probing: 16 control bytes per
group, one `vceqq`+narrow gives candidate slots in 2 instructions —
topic 2's hash table, now explained at lane level.

## 5. The masterclass codebases (per reading guide)

- **simdjson**: byte classification via `vqtbl1q` nibble lookups,
  carry-less multiply `prefix_xor` for quote parity — branch-free
  STATE MACHINES over 64-byte blocks.
- **polars-compute**: the production Rust shape — scalar body +
  `#[cfg]` AVX-512 compress + STRIPE'd sums.
- **hashbrown**: one abstraction (`Group`) with sse2/neon/generic
  backends — the portability pattern to copy.
- **SimSIMD**: distance kernels with port/latency tables in the
  comments; multiple ISA files per kernel (haswell/skylake/neon/
  sve...) dispatched at runtime.
- **memchr**: `Vector` trait over ISAs; the 4×-unrolled search loop.
- **Mojo**: `SIMD[type, width]` as a first-class parametric type —
  what `std::simd` wants to be with a compiler behind it.

## 6. FastLanes (bit-packing at SIMD speed)

Topic 12 decoded bit-packed values scalar. FastLanes' trick: an
interleaved "transposed" layout so unpacking any width is the SAME
shift/mask kernel across lanes, no cross-lane shuffles — decode at
memory bandwidth. Our `unpack4` stub is the baby version: 32
nibbles per 16-byte vector via shift+mask, no LUT needed.

## Experiments (`experiments/`)

Four kernels × four rungs (scalar / autovec-friendly / portable
`wide` / NEON intrinsics — `std::simd` is nightly, `wide` is its
stable stand-in):

1. `dot.rs` — dot product. PROVIDED: naive (1 chain) + unrolled-8
   (autovec). YOU implement: `wide` f32x4 and NEON `vfmaq_f32` with
   4 accumulators. Tests: equivalence within 1e-2 relative.
2. `filter.rs` — count + compact under a threshold. PROVIDED:
   branchy + branchless scalar. YOU implement: NEON count
   (`vcltq`+`vshrn` popcount) and LUT-compress compact (simdjson's
   trick, f32 edition). Tests: exact match vs branchy oracle.
3. `unpack.rs` — 4-bit unpack to u32. PROVIDED: scalar (topic 12's).
   YOU implement: NEON shift/mask. Tests: round-trip.
4. `bin/simd_bench` — PROVIDED (runs the provided rungs before
   panicking at stubs): GB/s per rung per kernel; the selectivity
   sweep (1/25/50/75/99%) for filter shapes.

## Reading guides

| guide | chapter |
|---|---|
| [reading-simdjson.md](reading-simdjson.md) | simdjson: parsing without branches |
| [reading-polars-compute.md](reading-polars-compute.md) | polars-compute: shipping SIMD in stable Rust |
| [reading-hashbrown-simd.md](reading-hashbrown-simd.md) | hashbrown & memchr: movemask without movemask |
| [reading-simsimd.md](reading-simsimd.md) | SimSIMD: the port/latency table is the design doc |
| [reading-sigmod15-vectorization.md](reading-sigmod15-vectorization.md) | SIMD for databases: two primitives, four operators |
| [reading-fastlanes.md](reading-fastlanes.md) | FastLanes: bit-unpacking at memory bandwidth |
| [reading-mojo-simd.md](reading-mojo-simd.md) | Mojo's `SIMD[type, width]`: width as a type parameter |

## Capstone M17

- [ ] NEON kernels behind the M11 vectorized runtime: filter
      compact, hash probe, dot/l2 distances (M14's kernels get their
      promised SIMD)
- [ ] scalar fallbacks kept + `is_aarch64_feature_detected!` dispatch
- [ ] bench: engine-level speedup per kernel (not just microbench)
      — record where Amdahl eats the 4×
- [ ] SIMD-ize one topic 12 decoder (bit-unpack) and re-run the
      compression-IS-performance table
