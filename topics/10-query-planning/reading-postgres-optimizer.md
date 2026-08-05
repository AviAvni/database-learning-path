# Postgres's optimizer: Selinger '79, still in production

Forty-six years on, postgres's join search is still Selinger's DP —
level-by-level over relation sets, interesting orders kept as extra DP state, a
genetic-algorithm escape hatch for big joins. Before the code, this chapter
builds the six ideas the source assumes — access paths, the level-by-level DP,
interesting orders, the two-cost path, the genetic fallback, and the default
selectivities — then maps each to its file:line. Read it for the search
skeleton and for the honesty of the default constants that run the world when
stats are missing.

**Every `file:line` below was read at `postgres/postgres@701f021`** using
`python3 tools/pinned-source.py show postgres <path> -r A:B`. Line numbers move
between releases; re-run the tool rather than trusting the numbers. **Topic 10
has no measured lane** — nothing here is a timing taken on this machine, and
none of these figures appear in `FINDINGS.md`. Where a runtime number is
needed, it is cited to the JOB paper (`reading-how-good-optimizers.md`).

## The problem in one sentence

Postgres must pick, in milliseconds and often with no statistics at all, one
plan out of a space that grows factorially — and when it knows nothing about an
equality predicate it substitutes the compiled-in constant `0.005`, a number
chosen not to be *accurate* but to be small enough that index scans still get
picked (`src/include/utils/selfuncs.h:24-34`).

## The concepts, step by step

### Step 1 — access paths: even one table has many ways to be read

> **In:** one base relation, its catalog statistics (row count, page count),
> its indexes, and the filter predicates that apply to it alone.
> **Out:** a *list* of costed **paths** for that relation — a `RelOptInfo` with
> a populated `pathlist`. This is level 1 of Step 2's DP.

Two vocabulary items, because the whole file is written in them.

- A **logical plan** is an expression in **relational algebra** — the algebra
  of select (σ, filter), project (π, choose columns), join (⋈), aggregate,
  union over bags of tuples. It says *what* rows you want.
- A **physical plan** picks an algorithm per logical operator. Postgres calls a
  physical subplan a **`Path`**: a node with a cost, an output row estimate, and
  an output sort order.

An **access path** is a `Path` for a single table: a **sequential scan** (read
every page front to back) or an **index scan** (walk a sorted side-structure,
then fetch the matching heap rows). Which wins depends entirely on how many
rows survive the filter, and postgres decides with five compiled-in constants
(`src/include/optimizer/cost.h:24-28`):

```
   seq_page_cost            1.0      one sequentially-read 8 KB page
   random_page_cost         4.0      one randomly-fetched 8 KB page
   cpu_tuple_cost           0.01     processing one tuple
   cpu_index_tuple_cost     0.005    processing one index entry
   cpu_operator_cost        0.0025   evaluating one operator/function
```

**Work it.** Take a 10,000,000-row table at 100 tuples per page — the density
`selfuncs.h:26` itself assumes — so 100,000 pages, and one filter predicate.
`cost_seqscan` (`src/backend/optimizer/path/costsize.c:300`, `:306-307`,
totalled at `:339`) is:

```
   total = seq_page_cost·pages + (cpu_tuple_cost + qual_per_tuple)·tuples

   seqscan = 1.0 × 100,000  +  (0.01 + 0.0025) × 10,000,000
           =    100,000     +          125,000
           =    225,000
```

Note what is *absent* from that formula: selectivity. A seqscan costs 225,000
whether the filter keeps 100 rows or 5 million — it always reads every page and
evaluates the qual on every tuple. Now the index scan at a selectivity of
0.00001 (100 rows out), assuming the worst case for locality, one random heap
fetch per row:

```
   idxscan ≈ random_page_cost × 100                     = 400
           + (cpu_tuple_cost + cpu_index_tuple_cost
              + cpu_operator_cost) × 100                =   1.75
           ≈ 402   (plus a few pages of B-tree descent)

   225,000 / 402  ≈  560× in the index's favour
```

Flip the filter to keep 5,000,000 rows and the index's random-fetch term alone
is `4.0 × min(5,000,000, 100,000) = 400,000` — already 1.8× the seqscan's
*entire* cost before any CPU terms, and that is why postgres switches. (The
real index cost uses the Mackert–Lohman approximation at `costsize.c:898` to
account for pages revisited within one scan; the bound above is enough to see
the crossover.) The single input that decides a 560× difference is the
selectivity estimate — which Step 6 shows is often a guess.

One path property matters beyond cost: an index scan delivers rows *already
sorted* by the index key. Hold that for Step 3.

Why it matters: this is literally what Selinger's paper title, "access path
selection", means, and it is the base case the DP builds on.

### Step 2 — the join search: dynamic programming, level by level

> **In:** the level-1 `RelOptInfo`s from Step 1, and the query's join
> predicates.
> **Out:** one `RelOptInfo` for the full relation set, whose `pathlist`
> contains the surviving complete plans — `standard_join_search`'s return
> value.

**Dynamic programming** (DP) is the technique of solving a problem by solving
each distinct subproblem once and memoizing the answer, exploiting the fact
that an optimal solution is built from optimal solutions to subproblems. Here
the subproblem is *a set of relations*, and the principle of optimality is:
the best plan for `{A,B,C}` can only be built from best plans for its subsets.
Postgres's own comment says so (`path/allpaths.c:3964-3968`):

> We employ a simple "dynamic programming" algorithm: we first find all ways to
> build joins of two jointree items, then all ways to build joins of three items
> (from two-item joins and single items), then four-item joins, and so on until
> we have considered all ways to join all the items into one rel.

**Why bother.** A **left-deep** plan is one where every join's right input is a
base table, so the tree is a single spine; there are n! of them for n
relations. The DP does not enumerate plans, it enumerates *sets*, so it
considers far fewer:

```
   left-deep orders enumerated one at a time     n!
   (set, last-relation) pairs the DP considers   n·2^(n-1) − n
   DP memo entries (subsets of n relations)      2^n − 1

    n            n!      DP considered      memo     n! / DP
    5           120                 75        31         1.6
   10     3,628,800              5,110     1,023       710.1
   15 1,307,674,368,000        245,745    32,767   5,321,265.4
```

At n = 5 the DP barely pays for itself. At n = 10 it does 710× less work than
naive enumeration; at n = 15, five million times less. And it is *still*
exponential — the memo column is `2^n − 1` — which is exactly why Step 5 exists.
(These are counts over a complete join graph and are computed here, not
measured; the real counts are smaller because Step 2's connectedness test
prunes disconnected subsets.)

Postgres extends Selinger by also building **bushy** plans — a join whose
*both* inputs are themselves joins. `join_search_one_level`
(`path/joinrels.c:78`) does the level in two passes:

```
 level 1: {A} {B} {C} {D}     Step 1's access paths, one RelOptInfo each

 pass 1  (joinrels.c:96-143)  "left-sided and right-sided plans": every
                              level-1 rel of the previous level joined
                              against each *initial* (single) relation
                              it has a join clause with        [:123]
                              — no join clause at all? cartesian
                              product against every initial rel [:139]

 pass 2  (joinrels.c:153-198) "bushy plans": for k = 2, 3, ... while
                              k <= level-k, join every level-k rel to
                              every level-(level-k) rel it shares a
                              join clause with. Halts at the halfway
                              point because make_join_rel(x,y) handles
                              both orders                       [:161]
```

Two precision points people get wrong. First, pass 1 is *not* purely left-deep:
the code's own comment (`:90-91`) says "left-sided **and** right-sided plans",
because `make_join_rel` is symmetric. Second, pass 2 is not just "k−2 with 2" —
it is *every* split from k = 2 up to level/2, so at level 6 it considers 2+4
**and** 3+3.

Connectedness prunes the space: pass 2 only pairs rels that share a join clause
or a join-order restriction, explicitly "in order to avoid unreasonable growth
of planning time" (`:149-151`). Pass 1 falls back to cartesian products only
for a rel with no join clauses at all.

Each set's surviving paths live in `root->join_rel_level[lev]`
(`allpaths.c:3974-3976`, driven by the loop at `:3978-3987`), and `set_cheapest`
(`util/pathnode.c:268`) is called on each finished joinrel before the next
level begins.

Why it matters: this is the skeleton every cost-based optimizer since 1979 has
either used or deliberately replaced. DuckDB's is the same idea with a
different enumeration order (`reading-duckdb-optimizer.md`).

### Step 3 — interesting orders: why one "best" plan per set isn't enough

> **In:** a candidate `Path` for some relation set, and the paths already
> surviving for that set.
> **Out:** an updated `pathlist` — the candidate inserted, or rejected, and any
> old paths it dominates removed. This is what makes each DP cell hold
> *several* plans instead of one.

An **interesting order** is a sort order of a subplan's output that some *later*
operator could exploit — a merge join (which joins two sorted inputs by scanning
them in lockstep), an `ORDER BY`, a `GROUP BY`. Postgres calls the
representation **pathkeys**.

Keeping only the single cheapest plan per relation set would be a bug: a
subplan that costs 20% more but delivers rows already sorted can win *globally*
by saving a full sort later. So the DP cell keeps multiple surviving paths, and
a new one survives unless some existing path beats it on *every* axis. That is
`add_path` (`util/pathnode.c:459`), and the axes are documented in its header
comment at `:391-412` and implemented at `:518-533`:

```
   add_path's dominance axes (util/pathnode.c:391-412, :518-533)
   1. disabled_nodes    how many disabled node types the path uses —
                        a higher-order component of cost           [:399-406]
   2. startup_cost      cost before the first row                  [:396-398]
   3. total_cost        cost of the whole result                   [:396-398]
   4. pathkeys          output sort order — Step 3's whole point   [:511-513]
   5. required_outer    parameterization (which outer rels it needs) [:518]
   6. rows              a path producing fewer rows can win        [:524]
   7. parallel_safe                                                [:525]
```

Three things about this that a summary usually loses:

- It is **seven axes, not two**. A guide that shows `add_path` comparing only
  cost and ordering is describing the 1979 paper, not this file.
- The cost comparison is deliberately **fuzzy**: `compare_path_costs_fuzzily`
  (`:181`) is called with `STD_FUZZ_FACTOR`, defined as `1.01` at `:47`. Costs
  within 1% of each other count as equal. The comment at `:545-548` gives the
  reason — an exact comparison "results in annoying platform-specific plan
  variations due to roundoff in the cost estimates".
- Parameterized paths are treated as having **no** pathkeys (`:472-473`,
  policy stated at `:420-424`), specifically to keep the pathlist small.

The cost of this refinement is a fatter memo (a handful of paths per set
instead of one); the payoff is that merge-join plans and index-order plans are
findable at all.

Why it matters: "interesting orders" is the one piece of Selinger that a naive
reimplementation always drops, and dropping it silently removes an entire join
algorithm from the search space.

### Step 4 — two costs per path: startup and total

> **In:** a costed `Path`.
> **Out:** the pair `(startup_cost, total_cost)` that Step 3's axes 2 and 3
> compare — and the reason a single scalar cannot rank plans.

Every path carries two numbers: what it costs before the *first* row comes out,
and what the *whole* result costs. `cost_seqscan` sets both explicitly
(`costsize.c:338-339`).

The distinction exists because of `LIMIT`. For `ORDER BY x LIMIT 10`:

- an index scan on `x` has near-zero **startup** cost — rows stream out already
  sorted — even if its total cost is high, because you stop after 10;
- a seqscan-then-sort has *all* of its cost in startup, because a sort cannot
  emit its first row until it has consumed its last input row.

Reverse the query to "no LIMIT, return everything" and the ranking flips. One
number cannot represent both queries; two numbers can. This is why
`add_path` keeps a path that is cheaper on startup even when it loses on total
(`pathnode.c:396-398`: "if one path is cheaper in one of these aspects and
another is cheaper in the other, we keep both") — and it is guarded by
`consider_startup`, so the extra paths are only retained when a `LIMIT`-like
construct actually exists (`:426-429`).

Why it matters: it is the cheapest possible generalization from "cost" to "cost
*function* of how much of the result you consume", and it costs one extra
`double` per path.

### Step 5 — when n is big: the genetic escape hatch

> **In:** the relation count `levels_needed` and the `initial_rels` list.
> **Out:** *either* the exhaustive DP of Step 2, *or* a genetic search that
> returns one `RelOptInfo` and no optimality guarantee — the branch is a single
> `else if`.

Step 2's memo is `2^n − 1` entries, so postgres gates on relation count
(`path/allpaths.c:3913-3918`, inside `make_rel_from_joinlist` at `:3847`):

```c
// postgres/postgres@701f021 — src/backend/optimizer/path/allpaths.c
3913  		if (join_search_hook)
3914  			return (*join_search_hook) (root, levels_needed, initial_rels);
3915  		else if (enable_geqo && levels_needed >= geqo_threshold)
3916  			return geqo(root, levels_needed, initial_rels);
3917  		else
3918  			return standard_join_search(root, levels_needed, initial_rels);
```

`geqo_threshold` defaults to **12** (`src/backend/utils/misc/guc_parameters.dat:1191`,
`boot_val => '12'`). Note the extension point on line 3913: an extension can
replace the join search wholesale.

**geqo** is a **genetic algorithm** — a randomized search that maintains a
population of candidate solutions, breeds new ones by recombining good ones,
and keeps the fitter offspring. Postgres's parameters are computed, not fixed
(`geqo/geqo_main.c:328-350` and `:360-367`):

```
   pool_size   = clamp( 2^(n+1), 10·Geqo_effort, 50·Geqo_effort )
   generations = pool_size
   Geqo_effort defaults to 5  (src/include/optimizer/geqo.h:57)

   so at n = 12:  2^13 = 8192, clamped to 50 × 5 = 250 individuals
                  and 250 generations
```

One new individual is bred and evaluated per generation (`geqo_main.c:192`
loop, `geqo_eval` at `:230`, `spread_chromo` at `:233`), so the whole search
costs roughly `pool_size + generations` ≈ 500 cost evaluations at n = 12 —
against a DP that would have considered tens of thousands of set pairs.

**The detail everybody states backwards.** A geqo chromosome is a **sequence**,
not a tree: `Gene *tour` (`geqo_eval.c:140`), recombined with **ERX**, edge
recombination crossover (`geqo.h:46`, applied at `geqo_main.c:198-204`) — an
operator borrowed from the travelling salesman problem. The tree is *derived*
from the tour afterwards by `gimme_tree` (`geqo_eval.c:163`), which walks the
tour maintaining "clumps" of already-joined relations and adds each new
relation to the first clump it can legally join (`:171-178`). The comment at
`:146-157` records the history: the original implementation joined strictly in
tour order and "could never produce a 'bushy' plan", which broke queries whose
only valid plans are bushy; the clump heuristic was added to fix that, "and as
a nice side-effect it seems to materially improve the quality of the generated
plans".

The trade: nondeterministic plans (the same query can be planned differently
twice) in exchange for bounded planning time. Nobody is proud of it; everybody
ships a fallback. DuckDB's is greedy operator ordering with the same threshold
of 12 (`reading-duckdb-optimizer.md`).

Why it matters: the escape hatch is where the "optimal plan" guarantee actually
ends, and knowing the threshold is 12 tells you exactly which of your queries
have no guarantee at all.

### Step 6 — the constants that run the world

> **In:** a predicate and whatever the catalog knows about its columns.
> **Out:** a **selectivity** in [0,1] — the fraction of rows the predicate
> keeps — which Step 1 multiplies into row counts and Step 2 propagates up the
> whole tree.

With statistics, `utils/adt/selfuncs.c` uses **histograms** (a table of value
ranges plus the row count falling in each, so a range predicate is answered by
summing buckets) and **MCV lists** (most-common-values: the top values with
their measured frequencies). Single-column skew is handled well by these.

Without stats — a fresh table, an opaque expression, a function call postgres
cannot see into — it falls back to compiled-in constants in
`src/include/utils/selfuncs.h`:

```c
// postgres/postgres@701f021 — src/include/utils/selfuncs.h
  23  /*
  24   * Note: the default selectivity estimates are not chosen entirely at random.
  25   * We want them to be small enough to ensure that indexscans will be used if
  26   * available, for typical table densities of ~100 tuples/page.  Thus, for
  27   * example, 0.01 is not quite small enough, since that makes it appear that
  28   * nearly all pages will be hit anyway.  Also, since we sometimes estimate
  29   * eqsel as 1/num_distinct, we probably want DEFAULT_NUM_DISTINCT to equal
  30   * 1/DEFAULT_EQ_SEL.
  31   */
  32
  33  /* default selectivity estimate for equalities such as "A = b" */
  34  #define DEFAULT_EQ_SEL	0.005
  35
  36  /* default selectivity estimate for inequalities such as "A < b" */
  37  #define DEFAULT_INEQ_SEL  0.3333333333333333
  38
  39  /* default selectivity estimate for range inequalities "A > b AND A < c" */
  40  #define DEFAULT_RANGE_INEQ_SEL	0.005
  // ... 41-51: MULTIRANGE_INEQ, MATCH_SEL, MATCHING_SEL ...
  52  #define DEFAULT_NUM_DISTINCT  200
```

Read the comment before you sneer at the number. `0.005` is not an estimate of
reality; it is a value **chosen to be small enough that index scans still get
picked** at ~100 tuples/page. `0.01` was tried and rejected as too large. And
`DEFAULT_NUM_DISTINCT 200` at `:52` is not independent — the comment says it
exists to satisfy `DEFAULT_NUM_DISTINCT = 1/DEFAULT_EQ_SEL`, and indeed
`1/0.005 = 200`. The constants are a *consistent* fiction, not a careless one.

`DEFAULT_INEQ_SEL` is a third, spelled to sixteen decimal places — the digits
are the only part of it that looks like a measurement.

**Work the composition, because this is where the constants bite.** Postgres
assumes **independence** across predicates, so a conjunction's selectivity is
the product of the parts. Three unknown equality predicates on a 10,000,000-row
table:

```
   sel(p1 ∧ p2 ∧ p3) = 0.005 × 0.005 × 0.005 = 1.25e-7
   estimated rows    = 10,000,000 × 1.25e-7  = 1.25
                     → clamped to 1 by clamp_row_est (costsize.c:215)
```

Postgres now believes one row comes out of a ten-million-row table, and will
happily put that on the inner side of a nested loop. That is the exact shape of
the failure the JOB paper measured (`reading-how-good-optimizers.md`, §4.1):
disabling non-index nested-loop joins removed *all* of their timeouts.

Now the same arithmetic *with* statistics, on this topic's schema (`notes.md`:
`users` 10,000 rows, NDV(city) = 100, NDV(age) = 50):

```
   sel(city = 'Paris')          = 1/100 = 0.01     uniformity
   sel(age  = 30)               = 1/50  = 0.02     uniformity
   sel(both), independence      = 0.01 × 0.02 = 0.0002
   estimated rows               = 10,000 × 0.0002 = 2
```

Perfect statistics, and still wrong the moment `city` and `age` correlate —
because cross-column correlation is assumed away unless you manually
`CREATE STATISTICS`. The JOB paper's Figure 3 lives exactly in that gap.

Why it matters: everything upstream — Step 1's 560× access-path decision,
Step 2's whole DP, Step 5's genetic fitness function — consumes this number. It
is the cheapest and least accurate input in the entire optimizer.

## Where each step lives in the code

All paths relative to the repo root of `postgres/postgres@701f021`.

| Step | File | Lines | What is there |
|---|---|---|---|
| 1 | `src/include/optimizer/cost.h` | 24-28 | the five cost constants: 1.0 / 4.0 / 0.01 / 0.005 / 0.0025 |
| 1 | `src/backend/optimizer/path/costsize.c` | 300, 306-307, 339 | `cost_seqscan` — pages, CPU per tuple, and the total |
| 1 | `src/backend/optimizer/path/costsize.c` | 898 | `index_pages_fetched` — the Mackert–Lohman approximation |
| 1 | `src/backend/optimizer/path/costsize.c` | 215 | `clamp_row_est` — why an estimate is never 0 rows |
| 1-2 | `src/backend/optimizer/path/allpaths.c` | 183 | `make_one_rel` — the whole story in one name: base rels → one final rel |
| 1 | `src/backend/optimizer/path/allpaths.c` | 384 | `set_base_rel_pathlists` — every table gets its access paths |
| 5 | `src/backend/optimizer/path/allpaths.c` | 3847, 3913-3918 | `make_rel_from_joinlist` and the three-way dispatch (hook / geqo / DP) |
| 2 | `src/backend/optimizer/path/allpaths.c` | 3952, 3964-3968, 3974-3987 | `standard_join_search` — the level loop, and its own "dynamic programming" comment |
| 2 | `src/backend/optimizer/path/joinrels.c` | 78, 90-143 | `join_search_one_level` pass 1 — left- and right-sided plans, cartesian fallback at :139 |
| 2 | `src/backend/optimizer/path/joinrels.c` | 145-198 | pass 2 — bushy plans for every k from 2 to level/2 |
| 2 | `src/backend/optimizer/path/joinrels.c` | 699 | `make_join_rel` — symmetric in its two arguments, which is why pass 1 gets right-sided plans free |
| 3 | `src/backend/optimizer/util/pathnode.c` | 459, 391-412, 518-533 | `add_path` and its seven dominance axes |
| 3-4 | `src/backend/optimizer/util/pathnode.c` | 47, 181, 491-492 | `STD_FUZZ_FACTOR 1.01` and `compare_path_costs_fuzzily` |
| 2-3 | `src/backend/optimizer/util/pathnode.c` | 268 | `set_cheapest` — run per joinrel at the end of each level |
| 5 | `src/backend/utils/misc/guc_parameters.dat` | 1187-1194 | `geqo_threshold`, `boot_val => '12'` |
| 5 | `src/backend/optimizer/geqo/geqo_main.c` | 192, 198-204, 230, 328-350, 360-367 | the GA loop, ERX crossover, pool-size and generation formulas |
| 5 | `src/backend/optimizer/geqo/geqo_eval.c` | 140, 146-160, 163 | `gimme_tree` — tour in, clumped (possibly bushy) tree out |
| 5 | `src/include/optimizer/geqo.h` | 46, 57 | `#define ERX`, `DEFAULT_GEQO_EFFORT 5` |
| 6 | `src/include/utils/selfuncs.h` | 23-30, 34, 37, 40, 52 | the rationale comment, then the constants |
| 6 | `src/backend/utils/adt/selfuncs.c` | — | the histogram + MCV machinery the constants stand in for |

Reproduce any row with:

```
python3 tools/pinned-source.py show postgres src/backend/optimizer/path/joinrels.c -r 145:198
```

## Questions for notes.md

1. Interesting orders: construct the query where the globally-cheapest `{AB}`
   subplan loses — a sorted-but-pricier `{AB}` wins at level 3 by feeding a
   merge join. Which of `add_path`'s seven axes keeps it alive?
2. geqo encodes a join order as a *tour* and recombines it with ERX, a TSP
   operator; `gimme_tree` then derives the tree. Why is searching sequences and
   deriving trees a reasonable compromise, and what does `geqo_eval.c:146-157`
   say went wrong with the strictly-in-tour-order version?
3. Two costs (startup, total): which plan flips between `LIMIT 10` and the full
   result — index scan vs sort — and why does one number fail? Then explain why
   postgres gates the extra path on `consider_startup` (`pathnode.c:426-429`)
   rather than always keeping it.
4. `STD_FUZZ_FACTOR` is 1.01, and the comment blames "platform-specific plan
   variations due to roundoff". What does that tell you about how much
   confidence to place in a cost difference of 5%?
5. MCV lists fix single-column skew. Give the graph-shaped failure that
   remains: super-node degree skew is a *join* skew, invisible to per-column
   stats. What statistic would M10 need instead — a degree histogram per label?

## Takeaway

Postgres's optimizer is Selinger's 1979 skeleton with three additions that all
turned out to matter more than the skeleton: bushy plans in pass 2 of
`join_search_one_level`, a seven-axis dominance test in `add_path` instead of
"cheapest wins", and a genetic escape hatch for when `2^n` stops fitting. The
DP's advantage over naive enumeration is real and enormous — 5.3 million× at
n = 15 — but it is exponential either way, which is why the threshold at
`geqo_threshold = 12` exists. And every one of those decisions is driven by a
selectivity number that, absent statistics, is the constant `0.005` chosen in
1996 to keep index scans attractive.

## Done when

Answer each before unfolding it.

- [ ] Walk `standard_join_search` for `A ⋈ B ⋈ C` on paper. How many levels,
      what happens at each, and where do the bushy plans come from?
  <details><summary>Answer</summary>

  Three levels. Level 1 is `initial_rels`, assigned directly at
  `allpaths.c:3976` — one `RelOptInfo` per base table, each holding the access
  paths Step 1 built (`set_base_rel_pathlists`, `:384`). The loop at `:3978`
  then runs `join_search_one_level` for lev = 2 and lev = 3
  (`joinrels.c:78`). At lev = 2, pass 1 pairs each level-1 rel with each other
  initial rel it shares a join clause with (`:123`), producing `{AB}`, `{AC}`,
  `{BC}` — pass 2 does nothing because it requires `k <= level - k`, i.e.
  `2 <= 0`, false. At lev = 3, pass 1 joins each level-2 rel to the remaining
  initial rel, and pass 2 again does nothing (k = 2 needs other_level = 1,
  and `k > other_level` breaks at `:161`). So **A⋈B⋈C has no bushy plans at
  all** — bushy first becomes possible at level 4, as 2+2. After each level,
  `set_cheapest` (`pathnode.c:268`) runs on every finished joinrel.
  </details>

- [ ] For n = 5, 10 and 15 relations, compare n! against the number of
      (set, last-relation) pairs the DP considers. Where does exhaustive search
      die?
  <details><summary>Answer</summary>

  The DP considers `n·2^(n-1) − n` pairs and memoizes `2^n − 1` subsets:

  ```
   n            n!      DP considered      memo     n! / DP
   5           120                 75        31         1.6
  10     3,628,800              5,110     1,023       710.1
  15 1,307,674,368,000        245,745    32,767   5,321,265.4
  ```

  At n = 5 the DP is barely worth the bookkeeping. At n = 10 it is 710× less
  work, at n = 15 it is 5.3 million× less. But look at the memo column: it is
  still `2^n`, so the DP buys you roughly five more relations, not unlimited
  scaling — which is precisely why `geqo_threshold` is 12
  (`guc_parameters.dat:1191`) and not 50. (Counts computed here for a complete
  join graph; connectedness pruning makes the real numbers smaller.)
  </details>

- [ ] Name the three default selectivities and explain why `0.005` rather than
      something rounder.
  <details><summary>Answer</summary>

  `DEFAULT_EQ_SEL 0.005` (`selfuncs.h:34`), `DEFAULT_INEQ_SEL 0.3333333333333333`
  (`:37`), `DEFAULT_RANGE_INEQ_SEL 0.005` (`:40`). The comment at `:24-30` gives
  the reason, and it is not accuracy: the values are "small enough to ensure
  that indexscans will be used if available, for typical table densities of
  ~100 tuples/page… 0.01 is not quite small enough, since that makes it appear
  that nearly all pages will be hit anyway". It is a *policy* constant
  disguised as an estimate. The same comment ties `DEFAULT_NUM_DISTINCT` to it:
  200 = 1/0.005 (`:52`), so the two fallbacks agree with each other.
  </details>

- [ ] Three unknown equality predicates on a 10,000,000-row table. What does
      postgres estimate, and what does that make it do?
  <details><summary>Answer</summary>

  Independence makes selectivity multiplicative: `0.005³ = 1.25e-7`, so
  `10,000,000 × 1.25e-7 = 1.25` rows, clamped to 1 by `clamp_row_est`
  (`costsize.c:215`). Postgres now believes a ten-million-row table yields one
  row, which makes it an ideal inner side for a nested loop — and if the truth
  is 100,000 rows, that nested loop runs 100,000× more iterations than planned.
  This is the mechanism behind the JOB paper's finding (§4.1) that disabling
  non-index nested-loop joins removed *every* timeout from their workload.
  </details>

- [ ] `add_path` is often summarized as "keep the cheapest path plus one per
      interesting order". What does that summary leave out?
  <details><summary>Answer</summary>

  Five of the seven axes. The real dominance test (`pathnode.c:391-412`,
  `:518-533`) compares `disabled_nodes` (a *higher-order* term above cost, so a
  path using no disabled node type wins regardless of cost, `:399-406`),
  `startup_cost` and `total_cost` separately, `pathkeys`, `required_outer`
  parameterization (`:518`), output `rows` (`:524`) and `parallel_safe`
  (`:525`). It also compares costs **fuzzily** at `STD_FUZZ_FACTOR = 1.01`
  (`:47`, used at `:491`), so a 1% cost difference is treated as a tie — the
  comment at `:545-548` blames platform-specific floating-point roundoff. And
  parameterized paths are forced to have no pathkeys (`:472-473`) so they
  cannot win on sort order.
  </details>

- [ ] Above `geqo_threshold` relations, what exactly is being searched — trees
      or sequences?
  <details><summary>Answer</summary>

  **Sequences.** A chromosome is a `Gene *tour` (`geqo_eval.c:140`), a
  permutation of the relations, and the crossover operator is ERX — edge
  recombination crossover (`geqo.h:46`, `geqo_main.c:198-204`) — lifted from
  the travelling salesman problem. The *tree* is derived from the tour by
  `gimme_tree` (`geqo_eval.c:163`), which greedily adds each relation to the
  first "clump" it can legally join to (`:171-178`); that clump heuristic is
  what allows bushy shapes. The comment at `:146-157` says the original version
  joined strictly in tour order, "could never produce a 'bushy' plan", and
  broke on queries whose only legal plans are bushy. Sizing at n = 12:
  `pool_size = clamp(2^13, 50, 250) = 250`, `generations = pool_size = 250`
  (`geqo_main.c:339-349`, `:366`), one evaluation per generation (`:230`) —
  roughly 500 cost evaluations total.
  </details>

## References

**Code** — `postgres/postgres@701f021`, ~1.5 h
- `src/backend/optimizer/path/allpaths.c` — `make_one_rel`,
  `make_rel_from_joinlist`, `standard_join_search`. Start here.
- `src/backend/optimizer/path/joinrels.c` — `join_search_one_level`; read both
  passes.
- `src/backend/optimizer/util/pathnode.c` — `add_path`; read the header comment
  before the body.
- `src/backend/optimizer/geqo/` — `geqo_main.c` and `geqo_eval.c`.
- `src/include/utils/selfuncs.h` and `src/backend/utils/adt/selfuncs.c` — the
  constants and the statistics machinery.
- `src/backend/optimizer/README` — postgres's own prose explanation of the
  above; read it if any of Step 3 was unclear.

**Papers**
- Selinger, Astrahan, Chamberlin, Lorie, Price — "Access Path Selection in a
  Relational Database Management System", SIGMOD 1979. The design this file
  still implements; see `reading-selinger-cascades.md`.
- Leis et al. — "How Good Are Query Optimizers, Really?", PVLDB 2015. What
  happens to Step 6's estimates on real data; see
  `reading-how-good-optimizers.md`.

**In this topic**
- `reading-selinger-cascades.md` — where the DP and interesting orders come
  from, and the rule-driven alternative.
- `reading-duckdb-optimizer.md` — the same job, done in 2024, with a different
  enumeration order and the same threshold of 12.
