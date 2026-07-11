# Reading guide — PyTorch Geometric's message-passing machinery ([`~/repos/pytorch_geometric`](https://github.com/pyg-team/pytorch_geometric))

Read PyG the way topic 20 read SuiteSparse: as an existence proof that
one abstraction (here `MessagePassing`) covers a whole literature, and as
a map of which kernels actually matter. 90 minutes, code-first.

## Read in this order

| stop | file:line | what to see |
|---|---|---|
| 1 | `torch_geometric/nn/conv/message_passing.py:39` | the base class — every conv layer subclasses this |
| 2 | `:421` `propagate()` | the dispatcher: fused path check at :469-470 (`if self.fuse`) |
| 3 | `:565/:577/:598/:609` | the four overridables: `message`, `aggregate`, `message_and_aggregate`, `update` |
| 4 | `nn/conv/gcn_conv.py:45-71` | `gcn_norm` — A_hat construction (our stub's reference) |
| 5 | `gcn_conv.py:270-274` | GCN's two personalities: per-edge `message` (COO gather-scatter) vs fused `spmm(adj_t, x)` |
| 6 | `nn/conv/sage_conv.py:146-152` | SAGE: same fusion, `reduce=mean` |
| 7 | `nn/conv/gat_conv.py:392-408` | GAT: why fusion is impossible (per-edge softmax) |
| 8 | `utils/_spmm.py:12` | the `spmm` shim — dispatches to torch.sparse CSR, torch_sparse, or EdgeIndex backends |
| 9 | `nn/models/node2vec.py:64,101-160` | walks as a custom op + SGNS loss |
| 10 | `loader/neighbor_loader.py:10` | minibatch sampling (GraphSAGE industrialized) |

## The two execution modes

```
  edge_index (COO 2 x m)              adj_t (CSR/SparseTensor)
  ─────────────────────               ────────────────────────
  gather x_j per edge                 message_and_aggregate:
  message(x_j) -> m x d temp!            spmm(adj_t, x)
  scatter-reduce by dst               no m x d materialization
  = "materialize the join"            = "pipelined aggregation"
```

The COO path builds an m x d intermediate — the unaggregated message
tensor. On our SBM bench that would be 566K x 64 floats = 145 MB per
layer, vs SpMM's zero temporaries. PyG docs call switching to SparseTensor
a "memory-efficient aggregation"; a database person calls it not
materializing a join before a group-by. Same lesson as topic 20's masked
SpGEMM never materializing L·U' (topic 24 TC).

SDDMM is the other primitive: GAT's per-edge scores are dense-dense
products sampled at A's nonzeros. DGL exposes it directly (`dgl.ops.gsddmm`);
PyG hides it inside `edge_updater`. SpMM + SDDMM together span every
mainstream GNN — that's the entire kernel inventory M25 needs.

## What PyG pays for generality

- `message()` as an arbitrary Python callable = Ligra's F-with-CAS
  (topic 24 reading-ligra.md Q5) — flexible, unfusable, and needs
  inspection tricks (`propagate` introspects the signature to build
  kwargs). A fixed semiring menu (GraphBLAS) fuses always but expresses
  less. Same tradeoff, third community.
- `torch.compile` support forced a template-generated `propagate`
  (message_passing.py builds a specialized module) — JIT-ing away the
  dynamism it advertised. Frameworks converge on: dynamic API, static
  hot path.

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
