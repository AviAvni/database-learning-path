# PostgreSQL's SQL/PGQ: a graph pattern rewritten into joins, and then reverted

For six months in 2026 PostgreSQL's master branch could run `GRAPH_TABLE`.
Peter Eisentraut and Ashutosh Bapat's implementation of SQL/PGQ was committed
as `2f094e7ac69` on 2026-03-16 and reverted, together with 40-odd follow-up
fixes, on 2026-09-07 — as `b1f106c80cb` on master and `2b9e1aff4d3` on
`REL_19_STABLE`. **It is not in any PostgreSQL release**, and PostgreSQL 19
does not ship it. The code is still worth reading, because it is the purest
form of "a graph query is a relational query": the whole feature is a rewriter
pass that turns a pattern into `JOIN`s and `UNION ALL`, and then hands the
result to the unchanged planner. This chapter reads that rewriter at the
reverted commit, works out how many queries it generates for a pattern, and
lists what it does not do — variable-length paths among them.

Every anchor is `postgres/postgres` at `2f094e7ac69`, read with
`tools/pinned-source.py --ref 2f094e7ac69` (the pin table records PostgreSQL
at `701f021` for other topics; this commit is read explicitly, never checked
out). Line numbers belong to that commit. Topic 13's README records the commit
and revert history; this is the code reading.

## The problem in one sentence

If a property graph is only a view over tables, then a graph pattern is only a
join — so what exactly does a relational engine have to generate from
`MATCH (a)-[e]->(b)`, and where does that translation stop being enough?

## The concepts, step by step

### Step 1 — what was committed, and what "reverted" means here

> **In:** nothing yet — this step fixes the version being read.
> **Out:** the commit, its scope, and its status.

The commit message of `2f094e7ac69` describes the feature: the `GRAPH_TABLE`
table function for pattern matching, `CREATE/ALTER/DROP PROPERTY GRAPH`, new
system catalogs and information-schema views, psql's `\dG`, and
`pg_get_propgraphdef()`. "A property graph is a relation with a new relkind
RELKIND_PROPGRAPH. It acts like a view in many ways. It is rewritten to a
standard relational query in the rewriter." The revert commit `b1f106c80cb`
lists the original and 48 follow-up commits it undoes and links the mailing-list
discussion; it states no reason in the message, and this guide does not guess
one.

Two of the follow-ups bear directly on this chapter. `2ff289d0393`
(2026-04-24): "For a property graph with hundreds of tables, a graph pattern
with a handful element patterns can cause stack overflow." `a82e33a83d7`
(2026-08-14): the path-generation recursion "may take very long time to
generate all the possible graph paths", so it was made interruptible. Step 4
explains both.

Why it matters: a **rewriter** in PostgreSQL runs after parsing and before
planning, rewriting the query tree (it is where views are expanded). Doing
SQL/PGQ there means the planner and executor never see a graph.

### Step 2 — the entry point: a `GRAPH_TABLE` becomes a subquery

> **In:** a parsed query containing a `GRAPH_TABLE` range-table entry.
> **Out:** the same query with that entry replaced by a lateral subquery
> (Steps 3–5 build the subquery).

A **range-table entry** (RTE) is one item of a query's `FROM` list.
`rewriteGraphTable` takes the `GRAPH_TABLE` RTE and swaps it for an ordinary
subquery:

```c
// src/backend/rewrite/rewriteGraphTable.c — lines 108–138, at 2f094e7ac69
108  Query *
109  rewriteGraphTable(Query *parsetree, int rt_index)
110  {
111  	RangeTblEntry *rte;
112  	Query	   *graph_table_query;
113  	List	   *path_pattern;
114  	List	   *pathqueries = NIL;
115
116  	rte = rt_fetch(rt_index, parsetree->rtable);
117
118  	Assert(list_length(rte->graph_pattern->path_pattern_list) == 1);
119
120  	path_pattern = linitial(rte->graph_pattern->path_pattern_list);
121  	pathqueries = generate_queries_for_path_pattern(rte, path_pattern);
122  	graph_table_query = generate_union_from_pathqueries(&pathqueries);
123
124  	AcquireRewriteLocks(graph_table_query, true, false);
125
126  	rte->rtekind = RTE_SUBQUERY;
127  	rte->subquery = graph_table_query;
128  	rte->lateral = true;
     // ... 129–136: reset graph-only fields, return ...
137  	return parsetree;
138  }
```

Lines 121–122 are the whole design: generate one query per possible path
(Steps 3–4), then union them (Step 5). Line 118 asserts a single path pattern;
the parser enforces it (Step 6).

Why it matters: after line 126 the query is plain SQL. Whatever the planner
can do with joins it does; whatever it cannot — factorize, intersect, iterate —
does not happen.

### Step 3 — path factors: one per element pattern, merged by variable name

> **In:** the element patterns of one path pattern, e.g. `(a)-[e]->(b)`.
> **Out:** a list of **path factors** with edges linked to their adjacent
> vertices — the skeleton Step 4 instantiates.

An **element pattern** is one `(…)` or `-[…]->` in the pattern. The rewriter
turns each into a **path factor**, except that patterns sharing a variable name
are merged into one factor, their `WHERE` clauses ANDed
(`rewriteGraphTable.c:203–245`). Merging is how a *cyclic* pattern is written:
`(a)-[e1]->(b)-[e2]->(a)` reuses `a`, so the two `(a)` patterns are one factor
and the path closes on itself. Each edge factor then gets `src_pf` and `dest_pf`
links to its neighbouring vertex factors, according to its direction
(`:285–324`); a "-[ ]-" edge (any direction) is linked like a right-pointing one
and both directions are tried later (`:271–274`).

Why it matters: the rewriter's unit is the factor, not the element pattern —
and a repeated variable is a join condition expressed by reusing a table
alias.

### Step 4 — one query per combination of elements: the K-partite DFS

> **In:** the path factors (Step 3) and the property graph's catalog.
> **Out:** a list of path queries, one per combination of concrete tables that
> can satisfy the factors.

Each factor's label expression resolves to a set of **graph elements** — the
vertex or edge *tables* carrying those labels; no label means all labels
(`get_labels_for_expr`, `:806–866`, citing the standard's §9.2). The comment on
`generate_queries_for_path_pattern` (`:140–172`) states the algorithm: "A path
pattern in itself is a K-partite graph where K = number of element patterns
in the path pattern. The possible paths are computed by performing a DFS in
this graph." The DFS picks one element per factor and recurses:

```c
// src/backend/rewrite/rewriteGraphTable.c — lines 351–383, at 2f094e7ac69
351  static List *
352  generate_queries_for_path_pattern_recurse(RangeTblEntry *rte, List *pathqueries, List *cur_path, List *path_elem_lists, int elempos)
353  {
354  	List	   *path_elems = list_nth_node(List, path_elem_lists, elempos);
355
356  	foreach_ptr(struct path_element, pe, path_elems)
357  	{
358  		/* Update current path being built with current element. */
359  		cur_path = lappend(cur_path, pe);
     // ... 360–364: comment ...
365  		if (list_length(path_elem_lists) == list_length(cur_path))
366  		{
367  			Query	   *pathquery = generate_query_for_graph_path(rte, cur_path);
368
369  			Assert(elempos == list_length(path_elem_lists) - 1);
370  			if (pathquery)
371  				pathqueries = lappend(pathqueries, pathquery);
372  		}
373  		else
374  			pathqueries = generate_queries_for_path_pattern_recurse(rte, pathqueries,
375  																	cur_path,
376  																	path_elem_lists,
377  																	elempos + 1);
     // ... 378: comment ...
379  		cur_path = list_delete_last(cur_path);
380  	}
381
382  	return pathqueries;
383  }
```

The number of leaves this recursion visits is the **product** of the element
counts per factor, `Π_f |elements(f)|`. A combination whose edge table does not
connect the chosen vertex tables is abandoned when its query is built
(`:476–481`, `return NULL`), but only after it has been enumerated.

Worked: a property graph with 3 vertex tables and 4 edge tables, pattern
`(a)-[e1]->(b)-[e2]->(c)` with no labels. Factors: 3 vertex, 2 edge →
3 × 4 × 3 × 4 × 3 = **432** combinations enumerated, however few are
connected. With the "hundreds of tables" of `2ff289d0393` — say 100 vertex and
100 edge tables — the same 5-element pattern enumerates 100⁵ = **10¹⁰**
combinations, which is the long-running, stack-deep recursion the two
follow-up commits had to make interruptible and stack-checked.

Why it matters: this is the many-to-many blow-up one level up. Lane 1's flat
join multiplies *rows* per key; this recursion multiplies *tables* per
factor, at plan time, before a single row is read.

### Step 5 — each path query is a join; the result is their `UNION ALL`

> **In:** one combination of elements (Step 4).
> **Out:** its SQL query — one `FROM` entry per element, equality quals for
> every edge–vertex link — and the union of all such queries.

`generate_query_for_graph_path` (`:406–562`) builds one `SELECT` per path: a
range-table entry for every element's table (`:498–507`), the element's
`WHERE` clause (`:517–524`), the pattern-level `WHERE` (`:527–534`), and for
every edge the **link quals** — the edge's source-key columns equal to the
source vertex's key, and likewise for the destination — built once per element
from the catalog by `build_edge_vertex_link_quals` (`:1145–1210`, called at
`:785–794`). For an any-direction edge between two elements of the same vertex
table, the quals of both directions are ORed (`:454–473`).

The paths are then combined by `generate_setop_from_pathqueries`:

```c
// src/backend/rewrite/rewriteGraphTable.c — lines 726–733, at 2f094e7ac69
726  	sostmt = makeNode(SetOperationStmt);
727  	sostmt->op = SETOP_UNION;
728  	sostmt->all = true;
729  	sostmt->larg = (Node *) lrtr;
730  	sostmt->rarg = rarg;
731  	constructSetOpTargetlist(NULL, sostmt, lquery->targetList, rtargetlist, targetlist, "UNION", false);
732
733  	return (Node *) sostmt;
```

Line 728 makes it `UNION ALL` — bag semantics, every binding kept, which is
SQL/PGQ's semantics for fixed patterns.

Worked: on a graph with one `person` vertex table and one `knows` edge table,
`(a)-[e1]->(b)-[e2]->(c)` produces exactly one path query —

```text
SELECT …  FROM person a, knows e1, person b, knows e2, person c
WHERE e1.src = a.id AND e1.dst = b.id AND e2.src = b.id AND e2.dst = c.id
```

— the 2-hop self-join whose output lane 1 counts: 47,900,847 rows for the
40,000-vertex graph, produced flat by PostgreSQL's hash or merge joins.

Why it matters: from here on the query is ordinary SQL. PostgreSQL's join
search (topic 10's [reading-postgres-optimizer.md](../10-query-planning/reading-postgres-optimizer.md))
picks the order; no operator knows the joins came from a path.

### Step 6 — what the commit did not do

> **In:** the parser and grammar at the same commit.
> **Out:** the features rejected before the rewriter runs.

The grammar accepts quantifiers — `opt_graph_pattern_quantifier` parses
`{n}`, `{,m}` and `{n,m}` (`src/backend/parser/gram.y:18215–18220`) — but the
parser refuses them:

```c
// src/backend/parser/parse_graphtable.c — lines 233–236, at 2f094e7ac69
233  	if (gep->quantifier)
234  		ereport(ERROR,
235  				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
236  				 errmsg("element pattern quantifier is not supported")));
```

and so does the rewriter's assertion `Assert(!gep->quantifier)`
(`rewriteGraphTable.c:201`). Multiple comma-separated path patterns in one
`GRAPH_TABLE` are refused too (`parse_graphtable.c:279–282`). An edge variable
repeated so that it would force two different vertex patterns to coincide is
refused as "slightly harder to implement and probably less useful"
(`rewriteGraphTable.c:276–283`).

So at `2f094e7ac69` PostgreSQL had **no variable-length paths at all** — no
`+`, no `*`, no `{1,5}`, no shortest-path selectors. Everything a CRPQ adds over
a conjunctive query (the previous chapters) was absent; what remained is a
conjunctive query over a union of table combinations.

Why it matters: the hard half of graph querying — the RPQ half that DuckPGQ
answers with a CSR and a BFS, and Pang et al. with a Kleene-closure operator —
was not attempted. The committed feature was the easy half, and it still had a
plan-time blow-up (Step 4).

### Step 7 — what a relational engine would need beyond this rewriter

> **In:** Steps 2–6, and the previous four chapters.
> **Out:** the list of mechanisms this topic has measured or read, set
> against what the rewriter produces.

| need | in the rewriter at `2f094e7ac69` | where this topic shows it |
|---|---|---|
| fixed patterns as joins | yes — Steps 4–5 | lane 1's flat column |
| factorized intermediates for m:n joins | no — joins are flat | lane 1's f-rep column; Lehner & Neumann; FFX |
| variable-length paths | no — refused (Step 6) | lane 3; DuckPGQ's CSR + BFS |
| closures planned with the joins | no | Pang et al.'s TI/KC algebra |
| bounded plan-time enumeration | no — `Π` of element counts (Step 4) | `2ff289d0393`, `a82e33a83d7` |

Why it matters: "SQL/PGQ support" in a release note can mean any row of this
table. The capstone's Cypher engine has to do the bottom four.

## How to read the code (with the concepts in hand)

1. `git -C ~/repos/postgres show 2f094e7ac69 --stat` — read the commit message
   and the file list; never check the commit out in the shared clone.
2. `src/backend/rewrite/rewriteGraphTable.c` top to bottom: `:48–102` (the two
   structs and the function list), `:108–138` (Step 2), `:174–343` (Step 3),
   `:351–383` (Step 4), `:406–562` (Step 5), `:607–734` (the union), then
   `:743–900` (resolving labels to elements).
3. `src/backend/parser/parse_graphtable.c:225–288` (Step 6) and
   `src/backend/parser/gram.y:18205–18235`.
4. The two follow-up commits `2ff289d0393` and `a82e33a83d7`, and the revert
   `b1f106c80cb`'s list of commits.

## Questions (answer in notes.md)

1. Rewrite `(a)-[e1]->(b)-[e2]->(a)` (a 2-cycle) by Step 5's rules for a graph
   with one vertex table and one edge table. How many range-table entries does
   the query have, and why only that many?
2. For a graph with vertex tables `person`, `company` and edge tables `knows`
   (person→person) and `works_at` (person→company), how many combinations does
   Step 4 enumerate for `(a)-[e]->(b)` with no labels, and how many survive?
3. The union is `UNION ALL`. What would `UNION` change for a pattern whose
   `COLUMNS` clause projects only `a.id`?
4. Write the `WITH RECURSIVE` query a user would have to write instead of
   `(a)-[:knows]->{1,3}(b)` against this commit, and say what lane 3's
   walks-versus-pairs table predicts about its row count if they write
   `UNION ALL`.
5. Which of Step 4's two follow-up commits fixes a *time* problem and which a
   *space* problem, and what in the recursion causes each?

## Done when

Answer each before unfolding it.

- [ ] You can state the status of SQL/PGQ in PostgreSQL with commit IDs and dates.

  <details><summary>Answer</summary>

  Committed to master as `2f094e7ac69` on 2026-03-16; reverted with its
  follow-ups on 2026-09-07 as `b1f106c80cb` on master and `2b9e1aff4d3` on
  `REL_19_STABLE`. It is in no release and PostgreSQL 19 does not ship it. The
  revert message lists the reverted commits and links the discussion without
  stating a reason.

  </details>

- [ ] You can describe the rewriter's output for a pattern in one sentence and point at the line that makes it a bag.

  <details><summary>Answer</summary>

  A lateral subquery that is the `UNION ALL` of one `SELECT` per combination of
  concrete element tables, each `SELECT` joining those tables with edge–vertex
  equality quals plus the element and pattern `WHERE` clauses
  (`rewriteGraphTable.c:121–128`, `:406–562`); `sostmt->all = true` at `:728`
  makes the union `UNION ALL`.

  </details>

- [ ] You can compute how many path queries the DFS enumerates for a given graph and pattern, and say why it is a product.

  <details><summary>Answer</summary>

  The recursion (`:351–383`) picks one element per factor at each depth, so it
  visits `Π_f |elements(f)|` leaves; disconnected combinations are discarded
  only when their query is built (`:476–481`). Three vertex and four edge
  tables with `(a)-[e1]->(b)-[e2]->(c)` unlabeled: 3·4·3·4·3 = 432. Hundreds of
  tables make it 10¹⁰ for five elements — the cause of `2ff289d0393` and
  `a82e33a83d7`.

  </details>

- [ ] You can name what the commit refused, with the anchors.

  <details><summary>Answer</summary>

  Element pattern quantifiers — so all variable-length paths
  (`parse_graphtable.c:233–236`, also `rewriteGraphTable.c:201`), though the
  grammar parses `{n,m}` (`gram.y:18215–18220`); multiple path patterns in one
  `GRAPH_TABLE` (`parse_graphtable.c:279–282`); and repeated edge variables that
  would merge different vertex patterns (`rewriteGraphTable.c:276–283`).

  </details>

- [ ] You can explain why this rewriter, on a single-table social graph, produces exactly lane 1's flat plan.

  <details><summary>Answer</summary>

  With one vertex and one edge table there is one combination, so one path
  query: `person a, knows e1, person b, knows e2, person c` with four key
  equalities — the self-join `R(a,b) ⋈ R(b,c)` once the vertex tables are
  reduced to their keys. PostgreSQL's planner orders and executes it as flat
  joins, producing every 2-hop row: 47,900,847 on lane 1's 40,000-vertex graph.

  </details>

## References

- `postgres/postgres` commit `2f094e7ac69` "SQL Property Graph Queries
  (SQL/PGQ)" (2026-03-16), and its revert `b1f106c80cb` (master) /
  `2b9e1aff4d3` (`REL_19_STABLE`), 2026-09-07; follow-ups `2ff289d0393` and
  `a82e33a83d7`. Files: `src/backend/rewrite/rewriteGraphTable.c`,
  `src/backend/parser/parse_graphtable.c`, `src/backend/parser/gram.y`.
- ISO/IEC 9075-16:2023, SQL/PGQ — the standard the commit implements (cited in
  its message).
- Alin Deutsch et al., **"Graph Pattern Matching in GQL and SQL/PGQ"**,
  SIGMOD 2022 — the language, read in topic 13's
  [reading-query-languages.md](../13-graph-engines/reading-query-languages.md).
- Previous chapter: [reading-duckpgq.md](reading-duckpgq.md), the same
  translation with the RPQ half added.
