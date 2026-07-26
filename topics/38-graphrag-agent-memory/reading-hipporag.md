# HippoRAG: pattern completion over a sparse association index, not iterative retrieval

HippoRAG (Gutiérrez et al., NeurIPS 2024, arXiv 2405.14831) asks why RAG systems need an LLM
loop to answer multi-hop questions when the brain retrieves associated memories in a single
pass. Its answer is an architecture copied from hippocampal memory indexing theory: keep the
content in a passage store, keep only a sparse index of associations in a knowledge graph, and
answer queries by spreading activation — Personalized PageRank — through that index. For a
graph-database developer the payoff is concrete: multi-hop retrieval becomes one cheap graph
query instead of N LLM calls, and the paper measures the difference in recall, dollars, and
latency.

## The problem in one sentence

**Single-step retrievers and iterative LLM retrievers both fail path-finding multi-hop
questions — questions whose answer is the one entity associated with several query entities
when no single passage mentions them together — because they chain lookups instead of
aggregating association strength across a corpus-wide index.**

Path-following questions can be solved hop by hop (each hop's answer names the next clue).
Path-finding questions cannot: the connection only exists as a pattern across passages, so
retrieval must complete the pattern, not follow a chain.

## The concepts, step by step

### Step 1 — The hippocampal memory indexing analogy

The theory separates where memories live from how they are found. HippoRAG maps each brain
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

The key claim: the hippocampus stores no content, only the index. Pattern completion —
retrieving a whole memory from a partial cue — happens by spreading activation through the
index. HippoRAG implements exactly that split.

### Step 2 — Offline indexing: two-step OpenIE into a schemaless graph

For each passage, an LLM (GPT-3.5-turbo-1106, temperature 0) performs one-shot OpenIE in two
steps: NER first, then triple extraction with the extracted entities included in the prompt.
The result is a schemaless knowledge graph whose nodes are noun phrases — no fixed ontology,
no entity resolution pipeline. Two artifacts matter downstream:

- Synonymy edges: connect nodes whose embedding cosine similarity exceeds τ = 0.8. This is
  the parahippocampal analogue — cheap fuzzy matching welded into graph structure.
- Matrix P (|N|×|P|): counts node-in-passage occurrences, linking index nodes back to the
  content they came from.

### Step 3 — Online retrieval: one PPR query, no LLM loop

```
  query --LLM NER--> named entities --embedding cosine--> query nodes R_q
                                                              |
                                              restart mass ONLY on R_q
                                                              v
                                          Personalized PageRank (damping 0.5)
                                                              |
                                                       stationary π⃗
                                                              v
                                        passage score = π⃗ · P  --> top-k passages
```

Extract named entities from the query with the LLM, map each to graph nodes by embedding
cosine, then run PPR with restart mass only on those query nodes and damping factor 0.5.
Passage score is π⃗·P — PPR mass summed over the nodes each passage mentions. A single graph
query replaces the per-hop LLM calls of iterative retrievers.

### Step 4 — Why PPR solves path-finding: mass sums at the meet node

Consider the paper's case study: "Which Stanford professor works on the neuroscience of
Alzheimer's?" — no passage mentions both Stanford and Alzheimer's together with the answer.

```
   [Stanford] ----seed----.                 .----seed---- [Alzheimer's]
        \                  \               /                  /
      (edges to           mass flows    mass flows        (edges to
       other nodes)          \           /                 other nodes)
                              v         v
                        [Thomas Südhof]   <-- PPR mass from BOTH seeds SUMS here
                              |
                    passages mentioning him rank first
```

Mass restarts at both query entities and diffuses along edges; the one node connected to both
accumulates the sum and outranks every node reachable from only one seed. ColBERTv2 and IRCoT
both fail this example; HippoRAG ranks Thomas Südhof first. This is association, not chaining
— the structural reason iterative retrieval cannot fix the problem by looping harder.

### Step 5 — Node specificity: local IDF without global statistics

Common nodes ("USA") would flood the graph with restart mass. HippoRAG multiplies each query
node's restart probability by sᵢ = |Pᵢ|⁻¹ — the inverse of the number of passages mentioning
node i. It behaves like IDF but is computable per node without global corpus statistics, which
the authors argue is neurobiologically plausible and which a database developer will recognize
as an update-friendly property: adding a passage touches only the counts of the nodes it
mentions.

### Step 6 — The two-pipeline view: index once, query cheap

```
  OFFLINE (per passage, LLM-priced)          ONLINE (per query, graph-priced)
  ---------------------------------          --------------------------------
  passage                                    query
    | LLM NER                                  | LLM NER (one call)
    | LLM triple extraction                    | cosine match -> R_q
    v                                          | weight by specificity s_i
  triples -> KG nodes/edges                    v
    | cosine > 0.8 -> synonymy edges         PPR (damping 0.5) -> π⃗
    v                                          v
  matrix P (node × passage counts)           score = π⃗ · P -> ranked passages
```

All LLM-heavy work moves offline. Online retrieval is 10-30× cheaper and 6-13× faster than
IRCoT, which loops an LLM per hop. This is the classic database trade: pay at write/index
time to make reads a single index probe.

### Step 7 — What the numbers say

Retrieval (recall@2 / recall@5): MuSiQue 40.9/51.9, 2WikiMultihopQA 70.7/89.1, HotpotQA
60.5/77.7 — against ColBERTv2 on 2Wiki that is roughly +11% R@2 and +20% R@5. The all-recall
metric (fraction of questions where ALL supporting passages are retrieved) jumps 37.1 → 75.7
AR@5 on 2Wiki. QA improves up to +3 F1 (MuSiQue), +17 F1 (2Wiki), +1 F1 (HotpotQA). HippoRAG
also composes: plugging it in as IRCoT's retriever adds about +4% (MuSiQue), +18% (2Wiki),
+1% (HotpotQA) R@5 over IRCoT alone.

### Step 8 — Ablations: where the design earns its keep

- Extractor quality dominates: replacing LLM OpenIE with REBEL (a small fine-tuned extraction
  model) drops recall sharply — GPT-3.5 produces about 2× as many triples as REBEL. Recall of
  the index bounds recall of retrieval.
- Llama-3.1-70B as extractor outperforms GPT-3.5 on 2 of 3 datasets — open models suffice.
- PPR strongly beats using query nodes only, or query nodes plus their direct neighbors:
  multi-hop diffusion is doing real work, not just neighborhood lookup.
- Node specificity helps MuSiQue and HotpotQA; synonymy edges help 2Wiki most.

## How to read the paper (with the concepts in hand)

1. §1 — read the introduction for the path-finding motivation figure; hold Step 1 and
   Step 4 in mind: the paper's whole bet is that association beats chaining.
2. §2.1 — the neurobiological framing; check the component mapping against the Step 1
   diagram and note the "index stores no content" claim.
3. §2.2 — offline indexing; verify the two-step OpenIE order (NER, then triples with
   entities in the prompt), τ = 0.8 synonymy edges, and matrix P (Step 2).
4. §2.3 — online retrieval; trace the Step 3 pipeline and find where node specificity
   (Step 5) multiplies restart probabilities.
5. §3 — experimental setup: MuSiQue, 2WikiMultihopQA, HotpotQA; baselines including
   ColBERTv2 and IRCoT; note Contriever/ColBERTv2 double as HippoRAG's encoders.
6. §4 — results; check the Step 7 numbers, especially the all-recall jump and the
   IRCoT + HippoRAG combination.
7. §5 — discussion: the ablations of Step 8 and the Stanford/Alzheimer's path-finding
   case study of Step 4; the appendices hold the prompts and further ablations if you
   want to reproduce the extraction.

## Questions to answer in notes.md

1. Why does PPR mass from two query entities summing at a shared node solve path-finding
   questions that iterative chaining cannot — and what graph shape would defeat it?
2. Node specificity sᵢ = |Pᵢ|⁻¹ is a local IDF analogue. What does locality buy you for
   incremental index maintenance compared to true corpus-level IDF?
3. The REBEL ablation shows extraction recall bounds retrieval recall. How would you
   monitor index recall in production without gold triples?
4. Online retrieval is 10-30× cheaper and 6-13× faster than IRCoT. Which costs moved
   offline to make that possible, and when does the offline bill dominate?
5. Synonymy edges (cosine above τ = 0.8) help 2Wiki most while specificity helps MuSiQue
   and HotpotQA. What does that split suggest about the datasets' entity-surface variety?

## Done when

- [ ] You can draw the brain-to-component mapping from memory and state what the
      hippocampus analogue does and does not store.
- [ ] You can explain path-finding vs path-following and why the Südhof example defeats
      ColBERTv2 and IRCoT but not PPR.
- [ ] You can write the online scoring pipeline end to end: query NER → R_q → specificity
      weights → PPR (damping 0.5) → π⃗·P.
- [ ] You have run the companion experiment and reproduced mention-count ranking degrading
      to 9.21 mean rank at 2 hops while PPR stays at 1.00.
- [ ] notes.md answers all 5 questions.

## References

- Gutiérrez et al., "HippoRAG: Neurobiologically Inspired Long-Term Memory for Large
  Language Models," NeurIPS 2024 — https://arxiv.org/abs/2405.14831
- Local copy of the PDF: /tmp/hipporag.pdf
- Companion experiment in this repo: [experiments/src/ppr.rs](experiments/src/ppr.rs)
  (PPR stub) and [experiments/src/kg.rs](experiments/src/kg.rs) (synthetic path-finding
  instances).
- Companion measurements: mention-count ranking (vector RAG's shape) gets mean rank 1.00
  at 1 hop but 9.21 at 2 hops (chance = 9.0 among 17 candidates); the PPR reference scores
  1.00 at 1, 2, and 3 hops; one PPR query on a 100k-node / ~400k-edge graph with 30 power
  iterations ≈ 57 ms.
- Retrieval encoders used by the paper: Contriever and ColBERTv2.
- Extraction model: GPT-3.5-turbo-1106 at temperature 0; Llama-3.1-70B tested in
  ablations, outperforming GPT-3.5 on 2 of 3 datasets.
- Benchmarks used: MuSiQue, 2WikiMultihopQA, HotpotQA; baselines include ColBERTv2
  and IRCoT.
