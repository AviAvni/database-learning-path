# GraphRAG-SDK: a RAG pipeline read as a workload spec

Your own SDK, re-read as a database workload spec. Every Python line here
is a feature request against FalkorDB: what it does client-side in
asyncio is what M25 should evaluate doing engine-side. Layout:
`src/graphrag_sdk/{ingestion, storage, retrieval, core}`. This chapter
builds the workload step by step — what RAG is, the ingestion
dataflow, the storage contract the SDK demands, the retrieval joins it
hand-rolls, and the four systems smells that name M25's work.

## The problem in one sentence

Answering a question over private documents means finding the right
evidence before the LLM ever runs — and this SDK does it with **k+1
round trips per question** (one vector search plus one graph
expansion per hit) and client-side score fusion in Python, all of
which is work a database with the right query surface would do in one
plan.

## The concepts, step by step

### Step 1 — RAG, and why a graph gets involved

> **In:** a private document corpus and a natural-language question.
> **Out:** the evidence the LLM needs — fetched by vector similarity and, in
> GraphRAG, by explicit graph structure. Step 2 is the pipeline that builds
> and reads both.

**RAG** (retrieval-augmented generation) answers questions by
retrieving relevant evidence from a private corpus and stuffing it
into an LLM's prompt — the LLM supplies fluency, retrieval supplies
facts it was never trained on. The standard pipeline splits documents
into **chunks** (passages of a few hundred tokens), computes an
**embedding** per chunk (a dense vector whose geometry encodes
meaning — reading-node2vec.md Step 1, but for text), and at question
time runs **ANN search** (approximate nearest neighbor — find the
k closest vectors fast, topic 14's index) to fetch the top-k chunks.
GraphRAG's addition: also extract *entities and relations* from the
text into a graph, so retrieval can follow explicit structure ("who
supplied whom") instead of only fuzzy similarity — questions whose
answers span multiple documents need the join, not just the nearest
neighbors.

### Step 2 — the pipeline as a dataflow

> **In:** the corpus (ingestion) and a question (query) from Step 1.
> **Out:** two indexes written once and read together — a graph store and a
> vector store on one FalkorDB instance. Step 3 is the storage contract they
> imply.

The SDK's whole shape is one ingestion path that writes two indexes,
and one query path that reads them both:

```mermaid
flowchart LR
    D["docs"] --> C["chunking"] --> X["LLM entity/relation<br/>extraction"] --> R["resolution<br/>(dedup entities)"]
    R --> G["graph_store:<br/>nodes + RELATES edges"]
    R --> V["vector_store:<br/>embed chunks/entities/rels"]
    Q["question"] --> RT["SemanticRouter<br/>(router.py:19)"] --> S["strategy"]
    S --> V2["ANN: db.idx.vector.queryNodes"] --> E["Cypher expansion"] --> A["assembly -> LLM"]
    G -.-> E
    V -.-> V2
```

Ingestion is LLM-heavy (extraction and resolution are model calls);
querying is database-heavy (ANN + Cypher). The dataflow's key
property for a database person: the graph store and the vector store
are the SAME FalkorDB instance — the split into "two stores" is a
client-side fiction, which is exactly why the joins in Step 4 hurt.

### Step 3 — the storage contract: what the SDK asks the database for

> **In:** the two-index dataflow from Step 2.
> **Out:** the concrete set of database features the SDK depends on — three
> index types queried through Cypher, plus an external write path. Step 4 is
> the joins layered on top.

`storage/vector_store.py` is the SDK's entire database contract, and
reading it tells you which features carry the workload:

| anchor | what |
|---|---|
| `:344` | `CALL db.idx.vector.queryNodes('{safe_label}', 'embedding', $top_k, vecf32($vector))` — chunk ANN |
| `:378` | same over `__Entity__` — entity ANN |
| `:426` | `queryRelationships('RELATES', ...)` — EDGE vectors, with a Cypher cosine-scan fallback (`vecf32.distance.cosine`, :454-458) if unsupported |
| `:219,:234,:312` | `SET c.embedding = vecf32($vector)` — embeddings computed OUTSIDE, written back as properties |
| `:133` | `create_fulltext_index` too — hybrid = vector + FT + graph, three indexes on one store |

Note the asymmetry: the read path is database-native (three index
types queried through Cypher), but the WRITE path — embedding
computation — is an external API call per chunk/entity, round-tripped
to OpenAI and written back as a property. M25's thesis: with
node2vec/GCN kernels in the engine, *structural* embeddings never
leave the database — only text embeddings need the round-trip.

### Step 4 — retrieval strategies: joins, hand-rolled in the client

> **In:** the storage contract from Step 3 (ANN + Cypher on one store).
> **Out:** each retrieval strategy as a query plan the Python client executes
> by hand — the k+1 round trips Step 6 files as smell #1.

Each retrieval strategy is a query plan executed by Python instead of
the database:

- `relationship_expansion.py:12` `expand_relationships`: ANN hits →
  `MATCH (a:__Entity__ {id: eid})-[r:RELATES]->(b)` (:35) and a 2-hop
  variant (:62). This is a client-side JOIN between the vector index
  and the graph: k queries where one Cypher query with a vector
  predicate should do — the exact hybrid query M25's capstone must
  serve in ONE plan.
- `multi_path.py:48` `MultiPathRetrieval` runs a 9-phase pipeline
  (`_execute`, :182) that fans out across chunk, entity and edge indexes
  plus Text-to-Cypher, then reranks with a client-side `_cosine_sim`
  (:362). Its explicit concurrency is one `asyncio.gather` (:198) running
  the RELATES-edge vector search and the Text-to-Cypher retrieval in
  parallel; the entity-discovery and chunk-retrieval phases are separate
  awaited steps, not a single three-way gather. Either way it is a
  scatter-gather union of several indexes with score fusion done in Python
  — compare topic 23's WAND: score fusion is what the engine's top-k
  machinery is FOR.

The cost is structural, not incidental: every strategy pays k+1 round
trips and recomputes distances the index already knew. The asyncio
sophistication is compensation for a missing query surface.

### Step 5 — the router: a planner with no cost model

> **In:** the several retrieval strategies from Step 4.
> **Out:** a per-question strategy choice — made by first-matching predicate,
> with no statistics. Step 6 collects this and the other systems smells.

`router.py:19` `SemanticRouter` picks a retrieval strategy per
question, but read what the pinned version actually does (rule 6): despite
the name, it is **not** embedding-driven. Strategies register a
`condition(query) -> bool` callable (`register`, :46 — the docstring's
example is `lambda q: "how" in q.lower()`), and `_select` (:84) returns the
first strategy whose predicate fires, falling back to a default (:98). The
class docstring says so outright — "In v1, this is a simple rule-based
router." So it is a query PLANNER driven by keyword rules, not by
cardinality (topic 9): the structure is right (multiple plans, a chooser),
but everything topic 9 built is missing — cost per plan, selectivity
estimates, feedback from execution. Question 4 asks what statistic would
turn "graph expansion vs pure ANN" into a costed choice — the router names
M25's planner-shaped hole.

### Step 6 — the four systems smells: M25's worklist

> **In:** everything Steps 3–5 found the client doing by hand.
> **Out:** four named engine deficiencies, each mapped to a topic already
> covered — the worklist M25 closes.

Reading the whole SDK as a bug report against the engine yields four
named deficiencies:

1. **k+1 round trips**: ANN then per-hit expansion — push the join down.
2. **Client-side rerank**: cosine in Python over returned vectors — the
   index already computed distances; return them.
3. **Embedding writes are not transactional** with the entities they
   describe (batch SET after ingest) — staleness window with no
   read-your-writes story (topic 8).
4. **No incremental re-embed**: edit a chunk → re-embed everything or
   drift silently (topic 27's IVM question, in RAG costume).

Each smell maps to a topic already covered — join pushdown (10),
top-k machinery (23), transactional visibility (8), incremental view
maintenance (27) — which is the point: a RAG SDK is a database
workload wearing an application costume, and M25 closes the loop by
computing embeddings with the engine's own SpMM, storing into the M14
vector index, and answering hybrid queries without leaving the
database.

## Where each step lives in the code

- **Step 2 — ingestion**: `src/graphrag_sdk/ingestion/` (chunking,
  extraction, resolution) and `core/` — skim; the LLM calls are the
  content, the orchestration is asyncio plumbing.
- **Step 3 — the contract**: `storage/vector_store.py` — the anchor
  table above; read `:344` and `:219` first, they are the read and
  write halves of the contract.
- **Step 4 — the joins**:
  `retrieval/strategies/relationship_expansion.py` (:12, :35, :62)
  and `retrieval/strategies/multi_path.py` (:48 class, :182 `_execute`,
  :198 `asyncio.gather`, :362 `_cosine_sim`).
- **Step 5 — the router**: `retrieval/router.py:19` (`_select` :84).
- Navigation advice: read each file as a feature request against the
  engine, not as Python to review — the question is never "is this
  code good" but "which missing engine feature made this code
  necessary".

## Questions (answer in notes.md)

1. Write the ONE Cypher query that replaces expand_relationships'
   ANN + k MATCHes. What must the planner know to not execute it as
   k+1 lookups anyway?
2. multi_path fuses three scores client-side — design the engine-side
   fusion: is it WAND-able (topic 23) given vector distances aren't
   monotone doc-at-a-time?
3. Which of the four smells does `SET c.embedding = vecf32(...)` inside
   the SAME transaction as entity creation fix, and what does it cost the
   ingest pipeline's throughput?
4. The router is a planner with no cost model. What statistic would make
   "graph expansion vs pure ANN" a COSTED choice (selectivity of the
   pattern? recall@k of the index?)?
5. M25 acceptance test: pattern + similarity in one query, verified
   against this SDK's answers on the same data — sketch it.

## Done when

Answer each before unfolding it.

- [ ] You can draw the pipeline as a dataflow and name what the SDK asks the database for.
  <details><summary>Answer</summary>

  Ingestion writes two indexes on one FalkorDB store — a property graph
  (`__Entity__` nodes, `RELATES` edges) and vector indexes over chunks,
  entities and edges (`vector_store.py` `:344/:378/:426`), plus a
  full-text index (`:133`). Query reads both together. What the SDK asks
  the database for: ANN over node/edge vectors, Cypher pattern matching,
  full-text search, and a write path that stores externally-computed
  embeddings back as `vecf32` properties (`:219/:234/:312`). Three index
  types, one store.
  </details>
- [ ] You can write the single Cypher query that replaces the client-side relationship expansion.
  <details><summary>Answer</summary>

  `expand_relationships` (`relationship_expansion.py:12`) does ANN, then
  one `MATCH (a:__Entity__ {id: eid})-[r:RELATES]->(b)` per hit (`:35`).
  The one-query form pushes the vector predicate INTO the pattern:
  `CALL db.idx.vector.queryNodes('__Entity__','embedding',$k,vecf32($v))
  YIELD node MATCH (node)-[r:RELATES]->(b) RETURN node, r, b`. For the
  planner not to run it as k+1 lookups anyway it must treat the vector
  index as a leaf operator feeding the expand, not a black box called k
  times — i.e. a join order that keeps the ANN result set streaming into
  the pattern match.
  </details>
- [ ] You can name all four systems smells and say which one `SET c.embedding = vecf32(...)` inside a loop is.
  <details><summary>Answer</summary>

  The four: (1) k+1 round trips, (2) client-side rerank, (3)
  non-transactional embedding writes, (4) no incremental re-embed.
  `SET c.embedding = vecf32(...)` after ingest is smell #3: the embedding
  write is a separate batch from entity creation (`vector_store.py`
  `:219/:234/:312`), so there is a staleness window with no
  read-your-writes guarantee (topic 8).
  </details>
- [ ] You can say what statistic would give the router a cost model.
  <details><summary>Answer</summary>

  The router (`router.py:19`) is first-match predicate selection
  (`_select` `:84`), no statistics. To cost "graph expansion vs pure
  ANN" you need the pattern's selectivity (how many `RELATES` neighbours
  a matched entity expands to — expansion fan-out) and the ANN index's
  recall@k, so a plan that expands can be priced against one that does
  not. That is exactly topic 9's cardinality-estimation machinery, absent
  here.
  </details>
- [ ] You wrote answers to all five questions in notes.md, including the M25 acceptance test that puts pattern and similarity in one query.
  <details><summary>Answer</summary>

  The acceptance test: a single query that both matches a graph pattern
  and ranks by vector similarity — e.g. entities within 2 hops of a seed
  ranked by embedding distance to the question — executed as ONE engine
  plan (vector predicate pushed into the pattern match, distances
  returned by the index, no Python rerank), and verified to return the
  same answers this SDK produces by its k+1 round trips on the same data.
  </details>

## References

**Code**
- [GraphRAG-SDK](https://github.com/FalkorDB/GraphRAG-SDK)
  `src/graphrag_sdk/` — `storage/vector_store.py` (the DB contract),
  `retrieval/strategies/relationship_expansion.py`,
  `retrieval/strategies/multi_path.py`, `retrieval/router.py`; read
  each as a feature request against the engine
