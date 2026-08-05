# Link prediction: one-line scores, and why degree alone loses

The three other guides in this topic are systems papers about serving recommendations fast. This
one is about whether the graph knows anything in the first place. Liben-Nowell and Kleinberg took
five arXiv co-authorship networks, hid the future, and asked a catalogue of proximity measures —
each a single line of arithmetic — to name the pairs of researchers who would collaborate next.
The measures beat chance by 20–55×. That is the result the whole recommendation industry rests
on: **network topology alone carries real signal about links that do not yet exist**. The second
result is the one that keeps mattering: the measure everyone reaches for first, degree, is the
worst of the family.

## The problem in one sentence

**Given a snapshot of a social network, rank the pairs of nodes not yet joined by how likely they
are to be joined next — using nothing but the topology.**

## The concepts, step by step

### Step 1 — The experimental setup, and why it is honest

> **In:** nothing yet — a network snapshot and the question "who connects next?".
> **Out:** an honest evaluation design — a training interval that sees only the past, a test interval
> that hides the future, predictions restricted to **Core** (nodes with ≥ κ = 3 edges in *both*
> intervals), and top-`n` scoring of a ranked list — plus the five arXiv datasets Steps 2–5 measure
> on. §2.

Two time intervals: a training interval `[t₀, t₀′]` and a test interval `[t₁, t₁′]`. The predictor
sees only the training graph. For the arXiv data these are 1994–1996 and 1997–1999.

Two subtleties that make the evaluation fair:

- **Node churn.** "social networks grow through the addition of nodes as well as edges, and it is
  not sensible to seek predictions for edges whose endpoints are not present in the training
  interval." So predictions are restricted to **Core** — nodes with at least κ = 3 edges in both
  intervals.
- **Ranking, not classification.** Each predictor outputs a ranked list; you take the top
  `n = |E_new|` pairs and count how many are real. No threshold to tune, no class balance to argue
  about.

Data (Figure 1): five arXiv sections, e.g. `astro-ph` with 5,343 authors, 5,816 papers and 41,852
training edges, of which Core is 1,561 authors with 6,178 old edges and 5,751 new ones.

### Step 2 — Why "factor improvement over random" is the only sane metric

> **In:** the ranked top-`n` predictions from Step 1.
> **Out:** the one interpretable score — **factor improvement over random** — because raw precision
> is a few percent *by design*; the random baseline is 0.15–0.48% across the datasets (this crate:
> 0.314%), so every later number is a multiple of that. §4.

> As discussed in Section 1, many collaborations form (or fail to form) for reasons outside the
> scope of the network; thus the raw performance of our predictors is relatively low. To more
> meaningfully represent predictor quality, we use as our baseline a *random predictor* which
> simply randomly selects pairs of authors who did not collaborate in the training interval. A
> random prediction is correct with probability between 0.15% (`cond-mat`) and 0.48% (`astro-ph`).

Raw accuracy of a few percent sounds terrible and is actually excellent; only the ratio is
interpretable. This topic's crate reproduces the setup exactly, with a random accuracy of
**0.314%**, and `graphs::evaluate` returns the factor.

Worked example — the factor is `(fraction of the top-n predictions that are real) / (random
accuracy)`. On `astro-ph`, random is correct 0.475% of the time, and common neighbours scores
**18.0×** (Figure 3). Invert it: common neighbours' raw precision is `18.0 × 0.00475 = 0.0855`, i.e.
**8.55%** of its top-n pairs actually collaborate. An 8.55% hit rate reads as a failure and is in
fact 18× better than chance — which is exactly why the raw number is useless without its baseline.

Keep this metric in mind whenever you see a recommender quoted at "5% precision@10" with no
baseline attached.

### Step 3 — Neighbourhood measures

> **In:** the factor-over-random metric from Step 2.
> **Out:** the four one-line **neighbourhood measures** — common neighbours, Jaccard, Adamic/Adar,
> preferential attachment — each a function of the two nodes' neighbour sets, and the hub-discount
> idea Adamic/Adar contributes. §3.

For a node `x`, `Γ(x)` is its neighbour set.

- **Common neighbours**: `|Γ(x) ∩ Γ(y)|`. The direct implementation of "friends of friends become
  friends". Newman verified the underlying correlation empirically in collaboration networks.
- **Jaccard**: `|Γ(x) ∩ Γ(y)| / |Γ(x) ∪ Γ(y)|`. Common neighbours, normalized — so two hubs with
  50 mutual friends out of 5,000 do not outrank two specialists with 5 out of 6.
- **Adamic/Adar**: `Σ_{z ∈ Γ(x) ∩ Γ(y)} 1 / log|Γ(z)|`. Common neighbours with each shared
  neighbour *discounted by how many people it knows*. Adamic and Adar invented it for deciding
  when two personal home pages are related, "weighting rarer features more heavily".
- **Preferential attachment**: `|Γ(x)| · |Γ(y)|`. Pure degree. Motivated by the network-growth
  model in which the probability a new edge involves `x` is proportional to `|Γ(x)|`.

Adamic/Adar's hub discount is the same idea as topic 23's inverse document frequency and topic
39's FRAUDAR column weights `1/log(d+5)`: **evidence everybody shares is worth less**. Three
fields, one line of arithmetic.

Worked example — nodes `x` and `y` share two neighbours: `z₁`, a hub with `|Γ(z₁)| = 1000`, and
`z₂`, a specialist with `|Γ(z₂)| = 4`. Common neighbours scores both the same — it counts 2, one
per shared neighbour. Adamic/Adar discounts each by `1/log|Γ(z)|`:

```
Adamic/Adar = 1/ln(1000) + 1/ln(4) = 1/6.9078 + 1/1.3863 = 0.1448 + 0.7213 = 0.8661
   specialist z₂ contributes 0.7213, hub z₁ contributes 0.1448
   ratio = ln(1000)/ln(4) = 4.98  ->  the specialist is worth ~5x the hub
```

The ratio is independent of the log's base, so "≈5×" holds whether you use natural log or log₁₀. Two
people who both know the same rarely-connected specialist is strong evidence they belong together;
two people who both follow the same celebrity is almost none.

### Step 4 — Path-ensemble measures

> **In:** the neighbourhood scores from Step 3, which see only *shared direct* neighbours.
> **Out:** the **path-ensemble measures** — Katz, hitting/commute time, rooted PageRank, SimRank —
> which sum over *all* paths between the two nodes, and the popularity trap that reappears inside
> hitting time. §3.

Shortest-path distance is a weak measure — "For all of our graphs, there are well more than n
pairs at shortest-path distance two, so our shortest-path predictor simply selects a random subset
of these distance-two pairs." The better measures sum over *all* paths:

- **Katz**: `Σ_ℓ β^ℓ · |paths^{⟨ℓ⟩}_{x,y}|`, exponentially damped by length. Closed form
  `(I − βM)^{-1} − I`. "A very small β yields predictions much like common neighbors, since paths
  of length three or more contribute very little."
- **Hitting / commute time**: expected steps for a random walk from `x` to reach `y`. Both need
  normalizing by the **stationary distribution** (the long-run fraction of time an unconstrained
  random walk spends at each node — large for popular hubs), "because `H_{x,y}` is quite small
  whenever `y` is a node with a large stationary probability, regardless of the identity of `x`" —
  the popularity trap again, arriving from a third direction.
- **Rooted PageRank**: restart at `x` with probability α each step. The reset is there to stop the
  measure depending on "parts of the graph far away from x and y". This is Pixie's walk, and
  HippoRAG's, in its 2003 clothes.
- **SimRank**: two nodes are similar to the extent that they are joined to similar neighbours; a
  fixed point of a recursive definition.

### Step 5 — Figure 3, and the row that matters

> **In:** every measure defined in Steps 3–4.
> **Out:** Figure 3's factor-over-random table and its three readings — no single winner,
> preferential attachment loses badly, Adamic/Adar's discount earns its line — the ordering lane 3
> reproduces. §4, Figure 3.

Factor improvement over random:

| predictor | astro-ph | cond-mat | gr-qc | hep-ph | hep-th |
|---|---|---|---|---|---|
| *random is correct* | 0.475% | 0.147% | 0.341% | 0.207% | 0.153% |
| graph distance | 9.6 | 25.3 | 21.4 | 12.2 | 29.2 |
| common neighbors | 18.0 | 41.1 | 27.2 | 27.0 | 47.2 |
| **preferential attachment** | **4.7** | **6.1** | **7.6** | **15.2** | **7.5** |
| Adamic/Adar | 16.8 | 54.8 | 30.1 | 33.3 | 50.5 |
| Jaccard | 16.4 | 42.3 | 19.9 | 27.7 | 41.7 |
| SimRank γ=0.8 | 14.6 | 39.3 | 22.8 | 26.1 | 41.7 |
| hitting time | 6.5 | 23.8 | 25.0 | 3.8 | 13.4 |
| rooted PageRank α=0.15 | 16.6 | 41.1 | 27.2 | 27.6 | 42.6 |
| Katz (weighted) β=0.005 | 13.4 | 54.8 | 30.1 | 24.0 | 52.2 |

Three readings:

1. **There is no winner.** The authors say so: "There is no single clear winner among the
   techniques", though "the Katz measure and its variants based on clustering and low-rank
   approximation perform consistently well."
2. **Preferential attachment loses, badly.** 4.7–15.2× where common neighbours reaches 18.0–47.2×.
   It is the only measure in the table that never looks at whether the two nodes have anything in
   common — it is lane 1's popularity baseline in a link-prediction costume, and it fails for the
   same reason. Knowing who is famous is not knowing who will connect.
3. **Adamic/Adar's discount earns its line.** It beats plain common neighbours on four of the five
   networks, and by a lot on `cond-mat` (54.8 vs 41.1).

Lane 3 of this crate reproduces the ordering on a synthetic collaboration graph grown with
preferential attachment and **triadic closure** (if `x` knows `y` and `y` knows `z`, the edge `x–z`
becomes more likely — the very mechanism common neighbours exploits):

```
   predictor                  hits / n     factor over random
   preferential attachment       6 / 985            1.9x
   common neighbors             64 / 985           20.7x
   Jaccard                      79 / 985           25.6x
   Adamic/Adar                  69 / 985           22.3x
```

Same shape, same loser. (Jaccard edging out Adamic/Adar here rather than the other way round is a
property of the generator — worth investigating rather than explaining away.)

### Step 6 — The meta-approaches

> **In:** any base measure from Steps 3–5, written in matrix form.
> **Out:** three techniques that *compose* with any of them — low-rank approximation, unseen
> bigrams, clustering — and the lines they draw to matrix-factorization recommenders, smoothing, and
> Pixie-style graph pruning. §3 (higher-level approaches).

Three techniques that compose with any measure above, and are the bridge to modern methods:

- **Low-rank approximation.** Every measure has a matrix formulation, so replace the adjacency
  matrix `M` with its rank-`k` SVD truncation `M_k` and compute the measure on that. "Intuitively,
  working with `M_k` rather than `M` can be viewed as a type of 'noise-reduction' technique". This
  is where matrix-factorization recommenders and, eventually, graph embeddings come from.
- **Unseen bigrams.** Borrowed from language modelling: augment `score(x, y)` using
  `score(z, y)` for nodes `z` similar to `x`. Smoothing, on a graph.
- **Clustering.** Delete the `(1−ρ)` fraction of training edges with the lowest score, then
  recompute on the cleaned subgraph — "in this way we determine node proximities using only edges
  for which the proximity measure itself has the most confidence." Which is Pixie's graph pruning
  (58% better F1 at 20% of the edges), arrived at fifteen years earlier from the algorithms side.

### Step 7 — What this does and does not license

> **In:** the whole catalogue's results from Steps 5–6.
> **Out:** the correct scope of the finding — topology carries *useful*, not *sufficient*,
> information, so a graph traversal is a legitimate first-stage *candidate generator*, not the final
> ranker. This is the architecture all three systems papers in this topic use. §4.

The honest framing, from §4: "a number of methods significantly outperform the random predictor,
suggesting that there is indeed useful information contained in the network topology alone."

*Useful information*, not *sufficient information*. Raw accuracy is a few percent. Collaborations
form for reasons the graph never sees. Every production system in this topic uses topology as one
signal among several — Pixie adds user features and content embeddings, GraphJet's output "is best
viewed as a set of candidates that are further reranked and filtered by machine-learned models",
TAO just stores the edges. Topology is the candidate generator, not the ranker.

That is the right way to hold this result: it tells you a graph traversal is a *legitimate*
first-stage retrieval, which is exactly the architecture all three systems use.

## How to read the paper (with the concepts in hand)

- **§1.** The framing, and the honest caveat that many collaborations form for non-network
  reasons.
- **§2 + Figure 1.** The setup: training/test intervals, Core with κ = 3, and the evaluation
  procedure (top-n from a ranked list). Note how much care goes into making the task fair.
- **§3 + Figure 2.** The measure catalogue. Figure 2 is a one-page reference you will come back
  to; the four neighbourhood measures are Step 3, the path ensembles are Step 4.
- **§3 higher-level approaches.** Low-rank approximation, unseen bigrams, clustering. Read the
  clustering paragraph against Pixie's §3.2.
- **§4 + Figure 3.** The results. Read the preferential-attachment row against every other row and
  ask why. Then Figures 5–7 for the relative-performance views.
- **After the paper.** Implement `linkpred.rs` and reproduce lane 3's ordering. Then extend it:
  Katz with β = 0.005 and rooted PageRank with α = 0.15 are each a dozen lines given the crate's
  candidate enumeration, and both should land near common neighbours.

## Questions to answer in notes.md

1. Preferential attachment is the worst neighbourhood measure on four of five networks — but it
   is the *best* motivated by network-growth theory. Reconcile those two facts: what is
   preferential attachment actually predicting correctly, and why does that not help here?
2. Adamic/Adar, IDF and FRAUDAR's `1/log(d+5)` are the same discount. Write the shared statement
   they are all instances of, and name a case where discounting hubs would be exactly wrong.
3. The paper normalizes hitting time by the stationary distribution because `H_{x,y}` is small
   whenever `y` is popular. Show that this is the popularity trap from lane 1, and say which of
   Pixie's four innovations addresses the same thing.
4. Their clustering meta-approach — delete low-confidence edges, then recompute — is Pixie's graph
   pruning. Implement it on lane 3's graph, sweep ρ, and see whether you can reproduce a
   pruning-improves-quality result on a synthetic graph. If not, why not?
5. Random accuracy on these tasks is 0.15%–0.48%, and the best predictors reach 20–55× that,
   which is still under 10% absolute. Argue whether that is good enough to build a product on,
   using how Pixie and GraphJet actually use topology in their pipelines.

## Done when

Answer each before unfolding it.

- [ ] You can write all four neighbourhood measures from memory.

  <details><summary>Answer</summary>

  With `Γ(x)` the neighbour set of `x` (§3): **common neighbours** `|Γ(x) ∩ Γ(y)|`; **Jaccard**
  `|Γ(x) ∩ Γ(y)| / |Γ(x) ∪ Γ(y)|` (common neighbours normalized by union size); **Adamic/Adar**
  `Σ_{z ∈ Γ(x) ∩ Γ(y)} 1/log|Γ(z)|` (each shared neighbour discounted by how many people it knows);
  **preferential attachment** `|Γ(x)| · |Γ(y)|` (pure degree product). The first three look at what
  the two nodes have *in common*; preferential attachment does not, which is why it loses.

  </details>

- [ ] You can explain why factor-over-random is the metric and raw accuracy is not.

  <details><summary>Answer</summary>

  Many collaborations form for reasons the graph never sees, so raw precision is a few percent even
  for a good predictor — the paper uses a random predictor (correct 0.15–0.48% of the time; this
  crate 0.314%) as the baseline and reports the *ratio*. Worked: on `astro-ph`, common neighbours
  scores 18.0×, i.e. raw precision `18.0 × 0.00475 = 8.55%`. An 8.55% hit rate reads as failure but
  is 18× chance. Only the ratio is interpretable — distrust any recommender quoted at "5%
  precision@10" with no baseline attached.

  </details>

- [ ] You can say why preferential attachment loses, in one sentence connecting it to lane 1.

  <details><summary>Answer</summary>

  Preferential attachment `|Γ(x)|·|Γ(y)|` is the only measure that never checks whether `x` and `y`
  have anything in common — it just multiplies their degrees, so it ranks pairs of *famous* nodes
  highly — which is lane 1's popularity baseline in a link-prediction costume, and it fails for the
  same reason: knowing who is popular is not knowing who will connect (Figure 3: 4.7–15.2× versus
  common neighbours' 18.0–47.2×; lane 3: 1.9× versus 20.7×).

  </details>

- [ ] You can explain Adamic/Adar's discount and name its two cousins in other topics.

  <details><summary>Answer</summary>

  Adamic/Adar weights each shared neighbour `z` by `1/log|Γ(z)|`, so a rarely-connected specialist
  counts far more than a hub — worked in Step 3, a degree-4 specialist is worth ~5× a degree-1000
  hub (`ln 1000 / ln 4 ≈ 4.98`). Its cousins: topic 23's **inverse document frequency** (rare terms
  weigh more) and topic 39's FRAUDAR column weight `1/log(d+5)` (edges to high-degree nodes count
  less). The shared statement: evidence that many nodes share is worth less than evidence that few
  share.

  </details>

- [ ] Your `linkpred.rs` reproduces lane 3's ordering with PA far behind the rest.

  <details><summary>Answer</summary>

  Lane 3's reference ordering on the synthetic graph: preferential attachment 1.9× (6/985 hits),
  common neighbours 20.7× (64/985), Jaccard 25.6× (79/985), Adamic/Adar 22.3× (69/985) —
  preferential attachment an order of magnitude behind the neighbourhood measures, matching Figure
  3's shape. (Jaccard edging Adamic/Adar here, the reverse of the paper's usual order, is a property
  of the generator — investigate it rather than explain it away.) If your PA lands near the others,
  your candidate enumeration is probably leaking degree information.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five questions live in `notes.md`'s guide-question checklist. The load-bearing ones: Q1
  (preferential attachment correctly predicts *how many* edges a node gains, not *which* pairs
  connect — right marginal, wrong joint); Q2 (Adamic/Adar, IDF and `1/log(d+5)` all instantiate
  "down-weight evidence shared by high-degree/high-frequency entities"; discounting hubs is wrong
  when the hub *is* the signal, e.g. a shared rare disease gene); Q3 (hitting time's popularity bias
  is lane 1's popularity trap, addressed by Pixie's biasing innovation). Q4 and Q5 are experiments
  and arguments you write yourself.

  </details>

## References

- Liben-Nowell & Kleinberg. *The Link-Prediction Problem for Social Networks.* CIKM 2003 / JASIST
  58(7), 2007 — [PDF](https://www.cs.cornell.edu/home/kleinber/link-pred.pdf).
- Adamic & Adar. *Friends and neighbors on the Web.* Social Networks 25(3), 2003 — the discount.
- Katz. *A new status index derived from sociometric analysis.* Psychometrika 18(1), 1953.
- Jeh & Widom. *SimRank: a measure of structural-context similarity.* KDD 2002.
- Local exercise stub: `topics/42-recommendations-social/experiments/linkpred.rs`.
- Topic 23 (full-text) — IDF; topic 39 (fraud) — FRAUDAR's column weights; topic 25 (graph ML) —
  where low-rank approximation leads.
