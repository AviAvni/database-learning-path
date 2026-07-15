# Selinger and Cascades: the two optimizer architectures

Two papers, 16 years apart, that define the design space every optimizer
lives in: Selinger '79 invented cost-based join search as bottom-up DP;
Graefe's Cascades '95 turned the whole optimization process into rules
firing in a memo. Before the papers, this chapter builds the eight ideas
they contributed, one at a time — cost as a number, selectivity factors,
access paths and interesting orders, the DP itself, then the memo, rules,
and top-down search — and closes with the comparison that decides M10.
Read Selinger closely (it's short and shockingly modern), then Cascades
for the generalization.

## The problem in one sentence

In 1979 nobody knew how to make a machine choose among the exponentially
many ways to evaluate one declarative query — Selinger's answer (estimate
costs, search bottom-up with DP) still ships in postgres, DuckDB, and
SQLite, and Cascades' answer (make the search itself programmable) ships
in SQL Server and CockroachDB.

## The concepts, step by step

### Step 1 — cost as a single number: weighted IO + CPU

To compare plans, you need each plan reduced to one comparable number.
Selinger's formula: `COST = PAGE FETCHES + W × RSI CALLS` — disk page
reads, plus CPU work (RSI calls — tuple-fetch calls into System R's
storage interface) scaled by a tuning weight W that says how many CPU
operations equal one IO. One formula, two resources. Everything since is
elaboration: modern engines still argue about W, and as storage moves
NVMe→RAM the right W shifts by ~100× — enough to flip plan choices
(question 1 below).

### Step 2 — selectivity factors: guessing what a predicate keeps

Costs depend on how many rows flow between operators, so Selinger's §4
introduces the **selectivity factor** — the estimated fraction of rows a
predicate keeps. For `col = value` with an index: 1/ICARD(index), where
ICARD is the number of distinct keys — i.e., assume every value equally
frequent. This is the **uniformity assumption**, born here, and it is
still the default in every engine you'll read. So are the fallback
constants: the paper's table says equality with no information = 1/10 —
the direct grandparent of postgres's `DEFAULT_EQ_SEL = 0.005`. A guess
from 1979, wearing modern clothes, still powering plan choices.

### Step 3 — access path selection, plus the interesting-orders refinement

For each single relation, cost every way to read it — each index versus
a full segment scan (an **access path** is one such concrete way) — and
keep the cheapest. But Selinger keeps more than one: also the cheapest
path per **interesting order** — a sort order of the output that some
later operator could exploit (a merge join, ORDER BY, GROUP BY). A
pricier-but-sorted path can win globally by saving a sort later, so
sortedness becomes part of the DP state. This one refinement is what
makes merge-join plans findable at all, and it's the state postgres kept
and DuckDB dropped (see those guides).

### Step 4 — the DP: best plans compose from best subplans

The join search (§5) is **dynamic programming**: the best plan joining a
set of n relations must be "best plan for some (n−1)-subset" joined with
the remaining relation — so compute and memoize best plans for sets of
size 1, then 2, then 3… Selinger restricts to **left-deep** trees (the
right input of every join is a base relation, never another join) —
smaller space, and every intermediate result pipelines into the next
join — and defers cartesian products (joins with no connecting predicate)
to last. The DP, as code:

```rust
fn best_plan(rels: RelSet, memo: &mut HashMap<RelSet, Plan>) -> Plan {
    if let Some(p) = memo.get(&rels) { return p.clone(); }
    let mut best = Plan::infinite_cost();
    for r in rels.iter() {
        let rest = rels.without(r);
        if !has_join_predicate(rest, r) { continue; }  // defer cartesians
        let p = cheapest_join(best_plan(rest, memo), access_paths(r));
        if p.cost < best.cost { best = p; }            // left-deep: (n−1) ⋈ 1
        // Selinger also keeps the cheapest plan per INTERESTING ORDER here —
        // a pricier-but-sorted subplan can win at a later merge join
    }
    memo.insert(rels, best.clone());
    best
}
```

Complexity: the famous "n joins considered in O(2ⁿ)-ish sets" — fine to
~12 relations, then every real system bolts on a fallback. Reading
exercise: follow §5's OPTIMAL plan tables by hand once; it's the same
table your experiments' `reorder_joins` builds.

### Step 5 — what Selinger punted: nested queries

§6 handles correlated subqueries (a subquery referencing the outer row)
by simply re-evaluating the subquery *per outer row* — correct, and
O(outer × inner). This is the pre-decorrelation world: turning correlated
subqueries into joins (DuckDB's "deliminator" pass) took decades to get
right, and it's still the hardest rewrite family in any pipeline. Reading
§6 tells you exactly what problem that machinery exists to escape.

### Step 6 — Cascades' memo: the search space as data

Sixteen years later, Graefe's move is to make optimization itself
programmable. The core structure is the **memo**: a set of **groups**,
each group being an equivalence class of plan fragments that all produce
the same result (and therefore share one cardinality estimate). Group
members can reference other groups as inputs — so one memo compactly
encodes exponentially many complete plans, duplication-free:

```
 memo:  G1 = {Join(G2,G3), Join(G3,G2), HashJoin(G2,G3), ...}
        G2 = {Scan(A), IndexScan(A)}     groups = equivalence classes,
        G3 = {Scan(B)}                   members share cardinality
```

Selinger's memo keyed by relation-set is a special case; Cascades'
groups can hold *any* logically-equivalent expressions, not just join
orders.

### Step 7 — everything is a rule; search is top-down and goal-driven

In Cascades, the optimizer's knowledge lives in **rules** of two kinds:
**transformation rules** (logical→logical: commute a join, associate)
and **implementation rules** (logical→physical: Join→HashJoin). Adding
an operator or algorithm = adding rules, not editing a search loop.
Search runs **top-down**: "optimize group G under requirement R (e.g.
sorted by x)" spawns tasks that fire rules into the memo; promise
heuristics order the firing; **branch-and-bound pruning** kills any
subtree already costlier than the best known complete plan (essential —
unlike Selinger's small space, the rule-generated space is unbounded).
Requirements are met by **enforcers** — sort (or, in distributed engines,
exchange/shuffle) inserted by the search itself to satisfy a required
property. Enforcers are Step 3's interesting orders, generalized: instead
of *keeping* sorted plans as extra state bottom-up, top-down search
*asks* for sortedness and inserts a sort when nothing provides it — and
that is how distributed engines later got shuffle planning for free.

### Step 8 — the design space, in one table

| | Selinger (bottom-up) | Cascades (top-down) |
|---|---|---|
| search | DP over relation sets | memoized task recursion |
| space | joins only; rewrites separate | rewrites + physical, one space |
| pruning | none needed (small space) | branch-and-bound essential |
| extensibility | edit the enumerator | add a rule |
| shipped in | postgres, DuckDB, SQLite | SQL Server, CockroachDB, Orca |

The pattern in the last row is not accidental: bottom-up DP is simple
and predictable — debuggable by whoever inherits it; Cascades pays
complexity for extensibility, which pays off where dedicated optimizer
teams write rules for a living (question 4 below).

## How to read the papers (with the concepts in hand)

**Selinger first** — read it all; it's short.

- **§2–3** — System R context; skim.
- **§4 — read carefully**: the selectivity-factor table (Step 2). Notice
  how many of the constants you can name modern descendants of.
- **§5 — the core**: access paths + interesting orders (Step 3) feeding
  the DP (Step 4). Work the OPTIMAL plans tables by hand — the single
  best exercise in this topic.
- **§6** — nested queries (Step 5); read as the "before" picture of
  decorrelation.

**Then Cascades** — a framework paper, denser and drier.

- The memo and groups first (Step 6), then the task structure and rule
  kinds (Step 7). Don't chase implementation details of the task
  scheduler; the durable content is memo + rules + enforcers +
  branch-and-bound.
- Keep Step 8's table beside you and, for every mechanism, ask "what is
  the Selinger equivalent, and why doesn't it scale to rules?"

## Questions for notes.md

1. Selinger's W (CPU weight): what happens to plan choice as storage
   moves NVMe→RAM (topic 6's numbers)? Which plans flip?
2. Interesting orders are DP state. What's the Cascades equivalent
   (required physical properties), and why is top-down more natural for
   propagating them?
3. Cascades promises "adding an operator = adding rules". Check it:
   list the rules M10 needs to add for `Expand` (graph traversal as an
   operator) — transformation (Expand commutes with Filter?) and
   implementation (Expand → mxv? → per-node lookup?).
4. Why did the simple architecture (bottom-up DP) win in open source and
   the complex one in commercial engines? (Consider: who writes the
   rules, who debugs the search.)
5. M10 decision to record: Selinger-style enumerator or mini-Cascades
   for the Cypher planner? (FalkorDB today: heuristic + label-cardinality
   anchor selection — which architecture is that closer to?)

## Done when

You can run Selinger's DP on a 3-table join by hand, and describe a memo
group's contents for the same query in Cascades terms.

## References

**Papers**
- Selinger, Astrahan, Chamberlin, Lorie, Price — "Access Path Selection
  in a Relational Database Management System" (SIGMOD 1979) — read it
  all; it's short, and §4's selectivity factors + §5's DP are the core
- Graefe — "The Cascades Framework for Query Optimization" (IEEE Data
  Engineering Bulletin 1995) — the memo, rules, and top-down task model
