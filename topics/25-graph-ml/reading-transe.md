# TransE: relations as vector translations

The knowledge-graph embedding paper: relations as VECTOR TRANSLATIONS.
Three pages of model, a decade of descendants. Read it for the scoring
function and the training loop — both trivially implementable — and for
what it means to index the result.

## The model, whole

```
  triple (h, r, t)  —  "head, relation, tail":  (Alice, works_at, Acme)

  embed everything in R^d:   want   z_h + z_r ≈ z_t
  score(h,r,t) = || z_h + z_r − z_t ||        (L1 or L2; lower = truer)

  z_Alice ●────z_works_at────▶● z_Acme         one arrow per RELATION,
  z_Bob   ●────z_works_at────▶● z_BobCorp      shared by all its edges
```

Training: margin ranking loss over corrupted triples —
`max(0, γ + score(h,r,t) − score(h',r,t'))` where the corrupted triple
swaps head OR tail with a random entity. Plus the detail everyone forgets:
entity embeddings are re-normalized to the unit ball every batch (else the
loss is trivially minimized by inflating norms).

The whole training step:

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

## Known failure modes (they define the descendants)

- 1-to-N relations: `works_at` maps many heads to one tail → all
  employees collapse toward `z_Acme − z_works_at`. TransH/TransR project
  per-relation; RotatE rotates instead of translates.
- Symmetric relations: `z_r ≈ −z_r` forces `z_r ≈ 0` → `married_to`
  becomes "same embedding". Translation can't express symmetry.
- Composition it CAN do: `z_born_in + z_city_of ≈ z_born_in_country` —
  translations compose by addition. Pick your relation algebra, pick
  your model.

## Why this topic includes it

Property graphs ARE knowledge graphs when edges carry types — FalkorDB's
per-relation delta matrices (one matrix per edge type, topic 20) mirror
TransE's one-vector-per-relation exactly. And the *serving* question is a
vector-index question: "predict missing tail" = argmin_t score(h,r,t) =
nearest-neighbor query for point `z_h + z_r` in the entity index — the
M14 HNSW answers KG completion natively. Embed with anything; serve with
the database.

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
