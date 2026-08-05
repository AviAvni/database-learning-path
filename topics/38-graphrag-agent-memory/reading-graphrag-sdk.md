# GraphRAG-SDK: the whole GraphRAG stack inside one FalkorDB round trip

Most GraphRAG stacks are Frankenstein architectures: a graph database for entities, a separate
vector database for embeddings, maybe an Elasticsearch for keyword search, and application glue
that pays a network round trip to each. FalkorDB's GraphRAG-SDK takes the opposite bet — the
knowledge graph, the vector indices, and the fulltext indices all live inside FalkorDB itself,
so entity discovery, vector search, and graph expansion are queries against one database. This
guide walks the pinned source `FalkorDB/GraphRAG-SDK@f42ab3d`, package root
`graphrag_sdk/src/graphrag_sdk/`, from the fixed ingestion pipeline to the multi-path retriever.

Every `file:line` below is quoted from that pinned commit; the anchors resolve against it
exactly.

## The problem in one sentence

**How do you turn raw documents into a queryable knowledge graph — and answer questions
over it — without bolting a separate vector store onto your graph database?**

The SDK's answer: make the graph store, the vector store, and the fulltext index three
views of the same FalkorDB instance, then build a deterministic ingestion pipeline and a
multi-path retriever on top. For a FalkorDB core dev, this is your database being used as
the entire RAG substrate — every design choice here is a workload you serve.

## The concepts, step by step

### Step 1 — One database, three index types

> **In:** a FalkorDB connection.
> **Out:** one instance exposing a property graph, vector indices, and a fulltext index —
> the substrate every later step queries.

**FalkorDB** is a graph database that also serves vector and fulltext (RediSearch) indices, so
"three stores" become three index types in one engine. `GraphStore`
(`storage/graph_store.py:21`) wraps a `FalkorDBConnection` and handles node/relationship
upserts (`:56`, `:129`) and traversals (`:214`). `VectorStore` (`storage/vector_store.py`)
creates the vector index on entities (`:104`) and on relationships (`:108`) and the **fulltext**
index (`:133`) — all FalkorDB indices, not external services. The fulltext *creation* call is
`create_fulltext_index`, and it emits a native index-creation Cypher:

```python
# storage/vector_store.py:133–148 — fulltext INDEX CREATION (not the query path)
133      async def create_fulltext_index(
134          self,
135          label: str = "Chunk",
136          *properties: str,
137      ) -> None:
144          if not properties:
145              properties = ("text",)
148          query = f"CALL db.idx.fulltext.createNodeIndex('{safe_label}', {props})"
```

Do not confuse this with `fulltext_search` at `:485`, which is the *query* side that calls
`db.idx.fulltext.queryNodes`. Creation lives at `:133`; search lives at `:485`.

```
            +--------------------------- FalkorDB ---------------------------+
            |                                                                |
  query --> |  property graph        vector indices         fulltext index   |
            |  (entities, chunks,    (entity embeddings,    (chunk text,     |
            |   MENTIONED_IN,         relationship/RELATES   entity names)   |
            |   RELATES edges)        embeddings)                            |
            |                                                                |
            +----------------------------------------------------------------+
                     one round trip: discover + vector-search + expand

  vs. the usual stack:   app --> graph DB --> app --> vector DB --> app --> ES
```

The join happens where the data is, not in the application tier.

### Step 2 — A fixed 9-step ingestion pipeline

> **In:** raw documents.
> **Out:** a populated FalkorDB graph — the corpus every retrieval step reads.

`IngestionPipeline` (`ingestion/pipeline.py:35`, `run` at `:94`) hard-codes the step order — it
is **not a configurable DAG**. The sequence: Load → Chunk → lexical graph → Extract → quality
filter (`:329`) → Prune (`:282`) → Resolve → Write → then Mentions and Index run **concurrently**
via `asyncio.gather` (`:175`).

```
 Load -> Chunk -> LexicalGraph -> Extract -> QualityFilter -> Prune -> Resolve -> Write
                  (MANDATORY)                                                      |
                                                                          asyncio.gather
                                                                          /            \
                                                                     Mentions        Index
```

A **lexical graph** here means chunk and document nodes joined by `MENTIONED_IN` edges — the
plain text-retrieval layer, separate from the extracted entity graph. It is mandatory: those
nodes and edges are always built, even when entity extraction is disabled. You always get a
retrievable corpus graph; entities are the optional enrichment, not the foundation.

### Step 3 — Two-step extraction: local NER first, LLM second

> **In:** text chunks from Step 2.
> **Out:** entities and relations, merged across chunks — the raw material Step 4 resolves.

`GraphExtraction` (`ingestion/extraction_strategies/graph_extraction.py:89`) splits extraction
into a pluggable **NER** (named-entity recognition) pass and an LLM pass — its own docstring
states the contract:

```python
# ingestion/extraction_strategies/graph_extraction.py:89–98 — the 2-step contract
89   class GraphExtraction(ExtractionStrategy):
90       """Composable 2-step extraction with pluggable entity NER.
92       **Step 1** — Entity extraction via a pluggable ``EntityExtractor``.
93       Default: ``GLiNERExtractor`` (local, no API calls).
96       **Step 2** — LLM verification + relationship extraction. The LLM
97       receives the pre-extracted entities and original text, verifies
98       entities, and extracts relationships.
```

This is the same shape as HippoRAG's two-step OpenIE (see
[reading-hipporag.md](reading-hipporag.md)): a cheap recall-oriented first pass (here a *local*
GLiNER model, no API call), an expensive precision-oriented second pass. Two details worth
reading closely. First, **budget awareness**: `ctx.budget_exceeded` is checked at `:148`, so
extraction degrades gracefully when the token budget runs out instead of failing the pipeline.
Second, **cross-chunk merging**: `_aggregate_entities` (`:438`) and `_aggregate_relations`
(`:479`) fold duplicates found in different chunks before resolution even starts.

### Step 4 — A ladder of resolution strategies, cheap to expensive

> **In:** the aggregated entities from Step 3.
> **Out:** merged entities — fewer, canonical nodes for Step 5 to dedup and write.

**Entity resolution** decides when two mentions are the same entity. `ingestion/
resolution_strategies/` is an escalation ladder — each rung costs more and catches more:

```
  cost
   ^   llm_verified_resolution.py:75    embedding candidates -> LLM-verified merge (:192)
   |   semantic_resolution.py:33        embedding similarity, _fuzzy_merge (:122)
   |   description_merge.py:30          merge descriptions of same-name entities
   |   exact_match.py:21                string equality
   +------------------------------------------------------------------> recall
```

The top rung — `_embedding_and_llm_merge` (`ingestion/resolution_strategies/
llm_verified_resolution.py:192`, inside `LLMVerifiedResolution` at `:75`) — searches for merge
candidates by embedding, then asks an LLM to confirm each merge. That is precisely the
escalation pattern Zep/Graphiti uses for entity resolution (see
[reading-zep-graphiti.md](reading-zep-graphiti.md)); it recurs in every serious KG-construction
system because embeddings alone over-merge (near-duplicate embeddings for genuinely distinct
entities).

### Step 5 — Deduplication with the survivor pattern

> **In:** resolved entity groups from Step 4.
> **Out:** a graph with duplicates removed and no dangling edges — Step 2's Write made safe.

`EntityDeduplicator` (`storage/deduplicator.py`) runs exact dedup (`:74`) and fuzzy dedup
(`:115`). The load-bearing graph mechanic is the **survivor pattern**: re-point the loser's
edges onto the survivor *before* deleting the loser, and only delete if the remap succeeded.

```python
# storage/deduplicator.py:97–105 — remap-then-delete, guarded
97               for dup in duplicates:
98                   if not await self._remap_entity_edges(dup["id"], survivor["id"]):
99                       logger.warning(f"Skipping deletion of {dup['id']} — edge remap incomplete")
100                      continue
101                  try:
102                      await self._graph.query_raw(
103                          "MATCH (e:__Entity__ {id: $dup_id}) DETACH DELETE e",
104                          {"dup_id": dup["id"]},
105                      )
```

`_remap_entity_edges` (`:228`) re-points the duplicate's `RELATES` and `MENTIONED_IN` edges to
the survivor and returns `False` on any failure; the guard at `:98–100` then *skips* the
`DETACH DELETE`. Miss that ordering (or the guard) and a merge silently drops relationships —
exactly the invariant a graph database's users depend on getting right.

### Step 6 — Rule-based routing: no LLM in the hot path

> **In:** a query string.
> **Out:** the chosen retrieval strategy — selected without an LLM call.

`SemanticRouter` (`retrieval/router.py:19`) picks a retrieval strategy with plain rules —
first match wins, default fallback. Its own code says so; it is **not** an embedding-driven
router:

```python
# retrieval/router.py:19–23 and 84–98 — rule-based FIRST-MATCH, not embeddings
19   class SemanticRouter:
20       """Route queries to the best retrieval strategy based on intent.
22       In v1, this is a simple rule-based router. Users register strategies
23       with keywords or conditions, and the router picks the best match.
84       def _select(self, query: str) -> tuple[str, RetrievalStrategy]:
90           for name, (strategy, condition) in self._strategies.items():
92               if callable(condition) and condition(query):
93                   return name, strategy
98           return "default", self._default
```

No LLM call means routing costs microseconds, is deterministic, and is debuggable. The
supporting classifiers are rule-based too: `is_enumeration_query`
(`retrieval/strategies/entity_discovery.py:20`) and `detect_question_type`
(`retrieval/strategies/result_assembly.py:105`).

### Step 7 — Multi-path retrieval: four chunk paths, one reranker

> **In:** a routed query (from Step 6) and the FalkorDB graph.
> **Out:** an assembled context block for the LLM.

`MultiPathRetrieval` (`retrieval/strategies/multi_path.py:48`) is the flagship, itself a 9-step
sequence — read it from the docstring:

```python
# retrieval/strategies/multi_path.py:48–63 — the retrieval pipeline docstring
48   class MultiPathRetrieval(RetrievalStrategy):
52       Retrieval pipeline:
53         1. Keyword extraction (stopword filter + LLM proper nouns)
54         2. Embed question only (single API call)
55         3. RELATES edge vector search -> fact strings + entity entry points
56         4. Entity discovery (2 paths: Cypher CONTAINS, fulltext)
58         5. Relationship expansion (1-hop + 2-hop from top entities)
59         6. Chunk retrieval (4 paths: fulltext, vector, MENTIONED_IN, 2-hop)
61         8. Cosine reranking of all candidate chunks
62         9. Context assembly into structured sections (...)
```

The **four chunk paths** (step 6 of the docstring) — fulltext, vector on the chunk index,
`MENTIONED_IN` from found entities, and 2-hop expansion chunks — are all queries against the
same FalkorDB, which is Step 1's thesis cashing out:

```
                     query (embedded ONCE at docstring step 2)
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

Defaults (the `__init__` signature, `multi_path.py:163–167`): `chunk_top_k=15`,
`max_entities=30`, `max_relationships=20`, `rel_top_k=15`, `keyword_limit=10`. The query
embedding is computed once (docstring step 2) and reused across the vector paths. The reranker is `CosineReranker`
(`retrieval/reranking_strategies/cosine.py:18`, `top_k` default 15). Note what is absent: there
is no community-summary layer, no global axis in the Microsoft GraphRAG sense (see
[reading-graphrag-paper.md](reading-graphrag-paper.md)). The SDK's retrieval is the
local/associative kind — start from entities, expand, rerank.

### Step 8 — Text-to-Cypher exists, but behind a guard rail

> **In:** a natural-language query, when `enable_cypher=True`.
> **Out:** a validated read-only Cypher query — or rejection before execution.

`enable_cypher=False` by default: the text-to-Cypher path is experimental and off. When enabled,
`retrieval/strategies/cypher_generation.py` runs generated queries through three guards before
execution:

```python
# retrieval/strategies/cypher_generation.py:145,162,187 — extract, sanitize, validate
145  def extract_cypher(text: str) -> str:
146      """Extract Cypher from LLM response, handling markdown code blocks."""
162  def _sanitize_cypher(cypher: str) -> str:
178      if not re.search(r"\bLIMIT\b", cypher, re.IGNORECASE):
179          cypher = cypher.rstrip().rstrip(";") + "\nLIMIT 25"
187  def validate_cypher(cypher: str) -> list[str]:
190      """Uses an allowlist approach: the query must start with a read-only
191      keyword, and dangerous constructs are explicitly rejected."""
```

LLM-written Cypher hitting a production graph gets extracted from the markdown, sanitized (a
`LIMIT 25` is injected if missing, to prevent runaway scans; `:178`), and validated against a
read-only allowlist (`:190`) before execution — a sensible posture for anyone who has watched an
LLM hallucinate a Cartesian product.

## Where each step lives in the code

Paths relative to `graphrag_sdk/src/graphrag_sdk/`, at `FalkorDB/GraphRAG-SDK@f42ab3d`.

| Step | Anchor (file:line) | What to see |
|---|---|---|
| 1 | storage/graph_store.py:21 | `GraphStore` wrapping FalkorDBConnection |
| 1 | storage/graph_store.py:56, :129, :214 | `upsert_nodes`, `upsert_relationships`, `get_connected_entities` |
| 1 | storage/vector_store.py:104, :108, :133 | entity/relationship vector indices + `create_fulltext_index` |
| 1 | storage/vector_store.py:169, :244, :485 | `index_chunks`, `embed_relationships`, `fulltext_search` (query, not creation) |
| 2 | ingestion/pipeline.py:35, :94 | `IngestionPipeline` and `run` — the fixed 9 steps |
| 2 | ingestion/pipeline.py:329, :282, :175 | quality filter, prune, concurrent Mentions+Index via `asyncio.gather` |
| 3 | ingestion/extraction_strategies/graph_extraction.py:89 | `GraphExtraction` — NER pass then LLM pass |
| 3 | ingestion/extraction_strategies/graph_extraction.py:148 | `ctx.budget_exceeded` graceful degradation |
| 3 | ingestion/extraction_strategies/graph_extraction.py:438, :479 | `_aggregate_entities`, `_aggregate_relations` |
| 4 | ingestion/resolution_strategies/exact_match.py:21 | cheapest rung: string equality |
| 4 | ingestion/resolution_strategies/description_merge.py:30 | same-name description merge |
| 4 | ingestion/resolution_strategies/semantic_resolution.py:33, :122 | embedding similarity, `_fuzzy_merge` |
| 4 | ingestion/resolution_strategies/llm_verified_resolution.py:75, :192 | `LLMVerifiedResolution`, `_embedding_and_llm_merge` |
| 5 | storage/deduplicator.py:74, :115, :228 | exact/fuzzy dedup, `_remap_entity_edges` survivor pattern |
| 6 | retrieval/router.py:19, :84 | `SemanticRouter`, first-match `_select` |
| 6 | retrieval/strategies/entity_discovery.py:20 | `is_enumeration_query` (rule-based) |
| 6 | retrieval/strategies/result_assembly.py:105 | `detect_question_type` (rule-based) |
| 7 | retrieval/strategies/multi_path.py:48 | `MultiPathRetrieval` — the 9-step flagship |
| 7 | storage/vector_store.py:371, :406 | `search_entities`, `search_relationships` |
| 7 | retrieval/reranking_strategies/cosine.py:18 | `CosineReranker` (top_k=15) |
| 8 | retrieval/strategies/cypher_generation.py:145, :162, :187 | `extract_cypher`, `_sanitize_cypher`, `validate_cypher` |

## Questions to answer in notes.md

1. The multi-path retriever issues fulltext, vector, `MENTIONED_IN`, and 2-hop chunk queries
   against one FalkorDB. Which of the four is the latency bottleneck at scale, and could they be
   fused into fewer Cypher round trips?
2. The lexical graph (chunk/document nodes + `MENTIONED_IN`) is built even with entity
   extraction disabled. What retrieval quality do you keep in that degenerate mode, and what
   breaks?
3. `_remap_entity_edges` re-points the loser's edges before deleting the node, and the delete is
   skipped if the remap fails. What atomicity gap remains between the remap and the delete, and
   how would you make merge-with-remap a single server-side FalkorDB operation?
4. The resolution ladder runs exact → description-merge → semantic → LLM-verified. Which rung
   dominates wall-clock time on a large ingest, and where would caching or batching help most?
5. Routing is rule-based first-match with a default fallback. What query shapes fall through to
   the default today, and would an LLM router earn its latency cost for any of them?

## Done when

Answer each before unfolding it.

- [ ] You can sketch the fixed 9-step ingestion pipeline from memory, including which step is mandatory and which two run concurrently at the tail.

  <details><summary>Answer</summary>

  Load → Chunk → **lexical graph (mandatory)** → Extract → Quality filter (`pipeline.py:329`) →
  Prune (`:282`) → Resolve → Write → then **Mentions and Index run concurrently** via
  `asyncio.gather` (`:175`). The order is hard-coded in `IngestionPipeline.run` (`:94`), not a
  configurable DAG. The lexical-graph step (chunk/document nodes + `MENTIONED_IN`) always runs,
  so even with entity extraction off you get a retrievable corpus graph.

  </details>

- [ ] You have traced one document through `IngestionPipeline.run` (pipeline.py:94) and watched the lexical graph land in FalkorDB.

  <details><summary>Answer</summary>

  Following `run` (`ingestion/pipeline.py:94`): the document is loaded and chunked, the lexical
  graph writes chunk/document nodes and `MENTIONED_IN` edges via `GraphStore.upsert_nodes`
  (`graph_store.py:56`) and `upsert_relationships` (`:129`), extraction (Step 3) optionally adds
  entities/relations, quality filter (`:329`) and prune (`:282`) trim them, resolution merges
  duplicates, Write persists, and finally Mentions + Index run together (`:175`). The chunk and
  document nodes are queryable in FalkorDB regardless of whether extraction ran.

  </details>

- [ ] You can explain the four chunk-retrieval paths in `MultiPathRetrieval` and where the query embedding is computed and reused.

  <details><summary>Answer</summary>

  From the docstring (`multi_path.py:48–63`): the query is embedded **once** (docstring step 2,
  "Embed question only (single API call)") and reused across the vector paths. Chunk retrieval
  (docstring step 6) runs **four paths**: **fulltext** (RediSearch), **vector** on the chunk
  index, **`MENTIONED_IN`** from discovered entities, and **2-hop expansion** chunks. All four
  are queries against the same FalkorDB. Candidates are then cosine-reranked (`CosineReranker`,
  `retrieval/reranking_strategies/cosine.py:18`, top_k=15) and assembled into a context block.

  </details>

- [ ] You can state why the resolution ladder escalates to `_embedding_and_llm_merge` and how it relates to Zep/Graphiti's approach.

  <details><summary>Answer</summary>

  The ladder runs exact-match (`exact_match.py:21`) → description-merge (`description_merge.py:30`)
  → semantic/embedding (`semantic_resolution.py:33`) → LLM-verified
  (`llm_verified_resolution.py:75`). It escalates to `_embedding_and_llm_merge` (`:192`) because
  **embeddings alone over-merge**: near-identical vectors for genuinely distinct entities would
  collapse them, so the LLM is used as a precision gate on the embedding-found candidates. That
  is exactly Zep/Graphiti's entity resolution (embedding candidate search, then LLM verify — see
  [reading-zep-graphiti.md](reading-zep-graphiti.md)); the pattern recurs across serious KG
  builders.

  </details>

- [ ] You have answered the 5 questions above in notes.md.

  <details><summary>Answer</summary>

  notes.md records all five with code anchors: the four-path latency bottleneck and whether the
  paths can be fused into fewer Cypher round trips (`multi_path.py:48`); what survives in the
  extraction-disabled lexical-graph mode and what breaks (Step 2); the residual atomicity gap
  between `_remap_entity_edges` and the guarded `DETACH DELETE` (`deduplicator.py:98–105`);
  which resolution rung dominates ingest wall-clock and where batching/caching helps (Step 4);
  and which query shapes fall through to the router default and whether an LLM router would pay
  for itself (`router.py:84–98`).

  </details>

## References

- Pinned source: `FalkorDB/GraphRAG-SDK@f42ab3d`; package under
  `graphrag_sdk/src/graphrag_sdk/`. Every `file:line` in this chapter resolves against that
  commit (verify with `tools/pinned-source.py show GraphRAG-SDK <path> -r A:B`).
- [reading-hipporag.md](reading-hipporag.md) — the two-step OpenIE that mirrors the SDK's
  NER-then-LLM extraction.
- [reading-zep-graphiti.md](reading-zep-graphiti.md) — the embedding-then-LLM entity resolution
  escalation the SDK's top rung reuses.
- [reading-graphrag-paper.md](reading-graphrag-paper.md) — Microsoft GraphRAG's community-summary
  global axis, which this SDK deliberately does not have.
- This topic's measured headline is in [FINDINGS.md](../../FINDINGS.md) row 38.
