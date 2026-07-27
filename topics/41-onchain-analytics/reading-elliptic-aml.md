# Elliptic: the graph neural network that lost to a random forest

The other three guides in this topic are about deterministic inference — union-find over
co-spending, FIFO over provenance. This one is about the statistical alternative, and it is here
partly as a counterweight. Weber et al. released the Elliptic data set, still the largest public
labelled transaction graph in any cryptocurrency, and then benchmarked graph convolutional
networks against ordinary supervised learning on it. The GCN lost. Random Forest on hand-built
aggregate features reaches illicit-F1 **0.796**; a two-layer GCN reaches **0.628**. That result is
worth more to a database engineer than a win would have been, because it says something precise
about when graph structure pays for itself — and because a single real-world event, the shutdown
of a dark market in the middle of the test period, breaks every model in the table.

## The problem in one sentence

**Classify each Bitcoin transaction as licit or illicit from a labelled graph where only 23% of
nodes have a label at all, the illicit class is 2%, and the process generating the labels changes
under you.**

## The concepts, step by step

### Step 1 — The data set

```
   203,769 node transactions          234,355 directed edge payment flows
   (the full Bitcoin network: ~438M nodes, 1.1B edges — this is a subgraph)

   labels:   4,545 illicit (2%)      42,019 licit (21%)    rest unlabelled (77%)
   features: 166 per node
   time:     49 steps, ~2 weeks apart, 1,000–8,000 nodes each
```

Nodes are transactions; a directed edge means BTC flowed from one transaction to the next. A
transaction is licit or illicit according to the *entity that initiated it* — exchanges, wallet
providers, miners, licit services on one side; scams, malware, terrorist organisations,
ransomware, Ponzi schemes on the other. The labelling is heuristic, and the paper says so: "a
higher number of inputs and the reuse of the same address is commonly associated with higher
address-clustering, which results in a degrade of anonymity for the entity signing the
transaction" — i.e. the labels were produced partly by the clustering machinery of the other
guides in this topic. Circularity worth noticing.

### Step 2 — Two kinds of feature, and what the split measures

The 166 features divide sharply:

- **94 local features**: time step, number of inputs/outputs, transaction fee, output volume, and
  aggregates over the transaction's own inputs and outputs.
- **72 aggregated features**: the maximum, minimum, standard deviation and correlation
  coefficients of the *same* local features taken over the node's one-hop neighbours, forward and
  backward.

So the 72 are a hand-rolled, single-layer, fixed-aggregation message pass. This is the comparison
the paper is really running: **hand-built one-hop aggregation vs a learned multi-hop one**. Keep
that in mind when you read the results, because it is a fairer fight than "features vs graphs".

The paper flags the limitation itself: "In building the 72 aggregated features, the problem of
heterogeneous neighborhoods is addressed by naively constructing statistical aggregates
(minimum, maximum, etc.) of the local features of a neighbor transaction. In general, this
solution is sub-optimal because it carries a significant loss of information."

### Step 3 — The temporal structure, and the choice that deletes it

Each of the 49 time steps is "a single connected component of transactions that appeared on the
blockchain within less than three hours between each other; **there are no edges connecting
different time steps**."

Read that twice. The data set is 49 disjoint graphs, not one temporal graph. Every path is
confined to a three-hour window, so no model on this data can learn anything about how money
moves *across* time steps — which is exactly the money-laundering behaviour anyone would want to
detect. Topic 33 spends a whole topic on time-respecting paths; this data set makes them
impossible by construction. That is a modelling decision with consequences, and it is the subject
of one of the questions below.

The evaluation uses a **70:30 temporal split**: train on steps 1–34, test on 35–49. Temporal, not
random — correct, and much harder.

### Step 4 — The results table, read carefully

Illicit-class precision / recall / F1, plus micro-averaged F1. `AF` = all 166 features, `LF` =
the 94 local ones only, `NE` = node embeddings from a GCN concatenated on.

| method | precision | recall | **illicit F1** | micro F1 |
|---|---|---|---|---|
| Logistic Regression `AF` | 0.404 | 0.593 | 0.481 | 0.931 |
| Logistic Regression `LF` | 0.348 | 0.668 | 0.457 | 0.920 |
| **Random Forest `AF`** | 0.956 | 0.670 | **0.788** | 0.977 |
| **Random Forest `AF+NE`** | 0.971 | 0.675 | **0.796** | 0.978 |
| Random Forest `LF` | 0.803 | 0.611 | 0.694 | 0.966 |
| MLP `AF` | 0.694 | 0.617 | 0.653 | 0.962 |
| GCN | 0.812 | 0.512 | 0.628 | 0.961 |
| Skip-GCN | 0.812 | 0.623 | 0.705 | 0.966 |
| EvolveGCN | 0.850 | 0.624 | 0.720 | 0.968 |

Four readings, in order of usefulness:

1. **Random Forest wins, comfortably.** 0.788 vs the GCN's 0.628. The paper's explanation is
   architectural: "Random Forest uses a voting mechanism to ensemble the prediction results from
   a number of decision trees... GCN, in contrast, like most deep learning models, uses Logistic
   Regression as the final output layer; hence, it can be considered a nontrivial generalization
   of Logistic Regression." And Logistic Regression is bottom of the table.
2. **The graph helps, but as features.** Every model improves from `LF` to `AF` (Random Forest
   0.694 → 0.788), so the one-hop aggregates carry real signal. And `AF+NE` — Random Forest over
   the GCN's *embeddings* — is the best row in the table at 0.796. The winning configuration uses
   both.
3. **Skip connections matter a lot.** Skip-GCN (0.705) beats plain GCN (0.628) by adding a direct
   path from the input features to the output, which makes it "at least as powerful as Logistic
   Regression". A four-point-nine-percentage-point-per-architecture-tweak result on a model that
   was already losing.
4. **Temporal modelling helps a little.** EvolveGCN, which drives the GCN's weights through a
   recurrent network across time steps, reaches 0.720 — "consistently outperforms GCN, although
   the improvement is not substantial for this data set." Which is what you would expect, given
   Step 3: there are no cross-time edges for a temporal model to exploit.

### Step 5 — The dark market shutdown: the finding that actually matters

At time step 43 — inside the test period — a dark market closed. Figure 2 plots illicit F1 per
time step, and every method falls off a cliff there and does not recover.

> One interesting aspect of this data set is the sudden closure of a dark market occurring during
> the time span of the data (at time step 43). As seen in Figure 2, this event causes all methods
> to perform poorly after the shutdown. Even a Random Forest model re-trained after every test
> time step, assuming the availability of ground truth after each time, is not able to reliably
> capture new illicit transactions after the dark market shutdown. The robustness of methods to
> such events emerges as a major challenge to address.

Note the strength of that: **re-training after every step with fresh ground truth does not fix
it.** This is not a stale-model problem, it is a distribution-shift problem — the illicit
behaviour that remains after the market closes is different behaviour, and no amount of fitting
the old behaviour helps.

If you have read topic 40, you have met this shape already: the adversary reads your score and
changes. Topic 39's FRAUDAR answers it by choosing a metric camouflage cannot move. There is no
equivalent move here, which is the honest state of the art.

### Step 6 — Why 2% illicit changes the whole evaluation

Micro-averaged F1 is above 0.92 for *every* method in the table, including the worst one. It is
meaningless: with 2% illicit, a classifier that says "licit" always scores well. The paper trains
the GCN "using a weighted cross entropy loss to provide higher importance to the illicit samples"
at a 0.3/0.7 ratio and reports illicit-class metrics separately — do the same in any comparable
setting, and be suspicious of any AML result quoted as accuracy.

The paper also frames the business constraint precisely: "Industry standard high false positive
rates of upwards of 90% inhibit this effort. We want to reduce false positive rates without
increasing false negative rates." Random Forest's 0.956 precision against Logistic Regression's
0.404 is exactly that axis, and it is why the boring model wins in production.

### Step 7 — What this says about graph ML generally

Three transferable lessons:

1. **A learned aggregation must beat a hand-built one to be worth it.** Here the hand-built
   72-feature one-hop aggregate, fed to a strong tabular model, beat two learned graph models.
   Before reaching for a GNN, build the aggregates and measure.
2. **Embeddings compose with tabular models.** The best row is Random Forest over
   features-plus-GCN-embeddings. Use the graph model as a feature extractor rather than an
   end-to-end classifier when the tabular model is stronger.
3. **The data set's structure caps what any model can learn.** No cross-time edges means no
   temporal path can be learned, and EvolveGCN's small gain follows directly. Check what the
   graph you are given makes *impossible* before comparing architectures on it.

## How to read the paper (with the concepts in hand)

- **§2 The Elliptic Data Set.** §2.1.1–2.1.3 for the node/edge/feature/time counts. Make sure you
  register the "no edges connecting different time steps" sentence and think about it before
  moving on.
- **§2.2 Notes on feature construction.** The circularity between labelling heuristics and
  address clustering, and the honest paragraph about naive aggregation losing information.
- **§3.1–3.2.** The benchmark methods and the GCN formulation. The Skip-GCN paragraph is short and
  is the explanation for a five-point jump; read it.
- **§3.3 Temporal modelling.** EvolveGCN in three paragraphs.
- **§4 + Table 1.** The results. Read the `LF` vs `AF` rows first (does the graph help at all?),
  then `AF` vs `AF+NE` (do embeddings add to features?), and only then the GCN rows.
- **§4 + Figure 2 + Table 2.** The per-time-step F1 plot and the dark market shutdown. This is the
  paper's real contribution.
- **§5 Discussion.** Why Random Forest beat the GCN, and the suggestion to make decision trees
  differentiable so the two can be trained end to end.
- **After the paper.** Do not implement a GCN here — topic 25 already does that. Instead, take
  this topic's `clustering.rs` output as a feature (cluster size, cluster age) and ask whether it
  would have helped: which of Elliptic's 166 features is a cluster-derived feature in disguise?

## Questions to answer in notes.md

1. The 72 aggregated features are a hand-built one-hop message pass. Rewrite the comparison in
   Table 1 as "learned vs hand-built aggregation" and say what result would have made the GCN
   clearly worth its complexity.
2. There are no edges between time steps. Name two laundering behaviours this makes undetectable
   in principle, and say what the data set would need to look like (in topic 33's vocabulary) for
   them to be detectable.
3. Micro-F1 is above 0.92 for every method including the worst. Compute what a
   "predict-licit-always" classifier scores on micro-F1 with 2% illicit, and write one sentence
   on why the paper reports illicit-class metrics separately.
4. Re-training after each test step does not recover performance after the dark market shutdown.
   Distinguish concept drift from covariate shift here, and say which one this is and how you
   would detect it in production *before* the F1 drops.
5. The labels come partly from address clustering, and this topic's lane 3 shows clustering can
   collapse. If a super-cluster merged a licit exchange with an illicit service, what would that
   do to the labels, the features, and the measured F1 — and would you be able to tell?

## Done when

- [ ] You can state the data set's size, label balance and temporal structure from memory.
- [ ] You can explain the 94/72 feature split and why it makes the GCN comparison a fair fight.
- [ ] You can give the headline result (RF 0.788/0.796 vs GCN 0.628, Skip-GCN 0.705, EvolveGCN
      0.720) and the paper's explanation for it.
- [ ] You can describe the dark market shutdown and why re-training does not fix it.
- [ ] You can say why micro-F1 is the wrong metric here.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Weber, Domeniconi, Chen, Weidele, Bellei, Robinson, Leiserson. *Anti-Money Laundering in
  Bitcoin: Experimenting with Graph Convolutional Networks for Financial Forensics.* KDD 2019
  Workshop on Anomaly Detection in Finance — [arXiv:1908.02591](https://arxiv.org/abs/1908.02591).
- Pareja et al. *EvolveGCN: Evolving Graph Convolutional Networks for Dynamic Graphs.* AAAI 2020 —
  the temporal model in §3.3.
- The Elliptic Data Set is public on Kaggle; 203,769 nodes, 234,355 edges, 166 features.
- Topic 25 (graph neural networks) — GCN, GraphSAGE and the message-passing framework this
  benchmarks; topic 33 (temporal graphs) — what the missing cross-time edges cost.
