# Microsoft GraphRAG: pay at index time so global questions get cheap

Vector RAG answers "what does the corpus say about X" by pulling the top-k most similar
passages. That works when the evidence lives in a few passages — it fails structurally when the
question is about the corpus as a whole ("what are the main themes across this dataset?"),
because no top-k retrieval can see everything. Edge et al. propose GraphRAG (arXiv 2404.16130):
extract an entity-relationship graph from the corpus once, partition it hierarchically with
Leiden, pre-summarize every community, and answer global queries by map-reduce over those
summaries. For a database engineer this is a familiar shape: a materialized view built offline
so that an expensive analytical query becomes a cheap scan.

Every number below is quoted with the section, table or figure it comes from in arXiv
2404.16130. The distinct **local search** algorithm (entity-anchored, for specific-fact
questions) is a different algorithm with a different cost profile; this guide is about the
paper's headline **global search** path, and says so wherever the two could be confused.

## The problem in one sentence

**Retrieval-augmented generation can only answer questions whose evidence fits
in top-k retrieved passages, while global sensemaking questions require
aggregating over the entire corpus — which query-focused summarization can do,
but not at RAG-scale corpus sizes.**

Vector RAG ("semantic search" — abbreviated **SS** in the paper) is local by construction: it
ranks passages by similarity to the query and reads the top few. **Query-focused
summarization (QFS)** — summarizing a whole corpus through the lens of a specific query —
produces the right kind of answer but does not scale to corpora of a million tokens or more.
GraphRAG bridges the two by moving the expensive aggregation work to index time.

## The concepts, step by step

### Step 1 — Chunk, extract, and re-extract ("gleanings")

> **In:** the source documents.
> **Out:** per-chunk entities, relationships and claims — each carrying a free-text
> description — which Step 2 merges into a graph.

Source documents are split into **600-token chunks with 100-token overlap** (§A.2). An LLM
extracts entities, relationships, and claims from each chunk — and crucially, each extracted
element carries a **free-text description**, not just a name and type, because those
descriptions are what Step 4 later summarizes. Because a single pass over a large chunk misses
things, the pipeline runs **gleanings**: after the first pass the LLM is asked whether it missed
anything and re-extracts, up to a maximum number of passes (§2.3). That is what lets larger
chunk sizes recover entities they would otherwise drop. Few-shot examples in the extraction
prompt tailor it to the domain.

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

> **In:** the per-chunk extractions from Step 1.
> **Out:** one weighted graph — the input Leiden partitions in Step 3.

Entity instances are merged by **exact string match on the entity name** (§2.3) — a deliberately
crude resolution step (the paper notes it is robust because later community summarization is
resilient to duplicate variants). The interesting move is on edges: when the same relationship
is detected multiple times across chunks, the **duplicate count becomes the edge weight**.
Frequency of independent extraction is treated as a signal of importance, for free, with no
extra LLM calls — and that weight is what Leiden optimizes over next.

### Step 3 — Hierarchical community detection with Leiden

> **In:** the weighted graph from Step 2.
> **Out:** a multi-level community hierarchy C0…C3 — the units Step 4 summarizes.

A **community** is a group of nodes more densely connected to each other than to the rest of the
graph. The **Leiden algorithm** (Traag et al. 2019, via the graspologic implementation; §2.4)
finds them and guarantees connected communities; run recursively it produces a hierarchy. Level
**C0** is the root partition — coarsest, fewest communities (**34 units** for Podcast, **55** for
News; Table 2) — then C1, C2, **C3** progressively finer (**1310** and **2142** units). Every
node belongs to exactly one community per level, so each level is a complete, mutually exclusive
cover of the graph.

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

> **In:** the community hierarchy from Step 3 and the element descriptions from Step 1.
> **Out:** one LLM-written summary per community per level — the artifacts Step 5 answers
> from.

Each community gets an LLM-written summary, built **bottom-up** (§2.5). For **leaf** communities,
element summaries (node, edge, and claim descriptions) are added in order of **decreasing
combined source+target node degree** until the context window fills — high-degree elements
first, so the most connected facts win budget. For **higher-level** communities: if all child
element summaries fit, use them directly; otherwise substitute the shorter **sub-community
summaries**, prioritizing the sub-communities that account for the most element summaries. This
is recursive compression with a degree-based eviction policy — the summaries themselves are
generated once, at index time, within the fixed 8k generation window (§3.3).

### Step 5 — Query time: map-reduce over summaries

> **In:** a global query and the community summaries at a chosen level (from Step 4).
> **Out:** one final answer — assembled without touching the source corpus.

At query time, pick a hierarchy level. The community summaries at that level are **shuffled** and
packed into chunks (§2.6). The **map** step has the LLM answer the query from each chunk
independently, producing a partial answer plus a **0–100 helpfulness score**. Partials scoring 0
are filtered out. The **reduce** step assembles the surviving partials in descending helpfulness
order into the final answer. The shuffle matters: it spreads relevant facts across chunks
"rather than concentrated (and potentially lost) in a single context window" (§2.6, line 281 of
the extracted text).

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

> **In:** the built index (Steps 1–4) and the query path (Step 5).
> **Out:** the six comparison conditions and the two corpora Step 7 judges.

Six conditions: **C0, C1, C2, C3** (map-reduce over community summaries, root down to the lowest
level), **TS** (map-reduce directly over the source texts — the no-index upper bound), and **SS**
(vanilla vector RAG). Two corpora: podcast transcripts (**1669** × 600-token chunks,
≈ 1,014,611 tokens → **8,564 nodes, 20,691 edges**; §4.1) and news articles (**3197** chunks,
≈ 1,707,694 tokens → **15,754 nodes, 19,520 edges**).

Get the cost anchor exactly right, because the paper states two different window sizes and they
are easy to conflate:

```
  600-token window  -> used to CHUNK for graph indexing (§A.2);
                       indexing took 281 minutes for the PODCAST dataset
                       on gpt-4-turbo (§3.3, "2M TPM, 10k RPM")
  8k-token window   -> used to GENERATE community summaries, community
                       answers, and global answers (§3.3, Appendix C)
```

The 281 minutes is the price of the materialized view, and it is the *Podcast* index built
under a *600-token* chunk window — not an 8k-window number.

### Step 7 — Evaluating global answers without gold labels

> **In:** the six conditions from Step 6.
> **Out:** the win-rate and claim-count evidence Step 8 reads for cost.

Global questions have no reference answers, so both question generation and judging use LLMs.
**Questions:** K=5 personas × M=5 tasks × N=5 questions = **125 per dataset** (§3.2), generated
from only a corpus description (no specific texts), so they stay global by construction.
**Judging:** head-to-head **LLM-as-judge** on **comprehensiveness, diversity, and empowerment**,
plus **directness** as a *control* — directness rewards concision, so it is "effectively in
opposition to comprehensiveness and diversity" (§3.4) and is expected to favor vector RAG. It
does, which sanity-checks the judge. A second experiment corroborates the judge (§5.2): an LLM
claim extractor (**Claimify**) pulled **47,075 unique claims** across all answers (**≈ 31 per
answer**); on News, C0 yielded **34.18 claims/answer** vs **25.23** for SS (Table 3).

### Step 8 — The token economics (why C0 is the headline)

> **In:** the win-rates and Table 2 token counts from Step 7.
> **Out:** the amortization argument — the whole reason to build the index.

Global (graph) conditions beat SS on **comprehensiveness** with win rates of **72–83%** on
Podcast and **72–80%** on News; **diversity** win rates are **75–82%** and **62–71%** (§5.1). The
cost side is **Table 2**: C0 answers a Podcast query with **26,657 tokens**, and TS needs
**1,014,611**; C0 on News is **39,770** vs TS **1,707,694**. Worked as percentages:

```
  Podcast   C0 / TS = 26,657 / 1,014,611 = 2.63%   (Table 2 "% Max" row: 2.6)
  News      C0 / TS = 39,770 / 1,707,694 = 2.33%   (Table 2 "% Max" row: 2.3)

  per-query tokens SAVED on News by using C0 instead of TS:
    1,707,694 - 39,770 = 1,667,924 tokens/query
```

Across levels, root summaries need **9×–43×** fewer query tokens than TS — **over 97% fewer** —
and even the finest level **C3** uses **26–33% fewer** tokens than TS (§5.1). The engineering
trade: spend the one-time indexing cost, then each of those 1.67M-token-per-query savings
accrues on every global query. The break-even is the materialized-view calculation — index cost
÷ per-query saving = the query volume at which the view pays for itself — with the caveat that
the paper reports indexing *time* (281 min, Podcast) rather than an indexing *token* count, so
the token break-even needs one stated assumption about index cost.

## How to read the paper (with the concepts in hand)

1. Read the abstract and introduction for the local-vs-global framing — the claim that vector
   RAG cannot, structurally, answer whole-corpus questions (the problem sentence above).
2. Read §2 and match it to Steps 1–4: chunking and gleanings (§2.3), the dedup/edge-weight
   construction (§2.3), Leiden levels (§2.4), then the bottom-up summary packing rules (§2.5).
   The degree-ordered element packing is easy to skim past — slow down there.
3. Read the query-time map-reduce (§2.6) against Step 5, paying attention to the shuffle, the
   0–100 scoring, and the score-0 filter.
4. Move to the evaluation setup (§3): the six conditions (Step 6), the two datasets with their
   node/edge counts (§4.1), the persona-based question generation (§3.2), and — carefully —
   the two window sizes in §3.3 (600-token indexing vs 8k generation). TS is the expensive
   upper bound, SS the cheap baseline, C0–C3 the dial between them.
5. Read the results (§5.1) with Step 8 in hand: win rates first, then Table 2 for token costs.
   Check the directness control behaves as predicted.
6. Finish with the Claimify claim-count experiment (§5.2, Table 3) — the authors' answer to
   "isn't LLM-as-judge circular?"

## Questions to answer in notes.md

1. Why does the gleanings mechanism specifically enable larger chunk sizes, and what is the
   cost model trade-off (extraction calls vs chunks) it buys?
2. Entity dedup is exact string match on the name. Where does that break, and what would a
   graph database bring to the resolution step instead?
3. Why must community summaries be built bottom-up with degree-ordered packing rather than
   summarizing each community's raw text independently?
4. In the map step, why shuffle the community summaries before chunking, and what failure mode
   would an unshuffled ordering create?
5. Using Table 2, sketch the break-even: News C0 saves 1,667,924 tokens/query vs TS. What
   index cost (in the same tokens) would make the crossover happen at, say, 100 queries — and
   what does the paper actually report instead (§3.3)?

## Done when

Answer each before unfolding it.

- [ ] You can draw the full indexing pipeline (chunks → extraction with gleanings → weighted graph → Leiden hierarchy → summaries) from memory.

  <details><summary>Answer</summary>

  Documents → **600-token chunks / 100-token overlap** (Step 1) → per-chunk LLM extraction of
  entities/relationships/claims *with free-text descriptions*, repeated as **gleanings** until
  the LLM finds nothing new → merge by **exact-string entity name**, with **duplicate
  relationship counts becoming edge weights** (Step 2) → **Leiden** recursive partition into a
  **C0…C3 hierarchy** where every node sits in exactly one community per level (Step 3) →
  **bottom-up** per-community summaries that pack element descriptions in **decreasing
  source+target node degree**, substituting shorter sub-community summaries on overflow (Step 4).
  All of it is index-time work.

  </details>

- [ ] You can explain why C0 wins on token cost and when you would pick C3 or TS instead.

  <details><summary>Answer</summary>

  C0 is the **root** level — the fewest communities (34 Podcast / 55 News units, Table 2), so a
  query touches the fewest, shortest summaries: **26,657 tokens on Podcast (2.6% of TS), 39,770
  on News (2.3%)**, i.e. **9–43× fewer** than TS and **over 97% fewer** at root (§5.1). It still
  wins comprehensiveness (72% win rate) and diversity (62%) over vector RAG. Pick a **finer
  level (C1–C3)** when you need more specific coverage and can pay 26–33% (C3) up to most-of-TS
  (C1/C2) token cost for a modest quality gain. Pick **TS** only as the no-index upper bound —
  it reads the whole corpus per query (1.0M/1.7M tokens) and is what the index exists to avoid.

  </details>

- [ ] You can state the three judged metrics plus the directness control and why the control matters.

  <details><summary>Answer</summary>

  The three judged metrics are **comprehensiveness, diversity, and empowerment** (§3.4).
  **Directness** is the **control**: it rewards concise, narrowly-on-point answers, so it is "in
  opposition to comprehensiveness and diversity" and is *expected* to favor vector RAG (SS). It
  does — and that expected result is the point: an LLM judge that also handed GraphRAG the
  directness win would be suspect, so directness going the other way is evidence the judge
  discriminates rather than rubber-stamps the graph method. The Claimify claim-count experiment
  (§5.2, Table 3: C0 News 34.18 vs SS 25.23 claims/answer) is the second, non-LLM-judge check.

  </details>

- [ ] You have answered the five questions above in notes.md, including the break-even arithmetic.

  <details><summary>Answer</summary>

  notes.md records all five with their anchors: gleanings vs chunk size and the extraction-call
  trade (§2.3); where exact-string dedup breaks and what real entity resolution would add
  (§2.3, and this repo's topic 25 SDK resolution ladder); why bottom-up degree-ordered packing
  beats independent per-community summarization (§2.5); why the map step shuffles summaries
  (§2.6, to avoid concentrating relevant facts in one dropped chunk); and the break-even sketch
  — News C0 saves **1,667,924 tokens/query** vs TS, so with an assumed index cost of X tokens
  the crossover is X ÷ 1,667,924 queries, noting the paper reports **281 min (Podcast)** wall
  clock rather than an index token count (§3.3).

  </details>

## References

- Edge et al., "From Local to Global: A Graph RAG Approach to Query-Focused Summarization,"
  arXiv 2404.16130 — https://arxiv.org/abs/2404.16130. Section, table and figure numbers in
  this chapter are from that version.

| Where | What it settles |
|---|---|
| §2.3 | 600-token / 100-overlap chunks; gleanings; exact-string entity merge; duplicate count → edge weight |
| §2.4 | Leiden hierarchical communities (graspologic) |
| §2.5 | bottom-up summaries; decreasing source+target degree packing; sub-community substitution |
| §2.6 | map-reduce; shuffle; 0–100 helpfulness; score-0 filter; descending-helpfulness reduce |
| §3.2 | K=M=N=5 → 125 questions from a corpus description |
| §3.3, App. C | 600-token indexing window vs 8k generation window; 281 min for the **Podcast** index; gpt-4-turbo |
| §3.4 | comprehensiveness / diversity / empowerment + directness control |
| §4.1 | Podcast 1669 chunks → 8,564 nodes / 20,691 edges; News 3197 → 15,754 / 19,520 |
| §5.1, Table 2 | tokens: Podcast C0 26,657 (2.6%), News C0 39,770 (2.3%); 9–43× / >97% fewer; C3 26–33% fewer; win rates 72–83% / 72–80% comprehensiveness, 75–82% / 62–71% diversity |
| §5.2, Table 3 | Claimify 47,075 unique claims (≈31/answer); C0 News 34.18 vs SS 25.23 |

- Companion notes in this topic: [README.md](README.md) — HippoRAG covers the local/associative
  axis and Zep the temporal axis; GraphRAG covers the global-question axis.
- [experiments/](experiments/) — this repo's crate demonstrates the local path-finding side, not
  community summarization; its measured headline is in [FINDINGS.md](../../FINDINGS.md) row 38.
- Leiden implementation used by the paper: graspologic. Local search (entity-anchored) is a
  separate algorithm in the same system and is not the subject of this guide.
