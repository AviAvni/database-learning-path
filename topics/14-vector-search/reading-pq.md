# Product quantization: 2^128 centroids in 16 bytes

The paper that made billion-scale ANN affordable — and the "PQ" in
IVF-PQ, DiskANN, and qdrant's `encoded_vectors_pq.rs`. One move does
all the work: quantize a PRODUCT of subspaces, so codebook size grows
exponentially while storage stays linear. Before the paper, this
chapter builds the idea from zero — what a quantizer is, why plain
quantizers hit a wall, the product trick, how to compute distances
on codes without decoding, and the residual system the paper
actually ships. Topic 12's dictionary encoding, but the dictionary
is learned and the code is a concatenation.

## The problem in one sentence

A billion 128-d f32 vectors is **512 GB** — they don't fit in RAM,
and even if they did, exact distances cost 128 multiply-adds each —
so we need a code a few *bytes* long per vector that still supports
distance computation, and a plain quantizer capable of that fidelity
would need more centroids than there are atoms in a datacenter.

## The concepts, step by step

### Step 1 — vector quantization: replace a vector with its nearest centroid

A vector quantizer maps each vector to the nearest of k
representative points called **centroids** (learned by k-means:
alternate "assign each vector to its nearest centroid" and "move
each centroid to the mean of its assignees"). The set of centroids
is the **codebook**; the stored code is just the centroid's index —
⌈log₂ k⌉ bits. Distance to a quantized vector ≈ distance to its
centroid, so the quantization error IS the accuracy loss.

The wall: fidelity needs many centroids, but a codebook with k
centroids costs k·d floats to store and k·d multiply-adds to encode
one vector. k = 2²⁰ (a 20-bit code) is about the practical limit —
and 20 bits is nowhere near enough to describe a 128-d vector well.
For a 64-bit code you'd need k = 2⁶⁴ centroids: unstorable,
unlearnable.

### Step 2 — the product move: quantize subspaces independently

Product quantization splits the d dimensions into m contiguous
chunks and runs a *separate small quantizer* (k* = 256 centroids,
so each chunk's code is exactly one byte) on each chunk. The full
code is the concatenation of the m chunk codes:

```
 x (d=128) → [x¹ | x² | ... | x¹⁶]   m=16 chunks of 8 dims
              q¹(x¹) q²(x²) ... — each an 8-bit centroid id

 effective centroids: 256¹⁶ = 2¹²⁸    stored: 16 bytes/vector
 codebook cost: m · 256 · (d/m) = 256·d floats — tiny
```

The implied codebook is the Cartesian product of the m small ones:
256¹⁶ = 2¹²⁸ distinct representable points, from codebooks totalling
256·d floats (128 KB for d=128). The exponential codebook for linear
storage is the whole paper. Same energy as topic 12's dictionary
encoding, but the dictionary is LEARNED (k-means per subspace) and
the code is a concatenation. The cost: the product structure assumes
the chunks are roughly statistically independent — correlated
dimensions split across chunks waste code space (question 2; OPQ
exists to fix this, Step 5).

### Step 3 — SDC vs ADC: where you eat the approximation

Distances on codes come in two flavors, differing in whether the
*query* gets quantized too:

- **SDC** (symmetric distance computation): quantize the query as
  well; distance = a precomputed centroid-to-centroid table lookup
  per chunk. Fastest possible, but TWO approximations (query error +
  database error).
- **ADC** (asymmetric distance computation): keep the query exact.
  Once per query, build the `[m × 256]` table of exact sub-distances
  `‖qʲ - cⱼ,ᵢ‖²` from each query chunk to every centroid; then any
  database vector's distance ≈ m table lookups + adds. ONE
  approximation — strictly better recall for the same codes.
  Everyone ships ADC (qdrant's `EncodedQueryPQ`,
  encoded_vectors_pq.rs:39-41).

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

For d=128, m=16: 16 KB of tables built once, then each candidate
costs 16 byte-indexed L1 loads instead of 128 multiply-adds — PQ
trades float math for L1-resident lookups. The paper also derives
the distance ESTIMATOR bias (ADC underestimates on average) and a
correction — worth knowing it exists; most systems skip the
correction and oversample instead.

### Step 4 — IVFADC: coarse cells + residual encoding

ADC still scans every code; the paper's shipped system adds a
**coarse quantizer** — a plain k-means with nlist cells (Step 1's
kind) — to make the scan sublinear. Each vector is assigned to its
nearest cell and stored in that cell's **inverted list** (the list
of all vectors in the cell). Query: find the nprobe nearest cells,
ADC-scan only their lists.

The subtle move: what gets PQ-encoded is not the vector but its
**residual** `x - c(x)` — the offset from its cell's centroid:

```
 query ─► nearest nprobe cells ─► ADC over residual codes ─► top-k
          (coarse index)           (16 B/vector, L1 LUTs)
```

Residuals matter: they're centered around 0 with much smaller
variance than raw vectors, so 256 centroids per subspace go further.
This is frame-of-reference (topic 12's FOR bit-packing) in learned
form: subtract the predictable part, encode the residual cheaply.
The cost knob is nprobe — more cells probed buys recall with scan
time, IVF's version of ef.

### Step 5 — what survived twenty years

Four pieces of this 2011 paper are load-bearing in 2026 systems:

- **ADC lookup tables** — unchanged everywhere; qdrant's
  `encoded_vectors_pq.rs` is Step 3 verbatim.
- **Residual encoding** — DiskANN keeps PQ codes in RAM to steer SSD
  reads ([reading-diskann.md](reading-diskann.md)).
- **OPQ** (rotate the space before chunking so subspaces
  decorrelate) — the main refinement worth knowing exists; it
  attacks Step 2's independence assumption directly.
- **The recall gap at high k** — why oversample+rescore became the
  standard pipeline
  ([reading-qdrant-quantization.md](reading-qdrant-quantization.md) §4).

## How to read the paper (with the concepts in hand)

- **§2 (the quantizer)** — Steps 1–2. The distortion formalism is
  denser than it needs to be; keep the "product of small codebooks =
  exponential effective codebook" picture in front of you and the
  math follows.
- **§3 (SDC/ADC and the estimator)** — Step 3. Read the ADC part
  carefully — it's the part every production system runs. The bias
  correction (§3.3) is skimmable; note it exists, note nobody ships
  it.
- **§4 (IVFADC)** — Step 4. The residual argument is two paragraphs;
  translate it to FOR terms as you read. The nprobe/recall curves
  here are the paper's version of the topic README's recall-vs-QPS
  curve.
- **§5 (evaluation)** — skim; SIFT1B was the headline dataset, and
  the numbers set the baseline DiskANN later chased.

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
