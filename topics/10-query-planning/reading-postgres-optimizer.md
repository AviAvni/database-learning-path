# Reading guide — postgres optimizer: 45 years of Selinger (~1.5 h)

Local clone: `~/repos/postgres`, dir `src/backend/optimizer/`. Read for
the join search skeleton and the honesty of the default constants —
this is Selinger '79, still in production.

## 1. The skeleton (path/allpaths.c)

- `make_one_rel` :183 — the whole story in one function name: from base
  relations to ONE final rel. First `set_base_rel_pathlists` :384 (every
  table gets its access paths: seqscan, index paths — Selinger's "access
  path selection"), then the join search.
- The dispatcher (:3915): if `enable_geqo && levels_needed >=
  geqo_threshold` (default 12) → GENETIC algorithm (geqo/ — join order
  as TSP-style chromosome evolution; nobody's proud of it, everybody
  ships a fallback); else `standard_join_search` :3952.

## 2. standard_join_search (:3952) + joinrels.c

Textbook Selinger DP, level by level:

```
 level 1: {A} {B} {C}          best path(s) per single rel
 level 2: {AB} {AC} {BC}       join_search_one_level (joinrels.c:78):
 level 3: {ABC}                combine level k-1 rels with level 1
                               (left-deep bias) AND k-2 with 2 (bushy)
 each set keeps: cheapest total path, cheapest startup path, plus one
 path per INTERESTING ORDER (sorted output that a later merge join /
 ORDER BY could exploit — the DP state postgres kept and DuckDB dropped)
```

- Connectedness: `join_search_one_level` only pairs rels linked by a
  predicate, unless forced into a cartesian product at the end.
- Paths carry (startup_cost, total_cost) — LIMIT queries pick differently
  than full scans. Two costs per path is the underrated design decision.

## 3. The constants that run the world (include/utils/selfuncs.h)

- `DEFAULT_EQ_SEL 0.005` :34 — "col = ?" with no stats: 0.5%.
- `DEFAULT_INEQ_SEL 0.3333…` :37 — "col < ?": one third. A COIN FLIP
  wearing three decimal places.
- `DEFAULT_RANGE_INEQ_SEL 0.005` :40.

With stats, `selfuncs.c` uses histograms + MCV (most-common-value) lists
— skew handled for single columns; CROSS-column correlation still assumed
independent unless you `CREATE STATISTICS`. VLDB'15's 10⁴× errors live
exactly in that gap.

## Questions for notes.md

1. Interesting orders: construct the query where the globally-cheapest
   {AB} subplan loses — a sorted-but-pricier {AB} wins at level 3.
2. Why does geqo exist instead of DuckDB-style greedy? What does genetic
   search preserve that greedy can't (hint: it searches TREES, not
   sequences)?
3. Two costs (startup, total): which plan flips between `LIMIT 10` and
   full result — index scan vs sort — and why does one number fail?
4. MCV lists fix single-column skew. Give the graph-shaped failure that
   remains: super-node degree skew is a JOIN skew, invisible to
   per-column stats. What stat would M10 need instead (degree histogram
   per label?).

## Done when

You can walk standard_join_search for A⋈B⋈C on paper, keeping two paths
per set (cheapest, interesting-order), and name the three default
selectivities from memory.
