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

The one arrow per relation is the model's entire capacity: every
works_at edge in the graph must be (approximately) the *same*
displacement vector. That's an aggressive compression — a relation
with a million instances becomes d floats — and both the model's
power (Step 5's serving trick) and its failures (Step 4) follow from
it. The score is just distance: low ‖z_h + z_r − z_t‖ means "the
model believes this fact".

### Step 3 — training: push true triples together, corrupted ones apart

Distances only mean something relative to alternatives, so TransE
trains with a **margin ranking loss**: for each true triple, make a
deliberately-broken one — a **corrupted triple**, the true triple
with head OR tail swapped for a random entity — and require the true
score to beat the corrupted score by a margin γ:
`max(0, γ + score(h,r,t) − score(h',r,t'))`. Plus the detail everyone
forgets: entity embeddings are re-normalized to the unit ball every
batch — otherwise the loss is trivially minimized by inflating all
norms (make every vector huge and every margin is satisfied without
learning anything). The whole training step:

```rust
fn train_step(ent: &mut Mat, rel: &Mat, (h, r, t): Triple,
              gamma: f32, lr: f32, rng: &mut Rng) {
    ent.renormalize_unit_ball();                 // the detail everyone forgets
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

## References

**Papers**
- Bordes, Usunier, Garcia-Durán, Weston, Yakhnenko — "Translating
  Embeddings for Modeling Multi-relational Data" (NeurIPS 2013) —
  three pages of model; read for the scoring function and training
  loop
