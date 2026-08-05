# HippoRAG: pattern completion over a sparse association index, not iterative retrieval

HippoRAG (Gutiérrez et al., NeurIPS 2024, arXiv 2405.14831) asks why RAG systems need an LLM
loop to answer multi-hop questions when the brain retrieves associated memories in a single
pass. Its answer is an architecture copied from hippocampal memory indexing theory: keep the
content in a passage store, keep only a sparse index of associations in a knowledge graph, and
answer queries by spreading activation — Personalized PageRank — through that index. For a
graph-database developer the payoff is concrete: multi-hop retrieval becomes one cheap graph
query instead of N LLM calls, and the paper measures the difference in recall, dollars, and
latency.

Every number below is quoted with the table, figure or section it comes from in the NeurIPS
2024 version of the paper (arXiv 2405.14831). Where a figure is this repo's own measurement it
says so and links [FINDINGS.md](../../FINDINGS.md).

## The problem in one sentence

**Single-step retrievers and iterative LLM retrievers both fail path-finding multi-hop
questions — questions whose answer is the one entity associated with several query entities
when no single passage mentions them together — because they chain lookups instead of
aggregating association strength across a corpus-wide index.**

Define the two shapes the paper contrasts (§1, Figure 1):

- A **path-following** multi-hop question can be solved hop by hop — each hop's answer *names*
  the next clue, so a retriever that finds passage 1 finds the bridge entity that leads to
  passage 2.
- A **path-finding** multi-hop question cannot: the answer is the entity that several query
  entities *both* point at, and no single passage names them together, so there is no chain
  to follow — retrieval must complete the pattern across passages instead.

## The concepts, step by step

### Step 1 — The hippocampal memory indexing analogy

> **In:** nothing yet — this step is the motivation and fixes the vocabulary.
> **Out:** the split — a content store versus a sparse association index — that every later
> step implements.

The **hippocampal memory indexing theory** (§2.1) is a model of human memory in which the
neocortex stores the actual content of memories while the hippocampus stores only a sparse
*index* of associations between them; recall is **pattern completion** — retrieving a whole
memory from a partial cue by spreading activation through that index. HippoRAG maps each brain
region to a system component:

```
  Brain region              HippoRAG component            Role
  ------------              ------------------            ----
  Neocortex          <-->   LLM + passage store           stores/processes actual content
  Parahippocampal    <-->   retrieval encoders            synonymy detection (similarity)
  regions                   (Contriever / ColBERTv2)
  Hippocampus        <-->   knowledge graph + PPR         sparse index of associations;
                                                          stores NO content, only links
```

The load-bearing claim (§2.1): the hippocampus stores no content, only the index. That is why
retrieval can be one graph operation rather than a scan of the corpus — the index is small and
the association is explicit. HippoRAG implements exactly that split, and Steps 2–7 are its two
halves: build the index offline, complete the pattern online.

### Step 2 — Offline indexing: two-step OpenIE into a schemaless graph

> **In:** the passage corpus P.
> **Out:** the knowledge graph — nodes N (noun phrases), triple edges E, and synonymy edges
> E′ — which Step 6's PPR walks. (Step 3 derives the other offline artifact from the same
> extraction.)

**OpenIE** (open information extraction) means pulling `(subject, relation, object)` triples
out of free text without a fixed schema. HippoRAG runs it with an LLM (GPT-3.5-turbo-1106,
temperature 0; §3.4) in **two prompted steps** (§2.3, 1-shot):

1. **NER** (**named-entity recognition** — find the entity mentions in the passage) extracts a
   set of named entities.
2. those entities are pasted into a second prompt that extracts the final triples, which may
   also contain concepts (noun phrases) beyond the named entities.

The paper's stated reason for the two-step order: it "leads to an appropriate balance between
generality and bias towards named entities" (§2.3). The result is a **schemaless** graph —
nodes are raw noun phrases, with no fixed ontology and no entity-resolution pipeline.

One more edge type is welded on. A **synonymy edge** connects two nodes whose embedding cosine
similarity exceeds a threshold **τ = 0.8** (§3.4) — the parahippocampal analogue of Step 1,
cheap fuzzy matching turned into graph structure so the walk can cross surface-form differences
("JFK" ↔ "John F. Kennedy"). Table A in the appendix reports tens of thousands of these E′
edges per corpus; they are ablated in Step 9.

### Step 3 — The fork: matrix P, the node→passage index

> **In:** the same OpenIE output from Step 2 — this is the *other* thing built from it.
> **Out:** a `|N| × |P|` count matrix P that Step 7 multiplies against the PPR distribution to
> score passages. Nothing else uses it.

Extraction produces two artifacts, and they feed different downstream steps — so the matrix
gets its own step. **Matrix P** (§2.3) is `|N| × |P|` (nodes × passages) and holds "the number
of times each noun phrase in the KG appears in each original passage." It is the only thing
that links an index node back to the *content* it was extracted from; the KG of Step 2 has no
passage text in it at all (Step 1's "index stores no content").

```
        passage₁  passage₂  passage₃ ...
 node_a     2         0         1
 node_b     0         3         0        P[i][j] = # times node i appears in passage j
 node_c     1         1         0
```

Keep the fork in mind: the **KG (Step 2)** is what PPR walks; **P (this step)** is what turns a
distribution over nodes into a ranking over passages. Step 7 is where they rejoin.

### Step 4 — Online retrieval: from query to seed nodes R_q

> **In:** a query string, plus the KG from Step 2.
> **Out:** the seed set R_q — the graph nodes PPR will restart from — which Step 5 then
> re-weights.

Online retrieval starts with one LLM call: the same NER prompt extracts the query's named
entities Cq = {c₁, …, cₙ} (§2.3 — "Stanford" and "Alzheimer's" in the paper's Figure 2
example). Each cᵢ is embedded and matched to its nearest graph node by cosine similarity, giving
the **query nodes** (the seeds) R_q = {r₁, …, rₙ}, where rᵢ = arg max over graph nodes eⱼ of
`cosine(M(cᵢ), M(eⱼ))`. These are the only nodes PPR will inject restart mass into — nothing
else is seeded.

This is the whole "single-step" claim in miniature: one LLM call to name the query's entities,
then a single graph computation. There is no second LLM call to decide the next hop.

### Step 5 — Node specificity: local IDF weighting of the seeds

> **In:** the seed set R_q from Step 4, and the passage counts behind P from Step 3.
> **Out:** the restart vector n⃗ — the seeds' probabilities re-weighted so common entities get
> less mass — which Step 6 restarts from.

Left alone, a seed like "USA" would flood the graph with restart mass because it connects to
everything. **Inverse document frequency (IDF)** is the classic fix — weight a term by the
inverse of how many documents contain it, so common terms count for less — but true IDF needs a
global corpus count. HippoRAG uses a local stand-in. **Node specificity** of node i is

```
  sᵢ = |Pᵢ|⁻¹          (§2.3)

  |Pᵢ| = the number of passages node i was extracted from — a count already
         stored at the node, so no global corpus statistic is needed.
```

It is used by multiplying each query node's restart probability n⃗ by sᵢ before PPR (§2.3).
Worked on two seeds, one common and one rare:

```
  seed "Alzheimer's"  in |P| = 20 passages → s = 1/20 = 0.05
  seed "Stanford"     in |P| =  5 passages → s = 1/5  = 0.20

  raw restart (equal split):   n(Alz)  = 0.50     n(Stan) = 0.50
  after × sᵢ:                  0.50·0.05 = 0.025   0.50·0.20 = 0.100
  renormalize (÷ 0.125):       n(Alz)  = 0.20     n(Stan) = 0.80
```

The rarer, more discriminating seed keeps 4× the restart mass of the common one — exactly the
Figure 2 illustration where "the Stanford logo grows larger than the Alzheimer's symbol since
it appears in fewer documents" (§2.3). The database-friendly property: adding a passage changes
only the counts of the nodes it mentions, so specificity is maintainable incrementally, which
true IDF is not.

### Step 6 — PPR: why mass sums at the meet node

> **In:** the weighted restart vector n⃗ from Step 5, walking over the KG from Step 2.
> **Out:** a stationary distribution π⃗ (the paper's n⃗′) over all nodes — high on nodes near
> *several* seeds — which Step 7 scores passages with.

**PageRank** is the stationary distribution of a random walk that, at each step, either follows
a random out-edge or teleports; **Personalized PageRank (PPR)** replaces the uniform teleport
with a fixed **restart vector** so the walk keeps returning to a chosen set of seeds. The paper
sets the **damping factor** to 0.5, which it defines (§3.4) as "the probability that PPR will
restart a random walk from the query nodes instead of continuing to explore the graph" — i.e.
restart probability 0.5, continue probability 0.5. The seeds are R_q, weighted by Step 5.

Why does this solve a path-finding question? Because the one node reachable from *both* seeds
collects restart-driven mass along *two* inflows, while every dead-end collects from one.
Worked on the README's instance shape — two seeds u, w, each of degree 9 (one edge to the
answer a, eight to distractor dead-ends), restart 0.5 split equally over the two seeds:

```
  restart vector r:  r(u) = 0.5,  r(w) = 0.5,  everything else 0
  one PPR step (restart prob 0.5, continue prob 0.5, seed degree 9):

    walk-mass into a from u = 0.5 · r(u)/deg(u) = 0.5 · 0.5/9 = 0.0278
    walk-mass into a from w = 0.5 · r(w)/deg(w) = 0.5 · 0.5/9 = 0.0278
    π(a)  walk-mass         = 0.0278 + 0.0278             = 0.0556   ← sums from BOTH
    π(dead-end from u only) = 0.5 · 0.5/9                 = 0.0278   ← ONE source

  ratio  a : dead-end = 0.0556 / 0.0278 = 2.0
```

The answer node ends one iteration ahead at ≈ 2× a dead-end's mass — the 2× gap README exercise
2 asks you to derive. The full stationary values are larger (the walk keeps circulating), but
the ordering holds because a is the only node collecting from both seeds at *every* iteration.
This is association, not chaining: on the paper's real example — "Which Stanford professor works
on the neuroscience of Alzheimer's?" (Table 7, §5.3) — ColBERTv2 and IRCoT return the wrong
people, while HippoRAG ranks the correct answer, **Thomas Südhof**, first. (The Figure 1 / §2.3
walkthrough abbreviates that same entity as "Professor Thomas".) Looping an iterative retriever
harder cannot fix it, because the connection never lives in a single passage for a hop to land
on.

### Step 7 — Passage scoring: π⃗ · P

> **In:** the PPR distribution π⃗ from Step 6 and the matrix P from Step 3 — the fork rejoins
> here.
> **Out:** one score per passage, ranked; the top-k are returned.

The final step multiplies the node distribution by the node→passage matrix: **passage score
p⃗ = π⃗ · P** (§2.3). Concretely, each passage's score is the PPR mass summed over the nodes it
mentions, weighted by how often it mentions them. A passage that mentions "Professor Thomas" —
the node Step 6 pushed mass onto — rises to the top even though it never mentions both query
entities. One matrix-vector product turns "which nodes matter" into "which passages to read".

### Step 8 — The two-pipeline view: index once, query cheap

> **In:** the offline and online halves as built in Steps 2–7.
> **Out:** the cost argument — LLM work at index time, graph work at query time.

```
  OFFLINE (per passage, LLM-priced)          ONLINE (per query, graph-priced)
  ---------------------------------          --------------------------------
  passage                                    query
    | LLM NER                                  | LLM NER (one call, Step 4)
    | LLM triple extraction (Step 2)           | cosine match -> R_q
    v                                          | weight by specificity sᵢ (Step 5)
  triples -> KG nodes/edges                    v
    | cosine > 0.8 -> synonymy edges         PPR (restart 0.5) -> π⃗ (Step 6)
    v                                          v
  matrix P (Step 3)                          score = π⃗ · P -> ranked passages (Step 7)
```

All LLM-heavy work is offline. Online retrieval is **10–30× cheaper and 6–13× faster** than
IRCoT, which loops an LLM per hop (§4, measured in Appendix G). This is the classic database
trade: pay at index time to make reads one index probe. The offline bill dominates when the
corpus is large and the query volume is low — the break-even is the same amortization argument
as a materialized view.

### Step 9 — What the numbers say, and where the design earns its keep

> **In:** the built system from Steps 2–7.
> **Out:** the measured recall/QA gains (Tables 2–4, 6) and the ablations that locate them
> (Table 5).

Single-step retrieval, recall@2 / recall@5 (**Table 2**), HippoRAG on the ColBERTv2 backbone:

```
                    MuSiQue      2Wiki        HotpotQA
  R@2 / R@5         40.9 / 51.9  70.7 / 89.1  60.5 / 77.7   (Table 2)
```

The §4 text reads the 2Wiki column as "an impressive improvement of 11 and 20% for R@2 and R@5"
over ColBERTv2 (which scores 59.2 / 68.2 in the same table) and "around 3% on MuSiQue". The
**all-recall** metric — the fraction of questions for which *every* supporting passage is
retrieved (**Table 6**) — shows an even larger gap on 2Wiki: ColBERTv2 AR@5 37.1 → HippoRAG
75.7. QA F1 improves by up to 3 (MuSiQue), 17 (2Wiki) and 1 (HotpotQA) point (**Table 4**).
HippoRAG also *composes*: as IRCoT's retriever it adds about +4 / +18 / +1% R@5 over IRCoT alone
(**Table 3**).

The ablations (**Table 5**) locate the gains:

- **Extractor quality dominates.** Swapping the LLM OpenIE for REBEL — a small fine-tuned
  end-to-end extraction model — drops the average R@5 from 72.9 to 58.4; "GPT-3.5 produces
  twice as many triples" as REBEL (§5.1). Recall of the index bounds recall of retrieval.
- **Open models suffice.** Llama-3.1-70B as the extractor beats GPT-3.5 on 2 of the 3 datasets
  (MuSiQue and HotpotQA; it trails on 2Wiki) — Table 5, rows for the OpenIE alternatives.
- **PPR is doing real work.** "Rq Nodes Only" (score only the seeds) and "Rq Nodes & Neighbors"
  (seeds plus their one-hop neighbors) both fall far below full PPR — Table 5, PPR-alternative
  rows — so the multi-hop diffusion, not a neighborhood lookup, is what pays.
- **Specificity and synonymy split by dataset.** Node specificity helps MuSiQue and HotpotQA
  and barely moves 2Wiki; synonymy edges help 2Wiki most (§5.1) — 2Wiki is entity-centric, so
  standardizing surface forms matters more there than term weighting.

## How to read the paper (with the concepts in hand)

1. **§1** — the introduction and Figure 1: the path-finding motivation. Hold Steps 1 and 6 in
   mind; the paper's whole bet is that association beats chaining.
2. **§2.1** — the neurobiological framing; check the component mapping against Step 1 and note
   the "index stores no content" claim.
3. **§2.2–§2.3** — indexing and retrieval; verify the two-step OpenIE order (NER, then triples
   with the entities pasted in), τ = 0.8 synonymy edges, matrix P (Steps 2–3), and where node
   specificity multiplies the restart probabilities (Step 5).
4. **§3** — setup: MuSiQue, 2WikiMultihopQA, HotpotQA; baselines including ColBERTv2 and IRCoT;
   note Contriever/ColBERTv2 double as HippoRAG's encoders. Confirm τ = 0.8 and damping 0.5 in
   §3.4.
5. **§4, Tables 2–4, 6** — results; check the Step 9 numbers, especially the Table 6 all-recall
   jump and the Table 3 IRCoT + HippoRAG combination.
6. **§5.1, Table 5** — the ablations of Step 9; then **§5.3, Table 7** — the path-finding vs
   path-following case study (answer Thomas Südhof); the appendices hold the prompts and the
   Appendix G cost/latency measurements.

## Questions to answer in notes.md

1. Why does PPR mass from two query entities summing at a shared node solve path-finding
   questions that iterative chaining cannot — and what graph shape would defeat it?
2. Node specificity sᵢ = |Pᵢ|⁻¹ is a local IDF analogue. What does locality buy you for
   incremental index maintenance compared to true corpus-level IDF?
3. The REBEL ablation shows extraction recall bounds retrieval recall. How would you
   monitor index recall in production without gold triples?
4. Online retrieval is 10–30× cheaper and 6–13× faster than IRCoT (§4, Appendix G). Which costs
   moved offline to make that possible, and when does the offline bill dominate?
5. Synonymy edges (cosine above τ = 0.8) help 2Wiki most while specificity helps MuSiQue
   and HotpotQA. What does that split suggest about the datasets' entity-surface variety?

## Done when

Answer each before unfolding it.

- [ ] You can draw the brain-to-component mapping from memory and state what the hippocampus analogue does and does not store.

  <details><summary>Answer</summary>

  Three regions map to three components (§2.1, Step 1): the **neocortex** to the LLM plus the
  passage store (actual content), the **parahippocampal regions** to the retrieval encoders
  (Contriever / ColBERTv2) that detect synonymy, and the **hippocampus** to the knowledge graph
  plus PPR. The hippocampal analogue stores **no content — only the index of associations**
  (nodes N, triple edges E, synonymy edges E′). The passage text lives only in the passage
  store, reachable from index nodes through matrix P (Step 3). That separation is what makes
  retrieval one graph computation rather than a corpus scan.

  </details>

- [ ] You can explain path-finding vs path-following and why the Figure 1 example defeats ColBERTv2 and IRCoT but not PPR.

  <details><summary>Answer</summary>

  A **path-following** question can be solved hop by hop because each hop's answer names the
  next clue; a **path-finding** question cannot, because the answer is the entity several query
  entities *both* point at and no single passage names them together (§1, Figure 1). In the
  paper's example — "Which Stanford professor works on the neuroscience of Alzheimer's?" — no
  passage mentions Stanford, Alzheimer's, and the answer together, so a passage encoder
  (ColBERTv2) scores every candidate near zero and an iterative retriever (IRCoT) has no chain
  to follow.

  PPR wins because it does not look for a passage containing the pattern; it lets the pattern
  emerge in the graph. Restart mass at both seeds diffuses, and the one node reachable from both
  — **Thomas Südhof** (Table 7, §5.3; abbreviated "Professor Thomas" in the §2.3 walkthrough) —
  sums both inflows and outranks every dead-end that collects from one seed. Step 6's arithmetic
  makes it ≈ 2× a dead-end after one iteration.

  </details>

- [ ] You can write the online scoring pipeline end to end: query NER → R_q → specificity weights → PPR (restart 0.5) → π⃗·P.

  <details><summary>Answer</summary>

  One LLM NER call extracts the query entities Cq (Step 4); each is embedded and matched to its
  nearest graph node by cosine, giving the seeds R_q (§2.3). Each seed's restart probability is
  multiplied by its node specificity sᵢ = |Pᵢ|⁻¹ and renormalized, so common entities get less
  mass (Step 5). PPR runs over the KG (nodes N, edges E + E′) with restart probability 0.5 from
  that weighted vector, yielding a stationary distribution π⃗ high on nodes near several seeds
  (Step 6). Finally p⃗ = π⃗ · P scores each passage by the PPR mass of the nodes it mentions,
  weighted by mention counts, and the top-k passages are returned (Step 7). Exactly one LLM call
  and one graph computation — no per-hop loop.

  </details>

- [ ] You have run the companion experiment and reproduced mention-count ranking degrading to 9.21 mean rank at 2 hops while PPR stays at 1.00.

  <details><summary>Answer</summary>

  The companion crate ([experiments/src/kg.rs](experiments/src/kg.rs),
  [experiments/src/ppr.rs](experiments/src/ppr.rs)) builds synthetic path-finding instances: two
  seeds, eight distractor chains each, one shared answer, one passage per fact, and no passage
  mentioning both seeds. **Mention-count** ranking (vector RAG's shape — score each passage
  against the query independently) finds the answer at mean rank **1.00** at one hop, because
  the answer is named next to both seeds, but collapses to **9.21** at two hops — chance among
  17 candidates is (1+17)/2 = 9 — because every candidate's interior passages mention no query
  entity and all score zero ([FINDINGS.md](../../FINDINGS.md) row 38; notes.md). PPR restores
  rank 1.00 at one, two and three hops, because mass still sums at the meet node regardless of
  chain length. One PPR query on a 100k-node / ~400k-edge graph runs 30 power iterations in
  ≈ 56.6 ms (notes.md reference).

  </details>

- [ ] notes.md answers all 5 questions.

  <details><summary>Answer</summary>

  The five questions above are recorded and answered in notes.md, each tied to the paper section
  or table that settles it: the meet-node argument and the graph shape that defeats it (Step 6);
  the incremental-maintenance property of local specificity versus global IDF (Step 5, §2.3);
  index-recall monitoring without gold triples (the REBEL ablation, Table 5 / §5.1); which costs
  moved offline and when the offline bill dominates (Step 8, §4 / Appendix G); and what the
  specificity-vs-synonymy dataset split implies about entity-surface variety (§5.1).

  </details>

## References

- Gutiérrez et al., "HippoRAG: Neurobiologically Inspired Long-Term Memory for Large Language
  Models," NeurIPS 2024 — https://arxiv.org/abs/2405.14831. Section, table and figure numbers
  in this chapter are from that version.

| Where | What it settles |
|---|---|
| §2.1, Figure 1 | the hippocampal analogy; the path-finding example (answer "Professor Thomas" in the walkthrough) |
| §2.3 | two-step OpenIE; matrix P (`|N|×|P|`); query nodes R_q; PPR; node specificity sᵢ = |Pᵢ|⁻¹; scoring p⃗ = π⃗·P |
| §3.4 | GPT-3.5-turbo-1106, temperature 0; synonymy threshold τ = 0.8; PPR damping (restart) 0.5 |
| §4, Appendix G | 11/20% R@2/R@5 gain on 2Wiki; 10–30× cheaper, 6–13× faster than IRCoT |
| Table 2 | single-step R@2/R@5: MuSiQue 40.9/51.9, 2Wiki 70.7/89.1, HotpotQA 60.5/77.7 |
| Table 3 | IRCoT + HippoRAG: +4/+18/+1% R@5 |
| Table 4 | QA F1: up to +3 / +17 / +1 |
| Table 5 | ablations: REBEL collapse, Llama-3.1-70B, Rq-only, w/o specificity, w/o synonymy |
| Table 6 | all-recall: ColBERTv2 2Wiki AR@5 37.1 → HippoRAG 75.7 |
| §5.3, Table 7 | the path-finding case study; answer **Thomas Südhof** ranked 1st by HippoRAG; ColBERTv2/IRCoT fail |

- Companion experiment: [experiments/src/ppr.rs](experiments/src/ppr.rs) (PPR stub) and
  [experiments/src/kg.rs](experiments/src/kg.rs) (synthetic path-finding instances). Measured
  shape in [FINDINGS.md](../../FINDINGS.md) row 38: mention-count mean rank 1.00 at 1 hop, 9.21
  at 2 hops (chance = 9 among 17); the PPR reference scores 1.00 at 1/2/3 hops; one PPR query on
  a 100k-node / ~400k-edge graph with 30 power iterations ≈ 56.6 ms (notes.md).
- Encoders used by the paper: Contriever and ColBERTv2. Benchmarks: MuSiQue, 2WikiMultihopQA,
  HotpotQA; baselines include ColBERTv2 and IRCoT.
