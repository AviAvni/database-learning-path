# Product quantization: 2^128 centroids in 16 bytes

The paper that made billion-scale ANN affordable — and the "PQ" in
IVF-PQ, DiskANN, and qdrant's `encoded_vectors_pq.rs`. One move does
all the work: quantize a PRODUCT of subspaces, so codebook size grows
exponentially while storage stays linear. Topic 12's dictionary
encoding, but the dictionary is learned and the code is a
concatenation.

## 1. The core move: quantize a PRODUCT of subspaces

A vector quantizer with k centroids costs k·d to store and can't
exceed ~2²⁰ centroids in practice. PQ splits d dims into m chunks
and quantizes each chunk independently with k* = 256 centroids:

```
 x (d=128) → [x¹ | x² | ... | x¹⁶]   m=16 chunks of 8 dims
              q¹(x¹) q²(x²) ... — each an 8-bit centroid id

 effective centroids: 256¹⁶ = 2¹²⁸    stored: 16 bytes/vector
 codebook cost: m · 256 · (d/m) = 256·d floats — tiny
```

The exponential codebook for linear storage is the whole paper.
Same energy as topic 12's dictionary encoding, but the dictionary
is LEARNED (k-means per subspace) and the code is a concatenation.

## 2. SDC vs ADC — where you eat the approximation

- **SDC** (symmetric): quantize the query too; distance =
  precomputed centroid-to-centroid tables. Fastest, two
  approximations.
- **ADC** (asymmetric): keep the query exact; per query build the
  `[m × 256]` table of `‖qʲ - cⱼ,ᵢ‖²`, then any database vector's
  distance ≈ m table lookups + adds. One approximation — strictly
  better recall for the same codes. Everyone ships ADC (qdrant's
  `EncodedQueryPQ`, encoded_vectors_pq.rs:39-41).

```rust
// ADC: pay m·256 exact sub-distances ONCE per query…
fn adc_table(q: &[f32], cb: &Codebook) -> Vec<[f32; 256]> {
    (0..cb.m).map(|j| {
        let qj = &q[j * cb.sub_d..(j + 1) * cb.sub_d];
        std::array::from_fn(|i| l2_sq(qj, cb.centroid(j, i)))
    }).collect()          // [m × 256] f32 — small enough to live in L1
}

// …then EVERY candidate costs m byte-indexed lookups, zero float math
fn adc_dist(code: &[u8], table: &[[f32; 256]]) -> f32 {
    code.iter().zip(table).map(|(&c, t)| t[c as usize]).sum()
}
```

The paper also derives the distance ESTIMATOR bias (ADC
underestimates on average) and a correction — worth knowing it
exists; most systems skip the correction and oversample instead.

## 3. IVFADC — the system the paper actually ships

Coarse quantizer (k-means, nlist cells) → residual `x - c(x)` →
PQ-encode the RESIDUAL. Query: probe nprobe cells, ADC-scan their
inverted lists.

```
 query ─► nearest nprobe cells ─► ADC over residual codes ─► top-k
          (coarse index)           (16 B/vector, L1 LUTs)
```

Residuals matter: they're centered around 0 with much smaller
variance than raw vectors, so 256 centroids per subspace go further.
This is frame-of-reference (topic 12's FOR bit-packing) in learned
form: subtract the predictable part, encode the residual cheaply.

## 4. What survived twenty years

- ADC lookup tables — unchanged everywhere
- residual encoding — DiskANN keeps PQ codes in RAM to steer SSD
  reads (reading-diskann.md)
- OPQ (rotate before chunking so subspaces decorrelate) — the main
  refinement worth knowing exists
- the recall gap at high k — why oversample+rescore became standard
  (reading-qdrant-quantization.md §4)

## Questions (answer in notes.md)

1. m=16 vs m=64 at fixed 16 bytes/vector total (256 vs 4 centroids
   per chunk?? — work out what actually changes): which knob trades
   what?
2. Why must chunks be (roughly) statistically independent for PQ to
   work well? What does OPQ's rotation fix — connect to
   BYTE_STREAM_SPLIT (topic 12).
3. ADC table build is m·256·(d/m) float ops per query. At what
   shortlist size does table build dominate scanning?
4. Why encode residuals instead of raw vectors in IVFADC? State it
   in FOR terms.
5. SDC would let you precompute ALL tables once (no per-query work).
   Why does nobody care?

## References

**Papers**
- Jégou, Douze, Schmid — "Product Quantization for Nearest Neighbor
  Search" (IEEE TPAMI 2011) — §2 the quantizer, §3 SDC/ADC and the
  estimator, §4 IVFADC; the paper everyone builds on
- Ge, He, Ke, Sun — "Optimized Product Quantization" (CVPR 2013) —
  optional; the rotation refinement worth knowing exists

**Code**
- [qdrant](https://github.com/qdrant/qdrant)
  `lib/quantization/src/encoded_vectors_pq.rs` — the production ADC,
  walked in
  [reading-qdrant-quantization.md](reading-qdrant-quantization.md)
