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

> As discussed in Section 1, many collaborations form (or fail to form) for reasons outside the
> scope of the network; thus the raw performance of our predictors is relatively low. To more
> meaningfully represent predictor quality, we use as our baseline a *random predictor* which
> simply randomly selects pairs of authors who did not collaborate in the training interval. A
> random prediction is correct with probability between 0.15% (`cond-mat`) and 0.48% (`astro-ph`).

Raw accuracy of a few percent sounds terrible and is actually excellent; only the ratio is
interpretable. This topic's crate reproduces the setup exactly, with a random accuracy of
**0.314%**, and `graphs::evaluate` returns the factor.

Keep this metric in mind whenever you see a recommender quoted at "5% precision@10" with no
baseline attached.

### Step 3 — Neighbourhood measures

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

### Step 4 — Path-ensemble measures

Shortest-path distance is a weak measure — "For all of our graphs, there are well more than n
pairs at shortest-path distance two, so our shortest-path predictor simply selects a random subset
of these distance-two pairs." The better measures sum over *all* paths:

- **Katz**: `Σ_ℓ β^ℓ · |paths^{⟨ℓ⟩}_{x,y}|`, exponentially damped by length. Closed form
  `(I − βM)^{-1} − I`. "A very small β yields predictions much like common neighbors, since paths
  of length three or more contribute very little."
- **Hitting / commute time**: expected steps for a random walk from `x` to reach `y`. Both need
  normalizing by the stationary distribution, "because `H_{x,y}` is quite small whenever `y` is a
  node with a large stationary probability, regardless of the identity of `x`" — the popularity
  trap again, arriving from a third direction.
- **Rooted PageRank**: restart at `x` with probability α each step. The reset is there to stop the
  measure depending on "parts of the graph far away from x and y". This is Pixie's walk, and
  HippoRAG's, in its 2003 clothes.
- **SimRank**: two nodes are similar to the extent that they are joined to similar neighbours; a
  fixed point of a recursive definition.

### Step 5 — Figure 3, and the row that matters

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
preferential attachment and triadic closure:

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

- [ ] You can write all four neighbourhood measures from memory.
- [ ] You can explain why factor-over-random is the metric and raw accuracy is not.
- [ ] You can say why preferential attachment loses, in one sentence connecting it to lane 1.
- [ ] You can explain Adamic/Adar's discount and name its two cousins in other topics.
- [ ] Your `linkpred.rs` reproduces lane 3's ordering with PA far behind the rest.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Liben-Nowell & Kleinberg. *The Link-Prediction Problem for Social Networks.* CIKM 2003 / JASIST
  58(7), 2007 — [PDF](https://www.cs.cornell.edu/home/kleinber/link-pred.pdf).
- Adamic & Adar. *Friends and neighbors on the Web.* Social Networks 25(3), 2003 — the discount.
- Katz. *A new status index derived from sociometric analysis.* Psychometrika 18(1), 1953.
- Jeh & Widom. *SimRank: a measure of structural-context similarity.* KDD 2002.
- Local exercise stub: `topics/42-recommendations-social/experiments/linkpred.rs`.
- Topic 23 (full-text) — IDF; topic 39 (fraud) — FRAUDAR's column weights; topic 25 (graph ML) —
  where low-rank approximation leads.
