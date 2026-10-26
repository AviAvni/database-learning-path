# DuckPGQ: SQL/PGQ as joins, a CSR for the stars, and a factorized count

DuckPGQ is the most complete open SQL/PGQ implementation in a relational
engine: a DuckDB extension from CWI that parses `GRAPH_TABLE … MATCH`, turns the
pattern into an ordinary SQL join plan, and handles variable-length paths by
building a CSR on the fly inside scalar UDFs and running a SIMD multi-source
BFS. Its CIDR'23 paper said the two things missing were worst-case-optimal joins
and factorization; three years later the repository has a factorized-join
optimizer pass. This chapter reads the paper and the code side by side at the
pinned commit, works the CSR construction and the BFS lanes on small numbers,
states exactly what a bounded quantifier compiles to (a filter on *shortest*
distance), and then puts TurboLynx's VLDB'26 measurement of DuckPGQ next to it —
including a row of TurboLynx's Table 4 whose ratios do not match its own
milliseconds.

Anchors are `cwida/duckpgq-extension` at `29ca668` (2026-10-21; clone and pin
recorded in the pin table of [resources/codebases.md](../../resources/codebases.md)),
read with `tools/pinned-source.py`. Line numbers belong to that commit.

## The problem in one sentence

SQL already has joins, so a graph pattern can be "just SQL" — but a
variable-length path is a recursive query that SQL evaluates badly, and an
explosive path count is an m:n join SQL evaluates flat — so which parts of a
graph query can a relational engine take as-is, and which need new machinery?

## The concepts, step by step

### Step 1 — SQL/PGQ in one query

> **In:** relational tables.
> **Out:** the vocabulary of SQL/PGQ — property graph, `GRAPH_TABLE`, element
> pattern, quantifier, path mode — used in every later step.

**SQL/PGQ** (ISO/IEC 9075-16:2023) adds two things to SQL (CIDR'23 §1). A
**property graph** is a view over existing tables, declared with
`CREATE PROPERTY GRAPH`: each **vertex table** contributes vertices, each
**edge table** contributes edges and names its `SOURCE KEY … REFERENCES` and
`DESTINATION KEY … REFERENCES` columns. A query reads it through
`GRAPH_TABLE (g MATCH … COLUMNS (…))`, a table function in `FROM`. Inside
`MATCH`, `(a:Person)` is a **vertex element pattern** and `-[e:knows]->` an
**edge element pattern**; a **quantifier** `{2,4}` repeats an edge pattern
between 2 and 4 times, `*` means `{0,}` and `+` means `{1,}`; a **path mode**
(`WALK`, `TRAIL` — no repeated edges, `ACYCLIC`, `SIMPLE`) and a **selector**
(`ANY SHORTEST`, `ALL SHORTEST`, `ANY k`) say which matching paths are returned.
A Kleene star needs a selector or a restrictive mode, because otherwise the
result can be infinite (CIDR'23 §1).

Why it matters: everything in `MATCH` without a quantifier is a conjunctive
query, and SQL engines are built for those. The quantifier is the part that is
not.

### Step 2 — a fixed pattern becomes joins

> **In:** a `MATCH` without quantifiers (Step 1).
> **Out:** the SQL join plan DuckPGQ builds, with the code that builds it.

The CIDR'23 paper states the design: "All SQL/PGQ pattern matching
functionality, with filters, label tests etc. are trivially translated into
equi-joins, unions, and filters" (§3), and the translation stays in SQL on
purpose, so that WCOJ and factorization, once they exist, "apply equally to
tabular queries as to SQL/PGQ". In the code, `MatchBindReplace`
(`src/core/functions/table/match.cpp:1046`) walks each path pattern through
`ProcessPathList` (`:869`), which adds one table reference per element and, for
each edge, records the two equality conditions linking it to its endpoints:

```cpp
// src/core/functions/table/match.cpp — lines 486–495, a right-pointing edge (a)-[e]->(c)
486  void PGQMatchFunction::EdgeTypeRight(const shared_ptr<PropertyGraphTable> &edge_table,
487                                       const Identifier &next_table_name, const Identifier &prev_table_name,
488                                       const string &edge_binding, const string &prev_binding, const string &next_binding,
489                                       VertexJoinMap &vertex_joins) {
490      CheckEdgeTableConstraints(prev_table_name, next_table_name, edge_table);
491      RecordVertexJoin(vertex_joins, prev_binding, edge_table->source_pk, edge_binding, edge_table->source_fk, edge_table,
492                       edge_table->source_pg_table);
493      RecordVertexJoin(vertex_joins, next_binding, edge_table->destination_pk, edge_binding, edge_table->destination_fk,
494                       edge_table, edge_table->destination_pg_table);
495  }
```

Lines 491 and 493 are the translation: `a.key = e.src` and `c.key = e.dst`. An
undirected edge `-[e]-` becomes a `UNION ALL` of the edge table with its columns
swapped (`EdgeTypeAny`, `:449–473`). Before the `FROM` list is emitted, a
**vertex-elimination** pass (`:1088–1103`) drops a vertex table entirely when
the query uses only its key — the edge's foreign key can stand in for it, saving
a join per such vertex.

So `(a)-[e1]->(b)-[e2]->(c)` is, to DuckDB, `Person a, knows e1, Person b,
knows e2, Person c` with four equalities — the 2-hop self-join whose output
this topic's lane 1 counts: 47,900,847 rows on a 313,512-edge graph.

Why it matters: the translation inherits DuckDB's optimizer and execution for
free (join ordering, hash joins, parallelism), and inherits the flat
many-to-many blow-up with them.

### Step 3 — a variable-length edge becomes a CSR and a BFS

> **In:** an edge pattern with a quantifier whose upper bound exceeds 1
> (Step 1).
> **Out:** the CSR built in UDFs, the BFS that runs over it, and the SQL
> condition that ties them into the join (Step 4 reads the condition).

**CSR** (compressed sparse row) stores a graph's adjacency as two arrays: `v`,
one offset per vertex, and `e`, every edge's destination, so that vertex `i`'s
neighbours are `e[v[i] .. v[i+1]]`. CIDR'23's choice (§3): "we could translate
[Kleene*] to RECURSIVE SQL, but it would be hard to express efficient
shortest path-finding algorithms in that way", so DuckPGQ builds a CSR *per
query* with scalar UDFs and runs a multi-source BFS over it.

Building it is a **counting sort** — count each vertex's degree, prefix-sum the
counts into offsets, then place each edge at its vertex's next free slot:

```cpp
// src/core/functions/scalar/csr_creation.cpp — lines 57–59 (prefix sum) and 141–147 (placement)
57      for (auto i = 1; i < v_size + 2; i++) {
58          csr_entry->second->v[i] += csr_entry->second->v[i - 1];
59      }
        // ... 60–140: initialization flags, the vertex UDF, argument checks ...
141         TernaryExecutor::Execute<int64_t, int64_t, int64_t, int32_t>(
142             args.data[4], args.data[5], args.data[6], result, [&](int64_t src, int64_t dst, int64_t edge_id) {
143                 auto pos = ++csr_entry->second->v[src + 1];
144                 csr_entry->second->e[(int64_t)pos - 1] = dst;
145                 csr_entry->second->edge_ids[(int64_t)pos - 1] = edge_id;
146                 return 1;
147             });
```

The line to focus on is 143: `v` is an array of `std::atomic<int64_t>`
(`compressed_sparse_row.hpp:32`), so the pre-increment is an atomic
fetch-and-add and many DuckDB worker threads can place edges concurrently. A
worked example with three vertices and edges 0→1, 0→2, 2→1: degrees `[2, 0, 1]`;
the arrays carry two leading sentinel slots, so after the prefix sum `v[i+1]` is
the *start* of vertex `i`'s slice and each `++v[src+1]` both claims a slot and
advances that start, leaving `v[i+1]` at the slice's *end* once all edges are
placed. Edge 0→1 claims slot 0, 0→2 slot 1, 2→1 slot 2: `e = [1, 2, 1]`.

The BFS kernel processes up to 512 searches at once, one bit per search in a
`std::bitset<LANE_LIMIT>` per vertex (`LANE_LIMIT` is 512,
`src/include/duckpgq/core/utils/duckpgq_utils.hpp:12`):

```cpp
// src/core/functions/scalar/iterativelength.cpp — lines 12–32, one BFS level for all lanes
12  static bool IterativeLength(int64_t v_size, int64_t *v, vector<int64_t> &e, vector<std::bitset<LANE_LIMIT>> &seen,
13                              vector<std::bitset<LANE_LIMIT>> &visit, vector<std::bitset<LANE_LIMIT>> &next) {
14      bool change = false;
15      for (auto i = 0; i < v_size; i++) {
16          next[i] = 0;
17      }
18      for (auto i = 0; i < v_size; i++) {
19          if (visit[i].any()) {
20              for (auto offset = v[i]; offset < v[i + 1]; offset++) {
21                  auto n = e[offset];
22                  next[n] = next[n] | visit[i];
23              }
24          }
25      }
26      for (auto i = 0; i < v_size; i++) {
27          next[i] = next[i] & ~seen[i];
28          seen[i] = seen[i] | next[i];
29          change |= next[i].any();
30      }
31      return change;
32  }
```

Line 22 is the trick: one OR of a 512-bit word advances every search that is
currently at vertex `i`. Two searches that both reach vertex `i` share that
vertex's adjacency scan. The caller records, per lane, the iteration at which
the lane's destination bit first appears in `seen` — that iteration is the
**shortest-path length** (`iterativelength.cpp:115–131`).

Why it matters: this is the graph-native machinery a relational engine does
not have, bolted on as a UDF so it parallelizes with DuckDB's morsels (CIDR'23
§3). It is a breadth-first search over one label — lane 3's product BFS for the
one-state automaton `b*`, batched.

### Step 4 — what a bounded quantifier actually computes

> **In:** the BFS of Step 3 and the join plan of Step 2.
> **Out:** the exact SQL condition a quantified edge becomes, and its
> semantics — a filter on shortest distance between endpoint pairs.

When an edge sub-path has an upper bound above 1, `ProcessPathList` calls
`AddPathFinding` instead of `AddEdgeJoins` (`match.cpp:930–934`), and the
condition it adds is:

```cpp
// src/core/functions/table/match.cpp — lines 730–744
730  unique_ptr<ParsedExpression>
731  PGQMatchFunction::AddPathQuantifierCondition(const string &prev_binding, const string &next_binding,
732                                               const shared_ptr<PropertyGraphTable> &edge_table, const SubPath *subpath) {
733      std::ostringstream expression;
734      expression << "add(" << DuckPGQSQL::Column(string("temp"), string("__x")) << ", iterativelength(0, ("
735                 << DuckPGQSQLCountTable(*edge_table->source_pg_table, prev_binding, edge_table->source_pk[0]) << "), "
736                 << DuckPGQSQL::Column(string("rowid"), prev_binding) << ", "
737                 << DuckPGQSQL::Column(string("rowid"), next_binding) << "))";
738      if (subpath->upper == NumericLimits<int64_t>::Maximum()) {
739          expression << " >= " << subpath->lower;
740          return DuckPGQSQL::ParseExpression(expression.str());
741      }
742      expression << " BETWEEN " << subpath->lower << " AND " << subpath->upper;
743      return DuckPGQSQL::ParseExpression(expression.str());
744  }
```

Read lines 734–742 together: `(a)-[e]->{l,u}(b)` becomes the predicate
`iterativelength(csr, |V|, a.rowid, b.rowid) BETWEEN l AND u` over the
endpoints `a` and `b`, which both stay in the `FROM` list (`AddPathFinding`
marks them required, `:752–754`). The `__x.temp` term is `count(...) * 0` over
the CSR-building CTE — it is always 0 and exists to force the CTE to run
(`:755–756`). Unbounded `{l,}` in WALK mode without `ANY SHORTEST` is refused
outright (`:126–130`); modes other than WALK are not implemented
(`:123–124`); `ALL SHORTEST` and top-k are not implemented (`:107–112`).

Two consequences follow from what the code computes, and neither is stated in
the CIDR'23 paper. First, the result has **one row per qualifying endpoint
pair**, not one row per walk: lane 3's table shows how far apart those are on a
power-law graph — 5,845,226 pairs against 6,679,113,406 walks within four
`b`-steps. Second, the filter is on the **shortest** distance: a pair whose
shortest path has length 1 fails `BETWEEN 2 AND 3` even if walks of length 2 and
3 also connect it. These are derived from the code, not from running DuckPGQ;
the questions below ask you to construct the query that shows the difference.

Why it matters: "SQL/PGQ support" covers very different semantics across
implementations. A Cypher engine that enumerates variable-length paths returns
the walk count; DuckPGQ returns pairs.

### Step 5 — the CIDR'23 evaluation

> **In:** Steps 2–4 as they stood in 2023.
> **Out:** the paper's measured claims and the open problems it named.

Setup (CIDR'23 §4): 48 vCPUs (Xeon Platinum 8375C), 248 GB; DuckPGQ embedded in
Python, Neo4j 4.4.2 Enterprise and Umbra in Docker; LDBC-derived data; a 1-hour
timeout per query. Results:

- **Pattern matching (LSQB, Fig. 2).** Umbra and DuckPGQ "within an order of
  magnitude for all queries"; both beat Neo4j, which could not finish 4 queries
  at SF100. Q6 and Q9 — long explosive paths ending in an aggregation — were
  hardest: "only Umbra was able to complete Q6 on SF100 and Q9 on SF30 … No
  system could run Q9 on SF100". The paper's diagnosis is this topic's: those
  queries "will benefit from factorization techniques … (which are currently
  not implemented in any practical DBMS)".
- **Path-finding (Fig. 3).** 16K source–destination pairs; Umbra's `RECURSIVE`
  formulation crashed with out-of-memory, producing slow results only with 2K
  pairs at SF10; DuckPGQ "is able to generally beat Neo4j" despite a
  uni-directional BFS against Neo4j's bidirectional one.
- **Scaling.** CSR creation "is never a bottleneck if there is significant
  path-finding work"; path-finding scales "a factor 6 on 16 threads", and with a
  single morsel of work there is no parallelism at all (Fig. 4).
- **Future work.** "g3+g4: better integration of (vectorized) WCOJ and
  integration of practical factorized query processing in analytical relational
  systems" (§5).

Why it matters: the open problem the paper named in 2023 is the problem
Lehner & Neumann and FFX attack — and the one DuckPGQ's own repository took on
in October 2026 (Step 6).

### Step 6 — the factorized join optimizer, three years later

> **In:** the flat join plan of Step 2, under an aggregate.
> **Out:** the `FactorizedJoinOptimizer` rewrite, its firing rule, and its
> directory variant.

`docs/FACTORIZED_JOINS.md` describes a DuckDB optimizer pass (added in commits
`afd86058`, `5aea3c5c`, `4977077d`, 2026-10-15/16). Starting from an aggregate
it walks down through projections, filters and inner equi-joins; a join side
that contributes no column needed above the join is replaced by
`GROUP BY key, count(*)`, and the aggregate is rewritten over the product of
multiplicities (`FACTORIZED_JOINS.md:22–45`). For the 2-hop count this is the
**degree product** `Σ_b in_degree(b) · out_degree(b)` (`:36`) — the same
identity lane 1's factorized column uses, and lane 1 checks it equals the flat
join's count in every row. `count(*)` becomes `sum(m)`, `sum(x)` becomes
`sum(x · m)` (`factorized_join_optimizer.cpp:282–316`), and `min`/`max` are left
alone because every distinct value is still present once (`:307–309`).

When does it fire? Pre-aggregating a side only pays for a genuinely n:m join:

```cpp
// src/core/optimizer/factorized_join_optimizer.cpp — lines 629–642
629  bool FactorizedJoinOptimizer::SideCollapses(const LogicalComparisonJoin &join, idx_t side) {
630      auto keys = EstimateKeyCount(join);
631      if (keys == DConstants::INVALID_INDEX) {
632          // Without statistics the pre-aggregation costs at most about as much as the hash join build it replaces
633          return true;
634      }
     // ... 635–638: comment on why the test is two-sided ...
639      auto rows = join.children[side]->estimated_cardinality;
640      auto other_rows = join.children[1 - side]->estimated_cardinality;
641      return rows >= 2 * keys && other_rows >= 2 * keys;
642  }
```

The line to focus on is 641: *both* inputs must have at least two rows per
matched key. The key count comes from `EstimateKeyCount` (`:582–594`): an n:m
join of `c0` and `c1` rows over `k` keys produces about `c0 · c1 / k` rows, so
`k ≈ c0 · c1 / cj`, capped at `min(c0, c1)`. Worked with lane 1's 40,000-vertex
graph and exact numbers in place of estimates: `c0 = c1 = 313,512` edges and
`cj = 47,900,847` 2-hop rows give `k = 313,512² / 47,900,847 ≈ 2,052` keys —
far below the real number of middle vertices, which lane 1's f-representation
size bounds from below: 656,622 values minus at most 2 × 313,512 list entries
leaves at least 29,598 distinct middles. The formula assumes the fan-out is
spread evenly; a power-law graph concentrates it in hubs. The test
`313,512 ≥ 2 × 2,052` passes easily on both sides, so the rewrite would fire;
it would also fire with any true key count up to 156,756, and there are only
40,000 vertices.

When both sides collapse, a single `FACTORIZED_GROUP_JOIN` operator counts both
sides into one hash table, or — when the key is a single integer whose range
from column statistics is at most 2²³ values and the per-thread arrays fit in a
sixteenth of memory — into a dense **directory** array indexed by `key − min`
(`factorized_join_optimizer.cpp:621`; `FACTORIZED_JOINS.md:89–101`).

The document reports its own measurements (M5 Pro, 18 threads, synthetic
follower graph): the 2-hop `count(*)` at 300K persons drops from 2,266 ms to
14 ms, and the operator's directory lookup beats the hash variant 14 ms to
51 ms at 300K (`FACTORIZED_JOINS.md:106–109`, `:137–140`). These are quoted from
the repository, not reproduced here; what this topic reproduces is the identity
the rewrite relies on.

Why it matters: the rewrite only fires under an aggregate whose inputs are
not needed above the join (`FACTORIZED_JOINS.md:58–61`: not for `DISTINCT`,
`avg`, outer joins, or queries that return columns of both sides). It is
factorization for counting, not a factorized intermediate representation —
the gap between this and Steps 4–6 of the FFX chapter.

### Step 7 — how DuckPGQ measured in TurboLynx's evaluation, and a row that does not check

> **In:** DuckPGQ as an evaluated system.
> **Out:** TurboLynx's Table 4 numbers for DuckPGQ next to plain DuckDB, and
> an internal inconsistency in the same table.

TurboLynx (Lee, Ha, Tak & Han, PVLDB 19(6), VLDB'26 — topic 13's "Newer work")
benchmarks DuckPGQ at commit `947eb8d` against DuckDB v1.2.0, single-threaded,
geometric mean of five runs after three warm-ups, 1-hour timeout counted as
1 hour (§5.1). Its Table 4 gives each competitor's geometric-mean time over the
queries that competitor completed. On LDBC SNB Interactive SF100 both DuckPGQ
and DuckDB are averaged over the same 84 TurboLynx denominator, so they can be
compared directly: **DuckPGQ 15,905 ms against DuckDB 3,467 ms** — the
SQL/PGQ front end over DuckDB was 4.6× slower than DuckDB running hand-written
SQL for the same queries (at SF10: 1,289 against 652 ms, 2.0×). On TPC-H the
two are within 1% at SF100 (18,924 vs 18,801 ms), as expected when no graph
pattern is involved. Table 2 notes that queries DuckPGQ's SQL/PGQ could not
express (e.g. optional match) were run in SQL.

The same table has a row that does not check. Each entry is printed as
"speed-up — competitor ms / TurboLynx ms", and for most rows the speed-up is
the quotient: DuckDB at SF100, 3,467 / 84 = 41.27 ✓; Neo4j, 880 / 84 = 10.48
against 10.47 ✓. The **Umbra** row is not: at LDBC SF100, 2,319 / 84 = 27.6,
printed 7.74×; at SF10, 684 / 45 = 15.2, printed 2.53×; at TPC-H SF100,
113,512 / 12,253 = 9.26, printed 0.58×. Only Umbra's DBpedia entry agrees
(11,139 / 483 = 23.06 vs 23.07). DuckPGQ's own SF100 entry is off by 3%
(15,905 / 84 = 189.3, printed 183.9×). The text quotes the printed ratios
("on average 7.74× slower", "0.58× against Umbra" — §5.3.1–5.3.2), and topic 13's
README repeats them; one of the two columns in the Umbra row is wrong, and the
paper does not let a reader tell which. If the milliseconds are right, Umbra's
TPC-H SF100 time is six times DuckDB's, which contradicts §5.3.2's "Umbra
exhibited the best performance among competitors".

Why it matters: the comparison this topic cares about — the same engine with
and without a graph front end — is the DuckPGQ/DuckDB pair, and it comes from
rows that do check.

## How to read the paper and the code (with the concepts in hand)

1. **CIDR'23 §1** for SQL/PGQ syntax by example; **§2**'s list c1–c8 and g1–g4
   is the paper's thesis — read g3 and g4 twice.
2. **§3** with Listings 1–2 (the 2023 versions of Steps 3's code; compare with
   the pinned files — Listing 2 used AVX-512 integers, the pinned code uses
   `std::bitset`).
3. **§4** with Step 5.
4. Code: `match.cpp:1046` (`MatchBindReplace`) → `:869` (`ProcessPathList`) →
   `:697` (`AddEdgeJoins`) / `:746` (`AddPathFinding`); then
   `csr_creation.cpp`, `iterativelength.cpp`; then `docs/FACTORIZED_JOINS.md`
   and `factorized_join_optimizer.cpp`.
5. TurboLynx §5.1 and Table 4 with Step 7.

## Where each step lives in the code

| step | file:line (`duckpgq-extension` @ `29ca668`) | what to notice |
|---|---|---|
| 2 | `src/core/functions/table/match.cpp:486–495` | a directed edge = two key equalities |
| 2 | `src/core/functions/table/match.cpp:449–473` | an undirected edge = `UNION ALL` of the edge table with columns swapped |
| 2 | `src/core/functions/table/match.cpp:1088–1103` | vertex-join elimination, then one `FROM` entry per remaining alias |
| 3 | `src/core/functions/scalar/csr_creation.cpp:57–59`, `:141–147` | prefix sum, then atomic placement |
| 3 | `src/core/functions/scalar/iterativelength.cpp:12–32`, `:115–131` | one BFS level for 512 lanes; the level at which a lane finds its target |
| 4 | `src/core/functions/table/match.cpp:730–744`, `:930–934` | a quantifier becomes `iterativelength(...) BETWEEN l AND u` |
| 4 | `src/core/functions/table/match.cpp:106–130` | which selectors and modes are refused |
| 6 | `src/core/optimizer/factorized_join_optimizer.cpp:282–316`, `:582–594`, `:629–642` | aggregate rewrite, key estimate, two-sided n:m test |
| 6 | `src/core/optimizer/factorized_join_optimizer.cpp:621` | the 2²³ directory bound |

## Questions (answer in notes.md)

1. Write the SQL DuckPGQ would generate (by Step 2's rules) for
   `MATCH (a:Person)-[e1:knows]->(b:Person)-[e2:knows]->(c:Person) COLUMNS (a.id)`.
   Which vertex tables can vertex elimination remove, and which must stay?
2. Construct a four-vertex graph and a `{2,3}` quantifier for which Step 4's
   shortest-distance filter and SQL/PGQ's WALK semantics return different
   answers. Which rows differ?
3. `IterativeLength` scans every vertex on every level (lines 15–30), even when
   few lanes are active. Estimate its work for one batch of 512 searches on
   lane 1's 40,000-vertex graph with 10 levels, and compare with lane 3's
   product BFS for 257 sources.
4. Step 6's key estimate gave ≈2,052 keys where at least 29,598 exist. In which
   direction does that error push `SideCollapses`, and when would it make the
   rewrite fire where it should not?
5. `MatchBindReplace` emits `FROM` entries by iterating `alias_map`
   (`match.cpp:1096`), a `case_insensitive_map_t`. Does DuckDB's join-order
   optimizer make the result independent of that iteration order? What would
   you check to be sure?

## Done when

Answer each before unfolding it.

- [ ] You can say what a fixed `MATCH` pattern becomes in DuckPGQ and point at the two lines that do it.

  <details><summary>Answer</summary>

  One table reference per element pattern and, per edge, two equality
  conditions — vertex key = edge source key and vertex key = edge destination
  key (`match.cpp:491` and `:493` for a right-pointing edge). Undirected edges
  become a `UNION ALL` of the edge table with columns swapped (`:449–473`), and
  vertex tables whose only use is their key are eliminated (`:1088–1103`). The
  result is an ordinary DuckDB join plan.

  </details>

- [ ] You can build a CSR by hand the way `csr_creation.cpp` does, and say why the increment is atomic.

  <details><summary>Answer</summary>

  Count degrees, prefix-sum them into start offsets, then for each edge
  pre-increment `v[src + 1]` to claim the next slot of `src`'s slice and write
  `dst` there (`:57–59`, `:143–144`). For edges 0→1, 0→2, 2→1: `e = [1, 2, 1]`.
  `v` is `std::atomic<int64_t>` because the UDF runs inside DuckDB's parallel
  pipelines, so several threads place edges of the same source concurrently.

  </details>

- [ ] You can explain how one bitset OR advances 512 searches, and what `iterativelength` returns.

  <details><summary>Answer</summary>

  Each vertex holds a 512-bit `visit` word, one bit per search currently at that
  vertex. Scanning the vertex's adjacency once and OR-ing its `visit` word into
  each neighbour's `next` word (`iterativelength.cpp:22`) moves all those
  searches at once; `next & ~seen` keeps only first visits. The function returns,
  per search, the level at which the destination's bit first appears — the
  shortest-path length — or NULL/−1 if none.

  </details>

- [ ] You can state precisely what `(a)-[e]->{2,3}(b)` computes in DuckPGQ at the pinned commit.

  <details><summary>Answer</summary>

  The endpoints `a` and `b` stay in the join, and the condition
  `iterativelength(csr, |V|, a.rowid, b.rowid) BETWEEN 2 AND 3` filters their
  pairs (`match.cpp:730–744`, reached via `:930–934`). So: one row per `(a, b)`
  pair whose *shortest* directed `e`-distance is 2 or 3 — not one row per walk of
  length 2–3, and not pairs at distance 1 that also have longer walks. This is
  read from the code, not run.

  </details>

- [ ] You can describe the factorized join optimizer's rewrite and its firing condition.

  <details><summary>Answer</summary>

  Under an aggregate, a join side that contributes no column above the join is
  replaced by `GROUP BY key, count(*)`, and `count(*)` becomes `sum` of the
  product of multiplicities — for the 2-hop count, `Σ_b in(b) · out(b)`. It
  fires only if both inputs have at least two estimated rows per matched key
  (`factorized_join_optimizer.cpp:641`), with keys estimated as
  `min(c0, c1, c0·c1/cj)` (`:592`). It does not fire for `DISTINCT`, `avg`, outer
  joins or queries returning columns of both sides.

  </details>

- [ ] You can name the TurboLynx Table 4 comparison that isolates the SQL/PGQ front end, and the row that does not check.

  <details><summary>Answer</summary>

  DuckPGQ against DuckDB on LDBC SNB, same 84-query denominator: 15,905 ms vs
  3,467 ms at SF100 (4.6× slower through SQL/PGQ), 1,289 vs 652 ms at SF10. The
  Umbra row's printed speed-ups (0.93, 2.53, 7.74, 0.43, 0.45, 0.58) do not equal
  its competitor/TurboLynx millisecond quotients (2.8, 15.2, 27.6, 5.3, 10.3,
  9.3); only its DBpedia entry agrees.

  </details>

## References

- Daniel ten Wolde, Tavneet Singh, Gábor Szárnyas, Peter Boncz, **"DuckPGQ:
  Efficient Property Graph Queries in an analytical RDBMS"**, CIDR 2023,
  [pdf](https://www.cidrdb.org/cidr2023/papers/p66-wolde.pdf). §1 (SQL/PGQ by
  example), §2 (c1–c8, g1–g4), §3 (Listings 1–2), §4 (Figs. 2–4, Table 1), §5.
- `cwida/duckpgq-extension` at `29ca668` — `src/core/functions/table/match.cpp`,
  `src/core/functions/scalar/csr_creation.cpp`,
  `src/core/functions/scalar/iterativelength.cpp`,
  `src/core/optimizer/factorized_join_optimizer.cpp`, `docs/FACTORIZED_JOINS.md`.
- Taesung Lee, Jaehyun Ha, Byungchul Tak, Wook-Shin Han, **"TurboLynx:
  Schemaless Graph Engine Strikes Back for General-Purpose Analytics"**, PVLDB
  19(6), 2026, [pdf](https://www.vldb.org/pvldb/vol19/p1250-han.pdf). §5.1,
  Table 2, Table 4, §5.3.1–5.3.2.
- Paul Groß, Daniel ten Wolde, Peter Boncz, **"Adaptive Factorization Using
  Linear-Chained Hash Tables"**, CIDR 2025 — cited by `FACTORIZED_JOINS.md:38`.
- Previous and next chapters: [reading-crpq-unified-planning.md](reading-crpq-unified-planning.md),
  [reading-postgres-sqlpgq.md](reading-postgres-sqlpgq.md).
