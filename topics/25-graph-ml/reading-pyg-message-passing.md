# PyTorch Geometric: one abstraction, the whole GNN literature

Read PyG the way topic 20 read SuiteSparse: as an existence proof that
one abstraction (here `MessagePassing`) covers a whole literature, and as
a map of which kernels actually matter. 90 minutes, code-first — but
first this chapter builds the abstraction step by step: what message
passing is, its two execution modes and the join they secretly
implement, the second primitive GAT forces into existence, and what
the generality costs.

## The problem in one sentence

Every GNN layer in the literature is "combine each vertex's
neighbors' vectors somehow" — and PyG's naive execution of that
builds an m × d temporary (one message per edge), which on our toy
566K-edge SBM at d=64 is already a **145 MB allocation per layer**
that the fused path replaces with zero bytes.

## The concepts, step by step

### Step 1 — message passing: three overridable functions

> **In:** the observation that every GNN layer is "combine each vertex's
> neighbours' vectors somehow".
> **Out:** the `MessagePassing` skeleton — `message`, `aggregate`, `update`
> (plus the fused `message_and_aggregate`) behind one `propagate` dispatcher.
> Step 2 executes it the literal way.

**Message passing** is the GNN literature's common skeleton: for each
edge, compute a **message** from the source vertex's vector; at each
vertex, **aggregate** the incoming messages with an order-insensitive
reduction (sum, mean, max); then **update** the vertex's vector from
the aggregate. Every layer type — GCN, SAGE, GAT, and hundreds more —
is this skeleton with different fillings, which is why PyG can be one
base class (`MessagePassing`, message_passing.py:39) with overridable
methods (`message`, `aggregate`, `update`, plus the fused
`message_and_aggregate` — Step 3) and a dispatcher (`propagate`,
:421) that orchestrates them. The payoff of recognizing the skeleton:
you stop reading 50 layer papers and start asking one question — what
goes in `message`, and can it fuse?

### Step 2 — the COO path: gather, message, scatter — and the m×d temp

> **In:** the `MessagePassing` skeleton from Step 1 and a COO `edge_index`.
> **Out:** the literal gather/message/scatter execution — and the `m×d`
> temporary it materializes. Step 3 fuses that temporary away.

The general execution strategy stores edges as a COO list
(coordinate format — a 2 × m array of (source, destination) pairs)
and runs the skeleton literally: gather each source's vector, apply
`message` per edge (message_passing.py:523), scatter-reduce the results by
destination (`aggregate`, :541), then `update` (:550). The literal reading
has a cost — the per-edge messages exist all at once:

```
  edge_index (COO 2 x m)              adj_t (CSR/SparseTensor)
  ─────────────────────               ────────────────────────
  gather x_j per edge                 message_and_aggregate:
  message(x_j) -> m x d temp!            spmm(adj_t, x)
  scatter-reduce by dst               no m x d materialization
  = "materialize the join"            = "pipelined aggregation"
```

The COO path, de-tensored — see the m×d temp being born:

```rust
// ILLUSTRATION — not quoted; PyG's COO branch is message_passing.py:499
// (the `else`), with message at :523 and scatter aggregate at :541.
fn propagate_coo(edges: &[(u32, u32)], x: &Mat, msg: impl Fn(&[f32]) -> Vec<f32>)
    -> Mat {
    let mut tmp = Vec::with_capacity(edges.len());   // m×d — THE temporary
    for &(src, _) in edges { tmp.push(msg(x.row(src))); }  // gather + message
    let mut out = Mat::zeros(x.n, x.d);
    for (&(_, dst), m) in edges.iter().zip(&tmp) {   // scatter-reduce by dst
        out.row_mut(dst).add_assign(m);
    }
    out   // fused CSR path: spmm(adj_t, x) — same result, no tmp at all
}
```

On our SBM bench that temp is 566K x 64 floats = 145 MB per layer. A
database person recognizes the shape instantly: this is a join
(edges ⋈ features) materialized in full before a group-by
(aggregate by destination). The next step is the obvious fix.

### Step 3 — the fused path: message_and_aggregate is one SpMM

> **In:** the `m×d` temporary from Step 2 and a CSR/`SparseTensor` adjacency.
> **Out:** a single fused SpMM that produces the same aggregate with zero
> temporaries. Step 4 is the layer that *cannot* take this path.

When the message is simple enough — a copy or a scalar multiple of
the source row — the gather/message/scatter triple collapses into a
single **SpMM** (sparse-times-dense matrix multiply: the adjacency in
CSR against the n × d feature matrix), streaming messages into their
destinations with zero temporaries. PyG's hook for this is
`message_and_aggregate`. The dispatch is two-part in the pinned version
(message_passing.py): a subclass sets `self.fuse` true only if it *overrides*
`message_and_aggregate` (checked once in `__init__` at :154 via
`inspector.implements`), and even then `propagate` fuses only when the
`edge_index` handed in is sparse — `fuse = False` at :469, flipped to true
at :471-472 (`if is_sparse(edge_index)`) inside the `if self.fuse` guard at
:470. So the same GCNConv fuses on a `SparseTensor` and falls back to the
COO gather/scatter on a plain `edge_index`. When it does fuse, `propagate`
calls `message_and_aggregate` (:489) and skips `message`/`aggregate`
entirely. What the big three put there:

| layer | message_and_aggregate | anchor |
|---|---|---|
| GCNConv | `spmm(adj_t, x, reduce=self.aggr)` (`aggr='add'`) | gcn_conv.py:273-274 |
| SAGEConv | `spmm(adj_t, x[0], reduce=self.aggr)` (`aggr='mean'`) | sage_conv.py:149-152 |
| GATConv | — (can't fuse: per-edge softmax weights) | gat_conv.py:392-408 |

GCN and SAGE are literally one SpMM per layer. PyG docs call
switching to SparseTensor a "memory-efficient aggregation"; a
database person calls it not materializing a join before a group-by.
Same lesson as topic 20's masked SpGEMM never materializing L·U'
(topic 24 TC). The dispatch machinery lives in `utils/_spmm.py:12` —
a shim over torch.sparse CSR, torch_sparse, or EdgeIndex backends.

### Step 4 — SDDMM: the second primitive, forced by attention

> **In:** GAT's feature-dependent edge weights, which block Step 3's fusion.
> **Out:** the second kernel — SDDMM — and the finding that SpMM + SDDMM span
> the whole GNN inventory. Step 5 is what the generality costs.

GAT breaks the fusion because its edge weights depend on the current
features — a per-edge score plus a per-row softmax must run *before*
the SpMM can. The kernel that computes those scores is **SDDMM**
(sampled dense-dense matrix multiply: a dense product over row pairs,
evaluated only at positions where the sparse adjacency has a
nonzero — the mask does the sampling). DGL exposes it directly
(`dgl.ops.gsddmm`); PyG hides it inside `edge_updater`. The
inventory result is the chapter's payload: SpMM + SDDMM together
span every mainstream GNN — that's the entire kernel inventory M25
needs. Two kernels, both of which the M20 sparse core already speaks
(SpMM = mxm with a dense operand; SDDMM = the masked-multiply
pattern from topic 24's triangle counting).

### Step 5 — what PyG pays for generality

> **In:** the `MessagePassing` abstraction from Steps 1–4.
> **Out:** the bill for its flexibility — an unfusable Python callable, a
> template-generated hot path — plus the two supporting pieces (walker,
> loader) that round out the map.

The abstraction's flexibility has a bill, and it reads like Ligra's
(topic 24):

- `message()` as an arbitrary Python callable = Ligra's F-with-CAS
  (topic 24 reading-ligra.md Q5) — flexible, unfusable, and needs
  inspection tricks (`propagate` introspects the signature to build
  kwargs). A fixed semiring menu (GraphBLAS) fuses always but
  expresses less. Same tradeoff, third community.
- `torch.compile` support forced a template-generated `propagate`
  (message_passing.py builds a specialized module) — JIT-ing away the
  dynamism it advertised. Frameworks converge on: dynamic API, static
  hot path.

Beyond the layers, two more pieces round out the map: `node2vec.py`
implements walks as a custom C++/CUDA op (:64) with the SGNS loss in
plain PyTorch (`loss` at :135, sampling helpers :101-129) — the walker,
not the learner, is the hot path (reading-node2vec.md); and
`neighbor_loader.py:10` is GraphSAGE's sampling industrialized into a
minibatch loader (reading-graphsage.md).

## Where each step lives in the code

Read in this order — the table is the 90-minute route:

| stop | file:line | what to see | step |
|---|---|---|---|
| 1 | `torch_geometric/nn/conv/message_passing.py:39` | the base class — every conv layer subclasses this | 1 |
| 2 | `:421` `propagate()` | the dispatcher: fused path check at :469-470 (`if self.fuse`) | 1, 3 |
| 3 | `:565/:577/:598/:609` | the four overridables: `message`, `aggregate`, `message_and_aggregate`, `update` | 1 |
| 4 | `nn/conv/gcn_conv.py:45-72` | `gcn_norm` — A_hat construction (our stub's reference) | 3 |
| 5 | `gcn_conv.py:270-274` | GCN's two personalities: per-edge `message` (:270, COO gather-scatter) vs fused `spmm(adj_t, x)` (:273-274) | 2, 3 |
| 6 | `nn/conv/sage_conv.py:146-152` | SAGE: same fusion, `reduce=mean` (`message_and_aggregate` :149-152) | 3 |
| 7 | `nn/conv/gat_conv.py:392-408` | GAT: why fusion is impossible (per-edge softmax) | 4 |
| 8 | `utils/_spmm.py:12` | the `spmm` shim — dispatches to torch.sparse CSR, torch_sparse, or EdgeIndex backends | 3 |
| 9 | `nn/models/node2vec.py:64,101-159` | walks as a custom op (:64) + SGNS loss (:135) | 5 |
| 10 | `loader/neighbor_loader.py:10` | minibatch sampling (GraphSAGE industrialized) | 5 |

Navigation advice: stops 1–3 are the skeleton — don't leave them
until you can say what `propagate` does when `fuse` is true vs false.
Stops 5–7 are the same layer question asked three times ("what's in
`message_and_aggregate`?") with three different answers. Stops 8–10
are the supporting cast.

## Questions (answer in notes.md)

1. Trace one GCNConv.forward on paper: which lines run gcn_norm, which
   dispatch to spmm, where the bias adds. What's cached across calls
   (hint: `self._cached_adj_t`) and what's the database name for it?
2. The COO path's m x d temp vs CSR spmm: compute both memory footprints
   for our bench config and for RMAT scale 16 (topic 24) at d=128.
3. `spmm`'s `reduce='max'` isn't a semiring on floats-with-gradients —
   what breaks in backward, and how does that constrain "GNN over
   GraphBLAS" ambitions (M20's semiring menu)?
4. NeighborLoader returns a renumbered subgraph per batch — relate to
   topic 5's buffer-pool page pinning: what's the working set, who evicts?
5. If M25 exposes ONE kernel to Cypher (`CALL algo.spmm`?), which PyG
   surface is the right shape to copy, and what stays engine-internal?

## Done when

Answer each before unfolding it.

- [ ] You can name the three overridable functions and trace one `GCNConv.forward`.

  <details><summary>Answer</summary>

  `message` (per-edge, message_passing.py:565), `aggregate` (order-insensitive
  reduce, :577), `update` (per-vertex, :609) — plus the fused
  `message_and_aggregate` (:598). `GCNConv.forward` runs `gcn_norm`
  (gcn_conv.py:45) to build the normalized adjacency, calls `propagate`
  (:263), which — on a `SparseTensor` — takes the fused branch and calls
  `message_and_aggregate` → `spmm(adj_t, x, reduce='add')` (gcn_conv.py:274);
  the bias adds afterward in `forward`. The cached `adj_t` is a materialized
  view (question 1).

  </details>

- [ ] You can compute the memory footprint of the COO path's m×d temporary against a CSR SpMM on this topic's graph.

  <details><summary>Answer</summary>

  The COO path materializes one message per edge: `m × d` floats. On the SBM
  (m=566,564, d=64, 4 bytes) that is `566564·64·4 ≈ 145 MB` per layer. The
  fused CSR SpMM writes only the `n × d` output (`16384·64·4 ≈ 4 MB`) and
  streams edges without a per-edge temporary — the m×d allocation drops to
  zero. Redo it for RMAT scale-16 at d=128 in notes.md.

  </details>

- [ ] You can explain what `message_and_aggregate` fuses and why that is exactly an SpMM.

  <details><summary>Answer</summary>

  It fuses the gather + `message` + scatter-reduce into one pass: for a copy
  or scalar-multiple message, aggregating source rows into destinations
  weighted by the adjacency *is* `A · X` — a sparse-times-dense multiply.
  PyG writes it literally as `spmm(adj_t, x, reduce=...)` (gcn_conv.py:274,
  sage_conv.py:152), so no `m × d` messages are ever built.

  </details>

- [ ] You can explain why attention forces SDDMM as a second primitive.

  <details><summary>Answer</summary>

  GAT's edge weight depends on both endpoints' current features, so a per-edge
  score must be computed *before* the SpMM can run — and computing a dense
  function over row pairs only at the adjacency's nonzeros is exactly SDDMM
  (sampled dense-dense matmul, the mask does the sampling). SpMM alone can't
  express it, so the inventory needs both; SpMM + SDDMM then span every
  mainstream GNN.

  </details>

- [ ] You can say why `reduce='max'` is not a semiring on floats-with-gradients.

  <details><summary>Answer</summary>

  `max` has no additive inverse and its "sum" (max) is not differentiable
  where two inputs tie — backward has to route the gradient to a single
  argmax, which is a subgradient choice, not a ring operation. So the clean
  (⊕,⊗)-semiring story that makes SpMM composable breaks for max-pooling
  aggregation, which constrains any "GNN over GraphBLAS" plan (M20's semiring
  menu) to the sum/mean reductions.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including which single kernel you would expose to Cypher.

  <details><summary>Answer</summary>

  All five `## Questions` answered in notes.md — the GCNConv.forward trace and
  `self._cached_adj_t` (a materialized view), the COO-vs-CSR memory
  computation, the `reduce='max'` gradient break, NeighborLoader's working set
  vs a buffer pool, and which single kernel (SpMM) is the right Cypher surface
  with SDDMM/softmax staying engine-internal.

  </details>

## References

**Code**
- [pytorch_geometric](https://github.com/pyg-team/pytorch_geometric) —
  read in the table's order:
  `torch_geometric/nn/conv/message_passing.py` (:39 base class, :421
  `propagate`, :469-470 fuse check), `nn/conv/gcn_conv.py`,
  `nn/conv/sage_conv.py`, `nn/conv/gat_conv.py`, `utils/_spmm.py`,
  `nn/models/node2vec.py`, `loader/neighbor_loader.py`
