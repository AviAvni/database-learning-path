# Topic 38 notes — GraphRAG & agent memory

## Predictions vs measurements

| question | predicted | measured |
|---|---|---|
| mention-count rank, h=1 | ~1 (answer named by both seeds) | **1.00** (400 trials, seed 42) |
| mention-count rank, h=2 | chance = (1+17)/2 = 9 | **9.21** (h=3: 8.71) |
| BFS-distance rank, any h | chance — all candidates equidistant | **9.51 / 8.95 / 9.15** at h=1/2/3 |
| lane 2: PPR rank at h=2,3 | 1 — mass sums at the meet node | (stub — reference run: **1.00/1.00/1.00**) |
| lane 2: PPR on 100k nodes / ~400k edges, 30 iters | tens of ms | (stub — reference: **56.6 ms**/query) |
| lane 3: 10k entities × 10 changes | 100k kept, 10k current | (stub — reference: **100,000 / 10,000**, as-of scan **0.09 ms**; 1k×10: 10,000 / 1,000 / 0.01 ms) |

The lane-1 mechanic, worth memorizing: vector RAG scores each passage
*independently* against the query, so it can only find answers whose
evidence co-occurs with query entities in one passage. Path-finding
questions (answer connects to both entities, no passage mentions both)
break that shape at 2 hops — every candidate scores zero and rank is
chance. PPR fixes it with arithmetic, not iteration: restart mass at
the seeds, walk; the answer is the only node that *sums* mass from two
sources. At damping 0.5 the meet node gets ~2× a dead-end's mass.

## Guide-question checklist

- [ ] reading-hipporag.md Q1–Q5
- [ ] reading-graphrag-paper.md Q1–Q5
- [ ] reading-zep-graphiti.md Q1–Q5
- [ ] reading-graphrag-sdk.md Q1–Q5

## Cross-topic threads (worked)

- Topic 18 ↔ 38: `ppr()` is the same push-style iterate-over-CSR loop
  as topic 18's PageRank; the only deltas are the restart vector
  (seeds, not uniform) and dangling-mass handling. M38 reuses the CSR.
- Topic 33 ↔ 38: Graphiti's (t_valid, t_invalid, t_created, t_expired)
  is valid-time × transaction-time bitemporality on edges; `as_of` is
  the 2D visibility filter, `current()` = as_of(∞, ∞).
- Topic 37 ↔ 38: PPR touches a seed neighborhood, so hash-sharded
  graphs turn one query into an all-shard fan-out — the tail math of
  topic 37 is why graph DBs prefer fat single nodes for this workload.

## Capstone M38 log

- Surface: entity ingest + resolution into M31's graph; PPR procedure
  over topic-18 CSR; bi-temporal edge versioning with as-of reads.
- Targets: PPR recall@5 ≈ 1.0 on synthetic 2-hop questions where
  direct mention is chance; PPR under 100 ms on 100k nodes; as-of read
  within 2× of a current-only read at 10 versions/entity.
- Order of work: PPR procedure first (pure read path over CSR), then
  ingest/resolution, then temporal versioning (touches write path).

## Infra notes

- Papers read in full from PDFs: /tmp/hipporag.pdf (arXiv 2405.14831v3),
  /tmp/graphrag.pdf (arXiv 2404.16130v2), /tmp/zep.pdf (arXiv
  2501.13956).
- HippoRAG facts: neocortex=LLM / parahippocampal=retrieval encoders
  (synonymy at cosine τ=0.8) / hippocampus=KG+PPR (damping 0.5).
  Two-step OpenIE (NER, then triples). Node specificity sᵢ=|Pᵢ|⁻¹
  multiplies query-node restart mass. Passage score = π⃗·P (P = |N|×|P|
  node-in-passage counts). R@2/R@5: MuSiQue 40.9/51.9, 2Wiki 70.7/89.1,
  HotpotQA 60.5/77.7; +IRCoT: +4/+18/+1% R@5. 10-30× cheaper, 6-13×
  faster than IRCoT online. Ablations: REBEL OpenIE collapses (GPT-3.5
  makes 2× triples); PPR ≫ query-nodes-only ≫ nodes+neighbors;
  synonymy helps 2Wiki most; all-recall AR@5 2Wiki 37.1→75.7.
  Path-finding vs path-following: the Stanford/Alzheimer's professor
  example — ColBERTv2 and IRCoT fail, HippoRAG ranks Südhof 1st.
- GraphRAG facts: 600-token chunks (100 overlap), gleaning re-passes,
  exact-string entity match with duplicate-count edge weights,
  hierarchical Leiden (graspologic), bottom-up community summaries
  (leaf: elements prioritized by source+target degree; parents
  substitute child summaries on overflow). Query: shuffle+chunk
  summaries, map partial answers scored 0-100 (0 filtered), reduce by
  helpfulness. Podcast corpus 1669 chunks (~1M tokens) → 8,564/20,691;
  News 3197 (~1.7M) → 15,754/19,520; indexing 281 min gpt-4-turbo.
  Win rates vs vector RAG: comprehensiveness 72-83%, diversity 62-82%.
  C0 query cost 26,657 tokens ≈ 2.6% of TS max; root 9-43× fewer
  tokens; C3 26-33% fewer. Claim experiment: 47,075 claims, avg
  31/answer, C0 News 34.18 vs SS 25.23. Question gen: 5×5×5 = 125.
- Zep facts: three tiers (episode/semantic/community), bi-temporal
  (T event, T' ingestion — 4 timestamps), LLM invalidation keeps old
  edge expired, label-propagation communities with dynamic extension,
  retrieval φ (cosine+BM25+BFS) → ρ (RRF/MMR/episode-mention/
  node-distance/cross-encoder) → χ. DMR 94.8 vs MemGPT 93.4;
  LongMemEval gpt-4o 60.2→71.2%, latency 28.9→2.58 s, context
  115k→1.6k; temporal +38.4%; single-session-preference +184%;
  regression single-session-assistant −17.7%.
- GraphRAG-SDK (~/repos/GraphRAG-SDK @ f42ab3d) anchors verified by
  grep/read this session — full list in README table plus:
  pipeline.py:94 run; graph_extraction.py:438 _aggregate_entities /
  :479 _aggregate_relations; exact_match.py:21, description_merge.py:30,
  semantic_resolution.py:33 (:122 _fuzzy_merge); graph_store.py:21/:56/
  :129/:214; vector_store.py:169 index_chunks / :244 embed_relationships
  / :371 search_entities / :406 search_relationships;
  cypher_generation.py:145/:162/:187; entity_discovery.py:20
  is_enumeration_query; result_assembly.py:105 detect_question_type.
  Multi-path defaults: chunk_top_k=15, max_entities=30,
  max_relationships=20, rel_top_k=15, keyword_limit=10,
  enable_cypher=False (experimental).
- Crate: 3 provided tests green (kg.rs — instance shape 36 passages /
  17 candidates / 4 gold; h=1 mean under 1.05; h=2 mean 9±1). 6 stub
  tests fix contracts for ppr.rs (distribution sums to 1; chain decay;
  meet-node rank 1 with mention > 7.0 / PPR < 1.05 over 100 trials)
  and temporal.rs (invalidate-without-delete with t_invalid=Some(200)/
  t_expired=Some(205); event-time reconstruction; late-arriving fact
  visible to as_of(10,1000) but not as_of(10,50)). Reference solution
  verified 9/9 then reverted; pristine crate: 0 warnings, lanes 2-3
  print `[stub …]` banners via catch_unwind.

## Done when

- [ ] All 9 tests pass; lanes 2-3 print real numbers.
- [ ] Exercise 2 hand-derivation: the 2× meet-node mass gap at h=2,
      damping 0.5, written out.
- [ ] Damping sweep (exercise 3): rank vs damping table recorded.
- [ ] Node-specificity variant (exercise 4): hub-seed instance built,
      rank with/without 1/degree weighting compared.
- [ ] Compaction trade-off (exercise 5): speedup measured, the lost
      audit question named.
- [ ] All 20 guide questions answered in writing.
- [ ] M38 sketch (exercise 6) upgraded to a design note.
