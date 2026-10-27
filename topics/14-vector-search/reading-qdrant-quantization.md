# The quantization ladder: shrink, search, rescore

Topic 12's thesis — compression IS performance — with a new twist:
here compression is LOSSY, so the system needs machinery to claw the
recall back. This chapter climbs qdrant's compression ladder step by
step — why lossy codes pay, scalar u8 and the score-without-decode
trick, PQ, binary, TurboQuant's rotate-then-quantize family — and
ends with the oversample+rescore pipeline that makes lossy codes
safe; that pipeline shape is what M14 copies. The encoders live in
their own crate, `lib/quantization/src/`; the wiring into search is
`lib/segment/src/vector_storage/quantized/`.

Every `file:line` below was read at **qdrant v1.20.0,
`qdrant/qdrant@222b2e44a`**, the pin in `resources/codebases.md`;
re-check any of them with
`python3 tools/pinned-source.py show qdrant <path> -r A:B`. Where a
number here contradicts folklore (u8 is *not* 256 levels; the
per-vector extra is *not* Σvᵢ), the anchor is given so you can settle
it yourself.

## The problem in one sentence

A million 1536-d f32 embeddings is **6 GB** of vectors that every
HNSW hop pokes at random, so bytes-per-vector is the real cost unit
— but every byte saved is precision lost, and distances computed on
compressed codes return the *wrong nearest neighbours* unless
something puts the recall back.

Work the size claim, because it is the reason the chapter exists:

```
  n = 1 000 000,  d = 1536,  f32
  vectors  = 1e6 × 1536 × 4 B = 6.14 GB
  HNSW links (M=16, from reading-hnsw-paper.md's §4.2.3 arithmetic)
           = 1e6 × 151 B      = 0.15 GB
```

Forty times more bytes in vectors than in graph, and unlike the
graph, the vectors are touched in random order — one cache miss per
distance computation. That ratio is why quantization, not graph
compaction, is the lever.

## The concepts, step by step

### Step 1 — the ladder: four families, one recall knob

> **In:** f32 vectors. **Out:** the four encodings qdrant ships,
> their real compression ratios, and the one property all of them
> must have to be worth anything.

Lossy vector compression trades bytes for distance accuracy. qdrant
ships **four** families — `QuantizationConfig` has four variants,
`Scalar`, `Product`, `Binary` and `Turbo`
(`lib/segment/src/types.rs:1158-1163`):

| scheme | stored per vector (d=128) | ratio | distance on encoded | recall cost |
|---|---|---|---|---|
| scalar u8 | d + 4 = 132 B | 3.88× | integer dot + affine postprocess | small |
| PQ, D* dims/chunk | d/D* bytes; D* ∈ {1,2,4,8,16} | 4×–64× | LUT sums — d/D* lookups | real |
| binary, 1 bit/dim | d/8 = 16 B | 32× | XOR + popcount | large, needs rescore |
| TurboQuant, 8 → 1 bits/dim | 132 B → 24 B | 3.88×–21.3× | rotated codes, per-vector scale factor | small at 8 bits, rescored by default at ≤ 2 |

The ratios are not round numbers, and two of them differ from what
gets repeated. Scalar u8 is **3.88×**, not 4×, because
`ADDITIONAL_CONSTANT_SIZE = size_of::<f32>()`
(`encoded_vectors_u8.rs:23-24`) prepends four bytes per vector —
`get_quantized_vector_size` is `actual_dim + 4`
(`encoded_vectors_u8.rs:732-735`), and `actual_dim` is itself d
rounded up to a multiple of `ALIGNMENT = 16` (:22, :728-730). PQ's
range is **4×–64×**, set by the `CompressionRatio` enum
(`lib/segment/src/types.rs:954-960`), whose variants map to
dimensions per chunk in `get_bucket_size`
(`lib/segment/src/vector_storage/quantized/quantized_vectors.rs:241-251`):
X4→1, X8→2, X16→4, X32→8, X64→16. TurboQuant's row is a range
because its bit width is a knob, `TurboQuantBitSize`
(`types.rs:1115-1122`: 1, 1.5, 2, 4 — the default — and 8 bits);
Step 5 works out its sizes.

Two things make the rungs *fast* rather than merely small.

First, distance must be computable **on the codes** — decoding to
f32 per candidate would eat the savings, since the decode is the same
arithmetic you were trying to avoid. Each rung has its own trick for
this: Step 2's algebraic expansion, Step 3's lookup table, Step 4's
popcount, Step 5's single per-vector scale.

Second, moving fewer bytes *is* the speedup. HNSW is memory-bound —
this topic's brute-force lane manages 117 QPS while running at 1.5 G
multiply-adds per second, which is not an arithmetic problem — so 4×
smaller codes means 4× more of the working set resident at each level
of cache.

The recall each rung loses is recovered by one shared mechanism,
Step 6's pipeline, which is why the riskier rungs are usable at all.

### Step 2 — scalar u8: the affine trick, and scoring without decode

> **In:** an f32 vector and a per-index scale/offset pair. **Out:**
> one byte per dimension plus one f32 correction term, and a dot
> product computed entirely in integers.

Scalar quantization maps each f32 dimension onto a small integer
through an affine transform. `alpha` (the scale) and `offset` live
in `MetadataInt8` (`encoded_vectors_u8.rs:85-91`, fields at :87-88),
and the encode is two lines:

```rust
// encoded_vectors_u8.rs — encode_value and postprocess_score, 94-103.
    94      #[inline]
    95      pub fn encode_value(&self, value: f32) -> u8 {
    96          let i = (value - self.offset) / self.alpha;
    97          i.clamp(0.0, 127.0).round() as u8
    98      }
    99
   100      #[inline]
   101      fn postprocess_score(&self, score: f32, query_offset: f32, vector_offset: f32) -> f32 {
   102          self.multiplier * score + query_offset + vector_offset
   103      }
```

The line to focus on is 97: the clamp is to **127**, not 255. It is a
`u8` container holding a **7-bit** code, so there are **128
distinguishable levels per dimension, not 256** — the widely repeated
"256 levels" figure is wrong for this code path. The range fitting
confirms it: `alpha_offset_from_min_max` (:629-633) sets
`alpha = (max - min) / 127.0` and `offset = min`. The payoff is
named in the code, in the doc comment of the AVX-512 VNNI kernel at
:905-910. **VNNI** (Vector Neural Network Instructions) adds
`vpdpbusd`, one instruction that multiplies 64 pairs of bytes —
one operand read as *unsigned*, the other as *signed* — and adds the
products into 32-bit accumulators. A code in `0..=127` reads the same
either way, so *"`vpdpbusd`'s unsigned-by-signed byte products are
exact"* and the kernel returns the same integer as the scalar loop
`impl_score_dot` (:1002-1010). `score_bytes` picks the widest kernel
the CPU has at :877-893 — AVX-512 VNNI, AVX-VNNI, AVX2, SSE4.1 — and
the AVX2/SSE/NEON ones are `unsafe extern "C"` (:1022-1035),
implemented in C rather than Rust.

The clever part is scoring WITHOUT decode. Expand the dot product of
two decoded vectors algebraically — qdrant writes the expansion in
its own comments at :223-231:

```
 dot(q, v) ≈ Σ (α·qᵢ + off)(α·vᵢ + off)
           = α² Σ qᵢvᵢ  +  α·off·(Σqᵢ + Σvᵢ)  +  d·off²
             ↑ integer dot    ↑ per-vector term   ↑ index constant
```

Only the first term depends on both operands, so only it has to be
computed per candidate — and it is an integer dot product over
bytes. The `multiplier` that scales it back is chosen per metric:

```rust
// encoded_vectors_u8.rs — the per-metric multiplier, 222-232.
   222          let multiplier = match vector_parameters.distance_type {
   223              // (alpha*x - offset) * (alpha*y - offset) = alpha^2*x*y - alpha*offset*x - alpha*offset*y + offset^2
   224              // multiplier is applied to xy term only, so we need to multiply score by alpha^2
   225              DistanceType::Dot | DistanceType::Cosine => alpha * alpha,
   226              // |(alpha*x - offset) - (alpha*y - offset)| = alpha*|x - y|
   227              // multiplier is applied to |x - y| term only, so we need to multiply score by alpha
   228              DistanceType::L1 => alpha,
   229              // ((alpha*x - offset) - (alpha*y - offset))^2 = alpha^2*(x - y)^2 = alpha^2*x^2 - 2*alpha^2*xy + alpha^2*y^2
   230              // multiplier is applied to (x - y)^2 term only, so we need to multiply score by -2*alpha^2
   231              DistanceType::L2 => -2.0 * alpha * alpha,
   232          };
```

Now the correction. It is **not** a stored `Σvᵢ`, as is often
claimed — everything metric-dependent is folded into a *single* f32
written at the front of each encoded vector:

```rust
// encoded_vectors_u8.rs — the per-vector correction term, 268-290,
// with the invert branch at 282-286 elided.
   268              let vector_offset = match vector_parameters.distance_type {
   269                  DistanceType::Dot | DistanceType::Cosine => {
   270                      let elements_sum = encoded_vector.iter().map(|&x| f32::from(x)).sum::<f32>();
   271                      elements_sum * alpha * offset
   272                  }
   273                  DistanceType::L1 => 0.0,
   274                  DistanceType::L2 => {
   275                      let elements_sqr_sum = encoded_vector
   276                          .iter()
// ... 277-278: .map(|&x| f32::from(x) * f32::from(x)).sum::<f32>() ...
   279                      elements_sqr_sum * alpha * alpha
   280                  }
   281              };
// ... 282-286: negate if vector_parameters.invert ...
   287              // apply `a^2` shift
   288              let vector_offset = metadata.get_shift() + vector_offset;
   289              encoded_vector[0..ADDITIONAL_CONSTANT_SIZE]
   290                  .copy_from_slice(&vector_offset.to_ne_bytes());
```

`elements_sum` at :270 is Σ of the *encoded* bytes, immediately
multiplied by `alpha·offset` (:271). The line to focus on is 288: it
adds the whole-index constant from `get_shift()` (:116-134), which is
`d·off²` for Dot and Cosine (:121-123) and **0** for L1 and L2 —
the comment at :124-126 explains that for L2 the common offset
cancels before squaring, so adding an absolute `off²` would make
scores depend on translating every vector by the same constant. The
result is stored as the first four bytes at :289-290. For L2 the
per-vector term is a sum of squares instead (:274-280). So what sits
in front of each vector is one already-scaled f32, not a raw sum —
and `postprocess_score` (:101-103) is a single fused
multiply-add-add.

```rust
// ILLUSTRATION — not quoted from any file; this is the algebra above
// in one function. The real code is split: the integer loop is
// encoded_vectors_u8.rs:1002-1010 (impl_score_dot), the correction is
// :101-103 (postprocess_score), and the SIMD kernels are chosen at
// :877-893.
fn dot_u8(q: &Encoded, v: &Encoded, multiplier: f32) -> f32 {
    let int_dot: i32 = q.codes.iter().zip(&v.codes)
        .map(|(&a, &b)| a as i32 * b as i32)
        .sum();                                 // the byte loop SIMD loves
    multiplier * int_dot as f32                 // alpha² for Dot/Cosine
        + q.correction                          // one f32, prepended
        + v.correction                          // to each encoded vector
}
```

Work the storage:

```
  d = 128, f32 source
  actual_dim = 128 rounded up to a multiple of ALIGNMENT(16) = 128
  stored     = actual_dim + ADDITIONAL_CONSTANT_SIZE = 128 + 4 = 132 B
  original   = 128 × 4                                        = 512 B
  ratio      = 512 / 132                                      = 3.88×

  d = 100 (not a multiple of 16)
  actual_dim = 112,  stored = 116 B,  original = 400 B → 3.45×
```

One refinement worth knowing: `find_quantile_interval`
(`lib/quantization/src/quantile.rs:34-79`) can pick alpha and offset
from a quantile of a *sample* rather than from the full min/max, so
a single outlier dimension does not spend the whole 0..127 range on
empty space. It samples up to `SAMPLE_SIZE = 5_000` vectors (:8,
:288), flattens them into one list of values, and trims `cut_index`
values off each tail (:61-65) before taking min and max. Note what
`cut_index` counts: `selected_vectors_count × (1 − quantile) / 2`
(:63) uses the number of *vectors*, but it is applied to the
flattened *values*. Work it at d = 128:

```
  sample            5 000 vectors × 128 dims = 640 000 values
  quantile = 0.95   cut_index = 5 000 × (1 − 0.95) / 2 = 125 values
  trimmed per tail  125 / 640 000 = 0.0195% of the values, not 2.5%
```

`ScalarQuantizationConfig.quantile`
(`lib/segment/src/types.rs:975-978`) is the user-facing knob,
validated to [0.5, 1.0], and :41 returns early — keeping the full
min/max — for fewer than 127 vectors or a quantile of 1.0. Only
vectors flagged as real are used for the fit (`encode`'s doc comment,
`encoded_vectors_u8.rs:144-148`), so the placeholders left by deleted
vectors cannot stretch the range.

Recall cost: small — which is why scalar is the default rung for
HNSW.

### Step 3 — product quantization: bytes per vector, not per dimension

> **In:** the full PQ derivation from [reading-pq.md](reading-pq.md).
> **Out:** where each piece of it is in qdrant, and the one property
> that makes PQ riskier inside a graph than inside a flat scan.

PQ splits the vector into chunks and replaces each chunk with the id
of its nearest learned centroid. qdrant's constants are at the top of
the file: `CENTROIDS_COUNT = 256` (`encoded_vectors_pq.rs:33`) so
each chunk codes as exactly one byte, with codebooks from k-means
over a `KMEANS_SAMPLE_SIZE = 10_000` sample (:30), capped at
`KMEANS_MAX_ITERATIONS = 100` (:31) and `KMEANS_ACCURACY = 1e-5`
(:32). Sampling rather than full-corpus training is BtrBlocks-style
(topic 12). A vector's code is one byte per chunk:
`get_quantized_vector_size` (:676-678) counts `dim` stepped by the
chunk size.

Scoring is ADC — **asymmetric distance computation**: the query stays
in f32 and only the stored side is coded. `EncodedQueryPQ` (:41-46)
holds a `lut: Vec<f32>` whose doc comment says exactly what it is —
*"Lookup table is a distance from each query chunk to each centroid
related to this chunk"*. `encode_query` (:520-542) builds it, sizing
it at :521 as `vector_division.len() * centroids.len()` and filling
each cell at :523-540 with an exact sub-distance. `Metadata` (:48-53)
holds the codebooks: `centroids` at :50 and `vector_division` — the
chunk ranges — at :51. Note qdrant stores each of the 256 centroids
as a *full-dimension* vector and slices it per chunk at :527, rather
than storing m separate small codebooks.

The scan (`score_point_simple`, :479-494) is one f32 load per code
byte, strided by `centroids_count`, summed; :410-444 and :446-477 are
the SSE and NEON versions, unrolling four chunks at a time.

Put qdrant's compression settings into the LUT arithmetic:

```
  LUT bytes = (d / D*) × 256 × 4

  d = 128, X64 (D* = 16) → m =  8 chunks →   8 kB  ✓ L1
  d = 128, X32 (D* =  8) → m = 16 chunks →  16 kB  ✓ L1 (32-48 kB typical)
  d = 128, X4  (D* =  1) → m = 128 chunks → 128 kB  ✗ L2 at best
```

So the highest-fidelity PQ setting is the one whose lookup table
stops fitting in L1 — the trade is not monotonic, and §II-B of the PQ
paper warns about precisely this.

The cost that is specific to graphs: PQ makes distances approximate
**everywhere**, including inside HNSW traversal, so wrong distances
mean wrong hops and the errors compound along the walk. A flat IVF
scan uses approximate distances only to *rank* a fixed candidate set,
so an error changes a position; in a graph an error changes which
candidates you ever see. That asymmetry is question 2, and it is why
qdrant defaults to scalar for HNSW and reaches for PQ mainly in
memory-starved setups.

### Step 4 — binary: one bit per dimension

> **In:** an f32 vector. **Out:** d bits, a Hamming distance, and the
> reason this rung is unusable without Step 6.

The bottom rung keeps only the sign of each dimension:
`EncodedVectorsBin` (`encoded_vectors_binary.rs:27`). At one bit per
dimension the ratio against f32 is `32/1 = 32×`.

Two corrections to the folk version, both from the pinned file.
First, binary quantization here is a *family*, not a single scheme:
`Encoding` (:35-40) offers `OneBit`, `TwoBits` and `OneAndHalfBits`,
which store d, 2d and ⌈1.5d⌉ bits (:882-886), so "32×" is the
`OneBit` default and the others are 16× and about 21×. Second, the
query need not use the same encoding as storage: `QueryEncoding`
(:49-54) offers `SameAsStorage`, `Scalar4bits` and `Scalar8bits`,
which is the PQ paper's *asymmetric* idea applied to bits — keep the
stored side at one bit, spend more on the query, get better ranking
for free (`xor_popcnt_scalar`, :320, and the dispatch in
`encode_query_vector` at :733-745).

The symmetric case collapses to **Hamming distance** — the number of
bit positions where two codes differ — computed as XOR plus popcount
(population count, the number of set bits). Single-vector storage
packs the bits into `u128` words (`quantized_vectors.rs:257-258`,
:283-285), so the implementation that runs is the `u128` one:

```rust
// encoded_vectors_binary.rs — BitsStoreType::xor_popcnt for u128,
// 455-503, with the NEON branch at 487-496 elided.
   455  impl BitsStoreType for u128 {
   456      fn xor_popcnt(v1: &[Self], v2: &[Self]) -> usize {
   457          debug_assert!(v1.len() == v2.len());
   458
   459          #[cfg(target_arch = "x86_64")]
   460          if is_x86_feature_detected!("avx512vl")
   461              && is_x86_feature_detected!("avx512vpopcntdq")
// ... 462-465: the avx2 / avx / sse4.1 / sse2 checks ...
   466          {
   467              unsafe {
   468                  return impl_xor_popcnt_avx512_uint128(
   469                      v1.as_ptr().cast::<u8>(),
   470                      v2.as_ptr().cast::<u8>(),
   471                      v1.len() as u32,
   472                  ) as usize;
   473              }
   474          }
   475
   476          #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
   477          if is_x86_feature_detected!("sse4.2") {
   478              unsafe {
   479                  return impl_xor_popcnt_sse_uint128(
   480                      v1.as_ptr().cast::<u8>(),
   481                      v2.as_ptr().cast::<u8>(),
   482                      v1.len() as u32,
   483                  ) as usize;
   484              }
   485          }
// ... 486-497: the NEON branch ...
   498          let mut result = 0;
   499          for (&b1, &b2) in v1.iter().zip(v2.iter()) {
   500              result += (b1 ^ b2).count_ones() as usize;
   501          }
   502          result
   503      }
```

The line to focus on is 500, the portable fallback that states the
whole algorithm; the kernels above it do the same on wider registers
— :460-461 dispatch to AVX-512's `vpopcntdq`, a per-lane popcount
instruction, when the CPU has it. Count the work for a realistic
embedding, since the usual "~48 ops" claim does not survive division:

```
  d = 1536, OneBit
  bits      = 1536
  bytes     = 1536 / 8              = 192 B
  u128 words= 192 / 16              = 12          ← v1.len() at :471 / :482
  u64 words = 192 / 8               = 24          ← 24 XORs + 24 popcounts

  compare f32: 1536 × 4 = 6144 B and 1536 multiply-adds
```

Twelve 128-bit words against 1 536 multiply-adds, on 192 bytes
instead of 6 144. (Multi-vector storage packs into `u8` instead; that
impl, :326-379, picks 128-, 64- or 32-bit SSE kernels by length.)

The recall cost is large by construction — one bit cannot rank close
neighbours, only separate hemispheres — so binary is only sane WITH
rescoring, and mainly for high-dimensional embeddings (1024-d and
up) where the sign pattern still carries most of the angular
information.

### Step 5 — TurboQuant: rotate first, then quantize

> **In:** an f32 vector. **Out:** a code of 1 to 8 bits per dimension
> on a rotated copy of it, one or two f32 correction terms, and the
> reason a rotation makes a crude per-coordinate grid work.

Steps 2-4 quantize each coordinate where it stands, so a vector whose
energy sits in a few coordinates wastes the grid on the rest — Step
2's quantile trimming exists to fight exactly that. TurboQuant
removes the problem first. `TurboQuantizer::new`
(`lib/quantization/src/turboquant/quantization.rs:133-168`) builds a
**Hadamard rotation** (:159). A **Walsh–Hadamard transform** (WHT)
replaces a block of coordinates with signed sums of all of them —
each output is ±x₁ ± x₂ ± … over the block, scaled so lengths are
kept. `wht_and_gather_rounds` (`turboquant/rotation.rs:90-127`) runs
one normalised WHT over the vector's chunks (:108), then three
rounds of a fixed, seeded permutation followed by another WHT
(:113-120; the seeds are at :4-10), so every output coordinate ends
up a mix of many input coordinates. Every stage is orthogonal, so the
rotation preserves lengths and dot products, and distances computed
in the rotated space are the distances you wanted. What it changes is the
*shape*: energy is spread evenly, and after `preprocess_into` rescales
the rotated vector to length √(padded_dim) (:206-217) each coordinate
behaves like a draw from a standard normal distribution, N(0, 1).

That predictable shape is what the codes exploit. The 1-, 2- and
4-bit widths quantize each coordinate to the nearest of 2, 4 or 16
fixed centroids — a **Lloyd-Max codebook**, the set of levels that
minimises mean squared error for a known distribution, here N(0, 1)
(`turboquant/lloyd_max.rs:3-16`: ±0.798 for 1 bit, up to ±2.733 for
4 bits). For those widths qdrant runs **TQ+** mode
(`quantized_vectors.rs:196-201`): a per-coordinate shift and scale,
fitted on a sample, that pulls each rotated coordinate onto the
codebook's grid before snapping (`quantization.rs:252-266`). The
8-bit width uses no codebook at all — `TQBits::Bits8`'s doc says
*"Uniform 8-bit grid with a per-vector scale instead of a Lloyd-Max
codebook"* (`turboquant/mod.rs:18-20`):

```rust
// turboquant/quantization.rs — quantize_impl's grid scaling for the
// 8- and 16-bit widths, 268-279.
   268          // Bits8 / Bits16: scale onto the uniform grid `[−g, g]` by the
   269          // vector's own max magnitude — no clipping, and the scale cancels in
   270          // the renormalized score (`scaling_factor = l2 / ‖grid values‖`).
   271          if let Some(grid_max) = self.bits.grid_max() {
   272              let abs_max = buf.iter().fold(0.0_f64, |m, &x| m.max(x.abs()));
   273              if abs_max > 0.0 {
   274                  let grid_scale = grid_max / abs_max;
   275                  for v in buf.iter_mut() {
   276                      *v *= grid_scale;
   277                  }
   278              }
   279          }
```

`grid_max` is 127 for 8 bits (`turboquant/mod.rs:37`), so the line
to focus on, 274, maps the largest rotated coordinate to exactly
±127. Run it on four rotated coordinates:

```
  rotated, rescaled buf   [ 0.5,  -2.0,  1.1,  0.25 ]
  abs_max                 2.0
  grid_scale              127 / 2.0 = 63.5
  scaled                  [ 31.75, -127.0, 69.85, 15.875 ]
  stored code             [ 32,    -127,   70,    16    ]   round().clamp(-127, 127)
```

(The rounding happens when the code is packed, at
`turboquant/encoding.rs:133-139`.)

No coordinate is ever clipped, and the scale itself is not stored:
:269-270 note that it cancels, because the score is multiplied by a
per-vector `scaling_factor = l2 / ‖grid values‖` stored beside the
code. This is also where the rotation pays twice: without it, one
outlier coordinate would set `abs_max` and squeeze every other
coordinate into a few grid steps.

Work the sizes. `quantized_size_for`
(`turboquant/encoding.rs:211-221`) is the packed codes plus the
per-vector extras: codes are `padded_dim × bits / 8` bytes, where
`padded_dim` (:224-233) rounds d up to fill whole bytes (and is ×1.5
for the 1.5-bit width, which gets its extra precision by rotating
into that padding); extras (:33-55) are 4 bytes of scaling factor,
4 more for L2's length, and 4 more for TQ+'s correction term:

```
  d = 128, Dot distance, original 512 B

  bits  mode    padded_dim  codes         extras   total   ratio
  8     Normal  128         128 B         4 B      132 B   3.88×
  4     TQ+     128          64 B         8 B       72 B   7.11×
  2     TQ+     128          32 B         8 B       40 B   12.8×
  1.5   TQ+     192          24 B         8 B       32 B   16.0×
  1     TQ+     128          16 B         8 B       24 B   21.3×
```

At 8 bits TurboQuant costs exactly what scalar u8 costs (132 B, Step
2) but spends it on a full signed 8-bit grid instead of a 7-bit one,
on rotated coordinates, with no quantile to tune. Two more facts
finish the family. Only TurboQuant distinguishes Cosine from Dot
(`quantized_vectors.rs:224-232`; the other three families score
Cosine as Dot). And it decides its own default for Step 6:
`tq_bits_default_rescoring` (:203-208) turns rescoring on by default
for 1, 1.5 and 2 bits and off for 4 and 8. The same encoder also
backs the `Turbo4`, `Turbo8` and `Turbo16` vector *storage* datatypes
(`types.rs:2104-2109`), where the original vectors themselves are
kept TurboQuant-coded rather than as f32.

### Step 6 — oversample + rescore: the recall clawback

> **In:** a quantized index (Steps 2-5) that returns
> approximately-ranked results. **Out:** exactly-ranked results, for
> a per-query cost that does not scale with n.

The shared safety net: search the quantized index for MORE than you
need, then re-rank the shortlist with the exact vectors.

```
 query ──► HNSW over quantized codes ──► top·x candidates
                                           │ rescore with f32
                                           ▼
                                         top k
```

The multiplier comes from `get_oversampled_top`. The call sites are
`lib/segment/src/index/hnsw_index/hnsw/read_view/search.rs:56`
(graph search) and :249 (plain search), but the **definition is in a
different module** — `lib/segment/src/index/vector_index_search_common.rs:26-44`
— which is worth knowing before you go looking:

```rust
// vector_index_search_common.rs — get_oversampled_top, 26-44.
    26  pub fn get_oversampled_top<Q: QuantizedVectorsRead>(
    27      quantized_storage: Option<&Q>,
    28      params: Option<&SearchParams>,
    29      top: usize,
    30  ) -> usize {
    31      let quantization_enabled = is_quantized_search(quantized_storage, params);
// ... 32-37: read params.quantization.oversampling, else the default ...
    38      match oversampling_value {
    39          Some(oversampling) if quantization_enabled && oversampling > 1.0 => {
    40              (oversampling * top as f64) as usize
    41          }
    42          _ => top,
    43      }
    44  }
```

Three guards at :39: quantization has to be on, the factor has to
exceed 1.0, and it has to be set at all — the default,
`default_quantization_oversampling_value()`
(`lib/segment/src/types.rs:642-644`), is `None`, so without an
explicit factor `top` passes through unchanged (:42).
`is_quantized_search` (:14-24) is where the `exact` and `ignore`
search params turn quantization off entirely.

The shortlist is re-ranked in `postprocess_search_result`:

```rust
// vector_index_search_common.rs — postprocess_search_result's rescore,
// 60-87, with the scorer construction at 72-78 elided.
    60      let quantization_enabled = is_quantized_search(quantized_vectors, params);
    61
    62      let default_rescoring = quantized_vectors
    63          .as_ref()
    64          .map(|q| q.default_rescoring())
    65          .unwrap_or(false);
    66      let rescore = quantization_enabled
    67          && params
    68              .and_then(|p| p.quantization)
    69              .and_then(|q| q.rescore)
    70              .unwrap_or(default_rescoring);
    71      if rescore {
// ... 72-79: a FilteredScorer over the original f32 storage, no quantized vectors, no filter ...
    80          search_result = scorer
    81              .score_points(&mut search_result.iter().map(|x| x.idx).collect_vec(), 0)?
    82              .collect();
    83          search_result.sort_unstable();
    84          search_result.reverse();
    85      }
    86      search_result.truncate(top);
    87      Ok(search_result)
```

The line to focus on is 70: whether to rescore is the query's
`rescore` param if given, else the storage's own default —
`QuantizedVectors::default_rescoring`
(`vector_storage/quantized/quantized_vectors/accessors.rs:15-54`)
returns `false` for scalar and PQ, `true` for binary, and Step 5's
per-width answer for TurboQuant. Then :80-84 score the shortlist
exactly and :86 trims it back to `top`.

The arithmetic that makes it nearly free:

```
  top = 10,  oversampling = 4.0,  d = 1536
  candidates rescored          = (4.0 × 10) = 40
  f32 work for the rescore     = 40 × 1536 = 61 440 multiply-adds
  code-distance work already done by an ef=64 HNSW walk
                               ≈ thousands of candidates × 192 B each

  and the brute-force alternative for n = 1e6:
       1e6 × 1536 = 1.54 × 10⁹ multiply-adds — 25 000× more
```

Forty exact distances is noise. And note what the weaker demand
buys: quantization error only has to keep the true neighbours
*inside the top 40*, which is a far weaker requirement than ranking
them correctly. This is late materialization (topic 12): cheap
representation for the scan, expensive one only for survivors.
`lib/segment/src/vector_storage/quantized/quantized_scorer_builder.rs`
picks the scorer per collection config, and the RAM/mmap/chunked
storage variants live beside it.

### Step 7 — recall is a property of decisions, not of average distance error

> **In:** Step 6's oversample-and-rescore pipeline and the error numbers of
> Steps 2–5. **Out:** the 2026 argument that quantization damages a graph search
> only where a comparison's margin is small, with its measured evidence and its
> stated scope — and what that says about how to choose a rung.

Every rung above is compared by *fidelity*: how closely the quantized distances
track the exact ones, summarised as mean squared error, a rank correlation, or
end-to-end recall. Xiao and Cao (*When Does Low-Bit Quantization Preserve the
Decisions of Vector Search?*, [arXiv:2609.09854](https://arxiv.org/abs/2609.09854),
v1, Sep 2026) start from a puzzle: the same two-bit coordinate code that the
authors report recovers 95 % of exact neighbours on Cohere text embeddings
recovers 2 % on GIST image descriptors (abstract; the figures come from their
earlier QuIVer paper, cited in §1, not from this one). Average distortion does
not explain the gap, they argue, because a graph search never consumes a
distance, only **comparisons** — "is candidate x closer than candidate y?" — and
a comparison fails only when quantization noise crosses the exact boundary it sits
on.

**The decomposition.** Let Ψ be the exact signed quantity a comparison decides
(for ranking, the difference of two distances), Γ = |Ψ| its **margin**, and R the
**calibrated residual**, the quantized quantity minus c times the exact one for a
fitted scale c (§3.1). A **flip** is the quantized comparison disagreeing with the
exact one. Theorem 2 (§3.1) says, for any threshold τ > 0 and with no assumption
about how R and Ψ relate,

  P(flip) ≤ P(0 < Γ ≤ τ) + P(|R| ≥ c·τ):

the chance a comparison is *near a tie*, plus the chance the noise is *large*. A
global fidelity metric averages over both and so cannot tell them apart; a
well-correlated code can still flip many comparisons if the candidates a search
actually compares are close together, which is exactly what candidate lists are.

**Two worked numbers from the paper.**

1. *Shared error cancels* (§1, Example 1; Cohere, two-bit codes). Two distances
   in a nearest-neighbour comparison have calibrated errors with standard
   deviations 0.024 and 0.027 and a pooled correlation of 0.87. The variance of
   their difference is 0.024² + 0.027² − 2 · 0.87 · 0.024 · 0.027 = 0.000576 +
   0.000729 − 0.001128 = 0.000177, a standard deviation of 0.0133 — the paper's
   0.013. Ignoring the covariance gives √(0.000576 + 0.000729) = 0.036, 7.4× the
   variance (0.001305 / 0.000177), the "independence ratio" of §3 and Table 2
   (7.42×). So a comparison is *less* noisy than either of its distances, because
   both distances share a query and its quantization error.
2. *Flip rate falls with standardized margin* (§8.2, Table 6; 4,960 held-out
   decisions, Cohere-768 two-bit codes). Standardized margin M = c·Γ / v divides
   the margin by the residual's scale v, so M = 2 means the exact gap is twice the
   noise level:

| M | < 0.25 | 0.25–1 | 1–2 | 2–3 | 3–5 | > 5 |
|---|---|---|---|---|---|---|
| decisions | 70 | 381 | 1,210 | 1,577 | 1,413 | 309 |
| flip rate | 48.6 % | 25.7 % | 9.50 % | 2.54 % | 0.14 % | 0.00 % |

Weighting by decisions, the overall flip rate is (0.486·70 + 0.257·381 +
0.095·1,210 + 0.0254·1,577 + 0.0014·1,413) / 4,960 ≈ 5.8 % — dominated by the
three small-margin bins that hold 1,661 of the 4,960 decisions. The rate drops
by more than two orders of magnitude and the paper observed no flips above M = 5.

**Predictive power.** Across 24 held-out dataset-and-quantizer configurations
(12 datasets, two-bit codes with and without rotation), the boundary-plus-tail
predictor tracks unseen flip rates with Spearman correlation 0.970 for ranking and
0.992 for pruning, whereas each configuration's global distance Spearman gives
0.739 and 0.053, and its mean squared error 0.184 and 0.447 (§8.2). "Fidelity is
a weak predictor of ranking risk and no predictor of pruning risk; the
standardized margin predicts both."

**Rotation is not a free win.** Step 5 rotates first so coordinates look alike.
The paper's rotation duality (§5.4): a random rotation equalises coordinate
variances, which destroys the structure a fixed *coordinate* code exploits and is
exactly what RaBitQ's randomised codebook needs. Measured effect on Cohere
(abstract): rotation raises global rank fidelity from 0.58 to 0.93 while leaving
one held-out flip rate unchanged and raising another by half — fidelity went up
and decision risk did not go down. Table 2 shows the mechanism for the shared
error: rotated two-bit codes have residual correlation 0.435 and independence
ratio 1.77×, against 0.870 and 7.42× unrotated, so rotation removes the
cancellation that was helping.

**Scope, stated by the authors (§1).** The theory covers fixed candidate sets and
frozen execution traces; end-to-end recall "additionally depends on candidate
coverage", which needs separate graph-expansion arguments and is outside the
paper. The 95 % / 2 % gap is therefore explained *per comparison*, not derived as
a recall formula; the quoted evidence is on 12 embedding sets, with 80 calibration
and 160 held-out queries each and fixed 32-neighbour candidate sets (§8.1).

**What this adds to Step 6.** Oversample-and-rescore is the remedy this analysis
predicts: it only requires the true neighbours to land inside the top
`oversampling × top`, so only flips among *near-tied* candidates matter, and the
exact rescore fixes the order inside the shortlist. The practical reading for
choosing a rung is to measure flip rate by margin bin on your own candidate lists,
not mean error on random pairs; a rung that looks fine on a global metric can still
flip the comparisons your search makes.

## Where each step lives in the code

All at qdrant v1.20.0, `qdrant/qdrant@222b2e44a`.

**Encoders** (`lib/quantization/src/`):

| step | anchors |
|---|---|
| 2 scalar | `encoded_vectors_u8.rs:22-24` ALIGNMENT / ADDITIONAL_CONSTANT_SIZE, `:85-91` MetadataInt8, `:94-98` encode_value (**clamp to 127**), `:100-103` postprocess_score, `:116-134` get_shift, `:222-232` the per-metric multiplier, `:268-290` the folded correction term, `:629-633` alpha from min/max, `:728-735` actual_dim and `+ 4`, `:877-893` kernel dispatch, `:905-913` the VNNI kernel and why 127 makes it exact, `:1002-1010` impl_score_dot, `:1022-1035` the C SIMD kernels |
| 2 range | `quantile.rs:34-79` find_quantile_interval; `:41` the early return; `:61-65` cut_index; `:282-310` take_random_vectors |
| 3 PQ | `encoded_vectors_pq.rs:30-33` k-means constants and CENTROIDS_COUNT, `:35` EncodedVectorsPQ, `:41-46` EncodedQueryPQ (the LUT), `:48-53` Metadata (centroids at :50, vector_division at :51), `:479-494` the ADC scan, `:520-542` encode_query, `:676-678` code size |
| 4 binary | `encoded_vectors_binary.rs:27` EncodedVectorsBin, `:35-40` Encoding (OneBit / TwoBits / OneAndHalfBits), `:49-54` QueryEncoding, `:313`/`:320` the trait methods, `:326-379` the u8 impl, `:455-503` the u128 impl with AVX-512 vpopcntdq, `:733-745` query-encoding dispatch, `:878-888` code size |
| 5 TurboQuant | `turboquant/mod.rs:17-41` TQBits and grid_max; `turboquant/quantization.rs:133-168` new (rotation at :159), `:206-217` length rescale, `:252-266` TQ+ shift/scale, `:268-279` the 8-bit grid; `turboquant/lloyd_max.rs:3-30` codebooks; `turboquant/rotation.rs:4-30,90-127` the Hadamard rotation; `turboquant/encoding.rs:33-55,211-233` sizes |

**Wiring** (`lib/segment/src/`):

| step | anchors |
|---|---|
| 1 config | `types.rs:954-960` CompressionRatio, `:964-967` ScalarType, `:971-988` ScalarQuantizationConfig, `:1021` ProductQuantizationConfig, `:1115-1122` TurboQuantBitSize, `:1158-1163` QuantizationConfig, `:2104-2109` Turbo storage datatypes |
| 3 ratio → D* | `vector_storage/quantized/quantized_vectors.rs:241-251` get_bucket_size |
| 4/5 sizes and modes | `vector_storage/quantized/quantized_vectors.rs:196-208` TQ mode and default rescoring, `:224-232` Cosine only for TQ, `:253-297` per-family vector size (u128 for single-vector binary) |
| 6 pipeline | `index/vector_index_search_common.rs:14-24` is_quantized_search, **`:26-44` get_oversampled_top** (the calls at `index/hnsw_index/hnsw/read_view/search.rs:56,249` are not the definition), `:47-88` postprocess_search_result; `vector_storage/quantized/quantized_vectors/accessors.rs:15-54` default_rescoring; `types.rs:642-644` default oversampling; `vector_storage/quantized/quantized_scorer_builder.rs` scorer selection |

Read order: `encoded_vectors_u8.rs` end to end first (it carries the
score-without-decode idea), then `get_oversampled_top` and
`postprocess_search_result`, then PQ, binary and TurboQuant as
variations.

## Questions (answer in notes.md)

1. Derive the u8 dot-product expansion above; what must be stored
   per vector for it to work? (Σvᵢ.)
2. Why does PQ hurt HNSW traversal more than it hurts a flat IVF
   scan? (Where do approximate distances compound?)
3. Binary quantization of a 1536-d embedding vs u8 of a 128-d one:
   bytes, distance cost, expected recall — which needs more
   oversampling and why?
4. The ADC lookup table is [m × 256] f32. For d=128, m=16: does it
   fit in L1? What happens to the trick when m=64?
5. M14 decision: which rung of the ladder for graph node embeddings,
   given M17 SIMD comes later — commit + reason.
6. Two quantizers have the same mean distance error. Why can one flip far
   more of a graph search's comparisons than the other? Use the margin
   decomposition, and say which measurement on your own data would tell them
   apart.

## Done when

Answer each before unfolding it.

- [ ] You can name the four families of the ladder and the compression ratio each achieves.
  <details><summary>Answer</summary>

  `QuantizationConfig` (`types.rs:1158-1163`) has `Scalar`,
  `Product`, `Binary` and `Turbo`. Scalar u8: one byte per dimension
  plus a 4-byte per-vector constant, so `512/132 = 3.88×` at d=128,
  not the round 4× — the extra is `ADDITIONAL_CONSTANT_SIZE`
  (`encoded_vectors_u8.rs:23-24`, size at :732-735), and `actual_dim`
  is d rounded up to a multiple of 16 (:728-730). PQ: one byte per
  chunk, with `CompressionRatio` X4…X64 (`types.rs:954-960`) mapping
  to 1, 2, 4, 8, 16 dimensions per chunk
  (`quantized_vectors.rs:241-251`), so **4×–64×**. Binary: one bit
  per dimension, `32×` for the `OneBit` default — but
  `encoded_vectors_binary.rs:35-40` also offers `TwoBits` (16×) and
  `OneAndHalfBits` (about 21×), so "binary = 32×" needs the
  qualifier. TurboQuant: 8, 4, 2, 1.5 or 1 bits per rotated
  dimension plus 4-8 bytes of extras
  (`turboquant/encoding.rs:33-55,211-233`), which at d=128 with Dot is
  132, 72, 40, 32 and 24 bytes — 3.88× to 21.3×.

  </details>

- [ ] You can derive the u8 affine dot-product expansion and say what must be stored per vector for it to work.
  <details><summary>Answer</summary>

  With `v ≈ α·code + offset`,
  `dot(q,v) = α²·Σqᵢvᵢ + α·off·(Σqᵢ + Σvᵢ) + d·off²` — qdrant writes
  the same expansion in comments at `encoded_vectors_u8.rs:223-231`.
  Only the first term is a function of both operands, so only it runs
  per candidate, as an integer dot over bytes (`impl_score_dot`,
  :1002-1010, accumulating in i32). What is actually stored per
  vector is **not** a raw `Σvᵢ`: :268-281 computes `Σcode · α · off`
  for Dot/Cosine (or `Σcode² · α²` for L2), :288 adds the
  whole-index constant from `get_shift()` (:116-134) — `d·off²` for
  Dot/Cosine, 0 for L1/L2 — and :289-290 writes the single resulting
  f32 into the four bytes at the front of the encoded vector.
  `postprocess_score` (:101-103) then costs one multiply and two
  adds. Also worth stating: the code is clamped to **127** (:97), so
  it is 7-bit — 128 levels per dimension, not 256 — which keeps
  VNNI's unsigned-by-signed byte products exact (:905-910).

  </details>

- [ ] You can explain why PQ hurts HNSW traversal more than it hurts a flat IVF scan.
  <details><summary>Answer</summary>

  Because in a graph the distance function chooses which vectors you
  ever look at. A flat IVF scan visits a candidate set determined by
  the coarse quantizer, and PQ error only perturbs the *ranking*
  within it — a mistake costs you a position. In HNSW, each hop
  selects the next node from the distance estimates, so an error
  routes the walk somewhere else, that wrong node's neighbourhood
  supplies the next candidates, and the error compounds along the
  path. Increasing `ef` mitigates but does not fix it, because the
  beam is being steered by the same corrupted signal. This is why
  qdrant treats scalar as the default rung for HNSW: at 128 levels
  per dimension the ranking of near neighbours is usually preserved,
  so the hops are the same hops.

  </details>

- [ ] You can say what TurboQuant's rotation buys, and work its 8-bit and 4-bit sizes at d=128.
  <details><summary>Answer</summary>

  The Hadamard rotation (`turboquant/quantization.rs:159`) is
  orthogonal, so it keeps every distance, but it spreads each
  vector's energy across all coordinates; after the length rescale
  (:206-217) each coordinate behaves like an N(0, 1) draw. That lets
  the 1-4-bit widths use fixed Lloyd-Max centroids for N(0, 1)
  (`lloyd_max.rs:3-16`) and lets the 8-bit width scale by the
  vector's own largest coordinate (:271-279) without one outlier
  squeezing the rest. Sizes at d=128, Dot: 8 bits is
  `128 × 8/8 = 128 B` of codes plus a 4-byte scaling factor = 132 B
  (3.88×, the same bytes as scalar u8); 4 bits runs in TQ+ mode
  (`quantized_vectors.rs:196-201`), so `128 × 4/8 = 64 B` plus 4 + 4
  bytes of extras = 72 B (7.11×) (`encoding.rs:33-55,211-221`).

  </details>

- [ ] You can say what oversample-and-rescore claws back, and predict where it lands against this topic's brute-force point before implementing `quant.rs`.
  <details><summary>Answer</summary>

  It claws back *ranking*, not *reachability*: the quantized search
  only has to place the true neighbours somewhere inside the
  oversampled shortlist, and the exact f32 pass then orders them
  correctly. `get_oversampled_top`
  (`vector_index_search_common.rs:26-44`) computes the shortlist as
  `(oversampling × top)` at :40, gated at :39 on quantization being
  enabled and the factor being set and above 1.0;
  `postprocess_search_result` rescores it when the query asks or the
  storage's default says so (:62-70) and trims to `top` (:86). Cost:
  at top=10 and oversampling 4.0, that is 40 exact distances —
  `40 × 1536 = 61 440` multiply-adds at d=1536, against
  `1e6 × 1536 = 1.54 × 10⁹` for the brute-force alternative, a
  25 000× difference. So the rescore is effectively free and the
  whole recall/latency question stays with the quantized traversal.
  Against this topic's 117 QPS / recall 1.000 point, expect the
  quantized+rescore lane to sit far to the right on QPS with recall
  close to but below 1.000 — and record the measured pair rather than
  this prediction.

  </details>

- [ ] You can say where `get_oversampled_top` is actually defined, and why the distinction matters.
  <details><summary>Answer</summary>

  Defined in `lib/segment/src/index/vector_index_search_common.rs:26-44`;
  `lib/segment/src/index/hnsw_index/hnsw/read_view/search.rs:56` and
  :249 are only call sites, one for the graph search and one for the
  plain scan. It matters because the function is shared machinery —
  `is_quantized_search` (:14-24) and `postprocess_search_result`
  (:47-88) live beside it and serve every search path, graph or not,
  so oversampling is a property of quantized search in general rather
  than of the HNSW planner. Reading it inside the HNSW read view would
  suggest the planner owns it, which it does not.

  </details>

- [ ] You can state why average distance error does not predict which comparisons a quantized search gets wrong, with the paper's numbers.
  <details><summary>Answer</summary>

  A graph search consumes comparisons, and a comparison flips only when the noise
  is large enough to cross the exact gap between the two candidates being
  compared. Theorem 2 (§3.1) bounds the flip probability by the mass of exact
  margins near zero plus the tail of the calibrated residual, and the two
  mechanisms are merged by any global fidelity metric. Evidence: on 4,960 held-out
  Cohere two-bit decisions the flip rate falls from 48.6 % at standardized
  margin below 0.25 to 0.14 % at 3–5 and 0.00 % above 5 (Table 6), and across 24
  configurations the margin predicts flip rates with Spearman 0.970 (ranking) and
  0.992 (pruning) against 0.739 and 0.053 for global rank fidelity (§8.2). Shared
  error also cancels: individual error standard deviations 0.024 and 0.027 with
  correlation 0.87 give a difference with standard deviation 0.013, not 0.036.
  The stated limit (§1): the theory covers fixed candidate sets, not end-to-end
  recall.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the M14 rung decision.
  <details><summary>Answer</summary>

  For question 4 specifically, the arithmetic is
  `LUT = m × 256 × 4 B`: at m=16 that is 16 kB, which fits a typical
  32-48 kB L1d alongside the codes being scanned. At m=64 it is
  64 kB, which does not — every lookup becomes an L2 access and the
  "free" table lookup stops being free. qdrant reaches m=128 at d=128
  with `CompressionRatio::X4` (128 kB of table), which is the
  configuration where the highest nominal fidelity buys the worst
  locality.

  </details>

## References

**Papers**
- Xiao, Cao — *When Does Low-Bit Quantization Preserve the Decisions of Vector
  Search?* ([arXiv:2609.09854](https://arxiv.org/abs/2609.09854), v1, Sep 2026):
  §1 (scope and Example 1), §3.1 (Theorem 2), §5.4 (rotation duality), §8.2
  (Table 6 and the Spearman comparison), Table 2 (residual correlation). Step 7.
- Jégou, Douze, Schmid — the PQ paper (IEEE TPAMI 33(1), 2011) —
  gets its own chapter: [reading-pq.md](reading-pq.md). §II-B's
  warning about the lookup table outgrowing cache is the one to have
  in mind while reading Step 3.

**Code** — all qdrant v1.20.0, `qdrant/qdrant@222b2e44a`, pinned in
`resources/codebases.md`.

| file:line | what |
|---|---|
| `lib/quantization/src/encoded_vectors_u8.rs:94-98` | `encode_value` — the clamp is to **127**, so 7-bit |
| `lib/quantization/src/encoded_vectors_u8.rs:100-103,116-134` | `postprocess_score`, `get_shift` |
| `lib/quantization/src/encoded_vectors_u8.rs:222-232` | per-metric multiplier, with the algebra in comments |
| `lib/quantization/src/encoded_vectors_u8.rs:268-290` | the single folded f32 correction, written at the front |
| `lib/quantization/src/encoded_vectors_u8.rs:629-633,728-735` | `alpha = (max-min)/127`; size = `actual_dim + 4` |
| `lib/quantization/src/encoded_vectors_u8.rs:877-893,905-913` | kernel dispatch; VNNI and the exactness of 7-bit codes |
| `lib/quantization/src/quantile.rs:34-79` | quantile range fitting instead of min/max |
| `lib/quantization/src/encoded_vectors_pq.rs:30-33,41-53,479-494,520-542` | k-means constants, the LUT, the ADC scan |
| `lib/quantization/src/encoded_vectors_binary.rs:27,35-40,49-54,455-503,878-888` | the binary family, asymmetric query encodings, XOR+popcount, sizes |
| `lib/quantization/src/turboquant/quantization.rs:133-168,206-217,268-279` | rotation, length rescale, the 8-bit grid |
| `lib/quantization/src/turboquant/encoding.rs:33-55,211-233` | TurboQuant vector sizes |
| `lib/segment/src/types.rs:954-988,1115-1163` | CompressionRatio, the quantization configs, TurboQuantBitSize |
| `lib/segment/src/vector_storage/quantized/quantized_vectors.rs:196-297` | TQ mode and rescoring defaults, get_bucket_size, per-family sizes |
| `lib/segment/src/index/vector_index_search_common.rs:14-88` | `is_quantized_search`, `get_oversampled_top`, `postprocess_search_result` |
| `lib/segment/src/index/hnsw_index/hnsw/read_view/search.rs:56,249` | the call sites only |
