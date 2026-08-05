# TransE: relations as vector translations

The knowledge-graph embedding paper: relations as VECTOR TRANSLATIONS.
Three pages of model, a decade of descendants. Read it for the scoring
function and the training loop — both trivially implementable — and for
what it means to index the result. This chapter builds it step by
step: what a knowledge graph is, the one-line model, the training
loop and its non-obvious detail, the failure modes that spawned the
descendants, and why serving the result is a vector-index query.

## The problem in one sentence

A knowledge graph stores facts it *has* — predicting the facts it's
*missing* ("Alice works at ___?") requires scoring candidate edges,
and TransE does it with a model so small it's one d-dimensional
vector per entity plus **one vector per relation type** (for
Freebase-scale data: millions of entities, a few thousand relations).

## The concepts, step by step

### Step 1 — the knowledge graph: facts as typed triples

> **In:** a store of known facts — typed edges.
> **Out:** the link-prediction task: rank candidate tails for a query (h, r,
> ?). Step 2 is the model that scores them.

A knowledge graph (KG) stores facts as **triples** (h, r, t) — "head
entity, relation, tail entity": (Alice, works_at, Acme), (Acme,
based_in, Berlin). It's a graph whose edges carry types, which means
property graphs ARE knowledge graphs when edges carry types —
FalkorDB's per-relation delta matrices (one matrix per edge type,
topic 20) are exactly a KG's storage layout. The task this paper
serves is **link prediction** (KG completion): given h and r, rank
all entities by how plausible (h, r, t) is — recommend the missing
tail. Why it matters: real KGs are radically incomplete (most people
in Freebase lack a birthplace fact), so completion is the workload.

### Step 2 — the model: relations are translations in vector space

> **In:** the triples from Step 1.
> **Out:** one `d`-vector per entity and per relation, with a scalar
> `score(h,r,t)` per candidate fact. Step 3 is how those vectors are trained.

Embed every entity AND every relation as a point in R^d, and demand
that a true fact line up as vector addition — head plus relation
lands near tail:

```
  triple (h, r, t)  —  "head, relation, tail":  (Alice, works_at, Acme)

  embed everything in R^d:   want   z_h + z_r ≈ z_t
  score(h,r,t) = || z_h + z_r − z_t ||        (L1 or L2; lower = truer)

  z_Alice ●────z_works_at────▶● z_Acme         one arrow per RELATION,
  z_Bob   ●────z_works_at────▶● z_BobCorp      shared by all its edges
```

This is exactly the paper's dissimilarity `d(h + ℓ, t)`, "which we take
to be either the L1 or the L2-norm" (Bordes et al. §2). Work one score by
hand in R²: let `z_Alice = (0,0)`, `z_works_at = (1,0)`, `z_Acme =
(0.9, 0.1)`. Then `z_h + z_r − z_t = (0.1, −0.1)` and the L2 score is
`√(0.1² + 0.1²) = √0.02 ≈ 0.14` — small, so the model believes it. A
corrupted tail `z_Berlin = (−1, 2)` scores `‖(1,0) − (−1,2)‖ = ‖(2,−2)‖ =
2.83` — far, so it does not.

The one arrow per relation is the model's entire capacity: every
works_at edge in the graph must be (approximately) the *same*
displacement vector. That's an aggressive compression — a relation
with a million instances becomes d floats — and both the model's
power (Step 5's serving trick) and its failures (Step 4) follow from
it. The score is just distance: low ‖z_h + z_r − z_t‖ means "the
model believes this fact".

### Step 3 — training: push true triples together, corrupted ones apart

> **In:** the entity and relation vectors from Step 2.
> **Out:** trained vectors, via a margin ranking loss over true-vs-corrupted
> triples. Step 4 reads off what this model structurally cannot learn.

Distances only mean something relative to alternatives, so TransE
trains with a **margin ranking loss**: for each true triple, make a
deliberately-broken one — a **corrupted triple**, the true triple
with head OR tail swapped for a random entity — and require the true
score to beat the corrupted score by a margin γ:
`[γ + score(h,r,t) − score(h',r,t')]_+` (Bordes et al. eq. 1, where
`[x]_+` is the positive part and γ > 0). Plus the detail everyone
forgets: entity embeddings are re-normalized to **unit L2 norm**
(‖z‖ = 1) at the start of every batch — Algorithm 1 line 5,
`e ← e/‖e‖ for each entity e` — otherwise the loss is trivially minimized
by inflating all norms (make every vector huge and every margin is
satisfied without learning anything). Note the asymmetry: the paper
normalizes *entities* every batch but the *relation* vectors only once at
init (Algorithm 1 line 2), which is why `train_step` below renormalizes
`ent` and leaves `rel` alone. The whole training step, as the paper's
Algorithm 1 spells it (this repo has no TransE lane — the experiments
crate implements node2vec/SGNS, GCN and SpMM only, so this is pseudocode,
not a quote):

```text
// PSEUDOCODE — transcribed from Bordes et al. Algorithm 1; no repo/pinned
// source implements TransE, so there is no file:line to anchor here.
fn train_step(ent: &mut Mat, rel: &Mat, (h, r, t): Triple,
              gamma: f32, lr: f32, rng: &mut Rng) {
    ent.renormalize_unit_norm();                 // Algorithm 1 line 5 (entities only)
    let (hc, tc) = corrupt(h, t, rng);           // swap head OR tail, random entity
    let pos = l2(ent.row(h) + rel.row(r) - ent.row(t));
    let neg = l2(ent.row(hc) + rel.row(r) - ent.row(tc));
    if gamma + pos - neg > 0.0 {                 // margin violated: push
        sgd(ent, rel, (h, r, t), (hc, r, tc), lr);  // pos triple closer,
    }                                               // neg triple apart
}
```

One hidden assumption to notice: random corruption presumes the
corrupted triple is *false*. On a dense relation that's often wrong
(a random company might actually employ Alice) — false negatives that
punish the model for being right. Question 2 connects this to
cardinality statistics.

### Step 4 — the failure modes: what one arrow per relation can't say

> **In:** the single-translation-per-relation model from Step 2.
> **Out:** its relation algebra — which relation shapes it can and cannot
> represent. Step 5 turns the trained vectors into a query.

The compression of Step 2 has a relation algebra, and knowing it is
knowing when to use the model:

- 1-to-N relations: `works_at` maps many heads to one tail → all
  employees collapse toward `z_Acme − z_works_at` — thousands of
  distinct people forced to (nearly) one point. TransH/TransR project
  per-relation; RotatE rotates instead of translates.
- Symmetric relations: (h, r, t) true iff (t, r, h) true forces
  `z_r ≈ −z_r`, i.e. `z_r ≈ 0` → `married_to` degenerates to "same
  embedding". Translation can't express symmetry (question 1 is the
  two-line proof).
- Composition it CAN do: `z_born_in + z_city_of ≈ z_born_in_country`
  — translations compose by addition, so chains of relations come
  free. Pick your relation algebra, pick your model — the decade of
  descendants is exactly this table with different geometry.

### Step 5 — serving is a nearest-neighbor query: why a database cares

> **In:** the trained entity/relation vectors and a query (h, r, ?).
> **Out:** the missing tail as `argmin_t ‖z_h + z_r − z_t‖` — a
> nearest-neighbour query over the entity index, filtered to exclude known
> tails.

Here is why this topic includes a 2013 ML paper: the *serving* path
lands squarely on database machinery. "Predict the missing tail" =
argmin over all entities t of ‖z_h + z_r − z_t‖ = a nearest-neighbor
query for the point `z_h + z_r` in the entity embedding index — the
M14 HNSW answers KG completion natively, in milliseconds, over
millions of entities. And the storage mirror is exact: FalkorDB keeps
one delta matrix per relation type; TransE keeps one vector per
relation type — the same schema decision ("relations are first-class,
few in number, worth their own artifact") made independently by a
storage engine and an embedding model. Embed with anything; serve
with the database. The catch is the evaluation protocol: ranking must
*exclude* tails already known true (the "filtered ranking" protocol),
which becomes a filtered ANN query — topic 14's filtered-search
problem wearing KG clothes (question 3).

## How to read the paper (with the concepts in hand)

- It's three pages of model — read the scoring function (Step 2) and
  the training algorithm (Step 3) closely; both should look like the
  code above.
- Check the renormalization step in Algorithm 1 — it's easy to skim
  past and impossible to train without (Step 3's inflating-norms
  argument).
- Read the evaluation protocol for the filtered-vs-raw ranking
  distinction (Step 5) — the filtered numbers are the meaningful
  ones, and the filter is a database predicate.
- Skip nothing else; there is nothing else. Spend the saved time on
  Step 4's failure modes against a KG you know — FalkorDB edge types
  from any real deployment sort cleanly into translation-friendly
  and translation-hostile.

## Questions (answer in notes.md)

1. Prove the symmetric-relation collapse (score(h,r,t) = score(t,r,h)
   for all pairs ⟹ what about z_r?).
2. Corrupted-triple sampling assumes false negatives are rare — when is
   that wrong on a real KG, and which database statistic (topic 9
   cardinality) would fix the sampler?
3. Link prediction = ANN query: what FILTER does the vector index need
   (exclude known tails — the "filtered ranking" protocol) and how does
   that interact with HNSW's search (topic 14's filtered-search problem)?
4. TransE on our SBM (untyped edges, one relation): what degenerates,
   and what does that say about when KG embeddings beat node2vec?
5. M25 stretch: `CALL algo.transe(rel_types...)` — where do per-relation
   vectors live (graph metadata? a relations table?) and do they update
   transactionally with edge-type DDL?

## Done when

Answer each before unfolding it.

- [ ] You can state the model in one equation and say what it assumes about relations.

  <details><summary>Answer</summary>

  `score(h,r,t) = ‖z_h + z_r − z_t‖` (L1 or L2), trained so true triples
  score low. The assumption is that every instance of a relation is the
  *same* translation vector `z_r` — one arrow per relation type, shared by
  all its edges. That is a strong compression: it forces `z_h + z_r ≈ z_t`
  to hold simultaneously for every head/tail pair the relation connects.

  </details>

- [ ] You can prove the symmetric-relation collapse.

  <details><summary>Answer</summary>

  If r is symmetric, `(h,r,t)` and `(t,r,h)` are both true, so the model
  wants `z_h + z_r ≈ z_t` and `z_t + z_r ≈ z_h`. Adding the two gives
  `2 z_r ≈ 0`, i.e. `z_r ≈ 0`, so a symmetric relation degenerates to the
  zero translation — `married_to` becomes "same embedding" and cannot be
  distinguished from identity. Translation simply cannot express symmetry.

  </details>

- [ ] You can name the failure modes one arrow per relation cannot express.

  <details><summary>Answer</summary>

  1-to-N relations (`works_at` maps many heads to one tail) collapse all
  those heads toward `z_t − z_r`; symmetric relations force `z_r ≈ 0`;
  reflexive/N-to-N relations are similarly crushed. What it *can* do is
  composition — `z_born_in + z_city_of ≈ z_born_in_country` — because
  translations add. TransH/TransR (per-relation projections) and RotatE
  (rotation instead of translation) exist to lift these limits.

  </details>

- [ ] You can explain why serving is a nearest-neighbour query and what filter the vector index needs.

  <details><summary>Answer</summary>

  Predicting the tail is `argmin_t ‖z_h + z_r − z_t‖` — a nearest-neighbour
  search for the query point `z_h + z_r` in the entity embedding index, which
  an HNSW index (topic 14) answers in milliseconds over millions of entities.
  The filter is the "filtered ranking" protocol: exclude tails already known
  true for (h, r), so it becomes a filtered ANN query — topic 14's
  filtered-search problem in KG clothing.

  </details>

- [ ] You can say what degenerates when TransE is applied to an untyped single-relation graph like this topic's SBM.

  <details><summary>Answer</summary>

  With one relation type, there is a single translation `z_r` shared by every
  edge, so the model reduces to "`z_h + z_r ≈ z_t` for all edges" — which for
  an undirected/symmetric graph drives `z_r ≈ 0` (the collapse above), leaving
  only `z_h ≈ z_t` across edges: a plain proximity embedding with none of the
  relational structure TransE was built for. That is exactly when node2vec's
  untyped proximity objective is the better tool, and when typed KG
  embeddings start to pay off.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  All five `## Questions` answered in notes.md — the symmetric-collapse proof,
  the false-negative sampler fix via cardinality statistics (topic 9), the
  filtered-ANN interaction with HNSW, the single-relation degeneracy, and
  where per-relation vectors live under `CALL algo.transe(...)`.

  </details>

## References

**Papers**
- Bordes, Usunier, Garcia-Durán, Weston, Yakhnenko — "Translating
  Embeddings for Modeling Multi-relational Data" (NeurIPS 2013) —
  three pages of model; read for the scoring function and training
  loop
