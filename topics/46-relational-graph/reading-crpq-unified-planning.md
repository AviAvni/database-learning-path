# CRPQ planning: one algebra for the joins and the Kleene stars

A Cypher or GQL `MATCH` mixes two things: fixed edges, which are joins, and
variable-length edges (`-[:KNOWS*1..]->`), which are regular path queries.
Pang, Zou, Bonifati, Özsu & Zhou (VLDB'26) observe that every engine they
survey plans the two halves with separate machinery and glues them together in
a fixed order — and that a single algebra, with operators that can interleave
a Kleene closure inside a multi-way join, finds plans no existing system can
express. This chapter defines RPQs and CRPQs from zero, connects the two ways
of evaluating an RPQ to this topic's lane 3 (where they turn out to do the
same number of steps), builds the six operators and their rewrite rules, works
the cost model's formulas on numbers, and reads Table 5 — including what its
"average speedup" averages, and the benchmark where timeouts did not move.

Everything quoted is from PVLDB 19(10): 2686–2698. Topic 13's README carries a
short "Newer work" note on this paper; this is the full reading.

## The problem in one sentence

A pattern like `(u1)-[:a+]->(u2), (u3)-[:b]->(u1), (u3)-[:b]->(u2)` is a join
over three "edges", one of which is a transitive closure — so how does a
planner choose an order for the whole thing, when the closure is evaluated by
an automaton or a fixpoint loop that join optimizers do not know how to cost?

## The concepts, step by step

### Step 1 — regular path queries

> **In:** an edge-labelled directed graph.
> **Out:** the definition of an RPQ and its result, a set of vertex pairs;
> Step 2 conjoins them.

An **edge-labelled graph** has a label on every edge (`a`, `b`, `knows`). A
**regular path query** (RPQ) `R(x, y)` asks for every pair of vertices `(x, y)`
connected by some path whose sequence of labels is a word of the regular
expression `R` (Definition 1). The expression is built from single labels `a`,
inverse labels `a⁻` (follow an edge backwards), **concatenation** `R1/R2`,
**alternation** `R1|R2`, **optional** `R?`, **Kleene star** `R*` (zero or more
repetitions) and **Kleene plus** `R+` (one or more). Definition 2 gives the
result as a relation: concatenation is a join on the shared middle vertex
(Eq. 5), alternation a union (Eq. 6), and `R+` the union of `R`, `R/R`,
`R/R/R`, … (Eq. 8).

Worked on the hand graph of this topic's lane 3 test
(`experiments/src/rpq.rs`, `provided_semi_naive_on_a_hand_graph`):
edges `0 -a-> 1`, `1 -b-> 2`, `2 -b-> 1`, `2 -b-> 3`, `0 -b-> 4`, and the RPQ
`a·b*` (written `a/b*` in the paper's notation) from vertex 0:

```text
a        : 0 → 1                       pair (0,1)
a/b      : 0 → 1 → 2                   pair (0,2)
a/b/b    : 0 → 1 → 2 → 1, 0 → 1 → 2 → 3   pairs (0,1) again, (0,3)
a/b/b/b  : 0 → 1 → 2 → 1 → 2           (0,2) again — the 1–2 cycle repeats forever
result   : {(0,1), (0,2), (0,3)}       — 3 pairs; 0 → 4 is a b-edge, never an a first
```

The result is a *set of pairs*, finite even though the cycle makes the number
of matching *paths* infinite.

Why it matters: an RPQ's output is bounded by |V|², but the paths that
witness it are not bounded at all. Which of the two an engine enumerates is
the difference between lane 3's two columns.

### Step 2 — conjunctive RPQs: a join whose atoms may be closures

> **In:** RPQs (Step 1).
> **Out:** CRPQs, the class the paper plans, and its running example.

A **conjunctive RPQ** (CRPQ) is a conjunction of RPQ atoms sharing variables,
with some variables projected as output (Definition 3); its result is the
natural join of the atoms' results (Definition 4, Eq. 11). Plain subgraph
matching is the special case where every atom is a single label. The running
example (Fig. 1, Example 2.1):

```text
Q(v0, v1, v2, v3) ← a(v1, v2) ∧ a(v2, v3) ∧ b+(v1, v0) ∧ b+(v2, v0) ∧ b+(v3, v0)
```

— a 2-edge `a` path whose three vertices all reach a common `v0` by `b`-paths.
In Cypher this is `MATCH (v1)-[:a]->(v2)-[:a]->(v3), (v1)-[:b*1..]->(v0),
(v2)-[:b*1..]->(v0), (v3)-[:b*1..]->(v0)`.

Why it matters: this is the shape of real graph queries — a few fixed edges
anchoring several reachability constraints — and its best plan interleaves the
two kinds of atom (Step 6).

### Step 3 — two ways to evaluate an RPQ, and why they do the same work

> **In:** one RPQ atom (Step 1).
> **Out:** the automaton paradigm and the extended-relational paradigm (§6),
> and lane 3's measurement that, under set semantics, they derive the same
> number of tuples.

§6 sorts RPQ evaluation into two paradigms:

- **Automaton-based.** Compile the expression into a **finite automaton** —
  states, with labelled transitions between them, and accepting states — and
  search the **product graph**, whose nodes are `(vertex, state)` pairs and
  whose edges follow a data edge only if its label has a transition from the
  current state. Every `(y, accepting)` reached from `(x, start)` is an answer.
  For `a·b*`: state 0 —a→ state 1, state 1 —b→ state 1, state 1 accepting.
- **Extended relational algebra.** Add a fixpoint operator to joins and
  unions: for `R+`, join the current result with `R` until nothing new appears.
  In SQL this is `WITH RECURSIVE … UNION`; evaluated **semi-naively**, each
  round joins only the pairs that are new since the last round.

Lane 3 runs both on the same graph and the same 257 sources. With *set*
semantics (`UNION`, not `UNION ALL`) the recursive join derives exactly as many
tuples as the product BFS scans adjacency entries — 40,832,336 each at
|V| = 40,000 — because each answer pair `(s, y)` is new exactly once, is joined
with `y`'s `b`-edges exactly once, and that is precisely the BFS visiting
`(y, state 1)` once and scanning its edges. The test
`product_bfs_visits_each_product_node_once_and_does_the_semi_naive_work` makes
this equality the specification. What differs is the cost of each step — a
hash-set insert versus an array slot — not the number of steps.

What *is* different is bag semantics. A `UNION ALL` recursion, or a
variable-length expansion that returns one row per path, emits walks, not
pairs; lane 3 counts them: at most 4 `b`-steps there are 6,679,113,406 walks
for 5,845,226 pairs (1,142.7×), and at 8 steps the ratio passes 7 × 10⁹.

Why it matters: the paper's §6 says "in the absence of Kleene closures,
automaton-based evaluation is equivalent to left-deep TI plans"; lane 3 shows
the closure case too is the same search once duplicates are removed per
round. Choosing a paradigm is a constant-factor choice; choosing set over bag
semantics is not.

### Step 4 — results as hypergraphs, and six operators

> **In:** CRPQs (Step 2).
> **Out:** the hypergraph result model (§3.1) and the six logical operators
> (§3.2): SCAN, SJ, TI, KC, UNION, INV.

A **hypergraph** generalises a graph: a **hyperedge** connects any number of
vertices, here an ordered tuple of them (a *sequence* hypergraph). The paper
represents every intermediate result as a hypergraph whose hyperedges are the
result's rows, each labelled with what kind of subquery produced it
(Definition 5). The point is **closedness**: every operator takes hypergraphs
and returns one, so operators compose (§3.2.4) — the data graph is the input
hypergraph whose hyperedges are its edges, via SCAN.

The six operators (§3.2.2, Fig. 4):

| operator | stands for | definition |
|---|---|---|
| **SCAN** | a single label `a` (or `a⁻`) | all edges with that label (Def. 6) |
| **SJ** — subgraph join | joining two matched subgraphs on any shared vertices | a natural join; associative (Def. 7, Eq. 28) |
| **TI** — traverse-intersect | concatenation `/`, and conjunction at one vertex | from each row of the leftmost operand, traverse one or more edges/paths and *intersect* the vertices they reach (Def. 8) |
| **KC** — Kleene closure | `R+`, `R*` | the fixpoint `U1 = R`, `U(i+1) = TI(Ui, R)` (Def. 9, Eqs. 19–22) |
| **UNION** | alternation `|` | set union (Def. 10) |
| **INV** | inverse direction | swap each pair; always pushed down to SCAN (Def. 11) |

TI is the one to understand. "Traverse" is concatenation — extend each partial
match by an edge or a path. "Intersect" is what happens when *several* edges or
paths arrive at the same query vertex: the candidates reached by each are
intersected, which is a multi-way join on that vertex — the worst-case-optimal
style of topic 13's [reading-wcoj.md](../13-graph-engines/reading-wcoj.md).
A KC result can be a non-leftmost operand of TI (Fig. 11), so a closure can be
*one of the edges intersected at a vertex*.

Why it matters: in this algebra a Kleene closure is not a separate stage glued
before or after the joins; it is an operand like any edge.

### Step 5 — physical operators: leapfrog, cached closures, hash joins

> **In:** logical operators (Step 4).
> **Out:** their implementations (§3.3) — what the cost model in Step 7 costs.

| logical | physical | how it works |
|---|---|---|
| SCAN | BPT-SCAN | two B+-trees on (label, source, target) and (label, target, source), as in MillenniumDB; a constant endpoint goes into the lookup key (§3.3.1) |
| SJ | HJ | hash join (§3.3.2) |
| TI | LFJ | **leapfrog join**: for each row of the leftmost operand, take the sorted neighbour lists of the other operands at the shared vertex and intersect them by repeatedly seeking each list to the largest current value (§3.3.3) |
| KC | CAKC | **cache-aided Kleene closure**: evaluate `R` once, cache its result, and in each iteration push the previous iteration's new targets in as constant sources (§3.3.4) |
| UNION | MU | merge union (§3.3.5) |

Two details of CAKC matter. For `R*`, the reflexive pairs `{⟨v, v⟩}` are not
materialised; an ε marker is attached and handled by the parent (§3.3.4). And a
constant *source* can be pushed into the first iteration, but a constant
*target* cannot — so the planner applies INV to turn a target into a source
(Eq. 26).

Why it matters: CAKC is Step 3's semi-naive loop, with the new targets of each
round as the delta. The paper's contribution is not a faster closure; it is
where in the plan the closure runs.

### Step 6 — the plan space: rewrite rules and a plan nobody else can express

> **In:** the operators (Steps 4–5).
> **Out:** the transformation rules of §4.1.1 and Example 3.3's plan.

The rules that generate equivalent plans:

- **TI right-recursive reducibility** (Eq. 27): `TI(L, G1, …, Gn) =
  TI(TI(L, G1, …, Gn−1), Gn)` — any non-leftmost operand can be peeled off into
  an outer TI, or merged back in.
- **SJ associativity** (Eq. 28).
- **Constant pushdown** (Eqs. 29–32) into TI and UNION operands.
- **INV pushdown** (Eqs. 33–36) through TI, KC and UNION down to SCAN.

Example 3.3 shows what this buys on the running query (Fig. 1, data graph
Fig. 2, plan Fig. 12). Match the `a` edges first — the data has few — then one
TI with *two* KC operands evaluates `b+` from `u1` and `u2` towards `u0` and
intersects them, which "narrows the intermediate results to a single match";
a second TI adds the `b+` from `u3`. Evaluating all three closures in one TI
would compute `b+` from `v3` too early, and any plan using SJ "will repeatedly
evaluate b+ from at least one data vertex". "Previous approaches cannot
express this plan" (§3.4.4): Neo4j and MillenniumDB "employ separate operators
for single-label edges, RPQs, and their conjunctions" with RPQ plans "fixed as
DFAs", and MillenniumDB uses multi-way joins only for single-label edges (§6).

Why it matters: the plan's win comes from intersecting *closures*, which needs
the closure to be a join operand — exactly what separate planning forbids.

### Step 7 — choosing a plan: dynamic programming and the cost model

> **In:** the plan space (Step 6).
> **Out:** Algorithm 2's enumeration and the cost formulas of §4.2, each
> worked once.

**Enumeration (Alg. 2).** Dynamic programming over connected subsets of the
query's edges, smallest first: for each subset `E_k`, try every way to build
it from a smaller connected subset plus edges meeting at one vertex (a TI), and
every split into two connected halves (an SJ); keep the cheapest in `PlanMap`.
RPQ atoms are planned on demand by Alg. 1, which plans the lowest-precedence
operator first and recurses.

**Cardinality of a TI (Eq. 37)** extends sampling-based estimation (Yang et
al.): keep a sample of the rows of `E_(k−1)` and count how many expand.

```text
card(E_k) = max( count × card(E_(k−1)) / S(E_(k−1)),  1 )
```

`S` is the sample size, `count` how many sampled rows expanded. Worked:
`card(E_(k−1)) = 50,000`, a sample of `S = 1,000` rows of which `count = 230`
expand → 230 × 50,000 / 1,000 = **11,500** rows estimated.

**Cost of a leapfrog TI (Eq. 38).** For each row `i` of the leftmost operand,
the intersection costs `N_min,i · log(N_max,i / N_min,i)`, where `N_min,i` and
`N_max,i` are the shortest and longest neighbour lists being intersected — the
leapfrog bound. Worked, with log base 2 (the paper does not name a base):
lists of 10 and 1,000 entries → 10 × log₂(100) = 10 × 6.64 = **66.4** steps,
against 1,010 for a linear merge of both lists. Sorting the operand lists
(Eq. 39) and binary-search checking (Eq. 40) are added on top; a binary TI has
no intersection and costs `|w1|`.

**Cost of a hash join (Eq. 41):** `|w1| + |w2|` — build one side, probe with the
other, O(1) amortised each.

**Cardinality of a Kleene closure (Eq. 42).** If each extra repetition of `R`
keeps a fraction `c` of the previous step's results (the join selectivity
estimated by Monte Carlo sampling) and the closure terminates after `D`
iterations,

```text
card(KC) ≤ Σ_{i=1..D} |R^i|  =  Σ_{i=0..D−1} c^i · |R|  =  (1 − c^D) / (1 − c) · |R|
```

Worked: `|R| = 10,000`, `c = 0.5`, `D = 3` → (1 − 0.125) / 0.5 × 10,000 =
1.75 × 10,000 = **17,500**. CAKC's cost is taken to equal its cardinality.

Why it matters: these formulas are what lets a closure compete with joins for
a place in the plan. Note what they are not: the KC estimate is a geometric
series in an average selectivity, which a power-law graph — where one hub's
`b`-reach is the whole graph — will violate (lane 3 needed 10 rounds and reached
6,536,711 of a possible 257 × 40,000 = 10,280,000 pairs).

### Step 8 — the evaluation, read carefully

> **In:** the planner (Steps 6–7), implemented on MillenniumDB's storage layer
> and as a Neo4j procedure.
> **Out:** Table 5's numbers, what "average speedup" means there, and the
> benchmark where timeouts did not change.

Setup (§5): 96 Intel Xeon 2.1 GHz CPUs, 1 TB RAM, single-threaded, a 60-second
end-to-end timeout (planning + execution). Workloads: the CRPQ class of
WDBench (Wikidata, 348,945,080 vertices, 958,844,164 edges, 5,419 labels —
Table 4), chosen because those queries historically took over 60 s on the
Wikidata endpoint; and the LDBC SNB SF10 read queries minus IS1–IS6, adapted to
CRPQs, with five parameter sets — 80 queries.

Table 5:

| benchmark | system | avg E2E time (s) | avg speedup | max speedup | timeouts |
|---|---|---|---|---|---|
| WDBench | MillenniumDB | 5.5544 | — | — | 186 |
| WDBench | this paper | 2.6662 | 2.0833 | 66,703.3178 | 60 |
| LDBC SNB | Neo4j | 5.3256 | — | — | 10 |
| LDBC SNB | this paper | 3.1638 | 1.6833 | 224.8514 | 10 |

Three readings. First, the "average speedup" is the **ratio of the average
times**, not an average of per-query speedups: 5.5544 / 2.6662 = 2.0833 and
5.3256 / 3.1638 = 1.6833, to the fourth decimal. The paper does not say how a
timed-out query enters the average; with a 60 s cap, it is bounded at 60.
Second, the timeout reduction is WDBench only: 186 → 60 is a 67.7% drop (the
paper's "approximately 68%"); on LDBC SNB both systems time out on 10 queries.
Third, the Neo4j number measures the planner running *inside* Neo4j through
its user-defined procedure API, and §5.1 notes Cypher "only allows Kleene
closures of single edge label unions" — so part of the comparison is a
language gap, not only a planning gap.

Why it matters: a 2.08× mean that is a ratio of means is dominated by the
slowest queries — consistent with Fig. 14, where the gain comes from the
execution phase of a few heavy queries, and with §5.1's count of queries over
55 s (five for MillenniumDB, one for the new planner).

### Step 9 — what the capstone planner should take from this

> **In:** Steps 3–8.
> **Out:** the design consequences for a Cypher planner over a CSR engine.

- Plan `(a)-[:KNOWS*1..5]->(b)` as an operand, not a stage: if the closure can
  be an input to a multi-way intersection at `b`, a selective fixed edge into
  `b` can prune it (Example 3.3).
- Keep set semantics inside closures unless the query asks for paths: lane 3's
  walks-versus-pairs table is the cost of not doing so.
- A cost model for a closure needs a per-source estimate on skewed graphs; the
  geometric-series bound of Eq. 42 is an average.

## How to read the paper (with the concepts in hand)

1. **§1** and **Table 1** (which systems have which of the three planning
   components).
2. **§2** with Step 1's hand graph; Example 2.1.
3. **§3.1–3.2** — Definition 8 (TI) is the dense one; read Fig. 6 alongside.
4. **§3.3**, then **§3.4.4** (Example 3.3) — the plan that motivates the paper.
5. **§4.1** (rules), **Alg. 2**, then **§4.2** with Step 7's worked formulas.
6. **§5** with Step 8's notes; **§6** for the two RPQ paradigms.

## Where each step lives in the code

The paper's code is at `github.com/SoftlySpoken/CRPQ-unified-planning`; it is
not cloned for this topic. Step 3 is this topic's lane 3:

| step | file | what to look at |
|---|---|---|
| 1, 3 | `experiments/src/rpq.rs:77` | `semi_naive` — `WITH RECURSIVE … UNION`, semi-naive, with hash joins |
| 3 | `experiments/src/rpq.rs:119` | `walks_up_to` — the bag-semantics row count, by dynamic programming |
| 3, 5 | `experiments/src/rpq.rs:157` | `product_bfs` — your product-automaton search (lane 3) |

## Questions (answer in notes.md)

1. Draw the automaton for `(a/b)+` and for `a|b*`. How many product-graph
   states can one source visit for each, on a graph with |V| vertices?
2. Lane 3 needed 10 rounds. What does the round count tell you about the
   longest shortest `b`-path among the reached vertices, and why is the
   semi-naive derived count independent of the round count?
3. In Example 3.3, why does intersecting `b+` from `u1` and `u2` *before*
   computing `b+` from `u3` save work, and what data property would make the
   opposite order better?
4. Eq. 42 assumes one selectivity `c` per repetition. Using lane 3's
   per-round new-pair counts (printed in the bench as cumulative pairs per
   `k`), estimate `c` round by round. Is it constant?
5. Table 5's average speedup is a ratio of averages. Construct three query
   times for which the ratio of averages is 2× but the geometric mean of
   per-query speedups is below 1×.

## Done when

Answer each before unfolding it.

- [ ] You can define an RPQ's result and say why it is finite even when the graph has cycles.

  <details><summary>Answer</summary>

  It is the set of vertex pairs `(x, y)` joined by at least one path whose label
  sequence matches the expression (Definition 2). A set of pairs over |V|
  vertices has at most |V|² members however many paths witness each one — the
  hand graph's 1–2 cycle gives infinitely many `a·b*` paths but only three
  pairs.

  </details>

- [ ] You can explain why semi-naive `WITH RECURSIVE … UNION` and a product-automaton BFS derive the same number of tuples.

  <details><summary>Answer</summary>

  Under set semantics each answer pair `(s, y)` enters the delta exactly once,
  in the round it is first found, and is then joined with `y`'s `b`-edges
  exactly once. The BFS visits `(y, accepting)` exactly once per source and
  scans `y`'s `b`-edges then. Both add the source's `a`-edges once. Lane 3
  measures 40,832,336 for both at |V| = 40,000. The cost per step differs (hash
  set vs array); the number of steps does not.

  </details>

- [ ] You can say what TI does that SJ does not, and why a Kleene closure being a TI operand matters.

  <details><summary>Answer</summary>

  SJ joins two matched subgraphs on whatever vertices they share (a hash join).
  TI extends each row of its leftmost operand by traversing one or more edges or
  paths and intersecting the vertices they reach — a leapfrog multi-way join at
  one query vertex. Because a KC result can be a non-leftmost TI operand, a
  closure can be intersected with other edges at the same vertex, so a
  selective edge prunes the closure's candidates (Example 3.3) — impossible when
  closures are planned as a separate stage.

  </details>

- [ ] You can compute Eq. 42's bound for given `|R|`, `c` and `D`.

  <details><summary>Answer</summary>

  `(1 − c^D) / (1 − c) · |R|`: with `|R| = 10,000`, `c = 0.5`, `D = 3`, it is
  0.875 / 0.5 × 10,000 = 17,500. It sums the geometric series `|R| + c|R| +
  c²|R|`, one term per iteration of the closure.

  </details>

- [ ] You can state Table 5's result honestly in two sentences.

  <details><summary>Answer</summary>

  On WDBench's CRPQs the new planner cuts MillenniumDB's average end-to-end
  time from 5.5544 s to 2.6662 s (a 2.08× ratio of averages) and timeouts from
  186 to 60; on LDBC SNB, as a Neo4j procedure, the average falls from 5.3256 s
  to 3.1638 s (1.68×) with the same 10 timeouts. Both are single-threaded with a
  60 s timeout (§5).

  </details>

## References

- Yue Pang, Lei Zou, Angela Bonifati, M. Tamer Özsu, Xiaofang Zhou, **"A
  Unified Query Planning Framework for Conjunctive Regular Path Queries"**,
  PVLDB 19(10): 2686–2698, 2026,
  [pdf](https://www.vldb.org/pvldb/vol19/p2686-pang.pdf). §2 (Definitions 1–4,
  Example 2.1), §3.1–3.2 (Definitions 5–11, Fig. 4, Fig. 11), §3.3, §3.4.4
  (Example 3.3, Fig. 12), §4.1 (Eqs. 27–36), Alg. 1–2, §4.2 (Eqs. 37–42), §5
  (Tables 4–5, Figs. 14–15), §6.
- Todd Veldhuizen, **"Leapfrog Triejoin"**, ICDT 2014 — the LFJ of Step 5;
  topic 13's [reading-wcoj.md](../13-graph-engines/reading-wcoj.md).
- Topic 13's [reading-query-languages.md](../13-graph-engines/reading-query-languages.md)
  for GQL quantified path patterns and path modes.
