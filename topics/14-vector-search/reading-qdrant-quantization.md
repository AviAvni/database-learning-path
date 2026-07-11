# The quantization ladder: shrink, search, rescore

Topic 12's thesis — compression IS performance — with a new twist:
here compression is LOSSY, so the system needs machinery to claw the
recall back (oversample + rescore). This chapter climbs qdrant's
three-rung ladder (scalar u8, PQ, binary) and the pipeline that makes
lossy codes safe; that pipeline shape is what M14 copies. The
encoders live in their own crate, `lib/quantization/src/`; the wiring
into search is `lib/segment/src/vector_storage/quantized/`.

## 1. Scalar u8 (`encoded_vectors_u8.rs`)

The affine trick: store `alpha`/`offset` (:86-87), quantize
`i = (value - offset) / alpha` (:95). The clever part is scoring
WITHOUT decode — expand the dot product:

```
 dot(q, v) ≈ Σ (α·qᵢ + off)(α·vᵢ + off)
           = α² Σ qᵢvᵢ  +  α·off·(Σqᵢ + Σvᵢ)  +  d·off²
             ↑ integer dot     ↑ per-vector precomputed sums
```

`postprocess_score` (:61, :100) applies the affine correction using
per-vector offsets stored alongside the codes. Integer dot on u8 =
4× fewer bytes moved AND SIMD-friendlier (topic 17 will vectorize
exactly this). Quantile-based range (`quantile.rs`) clips outliers
so alpha isn't wasted on the tails.

```rust
// score u8 codes WITHOUT decoding: integer dot + affine correction
fn dot_u8(q: &Encoded, v: &Encoded, alpha: f32, off: f32, d: usize) -> f32 {
    let int_dot: u32 = q.codes.iter().zip(&v.codes)
        .map(|(&a, &b)| a as u32 * b as u32)
        .sum();                              // the u8 loop SIMD loves
    alpha * alpha * int_dot as f32
        + alpha * off * (q.sum + v.sum)      // Σqᵢ, Σvᵢ: stored per vector
        + d as f32 * off * off               // constant for the whole index
}
```

## 2. Product quantization (`encoded_vectors_pq.rs`)

- `:30` `CENTROIDS_COUNT = 256` — one byte per chunk, by
  construction; `:27-29` k-means over a 10K sample (BtrBlocks-style
  sampling, topic 12), max 100 iterations
- `:32` `EncodedVectorsPQ` — codes = chunk-wise centroid ids;
  `:46` `Metadata.centroids`
- `:39-41` `EncodedQueryPQ` — THE trick (ADC): per query, precompute
  a `[chunks × 256]` table of distances from each query sub-vector
  to every centroid; scoring a vector = d/chunk_size table lookups +
  adds, no float math per candidate

PQ trades multiply-adds for L1-resident lookups. Note what it does
to HNSW: distances become approximate EVERYWHERE, so graph
traversal itself degrades — which is why qdrant defaults to scalar
for HNSW and PQ mostly for memory-starved setups.

## 3. Binary (`encoded_vectors_binary.rs`)

- `:26` `EncodedVectorsBin`, one bit per dim (sign)
- `:144` `xor_popcnt` — Hamming distance as XOR + popcount, with
  SSE/NEON paths (:165-190): 32× compression, distances in a few
  cycles
- only sane with **rescoring**, and mainly for high-d embeddings
  where signs carry most of the angle information

## 4. Oversample + rescore (the recall clawback)

`lib/segment/src/index/hnsw_index/hnsw/search.rs:57`
`get_oversampled_top` — search the quantized index for
`top × oversampling`, then rescore that shortlist with original f32
vectors and cut to `top`. Late materialization (topic 12): cheap
representation for the scan, expensive one only for survivors.
`quantized_scorer_builder.rs` picks the scorer; storage variants
(RAM/mmap/chunked) live next to it.

```
 query ──► HNSW over u8/PQ/bin codes ──► top·x candidates
                                            │ rescore with f32
                                            ▼
                                          top k
```

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

## References

**Papers**
- Jégou, Douze, Schmid — the PQ paper (IEEE TPAMI 2011) — gets its
  own chapter: [reading-pq.md](reading-pq.md)

**Code**
- [qdrant](https://github.com/qdrant/qdrant) — encoders in
  `lib/quantization/src/` (`encoded_vectors_u8.rs`,
  `encoded_vectors_pq.rs`, `encoded_vectors_binary.rs`,
  `quantile.rs`); wiring in
  `lib/segment/src/vector_storage/quantized/`
  (`quantized_scorer_builder.rs` and the storage variants) and
  `lib/segment/src/index/hnsw_index/hnsw/search.rs`
  (`get_oversampled_top`)
