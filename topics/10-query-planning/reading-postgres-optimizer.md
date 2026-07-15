# Postgres's optimizer: Selinger '79, still in production

Forty-five years on, postgres's join search is still Selinger's DP —
level-by-level over relation sets, interesting orders kept as extra DP
state, a genetic-algorithm escape hatch for big joins. Before the code,
this chapter builds the six ideas the source assumes — access paths, the
level-by-level DP, interesting orders, the two-cost path, the genetic
fallback, and the default selectivities — then maps each to its
file:line. Read it for the search skeleton and for the honesty of the
default constants that run the world when stats are missing.

## The problem in one sentence

Postgres must pick, in milliseconds and often with no statistics at all,
one plan out of an exponential space where the best and worst differ by
1000× — and when it knows nothing about a predicate it literally guesses
0.5%.

## The concepts, step by step

### Step 1 — access paths: even one table has many ways to be read

An **access path** is one concrete way to produce a single table's rows:
a **sequential scan** (read every page front to back) or an **index
scan** (walk an index — a sorted side-structure — to fetch matching rows
one at a time). For a 10M-row table with a filter matching 100 rows, the
index scan does ~100 page reads and the seqscan ~100K; flip the filter to
match 5M rows and the seqscan's sequential IO wins by 10×. So the first
thing the optimizer does is cost every access path *per table* — this is
literally what Selinger's paper title, "access path selection", means.
One path property matters beyond cost: an index scan delivers rows
*already sorted* by the index key. Hold that for Step 3.

### Step 2 — the join search: dynamic programming, level by level

To order n joins, postgres uses Selinger's **dynamic programming** (DP):
the best plan for a *set* of relations can only be built from best plans
of its subsets, so compute best plans for all 1-relation sets, then all
2-relation sets from level 1, then level 3 from levels 2+1 (left-deep
bias) *and* — postgres extends Selinger here — bushy combinations of
levels 2+2:

```
 level 1: {A} {B} {C}          best path(s) per single rel
 level 2: {AB} {AC} {BC}       join_search_one_level (joinrels.c:78):
 level 3: {ABC}                combine level k-1 rels with level 1
                               (left-deep bias) AND k-2 with 2 (bushy)
 each set keeps: cheapest total path, cheapest startup path, plus one
 path per INTERESTING ORDER (sorted output that a later merge join /
 ORDER BY could exploit — the DP state postgres kept and DuckDB dropped)
```

Connectedness prunes the space: only pair sets linked by a join
predicate, unless a cartesian product is forced at the end. Cost: the DP
memo holds one entry per relation subset — exponential in n, which is
why Step 5's escape hatch exists.

### Step 3 — interesting orders: why one "best" plan per set isn't enough

An **interesting order** is a sort order of a subplan's output that some
*later* operator could exploit — a merge join (joins two sorted inputs by
scanning them in lockstep), an ORDER BY, a GROUP BY. Keeping only the
single cheapest plan per relation set would be a bug: a subplan that
costs 20% more but delivers rows already sorted can win *globally* by
saving a full sort later. So the DP cell keeps MULTIPLE surviving paths —
one per useful ordering — and a new path survives unless some existing
path beats it on *every* axis. This is `add_path`, conceptually:

```rust
fn add_path(rel: &mut RelOptInfo, new: Path) {
    let dominated = rel.paths.iter().any(|p|
        p.total_cost <= new.total_cost
        && p.startup_cost <= new.startup_cost   // LIMIT-friendly axis
        && p.ordering.subsumes(&new.ordering)); // sorted output IS DP state
    if !dominated {
        rel.paths.retain(|p| !new.dominates(p));
        rel.paths.push(new);   // a pricier-but-sorted path survives here,
    }                          // to win later at a merge join or ORDER BY
}
```

The cost of this refinement is a fatter memo (a handful of paths per set
instead of one); the payoff is that merge-join plans are findable at all.

### Step 4 — two costs per path: startup and total

Every path carries a pair `(startup_cost, total_cost)`: what it costs
before the *first* row comes out, and what the *whole* result costs. The
distinction exists because of LIMIT: for `ORDER BY x LIMIT 10`, an index
scan on x has near-zero startup (rows stream out sorted immediately)
even if its total cost is high, while sort-everything has all its cost
in startup. One number can't represent both queries; two numbers is the
underrated design decision — it's the second dominance axis in Step 3's
`add_path`.

### Step 5 — when n is big: the genetic escape hatch

The DP's memo is exponential in the number of relations, so at
`geqo_threshold` relations (default 12) postgres abandons it for **geqo**
— a **genetic algorithm** (randomized search that "evolves" a population
of candidate solutions by recombining good ones): join orders are encoded
as chromosomes, evaluated with the normal cost model, and bred for a
fixed number of generations. Join order as TSP-style chromosome
evolution; nobody's proud of it, everybody ships a fallback (DuckDB's is
greedy — see reading-duckdb-optimizer.md). The trade: geqo explores
*tree-shaped* candidates rather than committing to one greedy sequence,
at the price of nondeterministic plans.

### Step 6 — the constants that run the world

All of the above consumes **selectivity** estimates (the fraction of rows
a predicate keeps). With statistics, `selfuncs.c` uses histograms +
MCV lists (most-common-values: the top values and their actual
frequencies) — single-column skew handled. Without stats — fresh table,
default-typed expression, anything opaque — postgres falls back to
compiled-in constants in `include/utils/selfuncs.h`:

- `DEFAULT_EQ_SEL 0.005` :34 — "col = ?" with no stats: 0.5%.
- `DEFAULT_INEQ_SEL 0.3333…` :37 — "col < ?": one third. A COIN FLIP
  wearing three decimal places.
- `DEFAULT_RANGE_INEQ_SEL 0.005` :40.

And even with full stats, CROSS-column correlation is assumed away
(independence) unless you manually `CREATE STATISTICS`. VLDB'15's 10⁴×
errors (reading-how-good-optimizers.md) live exactly in that gap — a
constant guess powering million-dollar plan choices.

## Where each step lives in the code

- **Steps 1–2 — the skeleton** (`path/allpaths.c`): `make_one_rel` :183
  — the whole story in one function name: from base relations to ONE
  final rel. First `set_base_rel_pathlists` :384 (every table gets its
  access paths: seqscan, index paths — Step 1), then the join search.
- **Step 5 gate, then Step 2** (`path/allpaths.c`): the dispatcher
  (:3915): if `enable_geqo && levels_needed >= geqo_threshold` (default
  12) → GENETIC algorithm (the `geqo/` directory); else
  `standard_join_search` :3952 — the level-by-level DP of Step 2's
  diagram, with `join_search_one_level` (`path/joinrels.c:78`) doing the
  per-level pairing and enforcing connectedness.
- **Steps 3–4** — `add_path` (in `util/pathnode.c`, but internalize the
  Rust sketch above): multi-path DP cells, dominance across
  (total_cost, startup_cost, ordering).
- **Step 6** — `src/include/utils/selfuncs.h` :34/:37/:40 for the
  defaults; `selfuncs.c` for the histogram + MCV machinery.

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

## References

**Code**
- [postgres](https://github.com/postgres/postgres) —
  `src/backend/optimizer/`: `path/allpaths.c` (make_one_rel,
  standard_join_search), `path/joinrels.c` (join_search_one_level),
  plus `src/include/utils/selfuncs.h` for the default selectivities;
  ~1.5 h
