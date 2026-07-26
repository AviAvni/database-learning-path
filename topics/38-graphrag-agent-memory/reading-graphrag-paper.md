# Microsoft GraphRAG: pay at index time so global questions get cheap

Vector RAG answers "what does the corpus say about X" by pulling the top-k most
similar passages. That works when the evidence lives in a few passages — it
fails structurally when the question is about the corpus as a whole ("what are
the main themes across this dataset?"), because no top-k retrieval can see
everything. Edge et al. propose GraphRAG: extract an entity-relationship graph
from the corpus once, partition it hierarchically with Leiden, pre-summarize
every community, and answer global queries by map-reduce over those summaries.
For a database engineer this is a familiar shape: a materialized view built
offline so that an expensive analytical query becomes a cheap scan.

## The problem in one sentence

**Retrieval-augmented generation can only answer questions whose evidence fits
in top-k retrieved passages, while global sensemaking questions require
aggregating over the entire corpus — which query-focused summarization can do,
but not at RAG-scale corpus sizes.**

Vector RAG ("semantic search" in the paper) is local by construction.
Query-focused summarization (QFS) methods produce the right kind of answer but
do not scale to corpora of a million tokens or more. GraphRAG bridges the two
by moving the expensive aggregation work to index time.

## The concepts, step by step

### Step 1 — Chunk, extract, and re-extract ("gleanings")

Source documents are split into 600-token chunks with 100-token overlap. An
LLM extracts entities, relationships, and claims from each chunk — and
crucially, each extracted element carries a free-text description, not just a
name and type. Because a single pass over a large chunk misses things, the
pipeline runs "gleanings": the LLM is asked whether it missed anything and
re-extracts, up to a maximum number of passes. This is what lets larger chunk
sizes recover entities they would otherwise drop. Few-shot examples in the
extraction prompt tailor it to the domain.

```
  documents
     |
     v
  600-token chunks (100-token overlap)
     |
     v
  LLM extraction pass 1  -->  entities / relationships / claims
     |                          (each with a free-text description)
     v
  "did you miss anything?" --> gleaning pass 2, 3, ... (up to max)
```

### Step 2 — Dedup builds the graph; duplicates become edge weights

Entity instances are merged by exact string match on the entity name — a
deliberately crude resolution step. The interesting move is on edges: when the
same relationship is detected multiple times across chunks, the duplicate
count becomes the edge weight. Frequency of independent extraction is treated
as a signal of importance, for free, with no extra LLM calls.

### Step 3 — Hierarchical community detection with Leiden

The weighted graph is partitioned with the Leiden algorithm (graspologic
implementation), recursively, producing a multi-level hierarchy: level C0 is
the root partition (coarsest, fewest communities), then C1, C2, C3
progressively finer. Every node belongs to exactly one community per level, so
each level is a complete, mutually exclusive cover of the graph.

```
  C0 (root):      [==== community A ====][==== community B ====]
                          |                        |
  C1:             [ A1 ][ A2 ][ A3 ]       [  B1  ][  B2  ]
                    |                          |
  C2:             [A1a][A1b]  ...          [B1a][B1b][B1c] ...
                    |
  C3 (finest):    ...          (one pre-written summary per box)
```

### Step 4 — Bottom-up community summaries under a token budget

Each community gets an LLM-written summary, built bottom-up. For leaf
communities, element summaries (node, edge, and claim descriptions) are added
in order of decreasing combined source+target node degree until the context
window fills — high-degree elements first, so the most connected facts win
budget. For higher-level communities: if all child element summaries fit, use
them directly; otherwise substitute the shorter sub-community summaries,
prioritizing the sub-communities that account for the most element summaries.
This is recursive compression with a degree-based eviction policy.

### Step 5 — Query time: map-reduce over summaries

At query time, pick a hierarchy level. The community summaries at that level
are shuffled and packed into chunks. The map step has the LLM answer the query
from each chunk independently, producing a partial answer plus a 0-100
helpfulness score. Partials scoring 0 are filtered out. The reduce step
assembles the surviving partials in descending helpfulness order into the
final answer.

```
  community summaries @ level Ck
     |  shuffle + chunk
     v
  [chunk1] [chunk2] [chunk3] ... [chunkN]
     |        |        |            |        MAP: partial answer
     v        v        v            v             + helpfulness 0-100
   (78)     (0)      (91)         (45)
     |      drop       |            |        filter score-0
     +--------+--------+------------+
              v
        sort by score desc, pack     REDUCE
              v
         final answer
```

### Step 6 — The experimental grid: C0-C3 vs TS vs SS

Six conditions: C0, C1, C2, C3 (map-reduce over community summaries, root
down to the lowest level), TS (map-reduce directly over the source texts — the
no-index upper bound), and SS (vanilla vector RAG). Two corpora: podcast
transcripts (1669 chunks of 600 tokens, ~1M tokens → 8,564 nodes, 20,691
edges) and news articles (3197 chunks, ~1.7M tokens → 15,754 nodes, 19,520
edges). Indexing took 281 minutes with gpt-4-turbo at an 8k context window —
that number is the price of the materialized view.

### Step 7 — Evaluating global answers without gold labels

Global questions have no reference answers, so both question generation and
judging use LLMs. Questions: K=5 personas × M=5 tasks × N=5 questions = 125
per dataset, generated from only a corpus description (no specific texts), so
they stay global by construction. Judging: head-to-head LLM-as-judge on
comprehensiveness, diversity, and empowerment, plus directness as a control —
directness is expected to favor vector RAG, and it does, which sanity-checks
the judge. A second experiment corroborates the judge: an LLM claim extractor
(Claimify) pulled 47,075 factual claims across all answers (≈31 per answer on
average); C0 on News yielded 34.18 claims per answer vs 25.23 for SS.

### Step 8 — The token economics (why C0 is the headline)

Graph conditions beat SS on comprehensiveness with win rates of 72-83% on
Podcast and 72-80% on News (diversity: 75-82% and 62-71%). The cost side is
Table 2: C0 answers a Podcast query with 26,657 tokens, roughly 2.6% of what
TS needs at its maximum (News C0: 39,770 tokens ≈ 2.3%). Root-level summaries
need 9-43× fewer query tokens than TS — over 97% fewer — and even the finest
level C3 uses 26-33% fewer tokens than TS. The engineering trade: spend the
281 minutes once, then every global query is an order of magnitude (or two)
cheaper. Same amortization argument as a materialized view or an analytical
replica.

## How to read the paper (with the concepts in hand)

1. Read the abstract and introduction for the local-vs-global framing — the
   claim that vector RAG cannot, structurally, answer whole-corpus questions
   (the problem sentence above).
2. Find the pipeline description and match it to Steps 1-4: chunking and
   gleanings, then the dedup/edge-weight construction, then Leiden levels,
   then the bottom-up summary packing rules. The degree-ordered element
   packing is easy to skim past — slow down there.
3. Read the query-time map-reduce description against Step 5, paying
   attention to the shuffle, the 0-100 scoring, and the score-0 filter.
4. Move to the evaluation setup: the six conditions (Step 6), the two
   datasets with their node/edge counts, and the persona-based question
   generation (Step 7). Note that TS is the expensive upper bound, SS the
   cheap baseline, and C0-C3 the dial between them.
5. Read the results with Step 8 in hand: win rates first, then Table 2 for
   token costs. Check the directness control behaves as predicted.
6. Finish with the Claimify claim-count experiment — the authors' answer to
   "isn't LLM-as-judge circular?"

## Questions to answer in notes.md

1. Why does the gleanings mechanism specifically enable larger chunk sizes,
   and what is the cost model trade-off (extraction calls vs chunks) it buys?
2. Entity dedup is exact string match on the name. Where does that break, and
   what would a graph database bring to the resolution step instead?
3. Why must community summaries be built bottom-up with degree-ordered
   packing rather than summarizing each community's raw text independently?
4. In the map step, why shuffle the community summaries before chunking, and
   what failure mode would an unshuffled ordering create?
5. Using Table 2, at what query volume does the 281-minute indexing cost
   break even against TS for the News corpus? Sketch the arithmetic.

## Done when

- [ ] You can draw the full indexing pipeline (chunks → extraction with
      gleanings → weighted graph → Leiden hierarchy → summaries) from memory.
- [ ] You can explain why C0 wins on token cost and when you would pick C3
      or TS instead.
- [ ] You can state the three judged metrics plus the directness control and
      why the control matters.
- [ ] You have answered the five questions above in notes.md, including the
      break-even arithmetic.

## References

- Edge et al., "From Local to Global: A Graph RAG Approach to Query-Focused
  Summarization", arXiv 2404.16130v2 — https://arxiv.org/abs/2404.16130
- Local PDF: /tmp/graphrag.pdf
- Companion notes in this topic: [README.md](README.md) — HippoRAG covers the
  local/associative axis and Zep the temporal axis; GraphRAG covers the
  global-question axis.
- [experiments/](experiments/) — this repo's crate demonstrates the local
  path-finding side, not community summarization.
- Leiden implementation used by the paper: graspologic.
