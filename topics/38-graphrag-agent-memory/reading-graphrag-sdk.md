# GraphRAG-SDK: the whole GraphRAG stack inside one FalkorDB round trip

Most GraphRAG stacks are Frankenstein architectures: a graph database for entities, a
separate vector database for embeddings, maybe an Elasticsearch for keyword search, and
application glue that pays a network round trip to each. FalkorDB's GraphRAG-SDK takes the
opposite bet — the knowledge graph, the vector indices, and the fulltext indices all live
inside FalkorDB itself, so entity discovery, vector search, and graph expansion are queries
against one database. This guide walks the code at commit `f42ab3d` (clone at
`~/repos/GraphRAG-SDK`), from the fixed ingestion pipeline to the multi-path retriever.

## The problem in one sentence

**How do you turn raw documents into a queryable knowledge graph — and answer questions
over it — without bolting a separate vector store onto your graph database?**

The SDK's answer: make the graph store, the vector store, and the fulltext index three
views of the same FalkorDB instance, then build a deterministic ingestion pipeline and a
multi-path retriever on top. For a FalkorDB core dev, this is your database being used as
the entire RAG substrate — every design choice here is a workload you serve.

## The concepts, step by step

### Step 1 — One database, three index types

The architectural thesis lives in `storage/`. `GraphStore` (graph_store.py:21) wraps a
`FalkorDBConnection` and handles node/relationship upserts and traversals. `vector_store.py`
(:35) creates vector indices on entities (:104) and on relationships (:108), plus a fulltext
index (:485) — all as FalkorDB indices, not external services.

```
            +--------------------------- FalkorDB ---------------------------+
            |                                                                |
  query --> |  property graph        vector indices         fulltext index   |
            |  (entities, chunks,    (entity embeddings,    (entity names,   |
            |   MENTIONED_IN,         relationship/RELATES   chunk text)     |
            |   RELATES edges)        embeddings)                            |
            |                                                                |
            +----------------------------------------------------------------+
                     one round trip: discover + vector-search + expand

  vs. the usual stack:   app --> graph DB --> app --> vector DB --> app --> ES
```

Contrast with stacks where a query fans out to three systems and reassembles in the
application tier. Here the join happens where the data is.

### Step 2 — A fixed 9-step ingestion pipeline

`IngestionPipeline` (ingestion/pipeline.py:35, `run` at :94) hard-codes the step order —
it is not a configurable DAG. The sequence: Load → Chunk → lexical graph → Extract →
quality filter (:329) → Prune (:282) → Resolve → Write → then Mentions and Index run
concurrently via `asyncio.gather` (:175).

```
 Load -> Chunk -> LexicalGraph -> Extract -> QualityFilter -> Prune -> Resolve -> Write
                  (MANDATORY)                                                      |
                                                                          asyncio.gather
                                                                          /            \
                                                                     Mentions        Index
```

The lexical-graph step is mandatory: chunk and document nodes plus MENTIONED_IN edges are
always built, even when entity extraction is disabled. You always get a retrievable corpus
graph; entities are the optional enrichment, not the foundation.

### Step 3 — Two-step extraction: local NER first, LLM second

`GraphExtraction` (extraction_strategies/graph_extraction.py:89) splits extraction into a
pluggable NER pass (default `GLiNERExtractor`, a local model — no API call) and an LLM pass
that verifies the entities and extracts relations. This is the same shape as HippoRAG's
2-step OpenIE (see [reading-hipporag.md](reading-hipporag.md)): a cheap recall-oriented
first pass, an expensive precision-oriented second pass.

Two details worth reading closely. First, budget awareness: `ctx.budget_exceeded` is
checked at :148, so extraction degrades gracefully when the token budget runs out instead
of failing the pipeline. Second, cross-chunk merging: `_aggregate_entities` (:438) and
`_aggregate_relations` (:479) fold duplicates found in different chunks before resolution
even starts. Coreference resolution is optional.

### Step 4 — A ladder of resolution strategies, cheap to expensive

`resolution_strategies/` is an escalation ladder — each rung costs more and catches more:

```
  cost
   ^   llm_verified_resolution.py:75   embedding candidates -> LLM-verified merge (:192)
   |   semantic_resolution.py:33       embedding similarity, _fuzzy_merge (:122)
   |   description_merge.py:30         merge descriptions of same-name entities
   |   exact_match.py:21               string equality
   +------------------------------------------------------------------> recall
```

The top rung — `_embedding_and_llm_merge` (llm_verified_resolution.py:192) — searches for
merge candidates by embedding, then asks an LLM to confirm each merge. That is precisely
the escalation pattern Zep/Graphiti uses for entity resolution
(see [reading-zep-graphiti.md](reading-zep-graphiti.md)); it shows up independently in
every serious KG-construction system because embeddings alone over-merge.

### Step 5 — Deduplication with the survivor pattern

`EntityDeduplicator` (storage/deduplicator.py:35) runs exact dedup (:74) and fuzzy dedup
(:115). The interesting graph-database mechanics are in `_remap_entity_edges` (:228): when
two nodes merge, the loser's edges are re-pointed to the surviving node *before* the loser
is deleted. Miss that ordering and every merge silently drops relationships — this is the
kind of invariant your database's users depend on getting right in application code.

### Step 6 — Rule-based routing: no LLM in the hot path

`SemanticRouter` (retrieval/router.py:19) picks a retrieval strategy with plain rules —
first match wins (`_select`, :84), falling back to a default strategy. No LLM call means
routing costs microseconds, is deterministic, and is debuggable. Supporting classifiers are
also rule-based: `is_enumeration_query` (retrieval/entity_discovery.py:20) and
`detect_question_type` (retrieval/result_assembly.py:105).

### Step 7 — Multi-path retrieval: four chunk paths, one reranker

`MultiPathRetrieval` (retrieval/strategies/multi_path.py:48) is the flagship, itself a
9-step sequence: (1) keyword extraction (stopword filter + LLM), (2) embed the query ONCE
and reuse it everywhere, (3) vector search over RELATES relationship embeddings, (4) entity
discovery via Cypher CONTAINS + fulltext, (5) 1-hop and 2-hop graph expansion from found
entities, (6) chunk retrieval over four parallel paths, (7) source-document fetch,
(8) cosine rerank, (9) context assembly.

```
                     query (embedded once)
                            |
        +----------+--------+---------+------------------+
        |          |                  |                  |
    fulltext    vector on         MENTIONED_IN        2-hop
    chunks      chunk index       from entities       expansion chunks
        |          |                  |                  |
        +----------+--------+---------+------------------+
                            v
                  CosineReranker (top_k=15)
                            v
                    context assembly
```

Defaults: chunk_top_k=15, max_entities=30, max_relationships=20, rel_top_k=15,
keyword_limit=10. All four chunk paths are queries against the same FalkorDB — Step 1's
thesis cashing out. The reranker is `CosineReranker` (reranking_strategies/cosine.py:18).

Note what is absent: there is no community-summary layer, no global axis in the Microsoft
GraphRAG sense (see [reading-graphrag-paper.md](reading-graphrag-paper.md)). The SDK's
retrieval is the local/associative kind — start from entities, expand, rerank.

### Step 8 — Text-to-Cypher exists, but behind a guard rail

`enable_cypher=False` by default: the text-to-Cypher path is experimental and off. When
enabled, `retrieval/cypher_generation.py` runs generated queries through `extract_cypher`
(:145), `_sanitize_cypher` (:162), and `validate_cypher` (:187) before execution. LLM-written
Cypher hitting a production graph gets extracted, sanitized, and validated — a sensible
posture for anyone who has watched an LLM hallucinate a Cartesian product.

## Where each step lives in the code

Paths relative to `graphrag_sdk/src/graphrag_sdk/`.

| Step | Anchor (file:line) | What to see |
|---|---|---|
| 1 | storage/graph_store.py:21 | `GraphStore` wrapping FalkorDBConnection |
| 1 | storage/graph_store.py:56, :129, :214 | `upsert_nodes`, `upsert_relationships`, `get_connected_entities` |
| 1 | storage/vector_store.py:104, :108, :485 | entity/relationship vector indices + fulltext, all in FalkorDB |
| 1 | storage/vector_store.py:169, :244 | `index_chunks`, `embed_relationships` |
| 2 | ingestion/pipeline.py:35, :94 | `IngestionPipeline` and `run` — the fixed 9 steps |
| 2 | ingestion/pipeline.py:329, :282, :175 | quality filter, prune, concurrent Mentions+Index via gather |
| 3 | extraction_strategies/graph_extraction.py:89 | `GraphExtraction` — NER pass then LLM pass |
| 3 | extraction_strategies/graph_extraction.py:148 | `ctx.budget_exceeded` graceful degradation |
| 3 | extraction_strategies/graph_extraction.py:438, :479 | `_aggregate_entities`, `_aggregate_relations` |
| 4 | resolution_strategies/exact_match.py:21 | cheapest rung: string equality |
| 4 | resolution_strategies/semantic_resolution.py:33, :122 | embedding similarity, `_fuzzy_merge` |
| 4 | resolution_strategies/llm_verified_resolution.py:75, :192 | `_embedding_and_llm_merge` escalation |
| 5 | storage/deduplicator.py:74, :115, :228 | exact/fuzzy dedup, `_remap_entity_edges` survivor pattern |
| 6 | retrieval/router.py:19, :84 | `SemanticRouter`, first-match `_select` |
| 7 | retrieval/strategies/multi_path.py:48 | `MultiPathRetrieval` — the 9-step flagship |
| 7 | storage/vector_store.py:371, :406 | `search_entities`, `search_relationships` |
| 7 | reranking_strategies/cosine.py:18 | `CosineReranker` (top_k=15) |
| 8 | retrieval/cypher_generation.py:145, :162, :187 | `extract_cypher`, `_sanitize_cypher`, `validate_cypher` |

## Questions to answer in notes.md

1. The multi-path retriever issues fulltext, vector, MENTIONED_IN, and 2-hop chunk queries
   against one FalkorDB. Which of the four is the latency bottleneck at scale, and could
   they be fused into fewer Cypher round trips?
2. The lexical graph (chunk/document nodes + MENTIONED_IN) is built even with entity
   extraction disabled. What retrieval quality do you keep in that degenerate mode, and
   what breaks?
3. `_remap_entity_edges` re-points the loser's edges before deleting the node. What are the
   atomicity guarantees during that window, and how would you implement merge-with-remap as
   a single server-side operation in FalkorDB?
4. The resolution ladder runs exact → description-merge → semantic → LLM-verified. Which
   rung dominates wall-clock time on a large ingest, and where would caching or batching
   help most?
5. Routing is rule-based first-match with a default fallback. What query shapes fall
   through to the default today, and would an LLM router earn its latency cost for any of
   them?

## Done when

- [ ] You can sketch the fixed 9-step ingestion pipeline from memory, including which step
      is mandatory and which two run concurrently at the tail.
- [ ] You have traced one document through `IngestionPipeline.run` (pipeline.py:94) in the
      clone and watched the lexical graph land in FalkorDB.
- [ ] You can explain the four chunk-retrieval paths in `MultiPathRetrieval` and where the
      query embedding is computed and reused.
- [ ] You can state why the resolution ladder escalates to `_embedding_and_llm_merge` and
      how it relates to Zep/Graphiti's approach.
- [ ] You have answered the 5 questions above in notes.md.

## References

- Clone: `~/repos/GraphRAG-SDK` at commit `f42ab3d`; package under
  `graphrag_sdk/src/graphrag_sdk/`.
- [reading-hipporag.md](reading-hipporag.md) — the 2-step OpenIE that mirrors the SDK's
  NER-then-LLM extraction.
- [reading-zep-graphiti.md](reading-zep-graphiti.md) — the embedding-then-LLM entity
  resolution escalation the SDK's top rung reuses.
- [reading-graphrag-paper.md](reading-graphrag-paper.md) — Microsoft GraphRAG's
  community-summary global axis, which this SDK deliberately does not have.
