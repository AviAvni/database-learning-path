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

> **In:** a graph — vertices with feature vectors, edges, and labels on a
> small fraction of vertices.
> **Out:** the requirement that drives every later step — each vertex's
> representation must depend on its neighbours', not just its own row. Step 2
> is the mechanism that delivers it.

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

> **In:** the adjacency and the feature rows from Step 1.
> **Out:** one layer's rule — a neighbour-averaged, linearly transformed
> representation per vertex. Step 3 fixes the two bugs in the plain average.

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

> **In:** the raw adjacency `A` and the **degree matrix** `D` (the diagonal
> matrix whose entry `D_ii` is vertex *i*'s number of edges).
> **Out:** `A_hat`, the fixed propagation matrix computed once from the graph
> alone. Step 4 multiplies it against the features.

Raw neighbor-averaging has two bugs, and A_hat is the two-line fix
baked into a single matrix. The layer is (Kipf & Welling eq. 2):

```
  H(l+1) = sigma( D^-1/2 (A + I) D^-1/2  ·  H(l)  ·  W(l) )
           └──┬──┘ └──────────┬─────────┘  └─┬─┘    └─┬─┘
            relu     A_hat: fixed, sparse,   n x d    d x h
                     precomputed ONCE        dense    tiny dense
```

- `A + I`: **self-loops** — adding the identity `I` so a vertex keeps its
  own features (the renormalization trick, §2.2). Kipf & Welling write the
  self-looped adjacency `Ã = A + I_N` and its degree `D̃_ii = Σ_j Ã_ij`
  (§2.2). Without it, a vertex's own signal is discarded each layer and deep
  stacking oscillates.
- Symmetric normalization `D̃^-1/2 · D̃^-1/2` (D̃ = the diagonal degree
  matrix of `Ã`): averages neighborhoods without letting hub degrees explode
  activations — each edge (u, v) is weighted `1/√(d̃_u · d̃_v)`. Compare
  topic 24's PageRank pull matrix (row-normalized `D^-1 A`) — same
  idea, but symmetric so the operator stays PSD-friendly, which is what
  keeps its eigenvalues in [-1, 1] (question 1: that bound is the
  whole point).

Work it by hand on a 3-vertex path `1—2—3` to see why symmetric ≠
row-normalized. `A` has edges (1,2) and (2,3); `Ã = A + I`, so the
self-looped degrees are `D̃ = diag(2, 3, 2)` (vertex 2 has two neighbours
plus itself). With `Â_ij = Ã_ij / √(D̃_ii·D̃_jj)`:

```text
        1        2        3
1 [   1/2    1/√6      0   ]      row sum 0.908
2 [  1/√6     1/3    1/√6  ]      row sum 1.149
3 [    0     1/√6     1/2  ]      row sum 0.908
```

The rows do **not** sum to 1 — the symmetric form scales every edge by
*both* endpoints' degrees, so it is not row-stochastic. The random-walk
matrix `D^-1 Ã` *would* be row-stochastic (rows `[1/2,1/2,0]`,
`[1/3,1/3,1/3]`, `[0,1/2,1/2]`). That distinction matters in Step 4: the
measured lane uses the row-stochastic form, not this symmetric one.

The critical systems fact: A_hat depends only on the graph, not the
features or weights — compute it ONCE, reuse it every layer, every
epoch, every inference. PyG's `gcn_norm` (gcn_conv.py:45, the dense/edge
branch) is the reference implementation: `add_self_loops` for `A + I`, then
`deg.pow_(-0.5)` with the infinities from isolated vertices masked to 0
(gcn_conv.py:67-68), then scale rows and columns (gcn_conv.py:69-70). Our
`gcn::gcn_norm` stub reproduces it in CSR; the dense oracle
`gcn_norm_dense` is the definitional check.

Two layers, softmax, cross-entropy on the few labeled nodes. That's
the whole model: `Z = softmax(A_hat · relu(A_hat X W1) · W2)` (eq. 9).

### Step 4 — the kernel view: one SpMM plus one tiny matmul

> **In:** `A_hat` from Step 3, the `n×d` feature matrix, and the `d×h` weight.
> **Out:** one layer's output, expressed as two kernels — a sparse aggregation
> and a dense transform. Step 5 chooses the order to run them in.

Strip the ML vocabulary and one layer is two matrix products: a
**SpMM** (sparse-times-dense matrix multiply — A_hat in CSR against
the n×h dense feature matrix; the aggregation) and a small dense
matmul (the n×d features against the d×h weights; the transform).
One layer, no framework — a query plan with two operators:

```rust
// ILLUSTRATION — not quoted; the measured kernels are spmm.rs:18
// (row_norm_adj SpMM) and gcn.rs:51 (gcn_layer), and PyG's fused form is
// gcn_conv.py:273 (message_and_aggregate -> spmm(adj_t, x, reduce=aggr)).
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
(`2·n·d·h`). On this topic's SBM bench the message-passing SpMM runs at
**16.82 GFLOP/s in 4.31 ms, against 5.65 ms for the dense transform beside
it** (FINDINGS.md row 25) — roughly 71% of the dense kernel's throughput
(16.82 / 23.75 GFLOP/s, the dense figure being `2·n·d·h = 2·16384·64·64 =
134 MFLOP` over 5.65 ms). The 64-float dense rows amortize the sparse
gather: fat right-hand sides forgive sparsity. One honesty note (rule 6):
the *measured* lane normalizes with the row-stochastic `D^-1 A`
(`spmm.rs:38 row_norm_adj`, driven from `bin/gnn_bench.rs`), not the
symmetric `A_hat` of Step 3 — `gcn::gcn_norm` is still a stub. Both are the
same SpMM shape (`2·nnz·64`), so the timing is a faithful proxy for the
symmetric kernel; the number is the aggregation cost, not a claim about
which normalization ran.

### Step 5 — associativity is a query plan

> **In:** the three-factor product `A_hat · X · W` from Step 4.
> **Out:** the cheaper of the two evaluation orders, chosen by which
> dimension the sparse multiply has to drag. Step 6 reuses this plan at
> inference time.

`A_hat · X · W` can be evaluated `(A_hat X) W` or `A_hat (X W)`, and
the choice swaps which term carries the big dimension — exactly a
join-ordering decision (topic 10). The SpMM costs `2·nnz·(width of
its dense operand)`: aggregate-first drags d-wide rows through the
sparse multiply, transform-first drags h-wide rows. On Cora (n=2708,
nnz=13K, d=1433, h=16) transform-first makes the sparse side 90x
cheaper, and the DENSE transform dominates; on our SBM (nnz=566K,
d=64) they're comparable — measured **4.31 ms SpMM against 5.65 ms dense**
at 64-wide (FINDINGS.md row 25). Transform-first wins whenever h < d.
Frameworks hardcode this; a database would COST it (topic 10).

### Step 6 — inference is a query

> **In:** a trained model — the materialized `A_hat` and the two weight
> matrices `W1`, `W2`.
> **Out:** a forward pass as a fixed two-operator plan over stored data.
> Step 7 names where that plan hits its ceilings.

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

> **In:** the working two-layer GCN from Steps 3–6.
> **Out:** its three structural ceilings, each naming the successor paper
> that lifts it.

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

## Done when

Answer each before unfolding it.

- [ ] You can write `A_hat = D^-1/2 (A+I) D^-1/2` and say why its eigenvalues lie in [-1, 1].

  <details><summary>Answer</summary>

  `Ã = A + I` adds self-loops; `D̃` is its diagonal degree matrix; the
  symmetric scaling `D̃^-1/2 Ã D̃^-1/2` weights each edge by
  `1/√(d̃_u·d̃_v)`. It is similar to the random-walk matrix `D̃^-1 Ã`
  (share a spectrum via `D̃^1/2`), whose eigenvalues lie in [-1, 1] because
  it is stochastic; the renormalization trick (§2.2) shifts the raw
  `I + D^-1/2 A D^-1/2` — whose spectrum reaches into [0, 2] and blows up
  under repeated application — into that stable band. Bounded eigenvalues
  are what let you stack layers without activations exploding or vanishing.

  </details>

- [ ] You can decompose a layer into one SpMM plus one small dense matmul.

  <details><summary>Answer</summary>

  A layer is `σ(A_hat · X · W)`. `X · W` is a dense `n×d · d×h` matmul (the
  transform, `2·n·d·h` FLOPs); `A_hat · (XW)` is a SpMM of the CSR matrix
  against the `n×h` dense result (the aggregation, `2·nnz·h` FLOPs); `σ` is
  a free elementwise relu. Two operators, one sparse and one dense — the
  same shape as `spmm.rs:18` beside a dense `matmul`.

  </details>

- [ ] You can explain why associativity is a query plan, and count FLOPs both ways on this topic's SBM — the measured SpMM is 4.31 ms against 5.65 ms for the dense transform.

  <details><summary>Answer</summary>

  `(A_hat X) W` vs `A_hat (X W)` are equal by associativity but cost
  differently: the SpMM's cost is `2·nnz·(width of its dense operand)`, so
  aggregate-first drags the `d`-wide feature rows through the sparse
  multiply and transform-first drags the `h`-wide rows. Transform-first
  wins whenever `h < d`. On the SBM (nnz≈566K, d=h=64) the two widths match,
  so the measured kernels are comparable: 4.31 ms for the SpMM at 16.82
  GFLOP/s against 5.65 ms for the dense transform (FINDINGS.md row 25).
  Picking the order is exactly join-ordering (topic 10).

  </details>

- [ ] You can say what being baked into `A_hat` at training time costs when a node arrives.

  <details><summary>Answer</summary>

  `A_hat` is materialized from the graph as it stood when you built it, so a
  new node or edge is invisible until you recompute the affected rows —
  `add_self_loops` and the `1/√(d̃_u·d̃_v)` scaling both shift when a
  neighbour's degree changes. `W1`/`W2` are unaffected (they re-mix feature
  channels, not structure) and need no refresh. So the cheap fix is to
  re-normalize the touched rows of `A_hat`; retraining `W` is the expensive
  path and usually unnecessary for a small structural delta.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including what pending deltas mean for a forward pass over the M20 graph.

  <details><summary>Answer</summary>

  The five questions live in `## Questions`; the M25 one asks whether the
  M20 delta-matrix's pending (un-merged) edges participate in `A_hat`. They
  do only if you fold the delta into the degree counts and re-scale the
  affected rows before the SpMM — the same three-way "read committed / read
  pending / merge-then-read" choice as topic 24's `CALL algo.wcc`. Write the
  reasoning out in notes.md.

  </details>

## References

**Papers**
- Kipf & Welling — "Semi-Supervised Classification with Graph
  Convolutional Networks" (ICLR 2017,
  [arXiv:1609.02907](https://arxiv.org/abs/1609.02907)) — §2 for the
  layer, §3 skimmable, appendix B for the dimensions

**Code**
- [pytorch_geometric](https://github.com/pyg-team/pytorch_geometric)
  `torch_geometric/nn/conv/gcn_conv.py` — `gcn_norm` (def at :45, the
  `deg.pow_(-0.5)` scaling at :67-70) is the reference A_hat construction our
  `gcn::gcn_norm` stub reproduces; `GCNConv.message_and_aggregate` (:273)
  is the fused `spmm(adj_t, x, reduce='add')` form of Step 4.
