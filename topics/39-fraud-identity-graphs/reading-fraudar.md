# FRAUDAR: density the fraudster cannot fake down

Fraud rings on bipartite platforms — paid follower farms, review farms — are dense blocks:
a set of controlled accounts all pointing at the same customers' targets. Every density-based
detector before FRAUDAR could be gamed by *camouflage*: fraud accounts add edges to genuinely
popular honest objects, diluting their apparent density until they blend into the background.
FRAUDAR's move is an engineering one you will appreciate as a graph-database developer: pick a
density metric whose value on the fraud block is provably *unchanged* by camouflage, then
maximize it with a greedy peel that is `O(|E| log |V|)` and carries a ½-approximation guarantee.

## The problem in one sentence

**Find the densest suspicious block in a users × objects bipartite graph even when fraud
accounts deliberately add edges to popular honest objects to look normal.**

## The concepts, step by step

### Step 1 — Fraud rings are dense bipartite blocks

> **In:** nothing yet — a users × objects bipartite graph (followers × followees, reviewers × products).
> **Out:** the framing that a fraud ring is a near-biclique, so detection reduces to dense-subgraph mining.

The setting is a bipartite graph: users on one side, objects on the other
(followers × followees, reviewers × products). A fraud ring is economically constrained —
the operator owns a finite pool of accounts and sells engagement to a finite set of customers —
so the fraudulent edges concentrate into a block: many of the same users hitting many of the
same objects. Honest activity is diffuse; fraud is a near-biclique. Detection therefore reduces
to dense-subgraph mining. The catch: the fraudster controls the *user* rows and can add edges
anywhere, which is what the next step exploits against naive detectors.

### Step 2 — Camouflage: four ways to hide a block

> **In:** the dense-block framing from Step 1, plus the fact that the fraudster owns the user rows.
> **Out:** four camouflage attacks (random, biased, hijacked, reverse) that defeat any averaging or popularity-trusting metric.

Fraud accounts add extra edges to honest, popular objects so their degree and neighborhood
statistics look organic. The paper studies four attack variants:

```
                objects
   users     P1  P2 | T1  T2  T3      P* = popular honest objects
   f1 ------ x   .  |  x   x   x      T* = fraud targets (the block)
   f2 ------ .   x  |  x   x   x
   f3 ------ x   x  |  x   x   x      x left of | = camouflage edges
   f4 ------ x   .  |  x   x   x
```

1. random camouflage — extra edges to uniformly random honest objects;
2. biased camouflage — extra edges proportional to object popularity (smarter);
3. hijacked accounts — real accounts with real history, repurposed to hit the targets;
4. reverse camouflage — honest-looking edges *into* the fraud objects, so targets look popular.

Any metric that averages over a node's edges gets diluted by 1–3; any metric that trusts
incoming popularity gets fooled by 4.

### Step 3 — Axioms: what a suspiciousness metric must satisfy

> **In:** the camouflage failure modes from Step 2 — the metric must survive them.
> **Out:** four axioms a suspiciousness metric must satisfy, and the admitted family g(S) = f(S)/|S|.

Section 3 of the paper pins down **four axioms** for a block-suspiciousness metric g:

- **node suspiciousness** (Axiom 1) — with size and edge weight fixed, a block of higher-suspiciousness nodes beats one of lower;
- **edge suspiciousness** (Axiom 2) — adding an edge inside the block must increase suspicion;
- **size** (Axiom 3) — with equal node/edge weights and equal edge density, a larger block is more suspicious;
- **concentration** (Axiom 4) — the same total suspiciousness on fewer nodes is more suspicious.

These rule out surprisingly many intuitive metrics: edge density ρ(S) violates **Size** (Axiom 3)
— it does not grow with |S| — and the total edge weight `Σ c_ij` violates **Concentration**
(Axiom 4), since it ignores how concentrated the mass is. Theorem 1 proves the family FRAUDAR
uses satisfies all four: `g(S) = f(S) / |S|`, where S spans both sides and f(S) sums the weights
of edges with both endpoints inside S. Unweighted, g is average degree up to a factor of 2.

### Step 4 — Why unweighted average degree finds the wrong block

> **In:** the metric g(S)=f(S)/|S| from Step 3 with unit edge weights.
> **Out:** the failure — unweighted average degree ranks the organic power-user × hit-product core above the fraud block, and worse as camouflage grows.

On real graphs with skewed (Zipf-like) degree distributions, the densest set under unweighted
average degree is not the fraud block — it is the power-users × hit-products core: the heaviest
reviewers crossed with the most-reviewed products. That core is organically dense. Worse,
camouflage edges land inside honest columns and *raise* the apparent density of any set mixing
fraud users with popular objects, so the fraud block sinks in the ranking as camouflage grows.
The local experiment reproduces this directly: unweighted peeling's F-score degrades
1.00 / 0.95 / 0.69 / 0.65 as camouflage goes 0 / 0.5 / 1 / 2 edges-per-fraud-edge. The fix is
not a smarter search — it is a smarter edge weight.

### Step 5 — Column weighting: agreement on a popular column is cheap

> **In:** the unweighted-metric failure from Step 4.
> **Out:** the camouflage-resistant edge weight c_ij = 1/log(d_j + 5), keyed on each column's global degree.

Weight each edge (i, j) into object j by its global column degree:

```
c_ij = 1 / log(d_j + 5)        d_j = GLOBAL degree of object column j
                               +5   = paper's recommended smoothing constant
```

This is the tf-idf idea transplanted to graphs: thousands of people follow a celebrity, so one
more follow edge carries almost no information; fifty accounts all hitting an obscure product
is a loud coincidence. Two properties matter for what follows: the weight depends only on the
column's *global* degree (fixed before any search starts, not recomputed per candidate set),
and camouflage edges — by definition aimed at *popular* honest columns — earn weights close to
zero. Down-weighting is logarithmic, not a hard threshold, so mid-popularity columns still count.

### Step 6 — Theorem 3: camouflage provably cannot lower g(block)

> **In:** the weighted metric g with c_ij from Step 5.
> **Out:** Theorem 3 — camouflage never changes the block's in-edges or its column degrees, so g(block) is identical before and after.

This is the paper's core guarantee, and the argument is one picture:

```
   BEFORE camouflage                 AFTER camouflage
   fraud users -> fraud cols  W      fraud users -> fraud cols  W   (unchanged)
   fraud users -> honest cols .      fraud users -> honest cols ####  (new edges)

   f(block) counts ONLY edges with both endpoints in the block:
     - camouflage edges end on HONEST columns  -> outside the block, not counted
     - fraud columns' global degrees d_j       -> unchanged, so weights unchanged
   => f(block) and g(block) = f/|S| are identical before and after.
```

Camouflage can only *add* edges from fraud rows to honest columns. Those edges are never inside
the fraud block, and they never touch the fraud columns' degrees, so every c_ij inside the block
is untouched. The fraudster can raise the score of honest-looking sets, but cannot push the
fraud block's own score down. Detection then rests on the block outscoring the background —
which the column weighting already arranged by deflating the power-user core.

### Step 7 — Greedy peeling: exonerate the least suspicious, O(|E| log |V|)

> **In:** the weighted metric g to maximize over all subsets — an intractable search.
> **Out:** the greedy peel returning a set with g ≥ g_OPT/2 (Theorem 2) in O(|E| log |V|).

Maximizing g exactly is hopeless; FRAUDAR uses the classic peel. Start with all nodes, and
repeatedly delete the node (either side) with minimum weighted degree — "exonerate the least
suspicious" — recording g of every intermediate set; return the best prefix.

```
  loop:                                  lazy min-heap variant (the exercise):
    u = argmin weighted_degree           pop (key, u)
    remove u; f -= wdeg(u)               if key != current_wdeg[u]: stale, skip
    for v in neighbors(u):               remove u, f -= wdeg(u)
        wdeg(v) -= c_uv                  for v in nbrs(u): wdeg[v] -= c_uv; push(v)
    g = f / |S|; keep best S             track argmax g over the peel sequence
```

With a priority tree or heap each deletion costs `O(log |V|)` plus degree work, giving
`O(|E| log |V|)` total. Theorem 2 proves a ½-approximation: g(returned) `>=` g_OPT / 2.
Proof shape: at the first moment a node of the optimal set S* is peeled, every remaining node
has weighted degree at least g of the current set (else it would be peeled first), and that
node's degree also upper-bounds contributions in S*; averaging closes the factor-2 gap.

### Step 8 — What it finds in the wild

> **In:** the weighted peel from Step 7 run on injected-block and real graphs.
> **Out:** F above 0.95 (block densities ≥ 0.04) under all four attacks, and a 4031×4313 Twitter block at 68% density.

On real review graphs with injected 200×200 fraud blocks, FRAUDAR scores F above 0.95 under
*all four* camouflage attacks for block densities of at least 0.04 (§5.2). On the Twitter follower
graph (41.7M users, 1.47B edges) it surfaced a 4031 × 4313 block at 68% edge density. Hand-labeling
found 57% of the detected *followers* and 40% of the detected *followees* were fraudulent, deleted,
or suspended, against 25% in a degree-matched control and 12% in an unconditioned control (§5.3);
many block accounts were created within the same short time window and used the follower-buying
services TweepMe and TweeterGetter. Cross-topic hook: the peel is the same degree-ordered vertex
elimination as k-core decomposition — topic 18's GPU graph analytics implements exactly this loop
with a bucketed frontier instead of a heap.

## How to read the paper (with the concepts in hand)

- **Section 1 (Introduction).** Skim for the three claims: camouflage resistance, the
  approximation guarantee, and the Twitter case study. You have the map from Steps 1–2.
- **Section 2 (Related work).** One pass. Note which prior dense-block methods lack camouflage
  guarantees — this motivates the axiomatic reset in Step 3.
- **Section 3 (Problem / axioms).** Read carefully against Step 3. Test edge density ρ(S)
  (fails **Size**, Axiom 3) and the total edge weight `Σ c_ij` (fails **Concentration**, Axiom 4);
  seeing them fail is the point. Confirm the metric family `g = f/|S|` and that S mixes rows and columns.
- **Section 4.1–4.2 (Algorithm, Theorem 2).** Step 7 is your companion. Walk the peel proof:
  find the sentence fixing "the first time an optimal node is removed" and check the averaging
  argument. Map the data-structure claim to the lazy-heap variant you will implement.
- **Section 4.3 (Column weights, Theorem 3).** The heart. Read with Steps 5–6 open. Verify the
  proof only uses two invariants: camouflage lands on honest columns, and block column degrees
  never change. Note c = 5 in `1/log(d_j + 5)` and the global-degree choice.
- **Section 5 (Experiments).** Match the injection setup to Step 4's failure numbers and Step 8's
  F above 0.95 across attacks (densities ≥ 0.04). Then the Twitter results: block size, 68% density,
  57%/40% follower/followee labeling vs 25% and 12% controls, account-creation timing. Ask what a
  degree-matched control actually controls for.
- **After the paper.** Do the local experiment: implement `fraudar.rs` (lazy min-heap peel) over
  `review_graph.rs`'s generator, reproduce the unweighted-vs-weighted F table, and time the peel
  on the 100k × 50k-node / ~1.02M-edge graph (~0.2 s with the reference solution).

## Questions to answer in notes.md

1. Which of the four axioms does plain edge density `f(S)/(|S_rows| * |S_cols|)` violate, and
   with what concrete counterexample block?
2. Theorem 3's proof needs column weights to depend on GLOBAL degree, fixed up front. What
   breaks — both in the guarantee and in peel complexity — if you recompute d_j inside the
   current candidate set at each step?
3. Reverse camouflage adds honest-looking edges INTO fraud objects, so it does change fraud
   column degrees. Why does FRAUDAR still hold F above 0.95 there — what does that attack cost
   the fraudster in g terms?
4. In the bench, degree-rank precision goes 0.00/0.28/0.60/0.76 while obscurity-rank goes
   0.52/0.00/0.00/0.00 across camo 0/0.5/1/2. Explain the opposite monotonicity, and why a
   fraudster can tune camouflage to slip between the two naive scores but not under g.
5. The peel is degree-ordered vertex elimination, same skeleton as k-core (topic 18). Sketch how
   you would run FRAUDAR's weighted peel with a bucketed frontier on GPU — what breaks bucketing
   when degrees are real-valued weights instead of integers?

## Done when

Answer each before unfolding it.

- [ ] You can state the four axioms and give one metric that fails each.

  <details><summary>Answer</summary>

  The four (§3) are Axiom 1 node suspiciousness, Axiom 2 edge suspiciousness,
  Axiom 3 size, and Axiom 4 concentration. Edge density ρ(S) fails **Size**: it
  does not grow with |S|, so a larger block at the same density scores no higher.
  The total edge weight `Σ c_ij` fails **Concentration**: it ignores how the mass
  is spread, so smearing the same weight over more nodes scores the same.

  FRAUDAR's `g(S) = f(S)/|S|` satisfies all four (Theorem 1). Note that unweighted
  average degree also satisfies the axioms — the axioms are necessary, not
  sufficient; camouflage resistance is a *separate* property that needs the column
  weight of Step 5.

  </details>

- [ ] You can reproduce Theorem 3's argument from memory as the two-line "block edges and block
      column degrees are untouched" invariant.

  <details><summary>Answer</summary>

  `f(block)` counts only edges with *both* endpoints inside the block. Camouflage
  edges run from fraud rows to honest columns, so they end outside the block and
  are never counted. And `c_ij = 1/log(d_j + 5)` depends only on the column's
  *global* degree `d_j`; camouflage aimed at honest columns never changes the
  fraud columns' degrees.

  Both quantities that determine `g(block) = f(block)/|S|` are therefore identical
  before and after camouflage (§4.3), so the fraudster cannot push the block's own
  score down — only raise the score of honest-looking sets, which the column
  weighting has already deflated.

  </details>

- [ ] You can sketch the ½-approximation proof shape (first optimal node peeled + averaging).

  <details><summary>Answer</summary>

  Theorem 2 gives `g(returned) ≥ g_OPT/2`. Look at the first moment the peel
  removes a node belonging to the optimal set S*. At that instant every surviving
  node has weighted degree at least `g(current set)` — otherwise it, not this one,
  would have been peeled first.

  That per-node degree bound also caps each node's edge contribution within S*.
  Since each edge is charged to at most two endpoints, summing the bound over S*
  and dividing by |S*| gives `g(S*) ≤ 2·g(current) ≤ 2·g(returned)`. The peel
  itself is `O(|E| log|V|)` with a priority tree keyed on weighted degree.

  </details>

- [ ] Your `fraudar.rs` reproduces log-weighted F = 1.00 at camo 0/0.5/1/2 while unweighted
      degrades to 0.65, and peels the ~1.02M-edge graph in about 0.2 s.

  <details><summary>Answer</summary>

  The lane shows unweighted peeling's F degrading 1.00 / 0.95 / 0.69 / 0.65 as
  camouflage goes 0 / 0.5 / 1 / 2 edges per fraud edge, while the log-weighted
  peel holds F = 1.00 across all four. The ~1.02M-edge graph (100k users × 50k
  objects) peels in ≈0.2 s with the reference solution.

  The weighting `c_ij = 1/log(d_j + 5)` is the *only* change between the two runs;
  the search — greedy minimum-weighted-degree peeling — is identical, which is
  the point of Theorem 3: the fix is the edge weight, not a smarter algorithm.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  This is a worklog check. The five questions test: which axiom edge density
  violates (Size, Axiom 3); why global-degree weights are load-bearing for *both*
  Theorem 3 and the `O(|E| log|V|)` bound; why reverse camouflage still fails
  despite changing fraud-column degrees; the opposite monotonicity of the two
  naive rankers (the repo's headline result); and how to port the weighted peel
  to a bucketed GPU frontier.

  Confirm each has a written answer in notes.md before ticking — the point is to
  force the reasoning, not to look up an answer here.

  </details>

## References

- Hooi, Song, Beutel, Shah, Shin, Faloutsos. *FRAUDAR: Bounding Graph Fraud in the Face of
  Camouflage.* KDD 2016.
- Local experiment: `topics/39-fraud-identity-graphs/experiments/review_graph.rs` — generator:
  Zipf(0.7) × Zipf(0.8) background, planted block, Zipf(1.5) popularity-biased camouflage.
- Local exercise stub: `topics/39-fraud-identity-graphs/experiments/fraudar.rs` — implement the
  lazy min-heap greedy peel; bench lane 1 covers the naive row scores.
- Topic 18 (GPU graph analytics) — bucketed-frontier k-core, the integer-degree cousin of this peel.
