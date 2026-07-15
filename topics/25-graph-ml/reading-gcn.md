# GCN: the two-line neural network your engine already runs

Kipf & Welling made GNNs a two-line equation. This chapter builds the
two lines step by step — the task, the neighbor-averaging idea, the
normalization that makes it stackable, and the kernel view — until
"a GCN forward pass is a query" stops being a slogan. Then read §2
for the layer, §3 for why it's a first-order spectral approximation
(skimmable), and appendix B for the actual dimensions — and notice
everything is operations your engine already has.

## The problem in one sentence

Classify every node in a graph when only a handful carry labels —
Cora is the canonical case: **2,708 papers, 1,433-dimensional
features, 7 classes, and only ~140 labeled nodes (5%)** — so the
model must propagate label information along edges instead of
treating rows as independent.

## The concepts, step by step

### Step 1 — the task: semi-supervised node classification

Each vertex carries a feature vector (for Cora: a 1,433-wide
bag-of-words per paper) and a few vertices carry labels; the job is
to predict labels for all the rest. A plain classifier over the
feature rows ignores the graph — but the graph is most of the
signal: papers cite papers on the same subject, fraudsters transact
with fraudsters. "Semi-supervised" names the regime: 95% of the rows
participate in training as *structure* (their features flow along
edges) even though they contribute no label term to the loss. What's
needed is a way to make each vertex's representation depend on its
neighbors'.

### Step 2 — the idea: average your neighbors, then transform

One GCN layer sets each vertex's new representation to (roughly) the
average of its neighbors' current representations, pushed through a
small learned linear map and a nonlinearity. That's it — the
"convolution" is neighbor averaging, the same shape as a pixel
averaging its 3×3 window, except the window is the adjacency list.
Stacking layers widens the horizon: after one layer a vertex has
mixed in its 1-hop neighborhood, after two layers its 2-hop
neighborhood. The learned part is deliberately tiny: a d×h weight
matrix per layer, shared by every vertex — the graph does the
spatial work, the weights only re-mix feature channels.

### Step 3 — A_hat: self-loops and symmetric normalization

Raw neighbor-averaging has two bugs, and A_hat is the two-line fix
baked into a single matrix. The layer is:

```
  H(l+1) = sigma( D^-1/2 (A + I) D^-1/2  ·  H(l)  ·  W(l) )
           └──┬──┘ └──────────┬─────────┘  └─┬─┘    └─┬─┘
            relu     A_hat: fixed, sparse,   n x d    d x h
                     precomputed ONCE        dense    tiny dense
```

- `A + I`: self-loops so a vertex keeps its own features (the
  renormalization trick, §2.2). Without it, a vertex's own signal is
  discarded each layer and deep stacking oscillates.
- Symmetric normalization `D^-1/2 · D^-1/2` (D = the diagonal degree
  matrix): averages neighborhoods without letting hub degrees explode
  activations — each edge (u, v) is weighted 1/√(d_u · d_v). Compare
  topic 24's PageRank pull matrix (row-normalized `D^-1 A`) — same
  idea, symmetric so the operator stays PSD-friendly, which is what
  keeps its eigenvalues in [-1, 1] (question 1: that bound is the
  whole point).

The critical systems fact: A_hat depends only on the graph, not the
features or weights — compute it ONCE, reuse it every layer, every
epoch, every inference. PyG's `gcn_norm` (gcn_conv.py:45-71) is the
reference implementation: fill_diag with 1, deg^-0.5 masked at inf,
scale rows then columns. Our `gcn::gcn_norm` stub reproduces it in
CSR; the dense oracle `gcn_norm_dense` is the definitional check.

Two layers, softmax, cross-entropy on the few labeled nodes. That's
the whole model: `Z = softmax(A_hat · relu(A_hat X W1) · W2)` (eq. 9).

### Step 4 — the kernel view: one SpMM plus one tiny matmul

Strip the ML vocabulary and one layer is two matrix products: a
**SpMM** (sparse-times-dense matrix multiply — A_hat in CSR against
the n×h dense feature matrix; the aggregation) and a small dense
matmul (the n×d features against the d×h weights; the transform).
One layer, no framework — a query plan with two operators:

```rust
fn gcn_layer(a_hat: &Csr, h: &Mat, w: &Dense) -> Mat {
    let t = h.matmul(w);              // transform FIRST: n×d · d×h — because
                                      //   h < d, this shrinks what SpMM drags
    let mut out = Mat::zeros(h.n, w.cols);
    for v in 0..a_hat.n {             // aggregate: one SpMM row at a time
        for (u, w_vu) in a_hat.row(v) {          // w_vu = 1/√(d_v·d_u)
            for k in 0..w.cols { out[v][k] += w_vu * t[u][k]; }
        }
    }
    out.relu()                        // sigma — free
}
```

Per layer: one SpMM (`2·nnz·h` FLOPs) + one small dense matmul
(`2·n·d·h`). On our SBM bench the SpMM runs at 21.2 GFLOP/s — 81% of
dense matmul's throughput — because the 64-float dense rows amortize
the sparse gather. Fat right-hand sides forgive sparsity.

### Step 5 — associativity is a query plan

`A_hat · X · W` can be evaluated `(A_hat X) W` or `A_hat (X W)`, and
the choice swaps which term carries the big dimension — exactly a
join-ordering decision (topic 10). The SpMM costs `2·nnz·(width of
its dense operand)`: aggregate-first drags d-wide rows through the
sparse multiply, transform-first drags h-wide rows. On Cora (n=2708,
nnz=13K, d=1433, h=16) transform-first makes the sparse side 90x
cheaper, and the DENSE transform dominates; on our SBM (nnz=566K,
d=64) they're comparable — measured 3.42 ms SpMM vs 5.12 ms dense at
64-wide. Transform-first wins whenever h < d. Frameworks hardcode
this; a database would COST it (topic 10).

### Step 6 — inference is a query

Training needs gradients and a framework; *inference* on a static
graph needs neither. A_hat is a materialized matrix, W1 and W2 are
two small constants, and a GCN forward pass is: SpMM, small matmul,
relu, repeat, softmax — a fixed two-operator plan over data the
engine already stores. That's the M25 claim in one sentence: the M20
sparse core plus a dense feature matrix IS a GNN inference engine.
What it costs: the graph is baked into A_hat at whatever moment you
materialized it — staleness semantics land on you, not the framework
(question 4).

### Step 7 — the limits, and why the next two papers exist

Three built-in ceilings, each motivating a successor:

- Full-batch: every layer touches every vertex — memory O(n·d) per
  layer. GraphSAGE's answer: sample (reading-graphsage.md).
- Fixed, feature-independent weights in A_hat. GAT's answer: learn
  them per-edge (reading-gat.md).
- Oversmoothing: stacking k layers ≈ k-step diffusion → features
  converge to the dominant eigenvector; deep GCNs die. Two layers is
  not a style choice, it's the working regime.

## How to read the paper (with the concepts in hand)

- §2 is Steps 2–3: the layer, the renormalization trick (§2.2), and
  eq. 9's full two-layer model. This is the part to read carefully.
- §3 derives the layer as a first-order approximation of spectral
  graph convolutions — skimmable; the derivation justifies but never
  changes the two lines.
- Appendix B has the actual dimensions — read it against Step 5's
  FLOP counts and check the associativity argument on the paper's
  own numbers.
- Keep `gcn_conv.py:45-71` open as the executable form of §2.2; the
  paper's notation and the code's variable names map one-to-one.

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

## References

**Papers**
- Kipf & Welling — "Semi-Supervised Classification with Graph
  Convolutional Networks" (ICLR 2017,
  [arXiv:1609.02907](https://arxiv.org/abs/1609.02907)) — §2 for the
  layer, §3 skimmable, appendix B for the dimensions

**Code**
- [pytorch_geometric](https://github.com/pyg-team/pytorch_geometric)
  `torch_geometric/nn/conv/gcn_conv.py` — `gcn_norm` (:45-71) is the
  reference A_hat construction our `gcn::gcn_norm` stub reproduces
