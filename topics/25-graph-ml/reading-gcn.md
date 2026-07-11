# Reading guide — "Semi-Supervised Classification with Graph Convolutional Networks" (Kipf & Welling, ICLR 2017)

The paper that made GNNs a two-line equation. Read §2 for the layer, §3
for why it's a first-order spectral approximation (skimmable), and
appendix B for the actual dimensions — then notice everything is
operations your engine already has.

## The layer

```
  H(l+1) = sigma( D^-1/2 (A + I) D^-1/2  ·  H(l)  ·  W(l) )
           └──┬──┘ └──────────┬─────────┘  └─┬─┘    └─┬─┘
            relu     A_hat: fixed, sparse,   n x d    d x h
                     precomputed ONCE        dense    tiny dense
```

- `A + I`: self-loops so a vertex keeps its own features (renormalization
  trick, §2.2). Without it, deep stacking oscillates.
- Symmetric normalization `D^-1/2 · D^-1/2`: averages neighborhoods
  without letting hub degrees explode activations. Compare topic 24's
  PageRank pull matrix (row-normalized `D^-1 A`) — same idea, symmetric so
  the operator stays PSD-friendly.
- Two layers, softmax, cross-entropy on the few labeled nodes. That's the
  whole model: `Z = softmax(A_hat · relu(A_hat X W1) · W2)` (eq. 9).

PyG's `gcn_norm` (gcn_conv.py:45-71) is the reference implementation of
A_hat: fill_diag with 1, deg^-0.5 masked at inf, scale rows then columns.
Our `gcn::gcn_norm` stub reproduces it in CSR; the dense oracle
`gcn_norm_dense` is the definitional check.

## What the engine sees

Per layer: one SpMM (`2·nnz·h` FLOPs) + one small dense matmul
(`2·n·d·h`). On Cora (n=2708, nnz=13K, d=1433, h=16) the DENSE transform
dominates; on our SBM (nnz=566K, d=64) they're comparable — measured
3.42 ms SpMM vs 5.12 ms dense at 64-wide. The associativity choice
`(A X) W` vs `A (X W)` swaps which term carries the big dimension:
transform-first wins whenever h < d. Frameworks hardcode this; a database
would COST it (topic 10).

Inference on a static graph needs no autograd, no framework: A_hat is a
materialized matrix, weights are two small constants — a GCN forward is a
**query**. That's the M25 claim in one sentence.

## Limits worth knowing (they motivate the next two papers)

- Full-batch: every layer touches every vertex — memory O(n·d) per layer.
  GraphSAGE's answer: sample (reading-graphsage.md).
- Fixed, feature-independent weights in A_hat. GAT's answer: learn them
  per-edge (reading-gat.md).
- Oversmoothing: stacking k layers ≈ k-step diffusion → features converge
  to the dominant eigenvector; deep GCNs die. Two layers is not a style
  choice, it's the working regime.

## Questions (answer in notes.md)

1. Show `A_hat = D^-1/2 (A+I) D^-1/2` has eigenvalues in [-1, 1] and why
   that matters for stacking (the renormalization trick's actual job).
2. Two GCN layers = each vertex sees its 2-hop neighborhood. Relate the
   receptive field to topic 24's BFS frontier — what graph property makes
   "2 hops" already cover most of an RMAT graph, and what does that do to
   oversmoothing there?
3. Count FLOPs both association orders for Cora and for our SBM bench
   config; where's the crossover h/d ratio?
4. The graph is BAKED into A_hat at training time. What happens to a
   trained GCN's accuracy when the graph gets 10% new edges — and which
   part (A_hat or W) can the database refresh cheaply?
5. For M25: a GCN forward over the M20 delta-matrix graph — do pending
   deltas participate in A_hat, and is that the same decision as topic
   24's `CALL algo.wcc` three-option question?
