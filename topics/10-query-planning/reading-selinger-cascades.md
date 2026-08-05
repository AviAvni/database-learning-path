# Selinger and Cascades: the two optimizer architectures

Two papers, 16 years apart, that define the design space every optimizer lives
in: Selinger '79 invented cost-based join search as bottom-up DP; Graefe's
Cascades '95 turned the whole optimization process into rules firing in a memo.
Before the papers, this chapter builds the eight ideas they contributed, one at
a time — cost as a number, selectivity factors, access paths and interesting
orders, the DP itself, then the memo, rules, and top-down search — and closes
with the comparison that decides M10. Read Selinger closely (it's short and
shockingly modern), then Cascades for the generalization.

**Every figure and formula below is quoted from one of these papers and named
to its section, table or figure**, or is arithmetic done here on stated
assumptions. Where a *runtime* number is needed it comes from the JOB paper
(`reading-how-good-optimizers.md`) with its section cited, because **topic 10
deliberately has no measured lane** — its harness measures only your own code —
so nothing here is a timing from this machine and none of it appears in
`FINDINGS.md`. Selinger is quoted from the SIGMOD 1979 proceedings text;
Cascades from the IEEE Data Engineering Bulletin 18(3), 1995.

## The problem in one sentence

In 1979 nobody knew how to make a machine choose among the exponentially many
ways to evaluate one declarative query — Selinger's answer (estimate costs,
search bottom-up over subsets rather than over the n! orderings) still ships in
postgres, DuckDB and SQLite, and Cascades' answer (make the search itself
programmable, as rules firing into a memo) ships in SQL Server, CockroachDB and
Orca.

## The concepts, step by step

### Step 1 — cost as a single number: weighted IO + CPU

> **In:** a candidate access plan, plus System R's catalog statistics.
> **Out:** one scalar, so that two plans become comparable with `<`. This is
> what a **cost model** is: a formula from plan to number, intended to be
> monotone in runtime.

To compare plans you need each plan reduced to one comparable number. Selinger's
formula, stated verbatim in §3:

```
   COST = PAGE FETCHES + W * (RSI CALLS)

   PAGE FETCHES   disk pages read — the I/O term
   RSI CALLS      tuples returned across the Research Storage Interface,
                  System R's tuple-at-a-time storage API — the CPU proxy
   W              "an adjustable weighting factor between I/O and CPU"
```

The paper's justification for using tuple count as the CPU term: "Since most of
System R's CPU time is spent in the RSS, the number of RSI calls is a good
approximation for CPU utilization." One formula, two resources, one knob. The
paper gives **no numeric value for W** — it is left as a tuning parameter
throughout.

**Work out what W has become, and what happens when it moves.** Postgres's
modern equivalents (`src/include/optimizer/cost.h:24-28` at `701f021`) are
`seq_page_cost = 1.0`, `random_page_cost = 4.0`, `cpu_tuple_cost = 0.01`. In
Selinger's units — cost denominated in sequential page fetches — that makes
`W = 0.01`: one page fetch is worth 100 tuples of CPU. Against a *random* page
it is `4.0/0.01 = 400`, which is exactly the ratio the JOB paper flags in §5.3
when it says PostgreSQL's defaults imply "processing a tuple is 400× cheaper
than reading it from a page". JOB §5.3 then *measures* the correction for a
main-memory machine: scaling the CPU cost parameters up by **50×** improved the
median runtime-prediction error from 38% to 30%. So `W: 0.01 → 0.5`.

Now push that through Selinger's own formula. Take a 10,000,000-row table in
100,000 pages, and compare a full segment scan against a non-clustered index
scan returning `k` rows (the paper's assumption for a large relation: one page
fetch per tuple retrieved, §4's TABLE 2 discussion):

```
   segment scan = 100,000 + W × 10,000,000
   index scan   =       k + W × k            = k(1 + W)

   W = 0.01   scan =   200,000   crossover at k = 198,020   =  2.0% of table
   W = 0.50   scan = 5,100,000   crossover at k = 3,400,000 = 34.0% of table
```

A 50× change in one constant moves the selectivity at which the optimizer
abandons the index from 2% of the table to 34% — a **17× shift in the crossover
point**. That is why W is not a detail: it is the parameter that decides which
half of your plans are index scans. (Arithmetic done here on the stated
assumptions; the 50× is JOB §5.3's measurement, the constants are postgres's.)

Why it matters: everything since is elaboration on this one line, and the
elaboration is mostly about W.

### Step 2 — selectivity factors: guessing what a predicate keeps

> **In:** a boolean factor (one conjunct of the WHERE clause in conjunctive
> normal form) and whatever the catalog knows.
> **Out:** a **selectivity factor** F in [0,1] — "the expected fraction of
> tuples which will satisfy the predicate" (§4) — from which **cardinality**
> (the row count of an intermediate result) follows by multiplication.

Costs depend on how many rows flow between operators, so §4 introduces the
selectivity factor and gives TABLE 1, which is worth reading as a list of
assumptions rather than a list of formulas:

```
   TABLE 1 (§4), the entries that matter

   column = value      F = 1 / ICARD(column index)   if an index exists
                       F = 1/10                      otherwise
   column1 = column2   F = 1 / MAX(ICARD1, ICARD2)   if both indexed
                       F = 1 / ICARD(i)              if only one indexed
                       F = 1/10                      otherwise
   column > value      F = (high - value)/(high - low)  if arithmetic & known
                       F = 1/3                       otherwise
   col BETWEEN a AND b F = (b - a)/(high - low),  else 1/4
   col IN (list)       F = n × F(=),  capped at 1/2
   p1 OR p2            F = F1 + F2 - F1 × F2
   p1 AND p2           F = F1 × F2
   NOT p               F = 1 - F
```

Four assumptions are born in that table, and all four are still the default in
every engine you will read:

- **Uniformity.** `F = 1/ICARD` for equality — ICARD is the index's count of
  distinct keys — with the paper's own gloss: "This assumes an even
  distribution of tuples among the index key values." A **histogram** (a table
  of value ranges with the row count in each) is precisely the later fix for
  this, and postgres's MCV lists are the fix for its worst case.
- **Independence.** The `AND` row is a bare product, and the paper says so
  directly: "Note that this assumes that column values are independent."
- **The containment / inclusion assumption**, stated for joins: "This assumes
  that each key value in the index with the smaller cardinality has a matching
  value in the other index." Note that `F = 1/MAX(ICARD1, ICARD2)` is
  *literally* the formula the JOB paper audits 36 years later as PostgreSQL's
  join estimator (`|T1||T2| / max(dom(x), dom(y))`, JOB §2.3).
- **Honest fallback constants.** The paper does not pretend these are
  measurements. On the 1/3: "There is no significance to this number, other
  than the fact that it is less selective than the guesses for equal predicates
  for which there are no indexes, and that it is less than 1/2. We hypothesize
  that few queries use predicates that are satisfied by more than half the
  tuples."

**Trace the lineage, because one constant survived to the digit.** Postgres at
`701f021` defines (`src/include/utils/selfuncs.h`):

```
   Selinger 1979              postgres 701f021                  verdict
   column > value  F = 1/3    DEFAULT_INEQ_SEL 0.3333333333333333  identical
                              (selfuncs.h:37)                      to the digit
   column = value  F = 1/10   DEFAULT_EQ_SEL   0.005               20× tighter
                              (selfuncs.h:34)
```

The inequality guess is unchanged after 46 years. The equality guess was
tightened 20×, and postgres's comment at `selfuncs.h:24-30` says why — not
because 1/10 was measured wrong, but because the defaults must be "small enough
to ensure that indexscans will be used if available". It is a policy constant,
same as Selinger's was.

Why it matters: this is the input that JOB §3 shows is wrong by orders of
magnitude on real data. Reading TABLE 1 tells you *which* assumption each error
came from.

### Step 3 — access path selection, plus the interesting-orders refinement

> **In:** one relation, its indexes, and the boolean factors that apply to it.
> **Out:** *several* surviving plans, not one — the cheapest unordered access
> path plus the cheapest path producing each **interesting order**. That plural
> is the whole content of this step.

An **access path** is one concrete way to read a single relation: one of its
indexes, or a full segment scan. Cost every one with §4's TABLE 2 formulas and
keep the cheapest — that is what "access path selection", the paper's title,
means.

But Selinger keeps more than the cheapest, and here is where summaries get
imprecise. The paper's definition is **enumerable and explicit**, not a vague
"any order a later operator might like". Stated first for single relations:

> "We say that a tuple order is an **interesting order** if that order is one
> specified by the query block's GROUP BY or ORDER BY clauses."

and then extended, in the join section:

> "As in the single relation case, 'interesting' orders are those listed in the
> query block's GROUP BY or ORDER BY clause, if any. **Also every join column
> defines an 'interesting' order.**"

So the set is exactly: `ORDER BY columns ∪ GROUP BY columns ∪ every join
column`. Finite, computable from the query text before search begins, and
usually small. That precision is what makes the refinement affordable — you are
not keeping a plan per *possible* sort order, you are keeping one per member of
a short list.

The paper is equally explicit about the payoff and the alternative: "If there
are GROUP BY or ORDER BY clauses, then the cost for producing that interesting
ordering must be compared to the cost of the cheapest unordered path **plus**
the cost of sorting QCARD tuples into the proper order." A pricier-but-sorted
path wins globally exactly when it saves more than the sort would cost.

Note the important negative: "If there are no GROUP BY or ORDER BY clauses on
the query, then there will be no interesting orderings, and the cheapest access
path is the one chosen." The refinement costs nothing on queries that cannot
use it.

Why it matters: sortedness becomes part of the search state, which is what
makes merge-join plans findable at all. It is the state postgres kept (as
pathkeys, `reading-postgres-optimizer.md`) and DuckDB dropped
(`reading-duckdb-optimizer.md`), and Step 7 shows Cascades reinventing it from
the other direction.

### Step 4 — the DP: best plans compose from best subplans

> **In:** the per-relation plan lists from Step 3 and the query's join
> predicates.
> **Out:** one plan per (relation set, interesting order), built for
> successively larger sets — and finally the plan for the full set.

**Dynamic programming** is solving each distinct subproblem once and memoizing
the answer, justified by a principle of optimality. Selinger states that
principle in §5 in his own terms, and it is worth reading slowly:

> "once the first k relations are joined, the method to join the composite to
> the k+1-st relation is independent of the order of joining the first k; i.e.
> the applicable predicates are the same, the set of interesting orderings is
> the same, the possible join methods are the same, etc. Using this property, an
> efficient way to organize the search is to find the best join order for
> successively larger subsets of tables."

Note "**successively larger subsets**" — Selinger's DP is **bottom-up**, level
by level. (The sketch below matches the paper; a top-down memoized recursion
computes the same answer but is not what §5 describes, and the difference
becomes the whole story in Step 7.)

Two restrictions make it affordable.

**Left-deep only.** A **left-deep** plan is one where the right ("inner") input
of every join is a base relation, so the tree is a single spine; a **bushy**
plan allows a join whose both inputs are themselves joins. The paper does not
use either word — that vocabulary came later — but it states the restriction
plainly: "two relations are joined together, the resulting composite relation is
joined with the third relation, etc. At each step of the n-way join it is
possible to identify the outer relation (which in general is composite) and the
inner relation (**the relation being added to the join**)." The inner is always
a single relation. That is left-deep, and the paper gives the pipelining
motivation: intermediate composites "are physically stored only if a sort is
required for the next join step", otherwise materialized "one tuple at a time".

**Cartesian products last.** The paper's heuristic, conditions (1) and (2) of
§5, is that a relation is only added if it has a join predicate with something
already joined — unless nothing does. "This means that all joins requiring
Cartesian products are performed as late in the join sequence as possible."

```rust
// ILLUSTRATION — not quoted from any repo in this course. This is Selinger
// §5's "successively larger subsets", written as code. The production
// version of exactly this is postgres standard_join_search at
// src/backend/optimizer/path/allpaths.c:3952, with the per-level pairing in
// src/backend/optimizer/path/joinrels.c:78.
  1  // memo maps (relation set, interesting order) -> cheapest plan
  2  for level in 2..=n {
  3      for set in subsets_of_size(rels, level) {
  4          for r in set.iter() {
  5              let rest = set.without(r);            // left-deep: (k) join 1
  6              if !has_join_predicate(rest, r) { continue; }   // §5 heuristic
  7              for sub in memo.plans_for(rest) {     // one per interesting order
  8                  for method in [NestedLoop, MergeScan] {
  9                      let p = join(sub, access_paths(r), method);
 10                      memo.keep_if_cheapest(set, p.order, p);
 11                  }   // "cheapest unordered" AND "cheapest per order" both kept
 12              }
 13          }
 14      }
 15  }
```

**Work the counting, because this is the argument the whole step rests on.**
The paper opens §5's search discussion with "If a query block has n relations
in its FROM list, then there are n factorial permutations of relation join
orders." Evaluate that, against what the DP actually explores:

```
   n!                    all left-deep orderings, enumerated one by one
   n·2^(n-1) − n         (set, last-relation) pairs the DP considers
   2^n − 1               memo entries — one per non-empty subset
   (2n-2)!/(n-1)!        all BUSHY trees, for comparison (Selinger excludes these)

    n              n!                    bushy   DP considered     memo   n!/DP
    5             120                    1,680             75       31     1.6
   10       3,628,800           17,643,225,600          5,110    1,023   710.1
   15 1,307,674,368,000 3,497,296,636,753,920,000     245,745   32,767 5.3e6
```

Three readings, and they are the reason this step exists:

1. **At n = 5 the DP barely pays for itself** — 75 vs 120. If you only ever join
   five tables, enumerate and go home.
2. **At n = 10 it is 710× cheaper, at n = 15 five million times.** This is a
   qualitative change, not a constant factor, and it is why the technique was
   worth a paper.
3. **It is still exponential.** The memo column is `2^n − 1`, so the DP buys
   about five more relations, not unlimited scaling. Every real system bolts on
   a fallback above roughly 12 relations: postgres's genetic optimizer at
   `geqo_threshold = 12`, DuckDB's greedy operator ordering at the same
   threshold. Not a coincidence — it is where `2^n` stops fitting in a planning
   budget.

Note also the bushy column: by excluding bushy trees Selinger is giving up a
space 2.7 million× larger at n = 15. JOB §6.2 Table 2 later measured what that
costs — left-deep is at the *median* 1.00× optimal with PK indexes and 1.06×
with PK+FK, and 1.63×/4.50× at the maximum. Cheap, and the pipelining argument
above is why.

Reading exercise: follow §5's OPTIMAL-plan tables by hand once. It is the same
table your experiments' `reorder_joins` builds.

Why it matters: this is the single most-copied algorithm in database history,
and the counting above is why nobody has replaced it for small n.

### Step 5 — what Selinger punted: nested queries

> **In:** a query block containing a correlated subquery — one that references
> a column of the enclosing block's row.
> **Out:** a plan that re-runs the subquery, in the general case once per outer
> tuple. Correct, and O(outer × inner).

§6 handles nested queries by evaluating the inner block per outer row: "A
correlation subquery must in principle be re-evaluated for each [tuple of the
enclosing block]." The paper does add optimizations — an *uncorrelated*
subquery is evaluated once, and re-evaluation "can be made conditional… to
avoid re-evaluating subqueries unnecessarily" when the correlated values repeat
— but the baseline semantics is a nested loop, and the cost is multiplicative.

This is the pre-decorrelation world. Turning correlated subqueries into joins —
DuckDB's "deliminator" pass (`OptimizerType::DELIMINATOR`,
`src/optimizer/optimizer.cpp:242`), DataFusion's
`datafusion/optimizer/src/decorrelate_predicate_subquery.rs:130` — took decades to get right and is still
the hardest rewrite family in any pipeline.

Why it matters: read §6 as the "before" picture. It tells you exactly what
problem all that machinery exists to escape, and why an engine's subquery
support is a fair proxy for its optimizer's maturity.

### Step 6 — Cascades' memo: the search space as data

> **In:** the original query tree.
> **Out:** a **memo** — the search space itself, stored as data that rules can
> read and write, rather than a control flow that a search loop walks.

Sixteen years later Graefe's move is to make optimization itself programmable.
Three terms, which must be kept distinct because Cascades papers use them
precisely:

- An **expression** is one operator with its inputs — but the inputs are
  *group* references, not sub-expressions. `Join(G2, G3)` is one expression.
- A **group** is an equivalence class: the set of all expressions that produce
  the same logical result. Because they are logically equivalent, they share one
  **cardinality** estimate — which is exactly the property that makes the memo
  compact.
- The **memo** is the whole collection of groups. `optimize()` "first copies the
  original query into the internal 'memo' structure" (§2) and everything after
  that is rules adding expressions to groups.

```
 memo:  G1 = { Join(G2,G3), Join(G3,G2), HashJoin(G2,G3), MergeJoin(G2,G3) }
        G2 = { Scan(A), IndexScan(A) }        groups = equivalence classes
        G3 = { Scan(B) }                      members share a cardinality
```

Because expressions reference groups rather than trees, one memo encodes
exponentially many complete plans without duplication: G1 above stands for
2 × 1 × 4 = 8 complete plans in seven stored nodes. Selinger's memo keyed by
relation-set is the special case where the only equivalences considered are
join reorderings; Cascades' groups can hold *any* logically equivalent
expressions — including rewritten predicates, since the paper explicitly allows
"logically equivalent forms of all expressions, e.g., of a predicate".

Why it matters: once the search space is data, "add a capability" means "add a
rule that writes into the memo", and that is the entire extensibility argument.

### Step 7 — everything is a rule; search is top-down and goal-driven

> **In:** the memo, a rule set, and one root **optimization goal**.
> **Out:** the cheapest complete physical plan satisfying that goal — produced
> by six task types pushing each other onto a stack.

Cascades' knowledge lives in **rules**, of two kinds:

- a **transformation rule** rewrites logical → logical (commute a join,
  associate two joins, push a predicate);
- an **implementation rule** rewrites logical → physical (`Join` → `HashJoin`).

Adding an operator or an algorithm means adding rules, not editing a search
loop. Rules are objects (§1's contribution list: "Rules as objects"), and the
paper lists schema- and even query-specific rules as supported.

**The goal is richer than "optimize this group".** §2: an optimization task
"combines a group or expression with a **cost limit** and with **required and
excluded physical properties**". Three components, and the excluded ones are
the part usually forgotten.

Search is a set of six task types (§2, Figure 1):

```
   Optimize Group        find the best plan for any expression in a group
   Optimize Expression   optimize a single new expression
   Explore Group         derive logical expressions matching a pattern
   Explore Expression    the same, for one expression
   Apply Rule            fire one rule
   Optimize Inputs       recurse into inputs, accumulate cost
```

Tasks are **objects, not procedure calls** — "A task object exists for each
task that has yet to be done; all such task objects are collected in a task
structure", currently "a last-in-first-out stack". The paper is explicit that
this is an implementation choice, not a requirement: the structure could be "a
graph that captures dependencies… and permit efficient parallel search", and
the stack exists only "in order to obtain a working system fast".

Three mechanisms are load-bearing:

- **Memoization is in the Optimize-Group task.** "Before initiating
  optimization of all a group's expressions, it checks whether the same
  optimization goal has been pursued already" — so Cascades is *also* dynamic
  programming, just keyed by (group, goal) instead of by relation set.
- **Branch-and-bound pruning via the cost limit.** In Optimize Inputs, "Each
  time after an input has been optimized, the optimize inputs task obtains the
  best execution cost derived, and derives a new cost limit for optimizing the
  next input. Thus, pruning is as tight as possible." Unlike Selinger's bounded
  space, the rule-generated space has no a-priori bound, so this is not an
  optimization — it is what makes termination practical.
- **Enforcers.** Required properties are met by rules that insert an operator
  to produce them: "Consider the inputs to a merge-join's inputs, which must be
  sorted. An enforcer rule may insert a sort operation." And crucially,
  "enforcers such as sorting are normal operators in all ways" — they are
  costed and optimized like everything else.

**Enforcers are Step 3's interesting orders, inverted.** Selinger, going
bottom-up, has to *guess in advance* which orders will be wanted and keep extra
plans for each. Cascades, going top-down, already knows what the parent wants —
it is in the goal — so it *asks* for sortedness and inserts a sort when nothing
in the group provides it. Same problem, opposite direction, and the top-down
version generalizes for free: replace "sorted by x" with "partitioned by x" and
you have shuffle planning in a distributed engine, which is how later systems
got it without new machinery.

**The Cascades-over-Volcano delta, which is easy to miss.** Volcano's optimizer
generator ran two phases: exhaustively generate *all* logically equivalent
expressions, then optimize. Cascades explores **on demand and by pattern** — "A
group is explored using transformation rules only on demand, and it is explored
only to create all members of the group that match a given pattern." The paper's
own criticism of its predecessor: "The Volcano technique generates all
equivalent logical expressions exhaustively in the first phase. Even if [only a
few are needed]…" — with join associativity the exhaustive set is the whole
factorial space. Lazy, pattern-directed exploration is what makes the rule-based
architecture affordable at all.

Why it matters: this is the architecture you would copy if M10's rule set is
going to keep growing, and the on-demand exploration is the part that makes it
tractable.

### Step 8 — the design space, in one table

> **In:** Steps 1-7.
> **Out:** the one decision M10 actually has to make, and the evidence for
> either answer.

| | Selinger (bottom-up) | Cascades (top-down) |
|---|---|---|
| search | DP over relation sets, level by level | memoized task recursion over goals |
| memo key | relation set × interesting order | group × (cost limit, required/excluded props) |
| space | joins only; rewrites are a separate phase | rewrites + physical choice, one space |
| ordering | interesting orders kept as extra state | required properties, met by enforcers |
| pruning | none needed — space is bounded by `2^n` | branch-and-bound cost limits, essential |
| extensibility | edit the enumerator | add a rule |
| exploration | implicit in the level loop | on demand, pattern-directed |
| shipped in | postgres, DuckDB, SQLite | SQL Server, CockroachDB, Orca |

The pattern in the last row is not accidental. Bottom-up DP is simple and
predictable — `standard_join_search` is one readable loop, debuggable by
whoever inherits it. Cascades pays real complexity (six task types, a rule
language, a pattern matcher, cost-limit plumbing) for extensibility, which pays
off where a dedicated optimizer team writes rules for a living.

And Step 4's counting says something about *when* the trade is even live: below
about 12 relations, Selinger's space is small enough that the search algorithm
is not your problem. JOB Table 3 (§6.3) makes the same point from the other
end — with true cardinalities, swapping exhaustive DP for a greedy heuristic
costs 1.20× at the median. The architecture choice matters far less than the
estimates it consumes.

Why it matters: it means "which architecture" is a maintainability question
first and a plan-quality question second.

## How to read the papers (with the concepts in hand)

**Selinger first** — read it all; it's twelve pages.

- **§1-2** — System R context and the RSS/RSI split; skim, but note the RSI
  because Step 1's cost formula is denominated in it.
- **§3** — the cost formula (Step 1). One paragraph, and W is in it.
- **§4 — read carefully.** TABLE 1's selectivity factors (Step 2), then TABLE 2's
  single-relation cost formulas and the interesting-order definition (Step 3).
  For each constant, name its modern descendant.
- **§5 — the core.** The n! opening, the independence-of-prefix argument, the
  Cartesian-deferral heuristic, and the OPTIMAL-plan tables (Step 4). Work the
  tables by hand — the single best exercise in this topic.
- **§6** — nested queries (Step 5); read as the "before" picture of
  decorrelation.

**Then Cascades** — a framework paper, denser and drier, and only ten pages of
which §2-4 matter.

- **§1's bullet list** — the contribution list. Read it as a diff against
  Volcano; several bullets are Step 7's mechanisms in one line each.
- **§2 — the algorithm.** Figure 1's six tasks, the goal as
  (group, cost limit, required/excluded properties), the memoization check, and
  the cost-limit derivation in Optimize Inputs. The explore-on-demand
  discussion is here too and is the paper's real contribution.
- **§3-4 — the interface and rules.** Rules as objects, enforcer rules,
  promise-ordered moves, group merging. Don't chase the task scheduler's
  implementation details; the durable content is memo + rules + enforcers +
  branch-and-bound.
- Keep Step 8's table beside you and, for every mechanism, ask "what is the
  Selinger equivalent, and why doesn't it scale to rules?"

## Questions for notes.md

1. Selinger's W: redo Step 1's crossover arithmetic for your own machine's
   numbers (topic 6 has the latency figures). At what selectivity does the
   index stop winning, and which of your queries sit near that line?
2. Interesting orders are DP state; required physical properties plus enforcers
   are the Cascades equivalent. Write out why top-down is more natural for
   propagating them — what does the bottom-up version have to guess that the
   top-down version is told?
3. Cascades promises "adding an operator = adding rules". Check it: list the
   rules M10 needs for `Expand` (graph traversal as an operator) — the
   transformation rules (does `Expand` commute with `Filter`? with another
   `Expand`?) and the implementation rules (`Expand` → mxv? → per-node
   adjacency lookup?).
4. Why did the simple architecture win in open source and the complex one in
   commercial engines? Consider who writes the rules and who debugs the search
   at 2 a.m.
5. M10 decision to record: Selinger-style enumerator or mini-Cascades for the
   Cypher planner? FalkorDB today is heuristic plus label-cardinality anchor
   selection — which architecture is that closer to, and what would it cost to
   move?

## Takeaway

Selinger's contribution was not the cost formula (one line) or the selectivity
table (admittedly guessed constants, two of which postgres still ships). It was
the observation that the best plan for a *set* of relations does not depend on
how that set was built — which converts `15! = 1.3 × 10¹²` orderings into
245,745 considerations, a 5.3-million-fold saving, at the price of an
exponential memo that runs out at about 12 relations. Cascades' contribution
was to notice that the same memoization works when the memo's keys are
equivalence classes and its contents are produced by rules rather than by a
loop — which makes the optimizer extensible, requires branch-and-bound pruning
to stay finite, and turns Selinger's interesting orders into required
properties that the search asks for rather than guesses at.

## Done when

Answer each before unfolding it.

- [ ] Run Selinger's DP on a three-table join by hand. What is in the memo
      after each level, and how many entries are there in total?
  <details><summary>Answer</summary>

  Level 1 holds three entries, `{A}`, `{B}`, `{C}`, and each entry holds the
  cheapest unordered access path *plus* one path per interesting order
  (§4: ORDER BY ∪ GROUP BY columns, plus §5's "every join column defines an
  interesting order"). Level 2 holds `{AB}`, `{AC}`, `{BC}` — each built by
  taking a level-1 entry as outer and adding a single relation as inner
  (left-deep), trying nested loops and merging scans, and skipping any pair with
  no join predicate unless nothing qualifies. Level 3 holds `{ABC}`, built from
  each level-2 entry plus the remaining relation. Total memo entries:
  `2^3 − 1 = 7`, against `3! = 6` orderings — at n = 3 the DP is not yet
  winning, which is exactly Step 4's point.

  </details>

- [ ] Define an interesting order the way the paper does, not the way summaries
      do.
  <details><summary>Answer</summary>

  Selinger defines it twice, and both times as an explicit finite set, not as
  "any order a later operator might exploit". Single relations (§4): "a tuple
  order is an interesting order if that order is one specified by the query
  block's GROUP BY or ORDER BY clauses". Joins (§5): "As in the single relation
  case… Also **every join column** defines an 'interesting' order." So the set
  is `ORDER BY columns ∪ GROUP BY columns ∪ every join column` — computable
  from the query text before search starts, and usually small. That finiteness
  is what makes keeping one plan per order affordable. The paper also states the
  negative: with no GROUP BY or ORDER BY there are no interesting orders from
  the query block at all, and the cheapest path simply wins.

  </details>

- [ ] For n = 5, 10 and 15, give n!, the number of considerations the DP makes,
      and the memo size. Where does exhaustive search die, and where does the DP
      die?
  <details><summary>Answer</summary>

  ```
    n              n!     DP considered     memo   n!/DP
    5             120               75       31     1.6
   10       3,628,800            5,110    1,023   710.1
   15 1,307,674,368,000       245,745   32,767   5.3e6
  ```

  DP considered is `n·2^(n-1) − n` (one per (set, last-relation) pair); memo is
  `2^n − 1`. Exhaustive enumeration dies between n = 10 and n = 15 — a trillion
  orderings is not a planning budget. The DP dies later but for the same reason:
  its memo is still `2^n`, so it buys roughly five more relations, which is why
  postgres switches to a genetic algorithm at `geqo_threshold = 12` and DuckDB
  switches to greedy at the same count. For context, Selinger also excludes all
  bushy trees, a space of `(2n-2)!/(n-1)!` = 3.5 × 10²¹ at n = 15; JOB §6.2
  Table 2 measured that exclusion as costing 1.00-1.06× at the median.

  </details>

- [ ] Which of Selinger's 1979 constants is still in postgres unchanged, and
      which one moved?
  <details><summary>Answer</summary>

  **Unchanged to the digit:** the open-ended-comparison fallback, `F = 1/3`,
  survives as `DEFAULT_INEQ_SEL 0.3333333333333333`
  (`src/include/utils/selfuncs.h:37` at `701f021`). Selinger's own note on it —
  "There is no significance to this number, other than… it is less than 1/2. We
  hypothesize that few queries use predicates that are satisfied by more than
  half the tuples" — is still the only justification anyone has. **Moved:**
  equality with no index was `F = 1/10`; postgres uses `DEFAULT_EQ_SEL 0.005`
  (`:34`), 20× tighter. The reason is in postgres's own comment at `:24-30`, and
  it is policy rather than measurement: the defaults must be "small enough to
  ensure that indexscans will be used if available", and 0.01 was tried and
  found too large.

  </details>

- [ ] In Cascades, distinguish a group, an expression and the memo — and say
      why the distinction buys compactness.
  <details><summary>Answer</summary>

  An **expression** is one operator whose inputs are *group references*, e.g.
  `Join(G2, G3)` — not a subtree. A **group** is an equivalence class holding
  every expression that produces the same logical result; because they are
  logically equivalent they share one cardinality estimate. The **memo** is the
  set of all groups, seeded by copying the original query into it (§2). The
  compactness follows from the indirection: a group holding 4 expressions whose
  inputs are groups of 2 and 1 stands for 8 complete plans in 7 stored nodes,
  and the factor is multiplicative down the tree. Selinger's relation-set memo
  is the special case where the only equivalence is join reordering; a Cascades
  group can hold any logically equivalent expressions, "e.g., of a predicate".

  </details>

- [ ] Cascades needs branch-and-bound pruning and Selinger does not. Why?
  <details><summary>Answer</summary>

  Because their search spaces are bounded differently. Selinger's space is
  fixed before search begins — left-deep trees over subsets, `2^n − 1` memo
  entries — so exhaustive is affordable by construction. Cascades' space is
  generated by rules, and a rule set containing join associativity and
  commutativity generates without a-priori bound, so something must cut it off.
  The mechanism is the **cost limit** carried in every optimization goal
  alongside the required and excluded physical properties (§2): in the Optimize
  Inputs task, "Each time after an input has been optimized, the optimize
  inputs task obtains the best execution cost derived, and derives a new cost
  limit for optimizing the next input. Thus, pruning is as tight as possible."
  The second half of the answer is exploration: Cascades explores groups "only
  on demand… only to create all members of the group that match a given
  pattern", which is precisely what Volcano did *not* do, and what made the
  exhaustive first phase untenable.

  </details>

- [ ] Enforcers and interesting orders solve the same problem. State the
      difference in one sentence, then say what the top-down version gets for
      free.
  <details><summary>Answer</summary>

  Selinger, searching bottom-up, must **guess in advance** which orders a
  not-yet-chosen parent will want and pay to keep an extra plan for each;
  Cascades, searching top-down, is **told** what the parent requires as part of
  the optimization goal and inserts an enforcer — "An enforcer rule may insert a
  sort operation" — only when no member of the group already provides it. The
  free lunch is generalization: the goal carries arbitrary *required physical
  properties*, so replacing "sorted by x" with "partitioned by x" gives you
  exchange/shuffle planning in a distributed engine with no new machinery, and
  because "enforcers such as sorting are normal operators in all ways" they get
  costed and optimized like anything else.

  </details>

## References

**Papers**
- Selinger, Astrahan, Chamberlin, Lorie, Price — "Access Path Selection in a
  Relational Database Management System", SIGMOD 1979, pp. 23-34. Read it all;
  it's short. §3's cost formula, §4's TABLE 1 selectivity factors and the
  interesting-order definition, and §5's DP are the core. ~1 h.
- Graefe — "The Cascades Framework for Query Optimization", IEEE Data
  Engineering Bulletin 18(3), 1995, pp. 19-29. The memo, the six tasks, rules
  as objects, enforcers, and explore-on-demand. ~1 h.
- Graefe, McKenna — "The Volcano Optimizer Generator: Extensibility and
  Efficient Search", ICDE 1993. Read second if Cascades' repeated criticisms of
  "the Volcano technique" are opaque; it is the two-phase design Cascades
  replaces.
- Leis et al. — "How Good Are Query Optimizers, Really?", PVLDB 2015. Measures
  what Step 2's assumptions cost on real data, and Table 2/Table 3 quantify
  Step 4's and Step 8's restrictions. See `reading-how-good-optimizers.md`.

**Code — the descendants**
- `postgres/postgres@701f021` — `src/backend/optimizer/path/allpaths.c:3952`
  (`standard_join_search`) is Step 4, `src/include/utils/selfuncs.h:34-40` is
  Step 2, still. See `reading-postgres-optimizer.md`.
- `duckdb/duckdb@6c0c1a68` — `src/optimizer/join_order/` is Step 4 with a
  different enumeration order and no interesting orders. See
  `reading-duckdb-optimizer.md`.
- `apache/datafusion@1e77af8` — `datafusion/optimizer/src/optimizer.rs` is a
  distant, memo-less descendant of Step 7's rule architecture. See
  `reading-rust-planner-stack.md`.
