# Reading guide — "Inductive Representation Learning on Large Graphs" (Hamilton, Ying, Leskovec, NeurIPS 2017) — GraphSAGE

Two contributions wearing one acronym: (1) **inductive** — learn an
aggregator FUNCTION, not per-node embeddings, so unseen nodes get
embeddings by running the function; (2) **neighbor sampling** — cap the
per-node fan-in so minibatches have bounded cost. The second one is the
databases-relevant idea: it's a page-budget for graph access.

## The algorithm (Alg. 1)

```
  for layer l = 1..K:
    for each node v in batch:
      h_N(v) = AGG_l( { h_u : u in SAMPLE(N(v), S_l) } )   ← fixed fan-in S_l
      h_v    = sigma( W_l · [ h_v || h_N(v) ] )            ← concat, not sum
```

- AGG ∈ {mean, LSTM, max-pool}. Mean-SAGE ≈ GCN without the symmetric
  normalization; PyG's SAGEConv fuses it as `spmm(adj_t, x, reduce=mean)`
  (sage_conv.py:149-152) with the self path as a separate `lin_r`
  (sage_conv.py:108,139) — concat implemented as sum of two linears.
- SAMPLE: uniform, S_l per layer (paper uses S1=25, S2=10).

## The fan-out explosion (why sampling exists)

```
  batch of B seeds, K=2 layers, fan-in S1=25, S2=10:
     layer-2 needs:  B·10 neighbors
     layer-1 needs:  B·10·25 = 250·B nodes touched
  WITHOUT sampling on a hub graph:  B · d_hub² — one Twitter celebrity
  in the batch pulls in millions.   Sampling = bounding worst-case I/O.
```

This is a query optimizer problem stated in ML clothes: the full
neighborhood is the correct answer, the sample is an approximation with a
resource bound. PyG's `NeighborLoader` (loader/neighbor_loader.py:10)
industrializes it; the sampled subgraph handed to the model is exactly a
database *view* — materialized per batch, biased by design.

## Engine-side notes

- Uniform neighbor sampling over CSR = pick S offsets in a row — O(S),
  cache-friendly, and identical to Afforest's "look at r neighbors" trick
  (topic 24): both refuse to pay for the full adjacency because a sample
  answers well enough.
- Inductive matters for databases: node2vec/GCN-transductive embeddings
  go stale on insert (the vertex wasn't in training). A SAGE aggregator is
  a stored FUNCTION: new node → one forward pass over its (sampled)
  neighborhood → embedding. That's the only variant that composes with a
  write-heavy database.
- The bias is real: sampled aggregation is an unbiased estimator of mean
  aggregation only pre-nonlinearity; variance shows up as accuracy noise.
  Benchmarks quote it; topic 22 says measure it yourself.

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
