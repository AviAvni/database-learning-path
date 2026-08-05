# Zep/Graphiti: agent memory as a bi-temporal knowledge graph

Zep (arXiv 2501.13956, Rasmussen et al.) is the paper behind Graphiti, an engine that turns an
agent's ever-growing chat history into an incrementally-built temporal knowledge graph. Instead
of stuffing the full conversation into the prompt or doing flat RAG over message chunks,
Graphiti extracts entities and facts as episodes arrive, stamps every edge with four timestamps
(valid time and transaction time), and answers queries with a hybrid search-rerank-construct
pipeline. For a graph-database developer, the interesting part is that this is **bi-temporal
versioning applied to graph edges** — version chains, time-travel reads, and
invalidation-instead-of-deletion, wearing an LLM costume. This repo's **topic 33 (temporal
graphs)** builds the same valid-time × transaction-time model from the storage side; read this
paper as that model shipped as a product.

Every number below is quoted with the section or table it comes from in arXiv 2501.13956.

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

> **In:** raw conversation data (messages, text, or JSON).
> **Out:** a three-layer graph — the substrate every later step reads and writes.

Graphiti layers three subgraphs in one graph, from raw to abstract (§2):

```
  +--------------------------------------------------+
  |  Community subgraph  (Gc, §2.3)                  |
  |  clusters of related entities + LLM summaries    |
  +------------------------^-------------------------+
                           | label propagation
  +------------------------+-------------------------+
  |  Semantic entity subgraph  (Gs, §2.2)            |
  |  entities + relations (facts), with embeddings   |
  +------------------------^-------------------------+
                           | LLM extraction
  +------------------------+-------------------------+
  |  Episode subgraph  (Ge, §2.1, non-lossy)         |
  |  raw messages, text, JSON — never rewritten      |
  +--------------------------------------------------+
```

An **episode** is one ingested data unit — a message, a text blob, or a JSON record (§2.1). The
episode layer is the **non-lossy** source of truth (the paper's word); entities and facts are
derived from it; communities summarize clusters of entities. Bidirectional **episodic edges**
(Ge) tie every derived fact back to the episode it came from, so "semantic artifacts can be
traced to their sources for citation or quotation" (§2.1). Think base table → index →
materialized view, with the episode log as the layer the others can always be rebuilt from.

### Step 2 — Extraction and entity resolution

> **In:** a new episode plus the last few messages as context (from Step 1's episode layer).
> **Out:** new/merged entity nodes and fact edges in the semantic layer — the things Steps 3–4
> timestamp and invalidate.

Entities are extracted per-episode with the last few messages as context, so **coreference**
("she", "the company" — resolving a pronoun or description to the entity it refers to) resolves
correctly (§2.2.1). Each entity gets an embedding and a summary; each edge (fact) carries a
relation name plus a fact string (§2.2.2). **Entity resolution** — deciding whether a new
mention is an entity the graph already knows — is a two-stage merge: an embedding candidate
search finds plausible existing nodes, then an **LLM verifies** whether the new mention is the
same entity before merging (§2.2.1, and the "is_duplicate" prompt in §6.1). This is fuzzy
upsert-by-similarity: the write path's dedup logic, with an LLM as the comparator.

### Step 3 — The bi-temporal model: two timelines, four timestamps

> **In:** an extracted fact edge from Step 2.
> **Out:** an edge stamped on two independent timelines — the state Step 4 mutates on
> contradiction and Step 6 filters on at query time.

Zep implements a **bi-temporal model** (§2.1): **timeline T** is *event time* — the chronological
ordering of events in the world — and **timeline T′** is *transaction time* — the order of Zep's
data ingestion (T′ "serves the traditional purpose of database auditing," §2.1). Every fact edge
carries **four timestamps** (§2.2): `t'_created, t'_expired ∈ T′` record when the fact was
created or invalidated *in the system*, while `t_valid, t_invalid ∈ T` record the range during
which the fact *held true in the world*.

```
  Timeline T  (event/valid time):     t_valid ......... t_invalid
  Timeline T' (transaction time):     t'_created ...... t'_expired

  edge: (Alice) -[WORKS_AT]-> (Acme)
        t_valid   = Jan   t_invalid  = Jun   "true in the world Jan..Jun"
        t'_created= Feb   t'_expired = Jul   "known to the system Feb..Jul"
```

The two timelines are independent: a fact can be learned long after it became true (T lags T′),
and superseded in the system long after it stopped being true. Relative mentions ("I started my
new job two weeks ago") are resolved against the episode's reference timestamp `t_ref` at
extraction time (§2.2), so the stored `t_valid` is an absolute datetime.

### Step 4 — Edge invalidation: contradiction as versioning

> **In:** a new fact edge from Step 3 and the semantically-related edges already in the graph.
> **Out:** old edges marked invalid (not deleted) — the version chain Step 6's as-of reads walk.

At ingest, an LLM compares each new fact against existing semantically-related edges. When a new
fact **temporally overlaps and contradicts** an old one, the old edge is *invalidated*, never
deleted. The precise contract (§2.2): the system sets the old edge's **`t_invalid` to the
`t_valid` of the invalidating edge**, and expires it on the transaction timeline.

```
  ingest: "Alice works at Beta" (valid from Jun)

  old edge  (Alice)-[WORKS_AT]->(Acme)
            t_invalid   := Jun   (:= the NEW fact's t_valid, per §2.2)
            t'_expired  := now
            edge KEPT — audit trail, nothing deleted

  new edge  (Alice)-[WORKS_AT]->(Beta)
            t_valid     := Jun
            t'_created  := now
```

"Graphiti consistently prioritizes new information when determining edge invalidation" (§2.2) —
new information wins by default. Because nothing is deleted, both temporal questions are one
predicate away. Worked as an **as-of** filter — "what did we believe was true in March, using
only what the system knew by end-of-June?":

```
  keep edge e iff
    e.t_valid    <=  Mar-31   <  e.t_invalid      (event-time slice: true in March)
  AND
    e.t'_created <=  Jun-30   <  e.t'_expired      (as-known-at: system's June view)

  Alice/Acme edge:  t_valid=Jan <= Mar <  t_invalid=Jun   -> TRUE  (event)
                    t'_created=Feb <= Jun < t'_expired=Jul -> TRUE  (transaction)
                    => returned: in March the system's June-view had Alice at Acme
```

Drop the second predicate and you get "what was true in March" regardless of when learned; drop
the first and you get "what did we know as of June." This is a version chain: invalidation
writes tombstone timestamps instead of removing tuples, and as-of queries are time-travel reads
over that chain — exactly topic 33's model.

### Step 5 — Communities via dynamic label propagation

> **In:** the semantic entity subgraph from Step 2.
> **Out:** community nodes with summaries (Gc) — a coarse retrieval target and one φ signal in
> Step 6.

Communities cluster related entities, each with an LLM-written summary. Graphiti uses **label
propagation** rather than **Leiden** (the algorithm GraphRAG uses) for one engineering reason
(§2.3): label propagation has a cheap **dynamic extension**. When a new node arrives, it adopts
"the community held by the **plurality** of its neighbors" (§2.3 — plurality, i.e. the most
common neighbor label, not necessarily a majority), then updates that community's summary —
full recomputation is postponed rather than triggered per write. The paper is candid that "the
resulting communities gradually diverge from those that would be generated by a complete label
propagation" (§2.3): that is the incremental-maintenance trade every streaming system makes —
accept slightly stale partitions in exchange for O(degree) update cost.

### Step 6 — Retrieval: the φ → ρ → χ funnel

> **In:** a text query α and the graph built by Steps 1–5.
> **Out:** a compact context string β for the agent's prompt.

Zep's search API is a function `f(α) = χ(ρ(φ(α))) = β` (§3): a query string in, a formatted
context string out. Three phases:

```
  query α
    |
    v
  φ  search (§3.1, three functions run in parallel):
       φ_cos  : cosine similarity on embeddings
       φ_bm25 : Okapi BM25 full-text (Neo4j/Lucene)
       φ_bfs  : breadth-first search over n-hops, SEEDED from
                recently-mentioned nodes
    |  returns a 3-tuple: (semantic edges, entity nodes, community nodes)
    v
  ρ  rerank (§3.2):  RRF (fusion) · MMR (diversity)
                     · episode-mentions reranker (frequency)
                     · node-distance reranker (locality to a centroid)
                     · cross-encoder (precision, highest cost)
    |
    v
  χ  construct (§3):  for each edge return fact + t_valid,t_invalid;
                      for each entity the name + summary; for each
                      community the summary -> context string β
```

φ casts a wide net across three signal types for **recall**; ρ fuses and reorders for
**precision** (RRF fuses the ranked lists, MMR trades relevance for diversity, the two graph
rerankers add locality, the cross-encoder is the most accurate and most expensive); χ serializes
the survivors — crucially, it emits each edge's **`t_valid`/`t_invalid`** alongside the fact
(§3), so the temporal model reaches the prompt. It is a query executor: scan operators feeding a
rank-merge feeding a projection.

### Step 7 — Results: accuracy up, latency way down

> **In:** the full system from Steps 1–6.
> **Out:** the measured accuracy/latency evidence Step 8 reads architecturally.

On **DMR** (Deep Memory Retrieval, MemGPT's benchmark), Zep scores **94.8% vs MemGPT's 93.4%**,
run on **gpt-4-turbo** for comparability (§4.2). The stronger evidence is **LongMemEval** with
gpt-4o (§4.3, **Table 2**): accuracy **60.2% → 71.2%** and response latency **28.9 s → 2.58 s**
— because the prompt shrinks from **≈ 115k tokens** (full conversation) to **≈ 1.6k tokens**
(retrieved facts). The abstract frames the accuracy gain as "up to 18.5%." Per-category (§4.3,
**Table 3**, gpt-4o, Zep vs full-context): **single-session-preference +184%** (20.0% → 56.7%),
**temporal-reasoning +38.4%** (45.1% → 62.4%), **multi-session +30.7%**. One honest regression:
**single-session-assistant −17.7%** (94.6% → 80.4%) — when the answer needs verbatim recall of
one recent session, full context beats retrieval.

### Step 8 — The database-internals reading

> **In:** everything above.
> **Out:** the one-sentence mental model to carry to topic 33.

Strip the LLM machinery and Graphiti is **bi-temporal versioning on a graph**. Invalidation
instead of deletion is a version chain; `t'_expired` is a tombstone; as-of queries are snapshot
reads; the episode layer is the WAL-like non-lossy log the derived layers can always be rebuilt
from. Topic 33 in this path (temporal graphs) covers the same valid-time × transaction-time
model from the storage side — this paper is that model deployed as a product, with the
extraction and retrieval stages bolted on where a database would have parsers and planners.

## How to read the paper (with the concepts in hand)

1. Read the abstract and §1 for the problem framing (Step 1's motivation): why context windows
   and flat RAG fail for agent memory.
2. Read §2 architecture — episodes (§2.1), entities/facts (§2.2), communities (§2.3) — and map
   each to the stack diagram in Step 1. Note the label-propagation choice and its plurality
   dynamic extension (Step 5).
3. Slow down at the bi-temporal passages: the timeline T/T′ definition in §2.1 and the
   four-timestamp + invalidation contract in §2.2. Draw the two timelines yourself and confirm
   they match Steps 3–4. This is the core of the paper.
4. Read the extraction and entity-resolution passages (§2.2.1) with Step 2 in hand; then the
   edge-invalidation contract in §2.2 against Step 4 — check that old edges are kept, not
   deleted, and that `t_invalid := t_valid` of the invalidating edge.
5. Read §3 retrieval, mapping each named component (RRF, MMR, episode-mentions, node-distance,
   cross-encoder, BFS) into the φ/ρ/χ funnel of Step 6, and note that χ emits `t_valid`/
   `t_invalid`.
6. Finish with §4: DMR (§4.2), then LongMemEval per-category numbers (§4.3, Tables 2–3). Find
   the token-count explanation of the latency drop, and the single-session-assistant regression
   — ask yourself why retrieval loses there.
7. Then run the companion experiment (see References) and compare its four-timestamp contract to
   the paper's §2.2.

## Questions to answer in notes.md

1. For each of the four timestamps (t_valid, t_invalid, t'_created, t'_expired), which timeline
   does it live on, and who sets it — the world, the LLM, or the ingest clock?
2. When a new fact contradicts an old edge, exactly which timestamps change on the old edge and
   why is the edge kept rather than deleted? What queries would break if it were deleted?
3. Why does Graphiti choose label propagation over Leiden for communities, and what staleness
   does the plurality dynamic extension accept in exchange?
4. In the φ → ρ → χ pipeline, which reranker would you expect to matter most for temporal
   reasoning questions, and what graph-database operator does each φ search method correspond to?
5. Why does single-session-assistant regress −17.7% while single-session-preference gains
   +184%? What does that say about when retrieval beats full context?

## Done when

Answer each before unfolding it.

- [ ] You can draw the two timelines and place all four edge timestamps without looking.

  <details><summary>Answer</summary>

  Two timelines (§2.1): **T = event/valid time** (when the fact was true in the world) and
  **T′ = transaction time** (when the system learned it; database-audit purpose). Four
  timestamps (§2.2): on **T**, `t_valid` (fact became true) and `t_invalid` (fact stopped being
  true); on **T′**, `t'_created` (edge written to the store) and `t'_expired` (edge expired in
  the store). For the Alice/Acme edge: t_valid=Jan, t_invalid=Jun on T; t'_created=Feb,
  t'_expired=Jul on T′. The two are independent — a fact learned in Feb can have been true since
  Jan.

  </details>

- [ ] You can state the invalidation contract (which timestamps are set, nothing deleted) and phrase both "true in March" and "known in March" as single filters.

  <details><summary>Answer</summary>

  On a contradicting, temporally-overlapping new fact, the old edge's **`t_invalid` is set to the
  new edge's `t_valid`** and its `t'_expired` is set to now; the edge is **kept, not deleted**
  (§2.2). "True in March" is the event-time filter `t_valid ≤ Mar < t_invalid`; "known in March"
  is the transaction-time filter `t'_created ≤ Mar < t'_expired`. An as-of query ANDs both. If
  the edge were deleted instead of tombstoned, every historical/as-of query — "what did we
  believe last quarter?" — would lose its answer, because the superseded version would be gone.

  </details>

- [ ] You can name the three φ search methods and at least three ρ rerankers.

  <details><summary>Answer</summary>

  φ (§3.1): **φ_cos** cosine similarity on embeddings, **φ_bm25** Okapi BM25 full-text (Neo4j/
  Lucene), and **φ_bfs** breadth-first search over n-hops seeded from recently-mentioned nodes;
  φ returns a 3-tuple of semantic edges, entity nodes, and community nodes. ρ (§3.2), any three
  of: **RRF** (Reciprocal Rank Fusion), **MMR** (Maximal Marginal Relevance), the
  **episode-mentions** reranker (frequency of mention), the **node-distance** reranker (locality
  to a centroid node), and the **cross-encoder** (LLM relevance scoring, highest cost). χ then
  builds the context string, emitting `t_valid`/`t_invalid` per edge.

  </details>

- [ ] You ran the companion temporal.rs experiment and reproduced the reference shape.

  <details><summary>Answer</summary>

  The companion crate ([experiments/src/temporal.rs](experiments/src/temporal.rs)) is a
  miniature bi-temporal edge store with the same four timestamps and the same
  contradiction-invalidation contract (`t_invalid := t_valid` of the superseding edge, nothing
  deleted). Its reference lane builds **100,000 edges from 10,000 entities × 10 job changes**,
  of which **10,000 are current**, and an **as-of scan runs in ≈ 0.09 ms** (notes.md baseline;
  [FINDINGS.md](../../FINDINGS.md) row 38). Reproducing it confirms that as-of reads are cheap
  filters over a version chain, not reconstructions.

  </details>

- [ ] You can explain the LongMemEval latency drop in tokens (~115k → ~1.6k) and name the one category that regressed.

  <details><summary>Answer</summary>

  Full-context feeds the whole conversation — **≈ 115k tokens on average** (§4.3) — to the LLM
  per query, costing **28.9 s** at gpt-4o. Zep retrieves only the relevant facts, shrinking the
  prompt to **≈ 1.6k tokens** and the latency to **2.58 s** (Table 2) — the token count *is* the
  latency story. The one regressing category is **single-session-assistant, −17.7%** at gpt-4o
  (Table 3): when the answer requires verbatim recall of a single recent session, full context
  still beats retrieval because retrieval can drop the exact wording.

  </details>

## References

- Rasmussen et al., "Zep: A Temporal Knowledge Graph Architecture for Agent Memory," arXiv
  2501.13956 — https://arxiv.org/abs/2501.13956. Section and table numbers in this chapter are
  from that version.

| Where | What it settles |
|---|---|
| §2.1 | episodes (message/text/JSON), non-lossy episode subgraph; bi-temporal model — timeline T (event) vs T′ (transaction) |
| §2.2 | four timestamps (t_valid/t_invalid ∈ T, t'_created/t'_expired ∈ T′); invalidation sets old t_invalid := new t_valid, nothing deleted; t_ref-based relative-date resolution |
| §2.2.1 | entity extraction with coreference; embedding-candidate + LLM-verified resolution |
| §2.3 | communities via **label propagation** (not Leiden); dynamic extension adopts **plurality** neighbor label |
| §3, §3.1, §3.2 | f(α)=χ(ρ(φ(α))); φ_cos/φ_bm25/φ_bfs; RRF, MMR, episode-mentions, node-distance, cross-encoder; χ emits t_valid/t_invalid |
| §4.2 | DMR 94.8% vs MemGPT 93.4% (gpt-4-turbo) |
| §4.3, Table 2 | LongMemEval gpt-4o: 60.2%→71.2%, 28.9s→2.58s, ~115k→~1.6k tokens; abstract "up to 18.5%" |
| §4.3, Table 3 | gpt-4o per-category: single-session-preference +184%, temporal-reasoning +38.4%, multi-session +30.7%, single-session-assistant −17.7% |

- Companion experiment in this repo: [experiments/src/temporal.rs](experiments/src/temporal.rs)
  — miniature bi-temporal edge store; reference shape 100k edges / 10k current, as-of scan
  ≈ 0.09 ms (notes.md; [FINDINGS.md](../../FINDINGS.md) row 38).
- Topic 33 of this learning path — temporal graphs; the same valid-time × transaction-time model
  from the storage-engine side.
