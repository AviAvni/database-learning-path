# GraphSAGE: sample the neighborhood, learn the function

Two contributions wearing one acronym: (1) **inductive** — learn an
aggregator FUNCTION, not per-node embeddings, so unseen nodes get
embeddings by running the function; (2) **neighbor sampling** — cap the
per-node fan-in so minibatches have bounded cost. The second one is the
databases-relevant idea: it's a page-budget for graph access. This
chapter builds both step by step — why per-node embeddings go stale,
what the aggregator layer computes, why full neighborhoods explode,
and what the sample costs in accuracy.

## The problem in one sentence

A 2-layer GNN minibatch needs each seed vertex's 2-hop neighborhood,
and on a hub-heavy graph one Twitter celebrity in the batch pulls in
**d_hub² neighbors — millions of vertices for one training example** —
so either you bound the fan-in or you don't train at all.

## The concepts, step by step

### Step 1 — transductive vs inductive: a lookup table vs a function

A **transductive** method learns one vector per vertex that existed
at training time — the model *is* a lookup table (node2vec, GCN as
usually trained). An **inductive** method learns a *function* from a
vertex's features and neighborhood to its embedding — apply it to any
vertex, including one inserted five minutes ago. The difference is
invisible on a frozen benchmark and decisive in a database: on
insert, a lookup table has no row for the new vertex (retrain, or
serve garbage), while a function needs one forward pass over the new
vertex's neighborhood. GraphSAGE's first contribution is making the
inductive version work: don't learn *where each node goes*, learn
*how any neighborhood is summarized*.

### Step 2 — the aggregator layer: summarize neighbors, keep yourself

Each GraphSAGE layer computes a vertex's new representation from two
inputs kept deliberately separate — a summary of its (sampled)
neighbors, and its own previous representation (Alg. 1):

```
  for layer l = 1..K:
    for each node v in batch:
      h_N(v) = AGG_l( { h_u : u in SAMPLE(N(v), S_l) } )   ← fixed fan-in S_l
      h_v    = sigma( W_l · [ h_v || h_N(v) ] )            ← concat, not sum
```

- AGG ∈ {mean, LSTM, max-pool} — any order-insensitive summary of a
  set of vectors. Mean-SAGE ≈ GCN without the symmetric
  normalization; PyG's SAGEConv fuses it as
  `spmm(adj_t, x, reduce=mean)` (sage_conv.py:149-152) with the self
  path as a separate `lin_r` (sage_conv.py:108,139) — concat
  implemented as sum of two linears.
- The concat `[h_v || h_N(v)]` (rather than adding self into the
  average) preserves "what I am" and "what surrounds me" as separate
  learnable channels — question 1 asks what the two-linears trick
  loses against true concat.
- SAMPLE: uniform, S_l per layer (paper uses S1=25, S2=10) — Step 4.

One mean-SAGE layer for one node, sampling included:

```rust
fn sage_layer(g: &Csr, h: &Mat, v: u32, s: usize,
              w_self: &Dense, w_nbr: &Dense, rng: &mut Rng) -> Vec<f32> {
    let mut agg = vec![0.0; h.d];
    let sample = g.neighbors(v).choose_multiple(rng, s);  // fan-in capped at s
    for &u in &sample {                                   // uniform sample of N(v)
        for k in 0..h.d { agg[k] += h.row(u)[k]; }
    }
    for k in 0..h.d { agg[k] /= sample.len() as f32; }    // AGG = mean
    // "concat then W" done as sum of two linears (PyG's lin_l/lin_r trick)
    relu(add(w_self.mul(h.row(v)), w_nbr.mul(&agg)))
}
```

### Step 3 — the fan-out explosion: why full neighborhoods can't ship

Stacking K layers means a vertex's output depends on its K-hop
neighborhood — so a minibatch of B seeds must *load* the union of
their K-hop neighborhoods, and that union multiplies per layer:

```
  batch of B seeds, K=2 layers, fan-in S1=25, S2=10:
     layer-2 needs:  B·10 neighbors
     layer-1 needs:  B·10·25 = 250·B nodes touched
  WITHOUT sampling on a hub graph:  B · d_hub² — one Twitter celebrity
  in the batch pulls in millions.   Sampling = bounding worst-case I/O.
```

Note the asymmetry: the *average* case is often fine (our SBM's
average degree is 34.6), but training cost is set by the worst batch,
and skew guarantees some batch contains a hub. Unbounded worst case
means unbounded memory means no training loop — hence Step 4.

### Step 4 — neighbor sampling: a page budget for graph access

GraphSAGE's fix is blunt: at each layer, use only a fixed-size
uniform sample of each vertex's neighbors — S1=25 at layer 1, S2=10
at layer 2 — making every batch cost B·S1·S2 regardless of what the
degree distribution does. This is a query optimizer problem stated in
ML clothes: the full neighborhood is the correct answer, the sample
is an approximation with a resource bound. PyG's `NeighborLoader`
(loader/neighbor_loader.py:10) industrializes it; the sampled
subgraph handed to the model is exactly a database *view* —
materialized per batch, biased by design. Mechanically it's cheap:
uniform sampling over CSR (compressed sparse row — each vertex's
neighbors as one contiguous slice) = pick S offsets in a row — O(S),
cache-friendly, and identical to Afforest's "look at r neighbors"
trick (topic 24): both refuse to pay for the full adjacency because a
sample answers well enough.

### Step 5 — what the sample costs: bias you must measure

The bound isn't free. Sampled mean-aggregation is an unbiased
estimator of the true mean only *before* the nonlinearity — after
sigma, the estimate is biased, and the per-epoch re-sampling variance
shows up as accuracy noise across runs. The papers quote the
resulting accuracy as if it were a constant; topic 22 says measure it
yourself: same model, same data, five seeds, report the spread. Also
note the resonance with topic 24: Afforest samples neighbors to *skip
work* whose answer it can infer; SAGE samples to *bound work* whose
exact answer it agrees to approximate. Question 3 asks where those
two meet.

### Step 6 — why inductive is the database-compatible variant

Put Steps 1 and 4 together and GraphSAGE is the only GNN variant in
this topic that composes with a write-heavy database. Transductive
embeddings (node2vec, GCN-as-trained) go stale on insert — the vertex
wasn't in training. A SAGE aggregator is a stored FUNCTION: new node
→ one forward pass over its (sampled) neighborhood → embedding, at a
bounded cost of S1·S2 neighbor reads. The remaining problem is
staleness semantics: an embedding computed at snapshot T and queried
at T+k is a stale materialized result, and topic 8's vocabulary
(read-your-writes, monotonic reads) is the right one for saying how
stale is acceptable — question 4 makes this precise.

## How to read the paper (with the concepts in hand)

- Alg. 1 is Step 2 — read it with the concat and the SAMPLE
  highlighted; everything else in the paper decorates those two
  lines.
- The sampling discussion is Steps 3–4: find where S1=25, S2=10 are
  justified, and notice the argument is a cost bound, not an
  accuracy claim.
- The aggregator zoo (mean/LSTM/pool comparisons) is skimmable —
  mean wins often enough that PyG's default is Step 2's fused
  `spmm(..., reduce=mean)`.
- Read the inductive evaluation (unseen-graph protein experiments)
  as the database case: that's Step 6 with benchmarks.
- Then the code: `sage_conv.py:108,139,146-152` for the two-linears
  concat and the fused path; `neighbor_loader.py:10` for the
  industrial sampler.

## Questions (answer in notes.md)

1. Why does mean aggregation + separate self-linear (lin_r) approximate
   concat? What expressiveness is lost vs true concat?
2. Compute nodes-touched for B=512, S=(25,10) vs full 2-hop on our SBM
   (avg_deg 34.6) and on an RMAT hub (deg 9,751, topic 24) — where does
   sampling stop being optional?
3. SAMPLE(N(v), S) per epoch is a fresh random view — relate to
   Afforest's neighbor_rounds sample (topic 24). One is for variance
   reduction, one for work skipping; do they meet?
4. An insert arrives: which embeddings does a SAGE model let you refresh
   lazily, and what's the staleness semantics (topic 8 vocabulary) of
   "embedding computed at snapshot T, queried at T+k"?
5. For M25's `algo.embed()`: transductive (node2vec) vs inductive (SAGE)
   as the stored artifact — which do you ship first, and what does the
   vector index (topic 14) need to know about staleness either way?

## References

**Papers**
- Hamilton, Ying, Leskovec — "Inductive Representation Learning on
  Large Graphs" (NeurIPS 2017,
  [arXiv:1706.02216](https://arxiv.org/abs/1706.02216)) — Alg. 1 and
  the sampling discussion; the aggregator zoo is skimmable

**Code**
- [pytorch_geometric](https://github.com/pyg-team/pytorch_geometric)
  `torch_geometric/nn/conv/sage_conv.py` (:108,139,146-152 — concat
  as two linears, fused `spmm` with `reduce=mean`) and
  `torch_geometric/loader/neighbor_loader.py` (:10 — sampling,
  industrialized)
