# The quantization ladder: shrink, search, rescore

Topic 12's thesis — compression IS performance — with a new twist:
here compression is LOSSY, so the system needs machinery to claw the
recall back. This chapter climbs qdrant's compression ladder step by
step — why lossy codes pay, scalar u8 and the score-without-decode
trick, PQ, binary — and ends with the oversample+rescore pipeline
that makes lossy codes safe; that pipeline shape is what M14 copies.
The encoders live in their own crate, `lib/quantization/src/`; the
wiring into search is `lib/segment/src/vector_storage/quantized/`.

Every `file:line` below was read at **`qdrant/qdrant@44ad62f`**, the
pin in `resources/codebases.md`; re-check any of them with
`python3 tools/pinned-source.py show qdrant <path> -r A:B`. Several
figures in the older version of this guide were off — where a number
here contradicts folklore (u8 is *not* 256 levels; the per-vector
extra is *not* Σvᵢ), the anchor is given so you can settle it
yourself.

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

### Step 1 — the ladder: three compression rungs, one recall knob

> **In:** f32 vectors. **Out:** the three encodings qdrant ships,
> their real compression ratios, and the one property all three must
> have to be worth anything.

Lossy vector compression trades bytes for distance accuracy. qdrant
ships three families:

| scheme | stored per vector (d=128) | ratio | distance on encoded | recall cost |
|---|---|---|---|---|
| scalar u8 | d + 4 = 132 B | 3.88× | integer dot + affine postprocess | small |
| PQ, D* dims/chunk | d/D* bytes; D* ∈ {1,2,4,8,16} | 4×–64× | LUT sums — d/D* lookups | real |
| binary, 1 bit/dim | d/8 = 16 B | 32× | XOR + popcount | large, needs rescore |

The ratios are not round numbers, and two of them differ from what
gets repeated. Scalar u8 is **3.88×**, not 4×, because
`ADDITIONAL_CONSTANT_SIZE = size_of::<f32>()`
(`encoded_vectors_u8.rs:22-23`) prepends four bytes per vector —
`get_quantized_vector_size` is `actual_dim + 4`
(`encoded_vectors_u8.rs:593-596`), and `actual_dim` is itself d
rounded up to a multiple of `ALIGNMENT = 16` (:21, :589-591). PQ's
range is **4×–64×**, set by the `CompressionRatio` enum
(`lib/segment/src/types.rs:749-757`), whose variants map to
dimensions per chunk in
`lib/segment/src/vector_storage/quantized/quantized_vectors.rs:2314-2322`:
X4→1, X8→2, X16→4, X32→8, X64→16.

Two things make the rungs *fast* rather than merely small.

First, distance must be computable **on the codes** — decoding to
f32 per candidate would eat the savings, since the decode is the same
arithmetic you were trying to avoid. Each rung has its own trick for
this: Step 2's algebraic expansion, Step 3's lookup table, Step 4's
popcount.

Second, moving fewer bytes *is* the speedup. HNSW is memory-bound —
this topic's brute-force lane manages 117 QPS while running at 1.5 G
multiply-adds per second, which is not an arithmetic problem — so 4×
smaller codes means 4× more of the working set resident at each level
of cache.

The recall each rung loses is recovered by one shared mechanism,
Step 5's pipeline, which is why the riskier rungs are usable at all.

### Step 2 — scalar u8: the affine trick, and scoring without decode

> **In:** an f32 vector and a per-index scale/offset pair. **Out:**
> one byte per dimension plus one f32 correction term, and a dot
> product computed entirely in integers.

Scalar quantization maps each f32 dimension onto a small integer
through an affine transform. `alpha` (the scale) and `offset` live
in `MetadataInt8` (`encoded_vectors_u8.rs:83-90`, fields at :86-87),
and the encode is two lines:

```rust
// encoded_vectors_u8.rs — encode_value and postprocess_score, 93-102.
    93      #[inline]
    94      pub fn encode_value(&self, value: f32) -> u8 {
    95          let i = (value - self.offset) / self.alpha;
    96          i.clamp(0.0, 127.0).round() as u8
    97      }
    98
    99      #[inline]
   100      fn postprocess_score(&self, score: f32, query_offset: f32, vector_offset: f32) -> f32 {
   101          self.multiplier * score + query_offset + vector_offset
   102      }
```

Read :96 carefully: the clamp is to **127**, not 255. It is a `u8`
container holding a **7-bit** code, so there are **128
distinguishable levels per dimension, not 256** — the widely repeated
"256 levels" figure is wrong for this code path. The range fitting
confirms it: `alpha_offset_from_min_max` (:501-505) sets
`alpha = (max - min) / 127.0` and `offset = min`. The reason is
headroom in the SIMD kernels — products of two 7-bit values
accumulate in `i32` (`impl_score_dot`, :780-788) without the
saturation care an 8-bit×8-bit product would need.

The clever part is scoring WITHOUT decode. Expand the dot product of
two decoded vectors algebraically — qdrant writes the expansion in
its own comments at :208-216:

```
 dot(q, v) ≈ Σ (α·qᵢ + off)(α·vᵢ + off)
           = α² Σ qᵢvᵢ  +  α·off·(Σqᵢ + Σvᵢ)  +  d·off²
             ↑ integer dot    ↑ per-vector term   ↑ index constant
```

Only the first term depends on both operands, so only it has to be
computed per candidate — and it is an integer dot product over
bytes. The `multiplier` that scales it back is chosen per metric:

```rust
// encoded_vectors_u8.rs — the per-metric multiplier, 207-217.
   207          let multiplier = match vector_parameters.distance_type {
   208              // (alpha*x - offset) * (alpha*y - offset) = alpha^2*x*y - alpha*offset*x - alpha*offset*y + offset^2
   209              // multiplier is applied to xy term only, so we need to multiply score by alpha^2
   210              DistanceType::Dot | DistanceType::Cosine => alpha * alpha,
   211              // |(alpha*x - offset) - (alpha*y - offset)| = alpha*|x - y|
   212              // multiplier is applied to |x - y| term only, so we need to multiply score by alpha
   213              DistanceType::L1 => alpha,
   214              // ((alpha*x - offset) - (alpha*y - offset))^2 = alpha^2*(x - y)^2 = alpha^2*x^2 - 2*alpha^2*xy + alpha^2*y^2
   215              // multiplier is applied to (x - y)^2 term only, so we need to multiply score by -2*alpha^2
   216              DistanceType::L2 => -2.0 * alpha * alpha,
   217          };
```

Now the correction. It is **not** a stored `Σvᵢ`, as is often
claimed — everything metric-dependent is folded into a *single* f32
written at the front of each encoded vector:

```rust
// encoded_vectors_u8.rs — the per-vector correction term, 253-275,
// with the invert branch at 267-271 elided.
   253              let vector_offset = match vector_parameters.distance_type {
   254                  DistanceType::Dot | DistanceType::Cosine => {
   255                      let elements_sum = encoded_vector.iter().map(|&x| f32::from(x)).sum::<f32>();
   256                      elements_sum * alpha * offset
   257                  }
   258                  DistanceType::L1 => 0.0,
   259                  DistanceType::L2 => {
   260                      let elements_sqr_sum = encoded_vector
   261                          .iter()
// ... 262-265: .map(|&x| x*x).sum() * alpha * alpha ...
   266              };
// ... 267-271: negate if vector_parameters.invert ...
   272              // apply `a^2` shift
   273              let vector_offset = metadata.get_shift() + vector_offset;
   274              encoded_vector[0..ADDITIONAL_CONSTANT_SIZE]
   275                  .copy_from_slice(&vector_offset.to_ne_bytes());
```

`elements_sum` at :255 is Σ of the *encoded* bytes, immediately
multiplied by `alpha·offset` (:256), then the whole-index constant
`d·off²` from `get_shift()` (:115-130) is added in at :273 and the
result stored as four bytes at :274-275. For L2 it is a sum of
squares instead (:259-265). So what sits in front of each vector is
one already-scaled f32, not a raw sum — and `postprocess_score`
(:100-102) is a single fused multiply-add-add.

```rust
// ILLUSTRATION — not quoted from any file; this is the algebra above
// in one function. The real code is split: the integer loop is
// encoded_vectors_u8.rs:780-788 (impl_score_dot), the correction is
// :100-102 (postprocess_score), and the SIMD kernels at :800-813 are
// `unsafe extern "C"` — implemented in C, not Rust.
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
(`lib/quantization/src/quantile.rs:35-80`) picks alpha and offset from
a quantile of a *sample* rather than from min/max, trimming
`cut_index` values off each tail (:62-66) so a single outlier
dimension does not spend the whole 0..127 range on empty space.
`ScalarQuantizationConfig.quantile` (`lib/segment/src/types.rs:766-779`)
is the user-facing knob, validated to [0.5, 1.0], and :42 short-circuits
for fewer than 127 vectors, where there is nothing to estimate.

Recall cost: small — which is why scalar is the default rung for
HNSW.

### Step 3 — product quantization: bytes per vector, not per dimension

> **In:** the full PQ derivation from [reading-pq.md](reading-pq.md).
> **Out:** where each piece of it is in qdrant, and the one property
> that makes PQ riskier inside a graph than inside a flat scan.

PQ splits the vector into chunks and replaces each chunk with the id
of its nearest learned centroid. qdrant's constants are at the top of
the file: `CENTROIDS_COUNT = 256` (`encoded_vectors_pq.rs:30`) so
each chunk codes as exactly one byte, with codebooks from k-means
over a `KMEANS_SAMPLE_SIZE = 10_000` sample (:27), capped at
`KMEANS_MAX_ITERATIONS = 100` (:28) and `KMEANS_ACCURACY = 1e-5`
(:29). Sampling rather than full-corpus training is BtrBlocks-style
(topic 12).

Scoring is ADC: `EncodedQueryPQ` (:38-43) holds a `lut: Vec<f32>`
whose doc comment says exactly what it is — *"Lookup table is a
distance from each query chunk to each centroid related to this
chunk"*. `encode_query` (:515-537) builds it, sizing it at :516 as
`vector_division.len() * centroids.len()` and filling each cell at
:518-534 with an exact sub-distance. `Metadata` (:45-50) holds the
codebooks: `centroids` at :47 and `vector_division` — the chunk
ranges — at :48. Note qdrant stores each of the 256 centroids as a
*full-dimension* vector and slices it per chunk at :522, rather than
storing m separate small codebooks.

The scan (`score_point_simple`, :474-489) is one f32 load per code
byte, strided by `centroids_count`, summed; :407-440 and :442-472 are
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
> reason this rung is unusable without Step 5.

The bottom rung keeps only the sign of each dimension:
`EncodedVectorsBin` (`encoded_vectors_binary.rs:26`). At one bit per
dimension the ratio against f32 is `32/1 = 32×`.

Two corrections to the folk version, both from the pinned file.
First, binary quantization here is a *family*, not a single scheme:
`Encoding` (:33-39) offers `OneBit`, `TwoBits` and
`OneAndHalfBits`, so "32×" is the `OneBit` default and the others are
16× and about 21×. Second, the query need not use the same encoding
as storage: `QueryEncoding` (:47-53) offers `SameAsStorage`,
`Scalar4bits` and `Scalar8bits`, which is the PQ paper's
*asymmetric* idea applied to bits — keep the stored side at one bit,
spend more on the query, get better ranking for free
(`xor_popcnt_scalar`, :151, and the dispatch at :336-395).

The symmetric case collapses to Hamming distance, computed as XOR
plus popcount:

```rust
// encoded_vectors_binary.rs — BitsStoreType::xor_popcnt for u8,
// 158-209, with the NEON branch at 186-203 elided.
   158      fn xor_popcnt(v1: &[Self], v2: &[Self]) -> usize {
   159          debug_assert!(v1.len() == v2.len());
   160
   161          #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
   162          if is_x86_feature_detected!("sse4.2") {
   163              unsafe {
   164                  if v1.len() > 16 {
   165                      return impl_xor_popcnt_sse_uint128(
   166                          v1.as_ptr(),
   167                          v2.as_ptr(),
   168                          (v1.len() as u32) / 16,
   169                      ) as usize;
// ... 170-184: the /8 and /4 fallbacks, then NEON at 186-203 ...
   205          let mut result = 0;
   206          for (&b1, &b2) in v1.iter().zip(v2.iter()) {
   207              result += (b1 ^ b2).count_ones() as usize;
   208          }
   209          result
   210      }
```

Count the work for a realistic embedding, since the usual "~48 ops"
claim does not survive division:

```
  d = 1536, OneBit
  bits      = 1536
  bytes     = 1536 / 8              = 192 B
  u128 lanes= 192 / 16              = 12          ← what :164-169 dispatches
  u64 words = 192 / 8               = 24          ← 24 XORs + 24 popcounts

  compare f32: 1536 × 4 = 6144 B and 1536 multiply-adds
```

Twelve SIMD iterations against 1 536 multiply-adds, on 192 bytes
instead of 6 144. There is also a `u128` `BitsStoreType` impl (:287)
that dispatches to AVX-512 `vpopcntdq` when available (:292, :299).

The recall cost is large by construction — one bit cannot rank close
neighbours, only separate hemispheres — so binary is only sane WITH
rescoring, and mainly for high-dimensional embeddings (1024-d and
up) where the sign pattern still carries most of the angular
information.

### Step 5 — oversample + rescore: the recall clawback

> **In:** a quantized index that returns approximately-ranked
> results. **Out:** exactly-ranked results, for a per-query cost that
> does not scale with n.

The shared safety net: search the quantized index for MORE than you
need, then re-rank the shortlist with the exact vectors.

```
 query ──► HNSW over u8/PQ/bin codes ──► top·x candidates
                                           │ rescore with f32
                                           ▼
                                         top k
```

The multiplier comes from `get_oversampled_top`. The call site is
`lib/segment/src/index/hnsw_index/hnsw/search.rs:57`, but the
**definition is in a different module** —
`lib/segment/src/index/vector_index_search_common.rs:27-45` — which
is worth knowing before you go looking:

```rust
// vector_index_search_common.rs — get_oversampled_top, 27-45.
    27  pub fn get_oversampled_top(
    28      quantized_storage: Option<&QuantizedVectors>,
    29      params: Option<&SearchParams>,
    30      top: usize,
    31  ) -> usize {
    32      let quantization_enabled = is_quantized_search(quantized_storage, params);
// ... 34-37: read params.quantization.oversampling, else the default ...
    39      match oversampling_value {
    40          Some(oversampling) if quantization_enabled && oversampling > 1.0 => {
    41              (oversampling * top as f64) as usize
    42          }
    43          _ => top,
    44      }
    45  }
```

Three guards at :40: quantization has to be on, the factor has to
exceed 1.0, and it has to be set at all — otherwise `top` passes
through unchanged (:43). `is_quantized_search` (:15-25) is where the
`exact` and `ignore` search params turn quantization off entirely.

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

`postprocess_search_result`
(`vector_index_search_common.rs:48`) is where the shortlist is
trimmed back to `top`;
`lib/segment/src/vector_storage/quantized/quantized_scorer_builder.rs`
picks the scorer per collection config, and the RAM/mmap/chunked
storage variants live beside it.

## Where each step lives in the code

All at `qdrant/qdrant@44ad62f`.

**Encoders** (`lib/quantization/src/`):

| step | anchors |
|---|---|
| 2 scalar | `encoded_vectors_u8.rs:21-23` ALIGNMENT / ADDITIONAL_CONSTANT_SIZE, `:83-90` MetadataInt8, `:93-97` encode_value (**clamp to 127**), `:100-102` postprocess_score, `:115-130` get_shift, `:207-217` the per-metric multiplier, `:253-275` the folded correction term, `:501-505` alpha from min/max, `:589-596` actual_dim and `+ 4`, `:780-788` impl_score_dot, `:800-813` the C SIMD kernels |
| 2 range | `quantile.rs:35-80` find_quantile_interval; `:42` the <127 short-circuit |
| 3 PQ | `encoded_vectors_pq.rs:27-30` k-means constants and CENTROIDS_COUNT, `:32` EncodedVectorsPQ, `:38-43` EncodedQueryPQ (the LUT), `:45-50` Metadata (centroids at :47, vector_division at :48), `:474-489` the ADC scan, `:515-537` encode_query |
| 4 binary | `encoded_vectors_binary.rs:26` EncodedVectorsBin, `:33-39` Encoding (OneBit / TwoBits / OneAndHalfBits), `:47-53` QueryEncoding, `:144`/`:151` the trait methods, `:158-210` the u8 impl, `:287-321` the u128 impl with AVX-512 vpopcntdq |

**Wiring** (`lib/segment/src/`):

| step | anchors |
|---|---|
| 1 config | `types.rs:749-757` CompressionRatio, `:761-764` ScalarType, `:766-779` ScalarQuantizationConfig, `:798-805` ProductQuantizationConfig |
| 3 ratio → D* | `vector_storage/quantized/quantized_vectors.rs:2314-2322` get_bucket_size |
| 5 pipeline | `index/vector_index_search_common.rs:15-25` is_quantized_search, **`:27-45` get_oversampled_top** (the call at `index/hnsw_index/hnsw/search.rs:57` is not the definition), `:48` postprocess_search_result; `vector_storage/quantized/quantized_scorer_builder.rs` scorer selection |

Read order: `encoded_vectors_u8.rs` end to end first (it is the
smallest and carries the score-without-decode idea), then
`get_oversampled_top`, then PQ and binary as variations.

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

## Done when

Answer each before unfolding it.

- [ ] You can name the three rungs of the ladder and the compression ratio each achieves.
  <details><summary>Answer</summary>

  Scalar u8: one byte per dimension plus a 4-byte per-vector
  constant, so `512/132 = 3.88×` at d=128, not the round 4× — the
  extra is `ADDITIONAL_CONSTANT_SIZE`
  (`encoded_vectors_u8.rs:22-23`, size at :593-596), and `actual_dim`
  is d rounded up to a multiple of 16 (:589-591). PQ: one byte per
  chunk, with `CompressionRatio` X4…X64 (`types.rs:749-757`) mapping
  to 1, 2, 4, 8, 16 dimensions per chunk
  (`quantized_vectors.rs:2314-2322`), so **4×–64×**. Binary: one bit
  per dimension, `32×` for the `OneBit` default — but
  `encoded_vectors_binary.rs:33-39` also offers `TwoBits` (16×) and
  `OneAndHalfBits`, so "binary = 32×" needs the qualifier.
  </details>

- [ ] You can derive the u8 affine dot-product expansion and say what must be stored per vector for it to work.
  <details><summary>Answer</summary>

  With `v ≈ α·code + offset`,
  `dot(q,v) = α²·Σqᵢvᵢ + α·off·(Σqᵢ + Σvᵢ) + d·off²` — qdrant writes
  the same expansion in comments at `encoded_vectors_u8.rs:208-216`.
  Only the first term is a function of both operands, so only it runs
  per candidate, as an integer dot over bytes (`impl_score_dot`,
  :780-788, accumulating in i32). What is actually stored per vector
  is **not** a raw `Σvᵢ`: :253-266 computes `Σcode · α · off` for
  Dot/Cosine (or `Σcode² · α²` for L2), :273 folds in the whole-index
  constant `d·off²` from `get_shift()` (:115-130), and :274-275
  writes the single resulting f32 into the four bytes at the front of
  the encoded vector. `postprocess_score` (:100-102) then costs one
  multiply and two adds. Also worth stating: the code is clamped to
  **127** (:96), so it is 7-bit — 128 levels per dimension, not 256.
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

- [ ] You can say what oversample-and-rescore claws back, and predict where it lands against this topic's brute-force point before implementing `quant.rs`.
  <details><summary>Answer</summary>

  It claws back *ranking*, not *reachability*: the quantized search
  only has to place the true neighbours somewhere inside the
  oversampled shortlist, and the exact f32 pass then orders them
  correctly. `get_oversampled_top`
  (`vector_index_search_common.rs:27-45`) computes the shortlist as
  `(oversampling × top)` at :41, gated at :40 on quantization being
  enabled and the factor exceeding 1.0. Cost: at top=10 and
  oversampling 4.0, that is 40 exact distances — `40 × 1536 = 61 440`
  multiply-adds at d=1536, against `1e6 × 1536 = 1.54 × 10⁹` for the
  brute-force alternative, a 25 000× difference. So the rescore is
  effectively free and the whole recall/latency question stays with
  the quantized traversal. Against this topic's 117 QPS / recall
  1.000 point, expect the quantized+rescore lane to sit far to the
  right on QPS with recall close to but below 1.000 — and record the
  measured pair rather than this prediction.
  </details>

- [ ] You can say where `get_oversampled_top` is actually defined, and why the distinction matters.
  <details><summary>Answer</summary>

  Defined in `lib/segment/src/index/vector_index_search_common.rs:27-45`;
  `lib/segment/src/index/hnsw_index/hnsw/search.rs:57` is only the
  call site. It matters because the function is shared machinery —
  `is_quantized_search` (:15-25) and `postprocess_search_result`
  (:48) live beside it and are used by the non-HNSW index paths too,
  so oversampling is a property of quantized search in general rather
  than of the HNSW planner. Reading it inside `hnsw/search.rs` would
  suggest the planner owns it, which it does not.
  </details>

- [ ] You wrote answers to all five questions in notes.md, including the M14 rung decision.
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
- Jégou, Douze, Schmid — the PQ paper (IEEE TPAMI 33(1), 2011) —
  gets its own chapter: [reading-pq.md](reading-pq.md). §II-B's
  warning about the lookup table outgrowing cache is the one to have
  in mind while reading Step 3.

**Code** — all `qdrant/qdrant@44ad62f`, pinned in
`resources/codebases.md`.

| file:line | what |
|---|---|
| `lib/quantization/src/encoded_vectors_u8.rs:93-97` | `encode_value` — the clamp is to **127**, so 7-bit |
| `lib/quantization/src/encoded_vectors_u8.rs:100-102,115-130` | `postprocess_score`, `get_shift` |
| `lib/quantization/src/encoded_vectors_u8.rs:207-217` | per-metric multiplier, with the algebra in comments |
| `lib/quantization/src/encoded_vectors_u8.rs:253-275` | the single folded f32 correction, written at the front |
| `lib/quantization/src/encoded_vectors_u8.rs:501-505,589-596` | `alpha = (max-min)/127`; size = `actual_dim + 4` |
| `lib/quantization/src/quantile.rs:35-80` | quantile range fitting instead of min/max |
| `lib/quantization/src/encoded_vectors_pq.rs:27-30,38-50,474-489,515-537` | k-means constants, the LUT, the ADC scan |
| `lib/quantization/src/encoded_vectors_binary.rs:26,33-39,47-53,158-210,287-321` | the binary family, asymmetric query encodings, XOR+popcount |
| `lib/segment/src/types.rs:749-757,766-779,798-805` | CompressionRatio and the two quantization configs |
| `lib/segment/src/vector_storage/quantized/quantized_vectors.rs:2314-2322` | compression ratio → dimensions per chunk |
| `lib/segment/src/index/vector_index_search_common.rs:15-48` | `is_quantized_search`, `get_oversampled_top`, `postprocess_search_result` |
| `lib/segment/src/index/hnsw_index/hnsw/search.rs:57` | the call site only |
