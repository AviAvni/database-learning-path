# RelGo: what the flat-join lowering costs, and what a graph-aware optimizer buys back

The README's first diagram has one arrow that carries the whole topic:
`MATCH (a)-[]->(b)-[]->(c)` becomes `R ⋈ R ⋈ R`, "flat binary joins". The
bench lane measures that plan; the papers on factorized execution say how to
shrink what it builds. RelGo asks a different question: *given* that a
relational engine will be handed this pattern, how much of the damage is the
join **order and operators** the optimizer picks, and how much is the
representation? Reading it next to lane 1's numbers separates the two.

**"Towards a Converged Relational-Graph Optimization Framework"** — Yunkai
Lou, Longbin Lai, Bingqing Lyu, Yufan Yang, Xiaoli Zhou, Wenyuan Yu, Ying
Zhang, Jingren Zhou (Alibaba Group; Zhejiang Gongshang University).
*Proc. ACM Manag. Data* 2(6) (SIGMOD), Article 252, December 2024
([arXiv:2408.13480](https://arxiv.org/abs/2408.13480), v3 of 9 December 2024;
the PDF's own reference line gives the venue, <https://doi.org/10.1145/3698828>).
Page numbers below are the article's (`252:N`).

A second paper sits at the other end of the stack and is read in Step 8:
**"Reducing the Cross-Model Tax: Query Optimization over Multi-Model Data"** —
Jáchym Bártík, Filip Štrobl, Irena Holubová (Charles University),
[arXiv:2609.05014](https://arxiv.org/abs/2609.05014), v2 of 13 September 2026.
The PDF carries no venue line, so it is cited as the arXiv preprint it is.

## The problem in one sentence

SQL/PGQ hands a relational engine a graph pattern, and an optimizer that
treats it as an ordinary join query ("graph-agnostic") searches an
exponentially larger space, cannot see that vertex–edge joins are adjacency
lookups, and ends up with an edge-at-a-time binary-join plan whose
intermediate results are the many-to-many blow-up of README Step 1.

## The concepts, step by step

### Step 1 — SPJ, SPJM and the matching operator

> **In:** nothing. **Out:** the vocabulary of the paper, in the order the
> later steps use it.

A relational optimizer's working language is **SPJ**: Select, Project, Join —
a query is a tree of those three operators over tables. SQL/PGQ adds one more
box: `GRAPH_TABLE (g MATCH pattern COLUMNS …)`, which takes a *property graph*
and returns a table of matches. RelGo names the extended skeleton **SPJM**
(M = matching, §1, p. 252:3). Four definitions carry the rest:

- **RGMapping** (§2.1, p. 252:5) — the rule that turns tables into a graph:
  every row of a *vertex relation* is a vertex, every row of an *edge
  relation* is an edge, and two **total functions** λˢ and λᵗ (usually a
  primary/foreign-key pair) send each edge row to its source and target vertex
  row. *Total* matters later: every edge has exactly one source and one
  target, so joining an edge row to its endpoint never changes the row count.
- **Pattern graph P** — the `MATCH` shape, a small graph with labels.
- **Matching operator M(P)** (§2.2) — all subgraphs of the data graph
  homomorphic to P. A **homomorphism** may map two pattern vertices to the same
  data vertex (an *isomorphism* may not); RelGo handles isomorphism by an
  extra all-distinct filter (Eq. 2 discussion, p. 252:7).
- **Graph relation** — M(P)'s output: a table whose columns are pattern
  vertices and edges.

The lane's `R(src, dst)` is a single edge relation whose two endpoint
columns point into an implicit vertex set `0..|V|`; the 3-hop pattern is P =
`a→b→c→d` with all four labels equal.

### Step 2 — the graph-agnostic lowering, as a lemma

> **In:** Step 1's RGMapping and homomorphism. **Out:** the exact join
> sequence a graph-agnostic engine runs for a path pattern, and why it is
> lossless.

**Lemma 1** (§3.1.1, p. 252:8): under an RGMapping, the matching operator
can be rewritten as a sequence of relational joins over *n* vertex relations
and *m* edge relations. The proof is an induction on the number of pattern
edges, and the whole construction is one equation (Eq. 4):

```
πÂ*(M(P_k)) = R'_k ⋈ R'_{k-1} ⋈ … ⋈ R'_1 ⋈ R_0,
where R'_i = R_ℓ(u_si) ⋈ᵉᵛ R_ℓ(e_i) ⋈ᵉᵛ R_ℓ(u_ti)
```

R₀ is the starting vertex relation. Each R′ᵢ is one pattern edge joined to
its two endpoint vertex relations by **EVJoin** — an equi-join on the
λ function (Eq. 3). Symbols: R_ℓ(x) is the relation whose label is the label
of pattern element x; u_si and u_ti are edge i's source and target; ⋈ᵉᵛ is
EVJoin; ⋈ with no decoration is the natural join on shared pattern vertices.

Worked on the lane's 3-hop pattern `a→b→c→d` (3 edges, 4 vertices). Start from
R₀ = V (the vertex relation for a). Adding edge 1 contributes V ⋈ᵉᵛ E ⋈ᵉᵛ V,
and the next two edges contribute E ⋈ᵉᵛ V each (the repeated vertex relation
is "redundant and can be removed", Example 4, p. 252:9). The plan is seven
relations — 4 vertex relations and 3 edge relations — and six joins. Four of
them are vertex–edge joins (one per edge endpoint that is not shared). Because
λ is total, each of those emits exactly as many rows as the edge relation it
touches. The other two join consecutive edges on a *shared pattern vertex* (b,
then c); those are the many-to-many joins, and they are the only ones whose
output can exceed |E|. That is why `pgq_bench` can leave the vertex table out
and still count the right tuples.

The lane's own flat plan is this sequence with the vertex joins elided
(`flat.rs:75–97`):

```rust
// flat.rs, topic 46 crate — lines 75–87 shown; the rest of `extend` is 88–97
75  pub fn k_hop(edges: &Edges, ht: &HashMap<u32, Vec<u32>, FxBuild>, k: usize, sink: &mut u64) -> JoinStats {
76      assert!(k >= 2);
77      let mut st = JoinStats { out: vec![0; k - 1], probes: 0 };
78      for &(_a, b) in edges {
79          extend(ht, b, 0, k - 1, &mut st, sink);
80      }
81      st
82  }
83  
84  fn extend(ht: &HashMap<u32, Vec<u32>, FxBuild>, key: u32, join: usize, joins: usize, st: &mut JoinStats, sink: &mut u64) {
85      st.probes += 1;
86      let Some(matches) = ht.get(&key) else { return };
87      st.out[join] += matches.len() as u64;
```

The lines to look at are 78–79 and 85–87: the scan of `R` is the probe side
of join 1, and every probe `ht.get(&key)` returns *all* matches of that key,
which is `out(key)` tuples — line 87 adds them to the operator's output
counter. `st.out[join]` is the number the README's table calls "flat tuples".

(Line numbers are this repository's `flat.rs`, checked by opening the file.)

### Step 3 — what that lowering costs, from the lane

> **In:** Step 2's join sequence and the lane-1 table in the README. **Out:**
> the cost split this guide leans on: representation vs access path.

The lane output already contains the cost. With |V| = 40,000 and 313,512
edges, the 3-hop plan's two many-to-many joins emit 47,900,847 (join 1) and
2,224,823,394 (join 2) tuples — 2,272,724,241 in all, 9,042,996,117 values —
against 47,997,149 values in the factorized representation (188×). The
arithmetic of the 2-hop count is the formula `Σ_b in(b)·out(b)`; on the
paper-sized example (lane 1a, ten edges) vertex 2 has 3 in-edges and 3
out-edges (9 tuples), vertex 3 has 1 and 2 (2 tuples), vertex 4 has 2 and 1
(2 tuples), every other vertex has no in-edge or no out-edge: 9 + 2 + 2 = 13,
which matches the 13 tuples the bench prints and Yasser et al. §2.2.1.

Two costs are separate things, and the lane isolates them:

| plan | tuples enumerated | ms at |V| = 40,000, 3-hop | what changed |
|---|---|---|
| hash join (graph-agnostic) | 2,272,724,241 | 624.2 | — |
| CSR expansion (adjacency) | 2,272,724,241 | 464.3 | access path only: 1.34× |
| factorized count | — (47,997,149 values) | 0.45 | representation: ~1,000× on the count |

(Timings are the README's run A, load 4.28; the ratios, not the
milliseconds, are the claim. 624.2 / 464.3 = 1.344. 464.3 / 0.45 = 1,032.)

So "the cost of the flat-binary-join lowering" is two numbers: a *graph index*
(adjacency instead of a hash table) removes a constant factor of about 1.3;
only a different *representation* changes the count of what is built.

### Step 4 — the graph index and the three physical operators

> **In:** Step 3's split. **Out:** what RelGo's EXPAND_EDGE, GET_VERTEX and
> EXPAND_INTERSECT do, and which of them the lane's CSR plan is.

RelGo borrows GRainDB's **predefined join** as a **graph index** (§3.2.1,
pp. 252:11–12, Fig. 5): an **EV-index** adds to each edge row the row ids of
its two endpoint rows (`pid_rowid`, `mid_rowid` in Fig. 5a), so an edge reaches
its vertices without a hash lookup; a **VE-index** gives each vertex row the
row ids of its edges and their far endpoints in a CSR layout (Fig. 5b). A
row id is just the array position, so both are the CSR of this topic's
`Graph`, attached to tables.

Three physical operators use it (§3.2.2, pp. 252:12–13):

- **Case I** — a join of two intermediate graph relations: an ordinary hash
  join on vertex/edge ids.
- **Case II** — `EXPAND_EDGE` looks up a vertex's VE-index to get its
  adjacent edge rows; `GET_VERTEX` follows the EV-index to the far vertex.
  "To obtain M(P′), we flatten the adjacent edges and vertices and pair them
  up" (p. 252:13): the operators emit one tuple per neighbour — **flat
  tuples**.
- **Case III** — a *complete star* (a vertex whose every other pattern
  neighbour is already bound) uses `EXPAND_INTERSECT`: expand each bound
  neighbour's adjacency and intersect the lists, which is a worst-case-optimal
  (wco) join step. Without a graph index it degrades to joining the single
  edges one after another.

For a path pattern there is never a bound neighbour on both sides of the
vertex being added, so a path is all Case II. The lane's `expand.rs` *is* that
plan: for each edge `(a,b)`, fetch `b`'s adjacency slice, emit one tuple per
neighbour, recurse. The paper's operators change the access path, not the
number of tuples — which is exactly the 1.34× row of Step 3's table. I found
no mention of factorized or compressed intermediate results in the paper's
text (a search for "factor" across §§1–7 returns nothing); its related-work
list names compression only for Ullmann-style matching (§6).

### Step 5 — the decomposition tree and why the search space shrinks

> **In:** Step 4's operators. **Out:** the paper's search-space claim
> (Theorem 1), and what it does and does not say about a path.

A graph-aware optimizer does not enumerate join orders; it enumerates
**decomposition trees** (§3.1.2, p. 252:9). A pattern P splits into two
overlapping sub-patterns P₁ and P₂ sharing vertices V_o and edges E_o,
`M(P) = M(P₁) ⋈ M(P₂)` (Eq. 2); the leaves are **minimum matching
components** (MMC): a single vertex or a complete star. To keep the plan
worst-case optimal every intermediate sub-pattern must be an **induced
subgraph** of P (all data edges among its vertices present). These
constraints are what shrink the space: **Theorem 1** (p. 252:11) says the
graph-aware space can be exponentially smaller than the graph-agnostic one.

The paper measured, not just proved, it: on a path graph of *m* edges it
programmed an enumerator for both spaces (Fig. 4a), and compared optimization
time of Apache Calcite (graph-agnostic) with RelGo (built on Calcite) on the
LDBC queries, 10-minute cap (§3.1.4, p. 252:11). RelGo finishes "almost all
queries within 10–100 milliseconds"; on IC5-1 it is "more than 10⁴ times
faster" than Calcite (the PDF text extraction prints "104"; the page image
shows 10⁴). Calcite often exceeds the cap on JOB, so JOB is not reported.

Two cautions for the reader of this topic. First, the comparison is against a
generic Volcano-style planner *without* the aggressive pruning DuckDB has
(p. 252:11 says so explicitly); with pruning, RelGo's optimization was
slightly *slower* than GRainDB's (§5.2, p. 252:19: "RelGo incurs a slightly
higher optimization cost"). Second, a path pattern is the easy case for any
optimizer — the join graph is a chain and the order is nearly forced — so the
exponential gap is a statement about dense patterns (cliques, cycles), not
about the lane's 2- and 3-hop paths.

### Step 6 — the heuristic rules: filters and projections cross the boundary

> **In:** Step 4's SCAN_GRAPH_TABLE operator. **Out:** the two rules that let
> a relational predicate or column list reach into the matching operator.

The optimizer treats the whole pattern as one operator, `SCAN_GRAPH_TABLE`
(Fig. 6, p. 252:14), and then optimizes the relational part around it with
Calcite. Two rules move information *across* that boundary (§4.2.3, §4.3):

- **FilterIntoMatchRule** pushes a relational selection (`p1.name = 'Tom'`)
  into the pattern as a constraint on the pattern vertex, so the matching
  operator starts from the matching person instead of all of them. The paper
  applies it greedily, before graph optimization so the cost model sees the
  constraint, and notes it is "actually a global optimization rule": pushing a
  predicate in does not always give a better plan (§4.2.3, p. 252:16).
- **TrimAndFuseRule** first checks whether anything downstream reads the
  *edge* columns an EXPAND_EDGE produces (a property projection, a filter on an
  edge attribute); if nothing does, the edges are trimmed and EXPAND_EDGE and
  GET_VERTEX fuse into a single EXPAND that reads neighbouring vertices
  straight from the VE-index (§4.2.3, p. 252:16).

Their measured effect (§5.2, p. 252:20, Fig. 8): FilterIntoMatchRule gives an
average speedup of 299.4× on LDBC10 and 699.8× on LDBC30 (queries QR1, QR2);
TrimAndFuseRule 2.0× and 2.3× (QR3, QR4). The ratio *grows* with the data
because the filtered plan's work does not scale with the dataset while the
unfiltered one's does — the same shape as lane 1's ratio growing with |V|.
These are the largest numbers in the paper and they are not about join
algorithms at all; they are about not building what the query throws away.

### Step 7 — the evaluation, and how far its numbers transfer

> **In:** Steps 4–6. **Out:** the headline comparisons, with the setup that
> bounds them.

**Setup** (§5.1, pp. 252:17–18): LDBC SNB scale factors 10, 30, 100 (the
largest has 282 million vertex tuples and 938 million edge tuples), ten
Interactive-complex queries (variable-length paths split into fixed-length
ones, suffix `-l`), and the 33 "a" variants of the Join Order Benchmark.
All systems except Kùzu use **DuckDB v0.9.2** as the execution engine and
differ only in optimizer: DuckDB itself (graph-agnostic), GRainDB (same
optimizer, plus the graph index), Umbra's plans run on DuckDB, and RelGo.
Single thread, Xeon E5-2682 at 2.5 GHz, 256 GB; LDBC queries run 50 times,
JOB 10 times; 10-minute timeout.

**Headline results** (§5.3, pp. 252:21–23, execution time of the plans):

| comparison | LDBC100 | JOB |
|---|---|---|
| RelGo vs DuckDB | 21.9× average | 8.2× |
| RelGo vs GRainDB (index, no graph optimizer) | 5.4× | 4.0× |
| RelGo vs Umbra's plans | 49.9× | 1.7× |
| RelGo vs Kùzu | 188.7× | 136.1× |
| IC7 (contains a cycle): vs DuckDB / GRainDB | 76.3× / 22.0× | |

Read against Step 3: the paper's baseline *is* the flat plan, and its
speedups come from (a) avoiding materialized intermediates by pushing
filters into the match (Step 6), (b) a join order that keeps expanding along
the index (the JOB17 case study, §5.4: RelGo scans KEYWORD, then expands to
TITLE, COMPANY_NAME, NAME, while GRainDB "misses the opportunity to
immediately join TITLE"), and (c) wco `EXPAND_INTERSECT` for cycles
(Fig. 9: 1.22× average on LDBC10 and 1.31× on LDBC30 over the multi-join
variant, and the 4-clique query QC3 runs out of memory without it). None of
the three is a reduction of what a *path* join enumerates. The paper's authors
also report where RelGo loses: JOB30 is 1.2× slower than Umbra because RelGo
has no attribute-value distributions for cardinality estimation (§5.3.2).

What does not transfer to this repo's lane: the 21.9× is relative to DuckDB's
optimizer on queries that mix selective filters, joins and cycles, with the
whole pipeline in one engine. Lane 1 has a single unfiltered, acyclic,
all-vertices pattern, where Steps 4–5 predict RelGo's plan is the CSR
expansion, 1.34× faster than the hash join, not 21.9×. That prediction is
about plan shape; the paper does not run this pattern, and it should not be
read as a measurement of RelGo.

### Step 8 — a different layer: the cross-model tax

> **In:** Step 6's pushdown idea. **Out:** what predicate pushdown and
> dependent joins look like when the "engine" is a federation of databases.

Bártík, Štrobl and Holubová study **decomposition-based multi-model
processing** (§2): a unifying processor (their MM-quecat) splits a query over
a PostgreSQL + MongoDB + Neo4j deployment into native queries, fetches their
results and joins the rest itself. The **cross-model tax** is the measurable
planning, retrieval, conversion and unifying-layer cost of that architecture
(§3.1: four components, and the paper says it is "not introduced as a
universal cost metric"). Their three optimizations are textbook ideas applied
across the boundary:

- **Predicate pushdown** (§3.2): a filter moves into a native query only if
  the mapping provides every column it uses *and* the target builder can
  express the whole condition. An input of N records and a filter of
  selectivity ρ shrinks from N to about ρN records before transfer
  (§3.2.2). With N = 100,000 bookings and one matching row, the unpushed plan
  moves 100,000 records; the pushed one moves one (their Example 5).
- **Dependent joins** (§3.3): evaluate one input first and use its join
  values to restrict the other native query — a semi-join across systems
  (compare topic 47). It is skipped when the generated restriction would
  exceed a size limit (Q12: both inputs hold several thousand records).
- **Non-redundant query-part construction** (§3.4): avoid exploring several
  insertion orders that build the same native query, which cuts planning time
  for Neo4j plans of up to eight kinds from "hundreds of milliseconds to
  several milliseconds" (abstract; §5.5).

The measurements (§5.3): pushdown gives "up to two orders of magnitude" in
the best query/environment combinations and removes every out-of-memory
failure seen in the unoptimized single-DBMS runs; dependent joins add
"approximately one order of magnitude" at most, only where joins stay in the
unifying layer; both maxima are explicitly not representative of every query.
Twenty read-only queries, one machine, containerized: this is a prototype
evaluation, not a benchmark of Neo4j.

Why it belongs next to RelGo: both papers find that the dominant cost is
*what crosses a boundary before it is filtered* — RelGo's boundary is
relational↔matching operator (FilterIntoMatchRule), theirs is DBMS↔DBMS.
It does not bear on the flat-join blow-up itself, which happens inside one
engine.

### Step 9 — the same pattern on the C engine

> **In:** Step 3's lane-1 graph (seed 46) and `experiments/cengine.py`.
> **Out:** how FalkorDB, which is neither a relational engine nor a
> factorized one, evaluates the lane's pattern. Numbers are in
> [notes.md](notes.md), section "C FalkorDB on the same graph".

FalkorDB is a graph engine whose adjacency is a GraphBLAS matrix. Loaded
with exactly lane 1's edges, it answers the question "does a graph engine
avoid the flat-join blow-up?" with two evaluation strategies, selected by
*which pattern variables the query mentions*, that give different answers.

Four findings, with the numbers in [notes.md](notes.md) (|V| = 10,000 and
20,000 of lane 1b, server-side time, best of two):

1. **It enumerates flat tuples, at engine speed.** When the query uses every
   hop variable the plan is a chain — `All Node Scan (a)`, then one
   `Conditional Traverse` per hop, then `Aggregate` — and the counts equal the
   bench's exactly (234,636,483 3-hop paths at 10,000; 721,738,265 at 20,000).
   It takes 30,091.5 ms for the first and 87,416.0 ms for the second, about
   120–130 ns per row. The Rust crate's bare loops did the same enumeration at
   0.2–0.35 ns per tuple (53.6 ms for the CSR expansion at 10,000): the C
   engine's per-row cost is a real engine's cost per flat tuple, 300–600× the
   lower bound the README warns about. That is the price of the flat plan
   *plus* an execution engine, and it is what a factorized intermediate (M46)
   removes before it removes any join.
2. **Its access path is the CSR plan of Step 4.** No hash join, no index
   lookup: each hop multiplies the frontier by the adjacency matrix
   (Case II without a relational layer). RelGo's graph index is what a
   relational engine needs to *reach* this plan; a graph engine starts there,
   and still pays Step 3's representation cost.
3. **Mention only the endpoints and the plan changes shape.** With the
   intermediate variables unused, `GRAPH.EXPLAIN` shows a single
   `Conditional Traverse | (a)->(v3)` — the whole chain folded into one
   algebraic expression over a boolean semiring — and `count(*)` returns the
   number of distinct endpoint pairs (45,794,367 at 3-hop/10,000), not the
   number of paths (234,636,483). I confirmed both counts independently with a
   dense matrix product over the same edge list. It is 12.2× faster (2,474.7
   ms) for 5.1× fewer rows. Set semantics collapses the middle the way a
   factorization does, but by changing the answer rather than the
   representation; naming an intermediate variable anywhere in the query
   restores the bag count.
4. **The lesson for RelGo's evaluation.** RelGo's baselines all return the
   bag. The C engine shows that the same pattern has *two* valid-looking
   answers depending on the query's wording, and that the cheap one is the
   set. Step 7's "21.9×" and this guide's 188× are both about the bag.


## How to read the paper (with the concepts in hand)

1. **§1 and Fig. 1–2** — SPJM and RGMapping (Step 1). The introduction's
   Fig. 1 query (two persons liking the same message, plus a join to Place)
   is the smallest example with a cycle.
2. **§3.1.1** — Lemma 1 and Example 4 (Step 2). Redo Example 4 for the 3-hop
   path to confirm seven relations.
3. **§3.1.2–3.1.4** — decomposition trees, Remark 1's three drawbacks,
   Theorem 1 and Fig. 4 (Step 5).
4. **§3.2** — graph index and the three physical cases (Step 4); Fig. 5 is the
   CSR.
5. **§4** — Fig. 6's workflow; skip GLogue statistics unless you plan to read
   the cardinality-estimation topic.
6. **§5.2–5.3** — Figs. 7–11 (Steps 6–7); keep the Setup paragraph in view
   when quoting any speedup.
7. For Step 8, read the cross-model paper's §3.1 and §5.3 only; its §2 is a
   schema-category formalism that needs its own guide.

## Where each step lives in this topic

| step | where | what to look at |
|---|---|---|
| 2 | `experiments/src/flat.rs:75–97` | the flat left-deep plan Lemma 1 produces, vertex joins elided |
| 3 | README, "The problem, measured" | flat tuples, values, f-rep values, hash vs CSR vs factorized ms |
| 4 | `experiments/src/expand.rs:31–55` | the Case II expansion: one adjacency slice per bound vertex |
| 4 | `experiments/src/factorized.rs`, `fsize.rs` | the representation RelGo's operators do not build |
| 9 | `experiments/cengine.py`, `src/bin/dump_edges.rs` | the harness that loads the lane's graph into the C engine |

## Questions (answer in notes.md)

1. For the 3-hop path, write Eq. 4 out with every relation named, then say
   which of its six joins can change a row count. (Hint: Step 2, total λ.)
2. A triangle `a→b→c→a` has one complete star. Which vertex is the star's
   root, which edges does `EXPAND_INTERSECT` intersect, and what does a hash-
   join plan build instead? Count the intermediate tuples on lane 1a's graph
   by hand.
3. FilterIntoMatchRule's speedup grows from 299.4× (LDBC10) to 699.8×
   (LDBC30). What does that say about the work of the unfiltered plan as a
   function of data size, and what would the same two numbers be for a
   *factorized* unfiltered plan?
4. RelGo flattens on `EXPAND_EDGE`. Sketch the change that would let
   `EXPAND_EDGE` return the adjacency list as one value (the VE-index entry)
   and defer the cross product; which later operator in Cases I–III cannot
   accept it?
5. The cross-model paper's pushdown shrinks N to ρN. Where in FalkorDB's
   pipeline would a filter on `a.id` have to be to reach the first scan, and
   what do Step 9's two plans tell you about when it can?

## Done when

Answer each before unfolding it.

- [ ] You can state what Lemma 1 builds for a path pattern and why the vertex joins do not change the count.

  <details><summary>Answer</summary>

  A 3-edge path with 4 pattern vertices becomes 4 vertex relations and 3
  edge relations, joined as V ⋈ᵉᵛ E ⋈ᵉᵛ V followed by E ⋈ᵉᵛ V twice (Eq. 4,
  Example 4; the repeated vertex relations are redundant and dropped). An
  EVJoin joins an edge row to the vertex row that λ names; λ is a total
  function (§2.1), so every edge row has exactly one partner and the join
  emits |E| rows. Only the joins on a *shared pattern vertex* (b, then c) are
  many-to-many, which is why the lane can drop the vertex table and still
  count the same tuples.

  </details>

- [ ] You can separate the two costs of the flat lowering and give the lane's number for each.

  <details><summary>Answer</summary>

  Access path: replacing the hash probe by an adjacency slice leaves the
  tuple count unchanged (2,272,724,241 at |V| = 40,000, 3-hop) and cut the
  time from 624.2 ms to 464.3 ms, 1.34×. Representation: the factorized
  form holds 47,997,149 values against 9,042,996,117 flat values, 188×, and
  counting from it took 0.45 ms. RelGo's graph index is the first kind of
  improvement; its operators emit flat tuples (§3.2.2, Case II), so it does
  not change the second.

  </details>

- [ ] You can say which of RelGo's three techniques applies to a path pattern with no filter.

  <details><summary>Answer</summary>

  Only the decomposition tree's single-edge expansion (Case II): a path
  has no complete star, so `EXPAND_INTERSECT` never fires, and with no
  filter or unused column, FilterIntoMatchRule and TrimAndFuseRule have
  nothing to move. The plan is the nested CSR expansion, which the lane
  measures at 1.30–1.35× over the hash join. The paper's 21.9× average
  (§5.3.1) comes from queries with selective filters, joins and cycles, so it
  is not a prediction for this pattern.

  </details>

- [ ] You can explain why Theorem 1 does not make a 3-hop path cheaper to plan.

  <details><summary>Answer</summary>

  Theorem 1 says the graph-aware search space can be exponentially smaller
  (§3.1.3), shown on a path of m edges against Calcite with no pruning
  (§3.1.4, Fig. 4). With DuckDB-style pruning the graph-agnostic side is
  already cheap, and the paper reports RelGo's optimization time as slightly
  higher than GRainDB's (§5.2). A chain's join order is nearly forced, so the
  gap is a statement about dense patterns, not a 3-hop path.

  </details>

- [ ] You can state what the cross-model paper's predicate pushdown does to the transferred volume and what its limits are.

  <details><summary>Answer</summary>

  A filter of selectivity ρ over N records reduces the retrieved result from
  N to about ρN (§3.2.2) — in their Example 5, about 100,000 booking records
  down to one. It applies only when the mapping provides every referenced
  value and the native query builder can express the whole condition; the
  implementation does not push part of a compound condition. The "up to two
  orders of magnitude" is a maximum over 20 queries and four environments and
  the paper says it is not representative of every query (§5.3.1).

  </details>

## References

- Yunkai Lou, Longbin Lai, Bingqing Lyu, Yufan Yang, Xiaoli Zhou, Wenyuan Yu,
  Ying Zhang, Jingren Zhou, **"Towards a Converged Relational-Graph
  Optimization Framework"**, PACMMOD 2(6) (SIGMOD), Article 252,
  Dec 2024, arXiv:2408.13480 v3 — §2 (RGMapping, matching), §3.1 (lowering,
  decomposition trees, Theorem 1), §3.2 (graph index, physical operators),
  §4 (workflow, rules), §5 (evaluation).
- Jáchym Bártík, Filip Štrobl, Irena Holubová, **"Reducing the Cross-Model
  Tax: Query Optimization over Multi-Model Data"**, arXiv:2609.05014 v2,
  Sep 2026 — §3.1–3.4, §5.3.
- Guodong Jin and Semih Salihoglu, **"Making RDBMSs Efficient on Graph
  Workloads Through Predefined Joins"**, PVLDB 15(5), 2022 — GRainDB, the
  predefined join RelGo's index follows (RelGo's reference [23]); not read for
  this guide.
- Previous: [reading-postgres-sqlpgq.md](reading-postgres-sqlpgq.md). Related:
  [reading-duckpgq.md](reading-duckpgq.md) (the graph-agnostic lowering
  implemented), [reading-lehner-neumann-factorized.md](reading-lehner-neumann-factorized.md)
  (the representation RelGo does not use).
