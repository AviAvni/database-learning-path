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

In GCN (reading-gcn.md), the weight on edge (u, v) during aggregation
is 1/√(d_u·d_v) — computed from degrees alone, identical for every
layer, every epoch, every input. That makes A_hat precomputable
(compute once, reuse forever — GCN's great systems virtue), but also
content-blind: the aggregation cannot prefer one neighbor over
another no matter what their features say. Any task where *which*
neighbor matters — one incriminating transaction among a hundred
routine ones — is beyond what a structural constant can express.

### Step 2 — attention: score each edge from its endpoints' features

GAT's move: compute a per-edge score from the *current features* of
the edge's two endpoints, using a small learned vector `a`. Transform
both endpoint features with the shared weight matrix W, concatenate,
dot with `a`, and pass through LeakyReLU (a ReLU variant that leaks a
small slope for negative inputs, keeping gradients alive):

```
  e_uv = LeakyReLU( a^T [ W h_u || W h_v ] )      per EDGE (u,v) ∈ A
```

The score says "how much should v listen to u, given what both look
like right now". Two properties matter: it's computed only where an
edge exists (the graph still gates who may talk to whom — attention
reweights the adjacency, it doesn't replace it), and it changes every
forward pass, because h changes. That second property is the whole
systems story — Step 6.

### Step 3 — softmax: turning scores into weights that sum to one

Raw scores have arbitrary scale, so each vertex normalizes the scores
on its incoming edges with a softmax (exponentiate, divide by the
sum), yielding attention weights alpha that sum to 1 over each
vertex's in-neighborhood:

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

PyG anchors: score assembly `alpha_j + alpha_i` at gat_conv.py:392
(the `a^T [x||y]` split into two halves — a_src·h_u + a_dst·h_v,
computed as per-NODE terms then added per-edge: an optimization worth
noticing, and question 3's subject — it turns O(m·d) score work into
O(n·d) + O(m), the classic factor-computation-out-of-the-join move),
`softmax(alpha, index, ptr)` at :404 (segmented softmax over CSR
rows), message = `x_j * alpha` at :408. No `message_and_aggregate` —
the fused SpMM path can't apply because the matrix values are
recomputed per forward pass.

### Step 5 — the price list: extra passes and multi-head structure sharing

Counting edge passes per layer: GCN does one (the SpMM). GAT does the
SDDMM, the softmax's max pass, its exp-sum pass, and the SpMM — the
sparse-softmax is a segmented reduction over CSR rows, same shape as
topic 20's row-wise SpMV, run twice. Call it ~3 extra passes over the
edges per layer (question 2 turns this into a forward-time estimate
against our 21 GFLOP/s SpMM lane). **Multi-head attention** (K
independent attention weightings whose outputs are concatenated — the
standard variance-reduction trick) multiplies everything by K — it's
K SpMMs with shared structure, different values. A delta-matrix
engine would store one structure + K value arrays (FalkorDB's
multi-value matrix problem, again).

### Step 6 — the line this pair of papers draws: materialize vs compute

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
