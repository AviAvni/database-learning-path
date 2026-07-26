# Zep/Graphiti: agent memory as a bi-temporal knowledge graph

Zep (arXiv 2501.13956, Rasmussen et al.) is the paper behind Graphiti, an engine that
turns an agent's ever-growing chat history into an incrementally-built temporal knowledge
graph. Instead of stuffing the full conversation into the prompt or doing flat RAG over
message chunks, Graphiti extracts entities and facts as episodes arrive, stamps every edge
with four timestamps (valid time and transaction time), and answers queries with a hybrid
search-rerank-construct pipeline. For a graph-database developer, the interesting part is
that this is bitemporal MVCC applied to graph edges — version chains, time-travel reads,
and invalidation-instead-of-deletion, wearing an LLM costume.

## The problem in one sentence

**Chat histories grow past any context window, and the facts inside them change over time —
so agent memory needs a store that knows not just what was said, but when each fact was
true and when it was superseded.**

Naive RAG over conversation logs treats every chunk as timelessly true: "Alice works at
Acme" and "Alice works at Beta" both retrieve with equal authority. Zep's answer is a
temporal knowledge graph built incrementally as messages arrive, where contradicted facts
are invalidated (never deleted) and every edge carries its own history.

## The concepts, step by step

### Step 1 — Three subgraphs: episodes, entities, communities

Graphiti layers three subgraphs in one graph, from raw to abstract:

```
  +--------------------------------------------------+
  |  Community subgraph                              |
  |  clusters of related entities + LLM summaries    |
  +------------------------^-------------------------+
                           | label propagation
  +------------------------+-------------------------+
  |  Semantic entity subgraph                        |
  |  entities + relations (facts), with embeddings   |
  +------------------------^-------------------------+
                           | LLM extraction
  +------------------------+-------------------------+
  |  Episode subgraph  (data layer, lossless)        |
  |  raw messages, text, JSON — never rewritten      |
  +--------------------------------------------------+
```

The episode layer is the lossless source of truth; entities and facts are derived from it;
communities summarize clusters of entities. Think base table → index → materialized view.

### Step 2 — Extraction and entity resolution

Entities are extracted per-episode with the last few messages as context, so coreference
("she", "the company") resolves correctly. Each entity gets an embedding and a summary;
each edge (fact) carries a relation name plus a fact string. Entity resolution is a
two-stage merge: embedding candidate search finds plausible existing nodes, then an LLM
verifies whether the new mention is the same entity before merging. This is fuzzy
upsert-by-similarity — the write path's dedup logic, done with an LLM as the comparator.

### Step 3 — The bi-temporal model (§2.1): two timelines, four timestamps

Every edge carries timestamps on two independent timelines. Timeline T is event time —
when the fact was true in the world. Timeline T' is ingestion (transactional) time — when
the system learned it. Four timestamps per edge:

```
  Timeline T  (event time):        t_valid ......... t_invalid
  Timeline T' (transaction time):  t'_created ...... t'_expired

  edge: (Alice) -[WORKS_AT]-> (Acme)
        t_valid=Jan   t_invalid=Jun     "true in the world Jan..Jun"
        t'_created=Feb t'_expired=Jul   "known to the system Feb..Jul"
```

This is classic valid-time × transaction-time bitemporality from the database literature,
applied to knowledge-graph edges. The two timelines are independent: a fact can be learned
long after it became true, and unlearned (superseded) long after it stopped being true.

### Step 4 — Edge invalidation: contradiction as versioning

At ingest, an LLM compares each new fact against existing semantically-related edges. When
a new fact contradicts an old one, the old edge is invalidated — never deleted:

```
  ingest: "Alice works at Beta" (valid from Jun)

  old edge  (Alice)-[WORKS_AT]->(Acme)
            t_invalid   := Jun   (new fact's validity start)
            t'_expired  := now
            edge KEPT — audit trail

  new edge  (Alice)-[WORKS_AT]->(Beta)
            t_valid     := Jun
            t'_created  := now
```

New information wins by default. Because nothing is deleted, "what was true in March?"
(filter on T) and "what did we know in March?" (filter on T') are each one predicate away.
This is exactly a version chain: invalidation writes tombstone timestamps instead of
removing tuples, and as-of queries are time-travel reads over that chain.

### Step 5 — Communities via dynamic label propagation

Communities cluster related entities, each with an LLM-written summary. Graphiti uses
label propagation rather than Leiden for one engineering reason: label propagation has a
cheap dynamic extension. When a new node arrives, it simply adopts the majority label of
its neighbors — full recomputation is postponed rather than triggered per write. That is
the incremental-maintenance trade every streaming system makes: accept slightly stale
partitions in exchange for O(degree) update cost.

### Step 6 — Retrieval: the φ → ρ → χ funnel

Query time is a three-phase pipeline:

```
  query
    |
    v
  φ  search (parallel):  cosine similarity on embeddings
                          + BM25 fulltext
                          + graph BFS from recently-mentioned nodes
    |
    v
  ρ  rerank:  RRF, MMR, episode-mentions reranker,
              node-distance reranker, cross-encoder
    |
    v
  χ  construct:  assemble facts / entities / summaries
                 into a compact context string for the prompt
```

φ casts a wide net across three signal types; ρ fuses and reorders (RRF for fusion, MMR
for diversity, graph-aware rerankers for locality, a cross-encoder for precision); χ
serializes the survivors into a small context block. It is a query executor: scan
operators feeding a rank-merge feeding a projection.

### Step 7 — Results: accuracy up, latency way down

On DMR (Deep Memory Retrieval), Zep scores 94.8% vs MemGPT's 93.4% (gpt-4-turbo). The
stronger evidence is LongMemEval with gpt-4o: accuracy 60.2% → 71.2%, and response latency
28.9 s → 2.58 s — about a 90% cut — because the prompt shrinks from ~115k tokens (full
conversation) to ~1.6k tokens (retrieved facts). Biggest category gains:
single-session-preference +184% and temporal reasoning +38.4%. One honest regression:
single-session-assistant −17.7% — when the answer needs verbatim recall of one recent
session, full context beats retrieval.

### Step 8 — The database-internals reading

Strip the LLM machinery and Graphiti is bitemporal MVCC on a graph. Invalidation instead
of deletion is a version chain; t'_expired is a tombstone; as-of queries are snapshot
reads; the episode layer is the WAL-like lossless log the derived layers can always be
rebuilt from. Topic 33 in this path (temporal graphs) covers the same model from the
storage side — this paper is that model deployed as a product, with the extraction and
retrieval stages bolted on where a database would have parsers and planners.

## How to read the paper (with the concepts in hand)

1. Read the abstract and introduction for the problem framing (Step 1's motivation): why
   context windows and flat RAG fail for agent memory.
2. Read the architecture description of the three subgraphs — episodes, entities,
   communities — and map each to the stack diagram in Step 1. Note the label-propagation
   choice and its dynamic extension (Step 5).
3. Slow down at §2.1, the bi-temporal model. Draw the two timelines yourself and confirm
   the four timestamps match Step 3. This is the core of the paper.
4. Read the extraction and entity-resolution passages with Step 2 in hand; then the edge
   invalidation contract against Step 4 — check that old edges are kept, not deleted.
5. Read the retrieval section mapping each named component (RRF, MMR, cross-encoder, BFS)
   into the φ/ρ/χ funnel of Step 6.
6. Finish with the evaluation: DMR, then LongMemEval per-category numbers from Step 7.
   Look for the token-count explanation of the latency drop, and find the
   single-session-assistant regression — ask yourself why retrieval loses there.
7. Then run the companion experiment (see References) and compare its four-timestamp
   contract to the paper's.

## Questions to answer in notes.md

1. For each of the four timestamps (t_valid, t_invalid, t'_created, t'_expired), which
   timeline does it live on, and who sets it — the world, the LLM, or the ingest clock?
2. When a new fact contradicts an old edge, exactly which timestamps change on the old
   edge and why is the edge kept rather than deleted? What queries would break if it were
   deleted?
3. Why does Graphiti choose label propagation over Leiden for communities, and what
   staleness does the dynamic extension accept in exchange?
4. In the φ → ρ → χ pipeline, which reranker would you expect to matter most for temporal
   reasoning questions, and what graph-database operator does each φ search method
   correspond to?
5. Why does single-session-assistant regress −17.7% while single-session-preference gains
   +184%? What does that say about when retrieval beats full context?

## Done when

- [ ] You can draw the two timelines and place all four edge timestamps without looking.
- [ ] You can state the invalidation contract (which timestamps are set, nothing deleted)
      and phrase both "true in March" and "known in March" as single filters.
- [ ] You can name the three φ search methods and at least three ρ rerankers.
- [ ] You ran the companion temporal.rs experiment and reproduced the reference shape:
      100,000 edges kept from 10,000 entities × 10 job changes, 10,000 current, as-of
      scan ≈ 0.09 ms.
- [ ] You can explain the LongMemEval latency drop in tokens (~115k → ~1.6k) and name the
      one category that regressed.

## References

- Paper: "Zep: A Temporal Knowledge Graph Architecture for Agent Memory", Rasmussen et
  al. — https://arxiv.org/abs/2501.13956 (local copy: /tmp/zep.pdf)
- Companion experiment in this repo: [experiments/src/temporal.rs](experiments/src/temporal.rs)
  — miniature bi-temporal edge store with the same four timestamps and
  contradiction-invalidation contract.
- Topic 33 of this learning path — temporal graphs; same valid-time × transaction-time
  model from the storage-engine side.
- Classic background: valid-time and transaction-time bitemporality in the temporal
  database literature (the model §2.1 instantiates on KG edges).
