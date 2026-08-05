# GAT: when the edge weights are computed per query

GCN's A_hat weights are structural constants (degree math). GAT makes
them FUNCTIONS of the features on each edge — learned, per-edge,
softmax-normalized. For an engine, the interesting part is what that
does to the kernel: aggregation stops being one SpMM and becomes
SDDMM + softmax + SpMM. This chapter builds that step by step — the
limitation GAT attacks, the attention score, the normalization, and
the three-kernel pipeline — ending at the materialize-vs-compute line
every database person will recognize.

## The problem in one sentence

GCN weighs every neighbor by 1/√(d_u·d_v) — pure degree arithmetic,
fixed before training starts — so a fraud vertex's one incriminating
neighbor counts exactly as much as its 99 innocuous ones; GAT lets
the model *learn* which of the 100 to listen to, at the price of
**~3 extra passes over all 566K edges per layer** on our bench graph.

## The concepts, step by step

### Step 1 — the limitation: GCN's weights are structural constants

> **In:** GCN's aggregation weight on each edge (reading-gcn.md).
> **Out:** the expressiveness gap — a degree constant cannot prefer one
> neighbour over another. Step 2 replaces the constant with a learned score.

In GCN (reading-gcn.md), the weight on edge (u, v) during aggregation
is 1/√(d_u·d_v) — computed from degrees alone, identical for every
layer, every epoch, every input. That makes A_hat precomputable
(compute once, reuse forever — GCN's great systems virtue), but also
content-blind: the aggregation cannot prefer one neighbor over
another no matter what their features say. Any task where *which*
neighbor matters — one incriminating transaction among a hundred
routine ones — is beyond what a structural constant can express.

### Step 2 — attention: score each edge from its endpoints' features

> **In:** the endpoint features `h_u`, `h_v` and the shared transform `W`.
> **Out:** a raw per-edge score `e_uv`. Step 3 normalizes these into weights.

GAT's move: compute a per-edge score from the *current features* of
the edge's two endpoints, using a small learned vector `a`. Transform
both endpoint features with the shared weight matrix W, concatenate,
dot with `a`, and pass through **LeakyReLU** (a ReLU variant that leaks a
small slope for negative inputs, keeping gradients alive; GAT fixes the
negative slope at **0.2**, §2.1, matched by PyG's
`negative_slope=0.2` default at gat_conv.py:136):

```
  e_uv = LeakyReLU_0.2( a^T [ W h_u || W h_v ] )      per EDGE (u,v) ∈ A
```

The score says "how much should v listen to u, given what both look
like right now". Two properties matter: it's computed only where an
edge exists (the graph still gates who may talk to whom — attention
reweights the adjacency, it doesn't replace it), and it changes every
forward pass, because h changes. That second property is the whole
systems story — Step 6.

### Step 3 — softmax: turning scores into weights that sum to one

> **In:** the raw per-edge scores `e_uv` from Step 2.
> **Out:** normalized attention weights `alpha_uv` summing to 1 over each
> vertex's in-neighbourhood, and the weighted aggregate `h'_v`. Step 4 maps
> all three formulas to kernels.

Raw scores have arbitrary scale, so each vertex normalizes the scores
on its incoming edges with a softmax (exponentiate, divide by the
sum), yielding attention weights alpha that sum to 1 over each
vertex's in-neighborhood (§2.1, eq. for α_ij):

```
  alpha_uv = softmax over v's in-edges ( e_uv )
  h'_v     = sigma( Σ_u  alpha_uv · W h_u )        weighted aggregate
```

The normalization is per-destination — over the in-edges of v, not
the out-edges of u — because it's v deciding how to divide its
attention among its sources. That choice has a storage consequence:
the kernel iterates in-neighborhoods, which means it wants the
transposed adjacency resident (question 1 — topic 20's transpose tax
again).

### Step 4 — the kernel view: SDDMM + segmented softmax + SpMM

> **In:** the three formulas from Steps 2–3 (score, softmax, weighted sum).
> **Out:** the three engine kernels they compile to. Step 5 prices them.

Now translate the three formulas into engine kernels. The score
computation is an **SDDMM** (sampled dense-dense matrix multiply: a
dense computation over pairs of rows, evaluated ONLY at positions
where the sparse matrix A has a nonzero) — a mask, exactly topic 24's
masked-SpGEMM pattern `(dense op) .* A`. Then a **segmented softmax**
(a softmax run independently over each CSR row — each vertex's
in-edge list is one segment). Then the familiar SpMM, but with values
that were computed microseconds ago:

```
  kernel view:
   step 1: SDDMM — dense scores computed ONLY where A is nonzero
            (a mask! topic 24's masked-SpGEMM pattern: (dense op) .* A)
   step 2: row-softmax over the sparse score matrix
   step 3: SpMM with the fresh weights
```

The three kernels for one destination row, spelled out:

```rust
// ILLUSTRATION — not quoted; PyG's real path is gat_conv.py:392
// (alpha_j + alpha_i), :403 (leaky_relu), :404 (segmented softmax),
// :408-409 (message = alpha * x_j). There is no message_and_aggregate.
fn gat_row(a_t: &Csr, v: u32, wh: &Mat, a_src: &[f32], a_dst: &[f32]) -> Vec<f32> {
    // SDDMM: dense scores, computed ONLY at A's nonzeros (in-edges of v)
    let e: Vec<f32> = a_t.row(v)
        .map(|u| leaky_relu(a_src[u as usize] + a_dst[v as usize])).collect();
    // segmented softmax over the CSR row (max pass, then exp-sum pass)
    let mx = e.iter().fold(f32::MIN, |m, &x| m.max(x));
    let z: f32 = e.iter().map(|&x| (x - mx).exp()).sum();
    // SpMM with the fresh weights — this row of A exists only for this query
    let mut out = vec![0.0; wh.d];
    for ((u, _), &ev) in a_t.row(v).zip(&e) {
        let alpha = (ev - mx).exp() / z;
        for k in 0..wh.d { out[k] += alpha * wh.row(u)[k]; }
    }
    out
}
```

PyG anchors: score assembly `alpha = alpha_j + alpha_i` at
gat_conv.py:392 (the `a^T [x||y]` split into two halves — a_src·h_u +
a_dst·h_v, computed as per-NODE terms `alpha_src`/`alpha_dst` at
gat_conv.py:330-331 then added per-edge: an optimization worth
noticing, and question 3's subject — it turns O(m·d) score work into
O(n·d) + O(m), the classic factor-computation-out-of-the-join move),
`softmax(alpha, index, ptr, ...)` at :404 (segmented softmax over CSR
rows), message = `alpha.unsqueeze(-1) * x_j` at :408-409. No
`message_and_aggregate` — the fused SpMM path can't apply because the
matrix values are recomputed per forward pass.

### Step 5 — the price list: extra passes and multi-head structure sharing

> **In:** the three-kernel pipeline from Step 4, and the multi-head count K.
> **Out:** the per-layer edge-pass count against GCN, and the K-fold cost of
> multi-head. Step 6 draws the materialize-vs-compute line.

Counting edge passes per layer: GCN does one (the SpMM). GAT does the
SDDMM, the softmax's max pass, its exp-sum pass, and the SpMM — the
sparse-softmax is a segmented reduction over CSR rows, same shape as
topic 20's row-wise SpMV, run twice. Call it ~3 extra passes over the
edges per layer (question 2 turns this into a forward-time estimate
against the **16.82 GFLOP/s** SpMM lane, FINDINGS.md row 25).
**Multi-head attention** (K independent attention weightings — the
standard variance-reduction trick) multiplies everything by K: it's
K SpMMs with shared structure, different values. The paper concatenates
the K heads on intermediate layers (`h'_i = ‖_{k=1}^K σ(Σ α_ij^k W^k h_j)`,
§2.1) but **averages** them on the final prediction layer, where concat
"is no longer sensible" (§2.1); on Cora it uses K=8 heads of 8 features
each. A delta-matrix engine would store one structure + K value arrays
(FalkorDB's multi-value matrix problem, again).

### Step 6 — the line this pair of papers draws: materialize vs compute

> **In:** GCN's precomputable `A_hat` and GAT's feature-dependent attention.
> **Out:** the materialized-view vs computed-view distinction that splits the
> two papers' systems profiles.

GCN's A_hat is a **materialized view**: computed once from the graph,
reused by every query, invalidated only by graph changes. GAT's
attention matrix is a **computed view**: its values depend on the
current features, so it exists only during a forward pass and can
never be cached across them. Dynamic edge weights kill
precomputation — that single fact separates the two papers' entire
systems profiles. The consolation prize is that the computed values
are *interpretable data*: a fraud analyst asks "WHY did this node
score high?", and sparse alpha — which edges carried the attention —
is the explanation (question 4: what Cypher surface exposes it).

## How to read the paper (with the concepts in hand)

- §2.1 is the layer — Steps 2–3 in the authors' notation. Read it
  mapping each formula to its kernel (Step 4): the a^T concat is the
  SDDMM, the alpha normalization is the segmented softmax, the
  weighted sum is the SpMM.
- Multi-head attention closes §2.1 — read it as Step 5: K value
  arrays over one shared sparsity structure.
- The rest is evaluation; skim it. The transductive results compare
  against GCN on the same citation graphs (reading-gcn.md's Cora),
  the inductive ones against GraphSAGE.
- Then `gat_conv.py:392-408` with Step 4's anchors — and notice what
  is *absent*: no `message_and_aggregate`, the fused path GCN and
  SAGE both take (reading-pyg-message-passing.md tells that story).

## Questions (answer in notes.md)

1. Why is the softmax over IN-edges of v (not out-edges of u), and what
   does that force about the storage direction (A vs A^T — topic 20's
   transpose tax)?
2. Count edge passes per GAT layer vs GCN layer. On our 566K-edge SBM
   at 21 GFLOP/s SpMM, estimate the forward-time ratio.
3. The a_src/a_dst per-node split at gat_conv.py:330-332 turns O(m·d) score
   work into O(n·d) + O(m). Which database trick is this (hint: factor
   computation out of a join)?
4. GAT attention weights are data — a fraud analyst asks "WHY did this
   node score high?" Sparse alpha is the explanation. What Cypher surface
   would expose it (edges with attention > t)?
5. For M25: is GAT worth engine support at all, or is GCN/SAGE + the
   vector index the 95% case? Argue from the kernel inventory each needs.

## Done when

Answer each before unfolding it.

- [ ] You can say what GCN's structural constant weights cannot express.

  <details><summary>Answer</summary>

  A GCN edge weight is `1/√(d_u·d_v)` — degree arithmetic, fixed before
  training and identical every layer and epoch. It is content-blind: it
  cannot prefer one neighbour over another based on their features, so any
  task where *which* neighbour matters (one incriminating transaction among a
  hundred routine ones) is beyond it. GAT makes the weight a learned function
  of the endpoints' current features instead.

  </details>

- [ ] You can explain why the softmax is over in-edges of v, and what normalizing over out-edges would mean.

  <details><summary>Answer</summary>

  Normalization is per-destination: it is v deciding how to divide its
  attention among its sources, so the softmax runs over v's in-edges and the
  weights sum to 1 there. Normalizing over u's out-edges would instead make u
  ration how much of itself it sends out — a different (and not what the paper
  wants) semantics. The in-edge choice forces the kernel to iterate
  in-neighbourhoods, which wants the transposed adjacency A^T resident
  (topic 20's transpose tax).

  </details>

- [ ] You can decompose a GAT layer into SDDMM, segmented softmax and SpMM.

  <details><summary>Answer</summary>

  SDDMM computes the per-edge scores `e_uv` only at A's nonzeros (a masked
  dense-dense product); a segmented softmax normalizes each CSR row's scores
  into `alpha`; the SpMM aggregates `Σ_u alpha_uv · W h_u`. The middle kernel
  is two passes (a max pass and an exp-sum pass), so a GAT layer is ~3 extra
  edge passes on top of GCN's single SpMM. PyG runs them at gat_conv.py:392
  (score), :404 (softmax), :408 (message).

  </details>

- [ ] You can count edge passes per GAT layer against per GCN layer on this topic's 566 K-edge SBM, whose SpMM measures 4.31 ms at 16.82 GFLOP/s.

  <details><summary>Answer</summary>

  GCN is one edge pass (the SpMM). GAT is roughly four: SDDMM, softmax-max,
  softmax-expsum, SpMM — about 4× the sparse traffic per layer, before the
  ×K for multi-head. Anchoring to the measured SpMM (4.31 ms at 16.82
  GFLOP/s, FINDINGS.md row 25) as the unit edge pass, a single-head GAT layer
  lands near 4×4.31 ≈ 17 ms of sparse work; do the full estimate against your
  own bench in notes.md, and remember the SDDMM/softmax passes move less data
  per edge than the 64-wide SpMM, so the true ratio is below 4.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including whether GAT is worth engine support at all.

  <details><summary>Answer</summary>

  All five `## Questions` answered in notes.md — the in-edge/transpose
  question, the edge-pass forward-time estimate, the a_src/a_dst
  factor-out-of-the-join trick, the Cypher surface for `attention > t`, and
  the M25 argument from each variant's kernel inventory (whether GCN/SAGE +
  the vector index already covers the 95% case).

  </details>

## References

**Papers**
- Veličković, Cucurull, Casanova, Romero, Liò, Bengio — "Graph
  Attention Networks" (ICLR 2018,
  [arXiv:1710.10903](https://arxiv.org/abs/1710.10903)) — §2.1 is the
  layer; the rest is evaluation

**Code**
- [pytorch_geometric](https://github.com/pyg-team/pytorch_geometric)
  `torch_geometric/nn/conv/gat_conv.py` — per-node score halves :330-331,
  edge score `alpha_j + alpha_i` :392, `leaky_relu` (slope 0.2) :403,
  segmented softmax :404, message :408-409; note the absent
  `message_and_aggregate`, and `negative_slope=0.2` default at :136
