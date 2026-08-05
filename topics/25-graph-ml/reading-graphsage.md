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

> **In:** the modelling choice — what a trained GNN actually stores.
> **Out:** the transductive/inductive split that decides whether a new vertex
> is serveable. Step 2 is the aggregator that makes the inductive side work.

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

> **In:** the sampled neighbourhood (Step 4's fan-in) and each vertex's
> previous-layer representation.
> **Out:** the vertex's next-layer representation. Step 3 shows why the
> neighbourhood must be sampled at all.

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
  set of vectors. Careful (rule 6): the paper's default **mean aggregator**
  (Alg. 1) concatenates the vertex's own vector with the neighbour mean,
  whereas its separate *convolutional/GCN variant* (Eq. 2) folds self
  *into* the mean and drops the concat — so "mean aggregator" is subtly
  **not** the GCN rule, and Hamilton et al. say so in §3.3. PyG's `SAGEConv`
  (default `aggr="mean"`, sage_conv.py:70) implements the concat form: it
  fuses the neighbour mean as `spmm(adj_t, x[0], reduce="mean")`
  (sage_conv.py:152) and adds the self path as a separate `lin_r`
  (sage_conv.py:108, used at :139) — a concat expressed as the sum of two
  linears.
- The concat `[h_v || h_N(v)]` (rather than adding self into the
  average) preserves "what I am" and "what surrounds me" as separate
  learnable channels — question 1 asks what the two-linears trick
  loses against true concat.
- SAMPLE: uniform, S_l per layer (paper uses S1=25, S2=10) — Step 4.

One mean-SAGE layer for one node, sampling included:

```rust
// ILLUSTRATION — not quoted; PyG's fused mean aggregator is
// sage_conv.py:149-152 (message_and_aggregate -> spmm(adj_t, x[0],
// reduce="mean")) with the self path at sage_conv.py:139.
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

> **In:** the K-layer aggregator from Step 2 and a minibatch of B seed
> vertices.
> **Out:** the size of the K-hop neighbourhood that batch must load — the
> quantity Step 4 bounds.

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

> **In:** Step 3's unbounded K-hop neighbourhood.
> **Out:** a fixed per-batch cost `B·S1·S2` from a uniform fan-in cap. Step 5
> is the accuracy price of that cap.

GraphSAGE's fix is blunt: at each layer, use only a fixed-size
uniform sample of each vertex's neighbors — S1=25 at layer 1, S2=10
at layer 2 — making every batch cost B·S1·S2 regardless of what the
degree distribution does. Those two numbers are the paper's own: "we set
K=2 with neighborhood sample sizes S1=25 and S2=10" with the budget
`S1·S2 ≤ 500` (Hamilton et al. §4.1). This is a query optimizer problem
stated in ML clothes: the full neighborhood is the correct answer, the
sample is an approximation with a resource bound. PyG's `NeighborLoader`
(loader/neighbor_loader.py:10) industrializes it; the sampled
subgraph handed to the model is exactly a database *view* —
materialized per batch, biased by design. Mechanically it's cheap:
uniform sampling over CSR (compressed sparse row — each vertex's
neighbors as one contiguous slice) = pick S offsets in a row — O(S),
cache-friendly, and identical to Afforest's "look at r neighbors"
trick (topic 24): both refuse to pay for the full adjacency because a
sample answers well enough.

### Step 5 — what the sample costs: bias you must measure

> **In:** the sampled aggregation from Step 4.
> **Out:** the bias and run-to-run variance it introduces — a number you
> measure, not assume. Step 6 is why the whole scheme is worth it.

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

> **In:** the inductive aggregator (Step 1) and the bounded fan-in (Step 4).
> **Out:** the reason SAGE is the only variant here that survives a
> write-heavy database — plus the staleness question it leaves open.

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

## Done when

Answer each before unfolding it.

- [ ] You can state the transductive/inductive distinction as a lookup table against a function.

  <details><summary>Answer</summary>

  Transductive learns one vector per training-time vertex — the model *is* a
  lookup table (node2vec, GCN as usually trained), and a vertex absent at
  training has no row. Inductive learns a *function* from features and
  neighbourhood to embedding, so any vertex — including one inserted after
  training — gets an embedding from a forward pass. The gap is invisible on a
  frozen benchmark and decisive under writes.

  </details>

- [ ] You can explain the fan-out explosion and compute nodes touched for B=512, S=(25,10) against a full 2-hop on this topic's SBM (avg degree 34.6).

  <details><summary>Answer</summary>

  A K-layer model needs each seed's K-hop neighbourhood, and the union
  multiplies per layer. With sampling, B=512 and (S1,S2)=(25,10) touches
  `B·S2·S1 = 512·10·25 = 128,000` nodes — a fixed budget. Full 2-hop on the
  SBM (avg deg 34.6) touches ≈ `B·34.6² = 512·1197 ≈ 613,000` on average, and
  far more on any batch containing a hub, since the worst case is `B·d_hub²`.
  Sampling flattens that skew to a constant.

  </details>

- [ ] You can explain why neighbour sampling is a page budget for graph access.

  <details><summary>Answer</summary>

  Fixing S1, S2 caps how much adjacency a batch reads regardless of degree —
  exactly a page/I-O budget. The full neighbourhood is the correct answer;
  the fixed-size uniform sample is an approximation with a hard resource
  bound, and over CSR it is O(S) contiguous reads per row. It is the same
  refusal-to-pay-for-everything as Afforest's r-neighbour sample (topic 24).

  </details>

- [ ] You can say what bias the sample introduces and how you would measure it.

  <details><summary>Answer</summary>

  The sampled mean is unbiased for the true mean *before* the nonlinearity;
  after σ the estimate is biased, and re-sampling each epoch turns into
  run-to-run accuracy variance. Measure it the topic-22 way: same model, same
  data, five seeds, report the spread — do not quote a single accuracy as if
  it were a constant.

  </details>

- [ ] You can explain why inductive is the database-compatible variant, in terms of which embeddings an insert invalidates.

  <details><summary>Answer</summary>

  A transductive table has no entry for a newly inserted vertex — you must
  retrain or serve garbage. A SAGE aggregator is a stored function: the new
  vertex gets an embedding from one forward pass over its sampled
  neighbourhood, at a bounded `S1·S2` reads. What remains is staleness — an
  embedding computed at snapshot T and read at T+k is a stale materialized
  result, and topic 8's read-your-writes / monotonic-reads vocabulary is how
  you state the tolerance.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  All five `## Questions` answered in notes.md — including the two-linears vs
  true-concat expressiveness question, the nodes-touched computation across
  the SBM and an RMAT hub, and the transductive-vs-inductive shipping
  decision for M25's `algo.embed()`.

  </details>

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
