# Reading guide — "Graph Attention Networks" (Veličković et al., ICLR 2018) — GAT

GCN's A_hat weights are structural constants (degree math). GAT makes
them FUNCTIONS of the features on each edge — learned, per-edge, softmax-
normalized. For an engine, the interesting part is what that does to the
kernel: aggregation stops being one SpMM and becomes SDDMM + softmax +
SpMM.

## The layer (§2.1)

```
  e_uv   = LeakyReLU( a^T [ W h_u || W h_v ] )      per EDGE (u,v) ∈ A
  alpha  = softmax_v( e_uv )                        normalize over v's in-edges
  h'_v   = sigma( Σ_u  alpha_uv · W h_u )           weighted aggregate

  kernel view:
   step 1: SDDMM — dense scores computed ONLY where A is nonzero
            (a mask! topic 24's masked-SpGEMM pattern: (dense op) .* A)
   step 2: row-softmax over the sparse score matrix
   step 3: SpMM with the fresh weights
```

PyG anchors: score assembly `alpha_j + alpha_i` at gat_conv.py:392 (the
`a^T [x||y]` split into two halves — a_src·h_u + a_dst·h_v, computed as
per-NODE terms then added per-edge: an optimization worth noticing),
`softmax(alpha, index, ptr)` at :404 (segmented softmax over CSR rows),
message = `x_j * alpha` at :408. No `message_and_aggregate` — the fused
SpMM path can't apply because the matrix values are recomputed per
forward pass.

## Why databases should care

- The sparse-softmax is a segmented reduction over CSR rows — same shape
  as topic 20's row-wise SpMV, run twice (max, then exp-sum). GAT costs
  ~3 extra passes over the edges vs GCN's one.
- Multi-head attention (K independent alpha sets, concat) multiplies
  everything by K — it's K SpMMs with shared structure, different values.
  A delta-matrix engine would store one structure + K value arrays
  (FalkorDB's multi-value matrix problem, again).
- Dynamic edge weights kill precomputation: GCN's A_hat is a materialized
  view; GAT's attention matrix is a per-query computed view. The
  materialize-vs-compute line runs exactly through this pair of papers.

## Questions (answer in notes.md)

1. Why is the softmax over IN-edges of v (not out-edges of u), and what
   does that force about the storage direction (A vs A^T — topic 20's
   transpose tax)?
2. Count edge passes per GAT layer vs GCN layer. On our 566K-edge SBM
   at 21 GFLOP/s SpMM, estimate the forward-time ratio.
3. The a_src/a_dst per-node split at gat_conv.py:332 turns O(m·d) score
   work into O(n·d) + O(m). Which database trick is this (hint: factor
   computation out of a join)?
4. GAT attention weights are data — a fraud analyst asks "WHY did this
   node score high?" Sparse alpha is the explanation. What Cypher surface
   would expose it (edges with attention > t)?
5. For M25: is GAT worth engine support at all, or is GCN/SAGE + the
   vector index the 95% case? Argue from the kernel inventory each needs.
