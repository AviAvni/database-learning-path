# The quantization ladder: shrink, search, rescore

Topic 12's thesis — compression IS performance — with a new twist:
here compression is LOSSY, so the system needs machinery to claw the
recall back. This chapter climbs qdrant's three-rung ladder step by
step — why lossy codes pay, scalar u8 and the score-without-decode
trick, PQ, binary — and ends with the oversample+rescore pipeline
that makes lossy codes safe; that pipeline shape is what M14 copies.
The encoders live in their own crate, `lib/quantization/src/`; the
wiring into search is `lib/segment/src/vector_storage/quantized/`.

## The problem in one sentence

A million 1536-d f32 embeddings is **6 GB** of vectors that every
HNSW hop pokes at random, so bytes-per-vector is the real cost unit
— but every byte saved is precision lost, and distances computed on
compressed codes return the *wrong nearest neighbors* unless
something puts the recall back.

## The concepts, step by step

### Step 1 — the ladder: three compression rungs, one recall knob

Lossy vector compression trades bytes for distance accuracy, and
qdrant ships three rungs — the topic README's table:

| scheme | bytes/dim (f32=4) | distance on encoded | recall cost |
|---|---|---|---|
| scalar u8 | 1 | integer dot + affine postprocess | tiny |
| PQ (m chunks × 256 centroids) | ~0.06–0.5 | LUT sums — d/m table lookups | real |
| binary | 1 bit | XOR + popcount | big, needs rescore |

Two things make the rungs *fast* rather than merely small. First,
distance must be computable ON the codes — decoding to f32 per
candidate would eat the savings. Second, moving fewer bytes is
itself the speedup: HNSW is memory-bound, so 4× smaller codes ≈ 4×
more of the index in cache. The recall each rung loses is recovered
by one shared mechanism — Step 5's pipeline — which is why the
riskier rungs are usable at all.

### Step 2 — scalar u8: the affine trick, and scoring without decode

Scalar quantization maps each f32 dimension to one byte through an
affine transform: store a shared `alpha` (scale) and `offset`
(encoded_vectors_u8.rs:86-87), encode `i = (value - offset) / alpha`
(:95) — 4× fewer bytes, ~256 distinguishable values per dimension.
The clever part is scoring WITHOUT decode — expand the dot product
algebraically:

```
 dot(q, v) ≈ Σ (α·qᵢ + off)(α·vᵢ + off)
           = α² Σ qᵢvᵢ  +  α·off·(Σqᵢ + Σvᵢ)  +  d·off²
             ↑ integer dot     ↑ per-vector precomputed sums
```

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

`postprocess_score` (:61, :100) applies the affine correction using
per-vector sums stored alongside the codes. The payoff is double:
4× fewer bytes moved AND an integer inner loop that vectorizes
beautifully (topic 17 will SIMD exactly this). One refinement:
`quantile.rs` picks the encoding range from quantiles rather than
min/max, so one outlier dimension doesn't waste alpha on the tails.
Recall cost: tiny — which is why u8 is the default rung for HNSW.

### Step 3 — product quantization: bytes per vector, not per dimension

PQ (the full derivation is [reading-pq.md](reading-pq.md); here,
the qdrant-shaped summary) splits the vector into m chunks and
replaces each chunk with the id of its nearest learned centroid —
`CENTROIDS_COUNT = 256` (encoded_vectors_pq.rs:30) so each chunk
codes as exactly one byte. Codebooks come from k-means over a 10K
sample (:27-29 — BtrBlocks-style sampling, topic 12), max 100
iterations. Scoring uses ADC (asymmetric distance computation):
`EncodedQueryPQ` (:39-41) precomputes a `[chunks × 256]` table of
exact sub-distances per query; each candidate then costs
d/chunk_size table lookups + adds, no float math (:32
`EncodedVectorsPQ` holds the codes, :46 `Metadata.centroids` the
codebooks).

PQ trades multiply-adds for L1-resident lookups and reaches
16–64× compression. The cost surfaces in the graph: distances
become approximate EVERYWHERE, so HNSW traversal itself degrades —
wrong distances mean wrong hops, and errors compound along the walk
— which is why qdrant defaults to scalar for HNSW and PQ mostly for
memory-starved setups (question 2).

### Step 4 — binary: one bit per dimension

The bottom rung keeps only the sign of each dimension:
`EncodedVectorsBin` (encoded_vectors_binary.rs:26), 32× compression.
Distance collapses to Hamming distance (the count of differing
bits), computed as XOR + popcount — `xor_popcnt` (:144) with
SSE/NEON paths (:165-190): a 1536-d comparison becomes ~48 64-bit
XOR+popcount ops, a few cycles total. The recall cost is big by
construction — 1 bit can't rank close neighbors — so binary is only
sane WITH rescoring (Step 5), and mainly for high-d embeddings
(1024-d+) where sign patterns carry most of the angle information.

### Step 5 — oversample + rescore: the recall clawback

The shared safety net: search the quantized index for MORE than you
need, then re-rank the shortlist with the exact vectors.
`get_oversampled_top`
(lib/segment/src/index/hnsw_index/hnsw/search.rs:57) fetches
`top × oversampling` candidates using the cheap codes, rescores that
shortlist with original f32 vectors, and cuts to `top`:

```
 query ──► HNSW over u8/PQ/bin codes ──► top·x candidates
                                            │ rescore with f32
                                            ▼
                                          top k
```

The arithmetic that makes it free-ish: at top=10, oversampling 4×
means exactly 40 f32 distance computations per query — noise next to
the ~thousands of code-distance computations the traversal did. The
quantization error only has to keep the true neighbors *inside the
top 40*, a far weaker demand than ranking them correctly. This is
late materialization (topic 12): cheap representation for the scan,
expensive one only for survivors. `quantized_scorer_builder.rs`
picks the scorer per collection config; storage variants
(RAM/mmap/chunked) live next to it.

## Where each step lives in the code

Encoders (`lib/quantization/src/`):

- **Step 2 — scalar**: `encoded_vectors_u8.rs` — `:86-87`
  alpha/offset, `:95` the quantize expression, `:61/:100`
  `postprocess_score`; `quantile.rs` — the outlier-clipping range.
- **Step 3 — PQ**: `encoded_vectors_pq.rs` — `:30` CENTROIDS_COUNT,
  `:27-29` the k-means sample, `:32` EncodedVectorsPQ, `:39-41`
  EncodedQueryPQ (the ADC table), `:46` Metadata.centroids.
- **Step 4 — binary**: `encoded_vectors_binary.rs` — `:26`
  EncodedVectorsBin, `:144` xor_popcnt, `:165-190` the SSE/NEON
  paths.

Wiring (`lib/segment/src/`):

- **Step 5 — the pipeline**: `index/hnsw_index/hnsw/search.rs:57`
  `get_oversampled_top`;
  `vector_storage/quantized/quantized_scorer_builder.rs` (scorer
  selection) and the RAM/mmap/chunked storage variants beside it.

Read order: `encoded_vectors_u8.rs` end to end first (it's the
smallest and carries the score-without-decode idea), then
`get_oversampled_top`, then PQ/binary as variations.

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
