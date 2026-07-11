# Topic 13 — Graph Engines (Home Turf, Deeper)

Compare FalkorDB's sparse-matrix approach against the alternatives it
competes with — with line numbers and benchmarks, not marketing. Four
architectures, one question: what does an Expand (get neighbors) cost,
and what does pattern matching (multi-way Expand) cost?

## 1. The adjacency representation menu

```
 edge list        [(0,5),(0,9),(1,5)...]        cheap writes, useless reads
 adjacency list   node -> Vec<neighbor>          per-node vectors, pointer-y
 CSR              offsets[n+1] + targets[m]      read-optimal, immutable-ish
 sparse matrix    A[i][j] = edge (GraphBLAS)     CSR + an ALGEBRA on top
 record store     fixed-size records, linked     neo4j: chains of pointers
```

```
 CSR for 0->{5,9}, 1->{5}, 2->{}:
 offsets: [0, 2, 3, 3]
 targets: [5, 9, 5]
 neighbors(i) = targets[offsets[i] .. offsets[i+1]]   one slice, zero chase
```

CSR IS a sparse matrix (it's the standard storage for one). The
GraphBLAS move is to treat the whole graph as a boolean matrix and
traversals as linear algebra: **BFS frontier expansion = SpMV**
(`y<¬visited> = A^T x` — mask does the dedup/visited check), **2-hop =
A², triangles = A ⊙ A²**. One engine (the semiring kernels) serves
every traversal.

## 2. The four architectures

| | neo4j | memgraph | kuzu | FalkorDB |
|---|---|---|---|---|
| store | fixed-size records on disk, linked lists | in-memory skip lists per vertex | columnar CSR node groups on disk | sparse matrices (SuiteSparse) |
| Expand | chase rel-chain pointers | walk vertex's edge vector | slice CSR + vectorized ops | matrix row extract / SpMV |
| pattern match | one expand at a time | one expand at a time | binary joins + WCOJ intersect | masked matrix multiply |
| MVCC | page-based + txn state | delta chains per object (topic 8's N2O) | MVCC on node groups | single-writer + matrix COW (delta matrices) |
| updates | in-place records | in-memory, GC'd deltas | CSR rebuild per node group, delta buffers | delta matrices merged on sync |

The recurring tension: CSR-shaped structures are READ-optimal but hate
single-edge inserts (shift everything). Every system grows a
delta/buffer mechanism: kuzu's in-mem CSR buffers, FalkorDB's
`Delta_Matrix` (additions/deletions matrices over the main one —
`src/graph/delta_matrix/`), even GraphBLAS's own pending-tuples. It's
the LSM idea (topic 4) applied to adjacency: fast structure + small
mutable overlay + background merge.

## 3. Why pointer chasing loses (and when it doesn't)

neo4j's pitch was "index-free adjacency" — neighbors are pointers, no
index lookup. But topic 0 taught the real cost model: a pointer chase
is a ~110 ns DRAM miss; a CSR slice is a prefetchable stream. Chasing a
34-byte relationship record chain = one miss per edge; scanning a CSR
row = bandwidth. Where records win: single-edge-centric OLTP mutations
and uniform record updates. Where they lose: any traversal wider than a
few edges — which is every interesting query.

## 4. Worst-case optimal joins (the kuzu angle)

Binary join plans can be asymptotically wrong for cyclic patterns: the
triangle `(a)-[]->(b)-[]->(c)<-[]-(a)` via pairwise joins materializes
O(m²) intermediates; the AGM bound says the output is at most m^1.5.
WCOJ (Generic Join): intersect one VARIABLE at a time —
for each edge (a,b), intersect N(a) ∩ N(b) for c. Kuzu ships this as an
`Intersect` operator on sorted CSR slices; EmptyHeaded showed the
GraphBLAS-adjacent insight that set intersection is the whole game.
FalkorDB's masked `A ⊙ A²` triangle counting is the matrix spelling of
the same intersection.

## 5. LDBC (the referee)

- **SNB Interactive**: OLTP-ish — short reads (2-hop neighborhoods,
  paths) + inserts; scale factors with power-law degree (the
  correlated-data lesson from VLDB'15/topic 10 — uniform synthetic
  graphs hide planner sins).
- **SNB BI**: analytics — global scans, aggregations over the graph.
- **Graphalytics**: pure algorithms (BFS, PageRank, WCC) — topic 24.
- The benchmark's real value: audited implementations + a spec that
  forces update handling (no read-only CSR cheating).

## 6. The query-language landscape

Six languages, three real fault lines — data model, matching semantics,
composability:

| | model | matching | composable? | pushdown-friendly? |
|---|---|---|---|---|
| Cypher/openCypher | property graph | homomorphism, rel-trail for var-length | weak (`CALL {}` bolted on) | good |
| GQL (ISO 39075:2024) | property graph | configurable: ALL/TRAIL/ACYCLIC + quantified path patterns | graph tables | good |
| SQL/PGQ | property graph *inside* SQL | GQL's MATCH in `GRAPH_TABLE(...)` | full SQL | inherits SQL |
| SPARQL | RDF triples | homomorphism (BGP) | subqueries | union-heavy plans |
| Gremlin | property graph | imperative traversal | pipelines | almost none — you ARE the plan |
| Datalog | relations | homomorphism + fixpoint | **total** — rules feed rules | recursion-native |

The trap: **the same pattern returns different answers per language**.
`(a)-[]->(b)-[]->(c)` under homomorphism lets `a=c` (Cypher: yes, nodes
may repeat); isomorphism forbids repeating nodes; trail forbids
repeating *edges* (Cypher's var-length `[*]`). Count 2-paths in a
triangle and you get three different numbers. GQL makes the mode
explicit syntax; Cypher hard-codes a hybrid — a semantics decision
disguised as a default.

RDF's edge-property hole: triples have no place for `since: 2019` on
`:alice :knows :bob` — you reify (a statement-node per edge, 4 triples)
or use RDF-star. Property graphs made the edge a first-class citizen;
that single modeling choice is most of why they won the app market.

GQL is the first new ISO database language since SQL (1987). Its
quantified path patterns (`(a) (-[:KNOWS]->){1,5} (b)`) and path modes
are the parts worth building into an AST *now* — hence M13's rule below.
→ guide: [`reading-query-languages.md`](reading-query-languages.md)

## Experiments (`experiments/`)

2-hop neighborhood (distinct nodes at distance 1 or 2, excluding self)
over three representations, same power-law graph:

1. `adj_list.rs` — PROVIDED: `Vec<Vec<u32>>` walk. The oracle.
2. `csr.rs` — YOU implement: build (counting sort) + two_hop over
   slices with a reusable visited bitmap.
3. `matrix.rs` — YOU implement: boolean SpMV two_hop — frontier vector ×
   CSR with a mask; structurally identical to csr.rs but written as
   y = xA then z = yA (feel where the algebra earns its keep and where
   it's overhead).
4. `hop_bench` — PROVIDED: 1M-node/16M-edge preferential-attachment
   graph, 10K random sources (plus the 100 highest-degree — supernodes
   are the graph-shaped tail), ns/query for all three.
5. Compare externally: same query on FalkorDB
   (`GRAPH.QUERY ... MATCH (a)-[*1..2]->(b) RETURN count(DISTINCT b)`)
   and neo4j if handy — record in notes.md.

## Reading guides

| guide | chapter |
|---|---|
| [reading-graphblas-internals.md](reading-graphblas-internals.md) | GraphBLAS & Delta_Matrix: the graph as matrices |
| [reading-neo4j-record-store.md](reading-neo4j-record-store.md) | Neo4j's record store: the price of index-free adjacency |
| [reading-memgraph-storage.md](reading-memgraph-storage.md) | Memgraph: skip lists, edge vectors, delta MVCC |
| [reading-kuzu.md](reading-kuzu.md) | Kùzu: DuckDB for graphs |
| [reading-wcoj.md](reading-wcoj.md) | Worst-case optimal joins: intersect, don't enumerate |
| [reading-ldbc-snb.md](reading-ldbc-snb.md) | LDBC SNB: the graph benchmark referee |
| [reading-query-languages.md](reading-query-languages.md) | Graph query languages: semantics, not syntax |

## Capstone M13

First graph core — the deliberately-naive baseline M20's sparse-matrix
core will replace (and be measured against):

- [ ] node + edge store over adjacency lists (CSR later — feel the
      update pain first and write it down)
- [ ] labels: node id sets (or bitmaps) per label
- [ ] basic pattern matching: single-directed-path patterns
      `(a:L)-[:R]->(b)` via scan-anchor-then-expand (M10's planner
      chooses the anchor)
- [ ] wire into M11's vectorized runtime: Expand fills batches
- [ ] bench: hop_bench queries through the whole engine vs the raw
      representation — the interpretation overhead is the M11 payoff
      measurement
- [ ] record the update-pain notes: what a CSR/matrix core must solve
      (delta overlay design → M20)
- [ ] language rule: target openCypher now, but keep the AST GQL-shaped —
      quantified path patterns and an explicit path-mode field
      (ALL/TRAIL/ACYCLIC) as first-class — so M10's parser survives GQL
      compatibility without a rewrite
