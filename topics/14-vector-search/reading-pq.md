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

Paper claims below cite Jégou, Douze & Schmid, *"Product
Quantization for Nearest Neighbor Search"*, IEEE TPAMI 33(1), 2011 —
read here as the author manuscript
[inria-00514462v2](https://inria.hal.science/inria-00514462v2). The
paper numbers its sections in Roman numerals, so **§II** is the
quantizer, **§III** is SDC/ADC, **§IV** is IVFADC and **§V** is the
evaluation; equation and table numbers are the paper's own. Code
anchors are `qdrant/qdrant@44ad62f`, the pin in
`resources/codebases.md`.

## The problem in one sentence

A billion 128-d f32 vectors is **512 GB** — they do not fit in RAM,
and even if they did, exact distances cost 128 multiply-adds each —
so we need a code a few *bytes* long per vector that still supports
distance computation, and a plain quantizer capable of that fidelity
would need more centroids than a datacenter could store.

Two definitions to fix before Step 1. A **centroid** is one of the k
representative points a quantizer maps vectors onto; the set of them
is the **codebook**. **Recall@R** in this paper means something
narrower than the repo's usual usage — §V-A defines it as *"the
proportion of query vectors for which the nearest neighbor is ranked
in the first R positions"*, i.e. a 1-NN measure with a re-ranking
shortlist of size R, not the k-NN recall the topic bench reports.
When you compare a PQ number against this topic's brute-force
recall 1.000, you are comparing two different quantities; say which
one you mean.

## The concepts, step by step

### Step 1 — vector quantization: replace a vector with its nearest centroid

> **In:** a set of d-dimensional vectors. **Out:** a codebook of k
> centroids, a code per vector of ⌈log₂ k⌉ bits, and the wall that
> makes plain quantization useless at the fidelity we need.

A **vector quantizer** is a function `q` mapping a d-dimensional
vector to one of k centroids (§II-A, Eq. 1-2). The centroids are
learned by k-means — the paper calls it the Lloyd quantizer and
states the two optimality conditions it satisfies: assign each vector
to its nearest centroid (Eq. 4), and set each centroid to the mean of
its assignees (Eq. 5). The stored code is just the centroid's index.
§II-A: *"The memory cost of storing the index value, without any
further processing (entropy coding), is ⌈log₂ k⌉ bits. Therefore, it
is convenient to use a power of two for k."*

Distance to a quantized vector ≈ distance to its centroid, so the
quantization error *is* the accuracy loss.

The wall is stated in §II-B's opening, with SIFT as the example. A
quantizer producing 64-bit codes for a 128-dimensional vector — the
paper's phrasing, *"only 0.5 bit per component"* — needs
`k = 2^64` centroids. Table I gives the two costs that kills:

```
  codebook storage (k-means)  = k·D floats
  assignment cost per vector  = k·D multiply-adds

  k = 2^64, D = 128, f32:
    storage = 2^64 × 128 × 4 B = 9.4 × 10^21 bytes = 9.4 zettabytes
    and you would need several times k training samples to learn it
```

The paper's own summary: *"it is impossible to use Lloyd's algorithm
or even HKM… It is even impossible to store the D×k floating point
values representing the k centroids."*

### Step 2 — the product move: quantize subspaces independently

> **In:** the impossible k = 2^64 codebook. **Out:** m small
> codebooks whose Cartesian product has the same cardinality, at
> storage that grows *linearly* in m.

§II-B, Eq. 8: split the input vector `x` into m distinct subvectors
`u_j` of dimension `D* = D/m` (D a multiple of m), and quantize each
with its own subquantizer `q_j`. The full code is the concatenation
of the m chunk codes. Eq. 9 makes the codebook the Cartesian product
`C = C_1 × … × C_m`, and Eq. 10 gives its size: **`k = (k*)^m`**,
where `k*` is the per-subquantizer centroid count.

```
 x (d=128) → [x¹ | x² | ... | x¹⁶]   m=16 chunks of D* = 8 dims
              q¹(x¹) q²(x²) ... — each an 8-bit centroid id

 effective centroids: 256¹⁶ = 2¹²⁸    stored: 16 bytes/vector
```

Work both sides of the trade with real numbers:

```
  d = 128, m = 16, D* = d/m = 8, k* = 256

  code length      = m · log₂ k*  = 16 × 8      = 128 bits = 16 bytes
  effective k      = (k*)^m       = 256^16      = 2^128 centroids
  codebook storage = m · k* · D*  = k* · d      = 256 × 128
                                                = 32 768 floats = 128 kB
  encode one vector= k* · D       = 256 × 128   = 32 768 mult-adds

  compare Step 1's plain quantizer at the same effective k = 2^128:
      storage 2^128 × 128 × 4 B — not a number worth writing down
```

128 kB of codebook and 32 768 operations to encode, for a codebook
of cardinality 2^128. That exchange — exponential effective codebook,
linear storage — is the whole paper. Table I states it in general
form: product k-means costs `m k* D* = k^(1/m) D`, where plain
k-means costs `kD`.

Why `k* = 256` specifically? Two reasons, both in §II-B. First,
`log₂ 256 = 8`, so each chunk's code is exactly one byte and the
concatenation needs no bit-shifting. Second, the paper measured which
side of the trade to be on and says: *"for a fixed number of bits, it
is better to use a small number of subquantizers with many centroids
than having many subquantizers with few bits"* — a claim repeated
with recall numbers in §V-B. It then names the convention everyone
inherited: *"Using k* = 256 and m = 8 is often a reasonable
choice."*

There is a ceiling on k* too, and it is a cache argument the paper
makes itself in §II-B: high k* *"increase[s] the memory usage of
storing the centroids (k* × D floating point values), which further
reduces the efficiency if the centroid look-up table does no longer
fit in cache memory."* Step 3 puts a number on that.

The cost of the product structure: it assumes the chunks are roughly
statistically independent — correlated dimensions split across
chunks waste code space (question 2; OPQ exists to fix this, Step 5).
§II-B is candid that the fix is sometimes a rotation and sometimes
nothing: *"One way to ensure this property is to multiply the vector
by a random orthogonal matrix prior to quantization. However, for
most vector types this is not required and not recommended, as
consecutive components are often correlated by construction and are
better quantized together with the same subquantizer."*

### Step 3 — SDC vs ADC: where you eat the approximation

> **In:** two PQ codes, or one code and one raw query. **Out:** two
> distance estimators with the same asymptotic cost and materially
> different accuracy, and the reason every production system ships
> the second one.

§III-A gives both, and the difference is exactly whether the *query*
gets quantized.

- **SDC** (symmetric distance computation, Eq. 12): quantize the
  query too, so `d̂(x,y) = d(q(x), q(y))`, read from a table of
  centroid-to-centroid distances. The table holds all `(k*)²` squared
  distances per subquantizer, though footnote 1 notes only
  `k*(k*−1)/2` need be stored by symmetry. **Two** approximations —
  query error and database error.
- **ADC** (asymmetric distance computation, Eq. 13): keep the query
  exact, so `d̃(x,y) = d(x, q(y))`. Once per query, build the
  `[m × k*]` table of exact sub-distances from each query chunk to
  every centroid in that chunk's codebook; then any database vector's
  distance is m table lookups plus adds. **One** approximation.

Table II is explicit that SDC does not buy speed: encoding the query
costs `k*D` for SDC and 0 for ADC, but computing the query's
sub-distances costs 0 for SDC and `k*D` for ADC — *"SDC and ADC have
the same query preparation cost, which does not depend on the dataset
size n"*, and both scan at `nm`. §III-A's conclusion is a
recommendation, not a hedge: *"The only advantage of SDC over ADC is
to limit the memory usage associated with the queries… one should
then use the asymmetric version, which obtains a lower distance
distortion for a similar complexity."*

Table V measures it, on GIST with 64-bit codes (m=8, k*=256):

| method | search time (ms) | code comparisons | recall@100 |
|---|---|---|---|
| SDC | 16.8 | 1 000 991 | 0.446 |
| ADC | 17.2 | 1 000 991 | **0.652** |

Same code length, same scan, 2.4% more time, and recall@100 goes
from 0.446 to 0.652. §V-B puts the same result the other way round:
*"For m=8 we obtain the same accuracy for ADC and k*=64 as for SDC
and k*=256"* — ADC buys you two bits per subquantizer for free.
That is why nobody ships SDC, and it answers question 5.

```rust
// ILLUSTRATION — not quoted from any file; this is Eq. 13 of the PQ
// paper as Rust. The production version is qdrant's
// lib/quantization/src/encoded_vectors_pq.rs:515-537 (the table) and
// :474-489 (the scan).

// ADC: pay m·k* exact sub-distances ONCE per query…
fn adc_table(q: &[f32], cb: &Codebook) -> Vec<[f32; 256]> {
    (0..cb.m).map(|j| {
        let qj = &q[j * cb.sub_d..(j + 1) * cb.sub_d];
        std::array::from_fn(|i| l2_sq(qj, cb.centroid(j, i)))
    }).collect()          // [m × 256] f32
}

// …then EVERY candidate costs m byte-indexed lookups, zero float math
fn adc_dist(code: &[u8], table: &[[f32; 256]]) -> f32 {
    code.iter().zip(table).map(|(&c, t)| t[c as usize]).sum()
}
```

The table's size is the number to keep in your head, because §II-B's
cache warning lands here:

```
  LUT bytes = m · k* · sizeof(f32) = m × 256 × 4 = 1024·m

  d=128, m=16 (16-byte codes) : 16 × 1024 =  16 kB  → fits a 32-48 kB L1d
  d=128, m=8  (8-byte codes)  :  8 × 1024 =   8 kB  → fits comfortably
  d=128, m=128 (qdrant's X4)  :128 × 1024 = 128 kB  → L2 at best

  build cost = m · k* · D* = k* · d = 256 × 128 = 32 768 mult-adds
  per-candidate cost       = m adds = 16
```

So the per-query table build costs the same as encoding one vector,
and it pays for itself after `32 768 / 16 = 2 048` candidates — below
a two-thousand-candidate shortlist, ADC is dominated by its own setup
and you may as well compute exact distances. That is question 3, and
it is the reason IVFADC's `w` (Step 4) cannot be too small.

§III-C is worth reading precisely because the paper argues itself out
of its own result. It derives a bias correction (Eq. 25): the ADC
estimator systematically underestimates, and adding the mean
distortion `ξ_j` of each subquantizer removes the bias. Figure 4
measures both on 10 000 SIFT vectors with m=8, k*=256: bias goes from
**−0.044** to **0.002**, but the variance goes *up*, from
`σ² = 0.00146` to `0.00155`. The paper's verdict: *"In our
experiments, we observe that the correction returns inferior results
on average. Therefore, we advocate the use of Equation 13 for the
nearest neighbor search. The corrected version is useful only if we
are interested in the distances themselves."* Nobody ships the
correction because the authors told them not to — not because the
industry ignored it.

### Step 4 — IVFADC: coarse cells + residual encoding

> **In:** ADC, which still touches every code. **Out:** a two-level
> system that touches `n·w/k′` of them, and the reason what gets
> encoded is a residual rather than a vector.

§IV: ADC's scan is still exhaustive, so the shipped system adds a
**coarse quantizer** `q_c` — a plain k-means of the Step 1 kind, with
`k′` centroids, *"typically ranges from k′ = 1 000 to k′ = 1 000
000"* for SIFT. Each vector goes into the **inverted list** of its
nearest coarse centroid (§IV-B). A query is assigned to its `w`
nearest coarse centroids (the **multiple assignment** of §IV-C, `w`
being IVF's version of `ef`) and only those lists are scanned.

The subtle move (§IV-A, Eq. 28-29): what gets PQ-encoded is not the
vector but its **residual** `r(y) = y − q_c(y)`, the offset from its
cell's centroid, so the stored approximation is
`ÿ = q_c(y) + q_p(y − q_c(y))`.

```
 query ─► nearest w cells ─► ADC over residual codes ─► top-k
          (coarse index)      (m bytes/vector, LUT per cell)
```

§IV's own justification: *"encoding the residual is more precise than
encoding the vector itself"*, because *"the energy of the residual
vector is small compared to that of the vector itself"* (§IV-A). This
is frame-of-reference — topic 12's FOR bit-packing — in learned form:
subtract the predictable part, encode the cheap remainder. The paper
makes the analogy itself: *"the coarse quantizer provides the most
significant bits, while the product quantizer code corresponds to the
least significant bits."*

§IV-C gives the scan cost directly — *"about n×w/k′ entries have to
be parsed"* — which is the whole point:

```
  n = 1 000 000 000,  k′ = 1024,  w = 8
  entries scanned = n·w/k′ = 1e9 × 8 / 1024 = 7.81 × 10^6
  vs flat ADC     = 1e9
  reduction       = k′/w = 128×
```

Table V measures the same shape on GIST, and shows both knobs:

| method | search time (ms) | code comparisons | recall@100 |
|---|---|---|---|
| ADC (flat) | 17.2 | 1 000 991 | 0.652 |
| IVFADC k′=1024, w=1 | 1.5 | 1 947 | 0.308 |
| IVFADC k′=1024, w=8 | 8.8 | 27 818 | 0.682 |
| IVFADC k′=1024, w=64 | 65.9 | 101 158 | 0.744 |
| IVFADC k′=8192, w=8 | 10.2 | 2 709 | 0.516 |

Read the first two rows together: `w=1` scans 514× fewer codes than
flat ADC and is 11× faster, but drops recall@100 from 0.652 to 0.308
— the query's true neighbour is often in a *neighbouring* cell.
`w=8` recovers it and then some. Note also the timing floor: `w=1` on
k′=1024 scans 1 947 codes in 1.5 ms, which is nowhere near
1 947 × m adds — most of that 1.5 ms is the coarse assignment and the
ADC table build, exactly the 2 048-candidate break-even from Step 3.

One implementation consequence that catches people: because the ADC
table is built from `x − q_c(y)` (Eq. 30-31), it depends on *which
cell* is being scanned, so IVFADC rebuilds the `m × k*` table once
per probed cell, not once per query. With `w = 8` that is eight table
builds.

### Step 5 — what survived twenty years

> **In:** the 2011 paper. **Out:** which four of its parts you will
> meet again in a 2026 codebase, and which one thing it got argued
> out of.

- **ADC lookup tables** — unchanged everywhere. qdrant's
  `lib/quantization/src/encoded_vectors_pq.rs:515-537` is Eq. 13
  verbatim: `lut_capacity = vector_division.len() * centroids.len()`
  at :516, then an exact sub-distance per (chunk, centroid) pair at
  :518-534. The scan at :474-489 is one f32 load per code byte,
  strided by `centroids_count`, summed.
- **k* = 256** — qdrant hard-codes `CENTROIDS_COUNT = 256`
  (`encoded_vectors_pq.rs:30`), and its user-facing
  `CompressionRatio` enum (`lib/segment/src/types.rs:749-757`) is
  really a choice of `D*`: `get_bucket_size`
  (`lib/segment/src/vector_storage/quantized/quantized_vectors.rs:2314-2322`)
  maps X4→1, X8→2, X16→4, X32→8, X64→16 dimensions per chunk. At
  d=128, qdrant's X64 *is* the paper's recommended m=8, k*=256,
  64-bit code.
- **Residual encoding** — the idea outlived IVF. DiskANN keeps PQ
  codes in RAM to steer SSD reads
  ([reading-diskann.md](reading-diskann.md)).
- **OPQ** (Ge, He, Ke, Sun, CVPR 2013 — rotate the space before
  chunking so subspaces decorrelate) — the main refinement worth
  knowing exists; it attacks Step 2's independence assumption
  directly, and §II-B's own remark about random orthogonal matrices
  is where the thread starts.
- **What did *not* survive**: the §III-C bias correction, rejected by
  its own authors. What replaced it in practice is
  oversample-and-rescore
  ([reading-qdrant-quantization.md](reading-qdrant-quantization.md)),
  which fixes ranking errors rather than distance bias.

## How to read the paper (with the concepts in hand)

| paper | step | what to extract |
|---|---|---|
| §II-A | 1 | Eq. 4-5 (Lloyd conditions); the ⌈log₂ k⌉-bit code |
| §II-B | 2 | Eq. 8 (the split), Eq. 10 (`k = (k*)^m`), Table I, and the "k*=256, m=8" recommendation |
| §III-A | 3 | Eq. 12 (SDC) vs Eq. 13 (ADC), Table II's cost columns, and the closing recommendation |
| §III-B, §III-C | 3 | the distortion analysis and Eq. 25's correction — then Fig. 4 and the sentence rejecting it |
| §IV-A | 4 | Eq. 28-29, the residual argument; translate it into FOR terms as you read |
| §IV-B, §IV-C | 4 | the inverted-list entry layout, and `n·w/k′` |
| §V-A | — | Table III (SIFT: d=128, 1M base, 10k queries) and the recall@R *definition* |
| §V-B | 2, 3 | the m-vs-k* trade at fixed code length; "ADC k*=64 ≈ SDC k*=256" |
| §V-E | 3, 4 | Table V — SDC/ADC/IVFADC times, comparisons and recall in one place |

The distortion formalism in §II is denser than it needs to be. Keep
"product of small codebooks = exponential effective codebook" in
front of you and the algebra follows.

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

## Done when

Answer each before unfolding it.

- [ ] You can explain the product move: why quantizing subspaces independently gives 2^128 effective centroids from 16 bytes.
  <details><summary>Answer</summary>

  §II-B, Eq. 8-10. Split d=128 into m=16 chunks of D*=8, run a
  separate 256-centroid k-means on each, concatenate the 16 one-byte
  codes. The implied codebook is the Cartesian product (Eq. 9), so
  its cardinality is `(k*)^m = 256^16 = 2^128` (Eq. 10), while
  storage is `m·k*·D* = k*·d = 256 × 128 = 32 768` floats = 128 kB
  and encoding costs `k*·d = 32 768` multiply-adds. A plain quantizer
  at the same effective k would need `k·D` floats, which for
  `k = 2^128` is not a storable number. Table I states the general
  form: `k^(1/m)·D` versus `k·D`.
  </details>

- [ ] You can state the difference between SDC and ADC and say where each eats its approximation.
  <details><summary>Answer</summary>

  SDC (Eq. 12) quantizes the query as well and looks up
  centroid-to-centroid distances, so it eats *two* approximations —
  query error plus database error — and its per-subquantizer table
  holds `(k*)²` entries (or `k*(k*−1)/2` by symmetry, footnote 1).
  ADC (Eq. 13) leaves the query exact and builds an `[m × k*]` table
  of query-chunk-to-centroid distances per query, eating *one*
  approximation. Table II shows their costs are the same shape: SDC
  pays `k*D` to encode the query, ADC pays `k*D` to build its table,
  both scan at `nm`. Table V measures the payoff on GIST at 64-bit
  codes: 16.8 ms / recall@100 0.446 for SDC versus 17.2 ms / 0.652
  for ADC. §III-A's own words: *"one should then use the asymmetric
  version."*
  </details>

- [ ] You can explain why chunks must be roughly statistically independent, and what correlated dimensions do to the code.
  <details><summary>Answer</summary>

  The product quantizer's error decomposes as
  `MSE(q) = Σ_j MSE(q_j)` (Eq. 11) *because the subspaces are
  orthogonal*, and each subquantizer spends its 8 bits describing
  variation within its own chunk only. If two chunks carry
  near-duplicate information, both spend bits encoding the same
  degree of freedom and the effective code length is shorter than
  `m·log₂ k*`. Conversely, if one chunk carries most of the variance
  and another almost none, the low-variance chunk's 256 centroids are
  wasted while the high-variance chunk is under-resolved — which is
  why §II-B says each subvector should have *"on average, a
  comparable energy"*. OPQ learns a rotation that equalises and
  decorrelates before splitting. The topic 12 analogue is
  BYTE_STREAM_SPLIT: regrouping bytes so each stream is internally
  homogeneous, letting a per-stream encoder do its job.
  </details>

- [ ] You can compute the per-query ADC table build cost and say at what candidate count it stops mattering.
  <details><summary>Answer</summary>

  Build cost is `m · k* · D* = k* · d` — the m cancels — so at
  k*=256, d=128 it is 32 768 multiply-adds, exactly the cost of
  encoding one vector (Table I). Per-candidate cost is `m` table
  lookups and adds; at m=16 that is 16 operations. Break-even is
  `32 768 / 16 = 2 048` candidates. Below a ~2 000-candidate
  shortlist you are paying more to build the table than to use it,
  which is visible in Table V: IVFADC with k′=1024, w=1 scans only
  1 947 codes yet still takes 1.5 ms. The table also has to fit in
  cache to be worth it — `m × 256 × 4 B` is 16 kB at m=16, and §II-B
  warns explicitly about the case where it *"does no longer fit in
  cache memory."*
  </details>

- [ ] You can say why IVFADC encodes residuals rather than raw vectors, in terms of the quantizer's dynamic range.
  <details><summary>Answer</summary>

  §IV-A, Eq. 28-29: the coarse quantizer already captures where in
  the space the vector lives, so the PQ only has to describe the
  offset within one Voronoi cell — *"the energy of the residual
  vector is small compared to that of the vector itself"*. The same
  256 centroids per subspace therefore cover a much smaller range and
  quantize it finer, so §IV can claim residual encoding *"slightly
  improves the search accuracy"* on top of the speedup. In topic 12's
  vocabulary this is frame-of-reference: subtract a per-block
  reference value, encode the small remainder in fewer bits. The
  paper's own analogy is positional: coarse code = most significant
  bits, PQ code = least significant bits. The consequence to remember
  is that the ADC table depends on the probed cell (Eq. 30-31), so it
  is rebuilt `w` times per query, not once.
  </details>

- [ ] You can say what the paper derives in §III-C and why it then tells you not to use it.
  <details><summary>Answer</summary>

  §III-C derives an unbiased estimator (Eq. 25) by adding each
  subquantizer's mean distortion `ξ_j` to the ADC estimate, since
  `E[d(x,y)²] = d̃(x,y)² + ξ(q, q(y))` (Eq. 24). Figure 4 measures it
  on 10 000 SIFT vectors at m=8, k*=256: the bias drops from −0.044
  to 0.002, but the variance rises from 0.00146 to 0.00155 — the
  classic bias/variance exchange. Worse, the correction is largest
  for rare codes, so it penalises exactly the vectors most likely to
  be true near neighbours. The paper concludes *"the correction
  returns inferior results on average… we advocate the use of
  Equation 13"* and keeps the corrected form only for when you want
  the distances themselves rather than a ranking. This is the repo's
  "report the negative result" rule, in a 2011 paper.
  </details>

- [ ] You wrote answers to all five questions in notes.md.
  <details><summary>Answer</summary>

  Question 1's arithmetic, since it is the one most often fudged: at
  a fixed 16 bytes = 128 bits, m=16 means `128/16 = 8` bits per
  subquantizer (k*=256, D*=8), while m=64 means `128/64 = 2` bits
  (k*=4, D*=2). Same storage, same effective `(k*)^m = 2^128`, but
  §II-B and §V-B both say the m=16 side wins: *"for a fixed number of
  bits, it is better to use a small number of subquantizers with many
  centroids."* What changes is the LUT (16 kB vs 64 × 4 × 4 = 1 kB),
  the scan cost (16 adds vs 64), and the resolution within each
  subspace — 4 centroids cannot describe an 2-d chunk usefully.
  </details>

## References

**Papers**
- Jégou, Douze, Schmid — "Product Quantization for Nearest Neighbor
  Search" (IEEE TPAMI 33(1):117-128, 2011;
  [inria-00514462v2](https://inria.hal.science/inria-00514462v2))

| where | what it says |
|---|---|
| §II-A, Eq. 4-5 | Lloyd conditions; code is ⌈log₂ k⌉ bits |
| §II-B, Eq. 8 | the split into m subvectors of D* = D/m |
| §II-B, Eq. 10 | `k = (k*)^m` — the exponential effective codebook |
| §II-B, Table I | `mk*D* = k^(1/m)D` storage vs plain k-means's `kD` |
| §II-B | *"Using k* = 256 and m = 8 is often a reasonable choice"*; the LUT-in-cache warning |
| §III-A, Eq. 12 | SDC; `(k*)²` table per subquantizer (footnote 1: `k*(k*−1)/2`) |
| §III-A, Eq. 13 | ADC; `[m × k*]` table built per query |
| §III-A, Table II | equal query-prep cost; *"one should then use the asymmetric version"* |
| §III-C, Eq. 25, Fig. 4 | the bias correction: −0.044 → 0.002 bias, 0.00146 → 0.00155 variance, and the recommendation against it |
| §IV-A, Eq. 28-29 | residual `r(y) = y − q_c(y)`; coarse = MSBs, PQ = LSBs |
| §IV-A | `k′` from 1 000 to 1 000 000 for SIFT |
| §IV-B | inverted-list entry: identifier (8-32 bits) + code (`m⌈log₂ k*⌉` bits) |
| §IV-C | multiple assignment `w`; *"about n×w/k′ entries have to be parsed"* |
| §V-A, Table III | SIFT d=128 / 1M base / 10k queries; recall@R is a 1-NN measure |
| §V-B | *"for a fixed number of bits, … a small number of subquantizers with many centroids"*; ADC k*=64 ≈ SDC k*=256 |
| §V-E, Table V | GIST 64-bit codes: SDC 16.8 ms/0.446, ADC 17.2 ms/0.652, IVFADC rows |

- Ge, He, Ke, Sun — "Optimized Product Quantization" (CVPR 2013) —
  optional; the rotation refinement worth knowing exists

**Code** — `qdrant/qdrant@44ad62f`, pinned in `resources/codebases.md`.

| file:line | what |
|---|---|
| `lib/quantization/src/encoded_vectors_pq.rs:30` | `CENTROIDS_COUNT = 256` |
| `lib/quantization/src/encoded_vectors_pq.rs:38-43` | `EncodedQueryPQ { lut }` — the ADC table |
| `lib/quantization/src/encoded_vectors_pq.rs:515-537` | building it, Eq. 13 |
| `lib/quantization/src/encoded_vectors_pq.rs:474-489` | the ADC scan |
| `lib/segment/src/types.rs:749-757` | `CompressionRatio { X4 … X64 }` |
| `lib/segment/src/vector_storage/quantized/quantized_vectors.rs:2314-2322` | ratio → dimensions per chunk (D*) |

Walked in
[reading-qdrant-quantization.md](reading-qdrant-quantization.md).
