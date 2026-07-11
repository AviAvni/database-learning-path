# GAT: when the edge weights are computed per query

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

The three kernels for one destination row, spelled out:

```rust
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

## References

**Papers**
- Veličković, Cucurull, Casanova, Romero, Liò, Bengio — "Graph
  Attention Networks" (ICLR 2018,
  [arXiv:1710.10903](https://arxiv.org/abs/1710.10903)) — §2.1 is the
  layer; the rest is evaluation

**Code**
- [pytorch_geometric](https://github.com/pyg-team/pytorch_geometric)
  `torch_geometric/nn/conv/gat_conv.py` — score split :392, segmented
  softmax :404, message :408; note the absent `message_and_aggregate`
