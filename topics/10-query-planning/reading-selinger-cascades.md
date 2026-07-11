# Selinger and Cascades: the two optimizer architectures

Two papers, 16 years apart, that define the design space every optimizer
lives in: Selinger '79 invented cost-based join search as bottom-up DP;
Graefe's Cascades '95 turned the whole optimization process into rules
firing in a memo. Read Selinger closely (it's short and shockingly
modern), then Cascades for the generalization.

## 1. "Access Path Selection in a Relational DBMS" (Selinger et al., SIGMOD '79)

System R's optimizer. Nearly everything survives:

- **Cost = weighted I/O + CPU**: `PAGE FETCHES + W × RSI CALLS`. One
  formula, two resources. (Modern engines still argue about W.)
- **Selectivity factors** (§4): 1/ICARD(index) for equality — the 1/NDV
  uniformity assumption, born here. The defaults table (1/10 for "no
  info" equality…) is postgres's DEFAULT_EQ_SEL's grandparent.
- **Access path selection**: per relation, cost every index vs segment
  scan, keep the cheapest — plus the cheapest per INTERESTING ORDER
  (order useful to a later join or ORDER BY/GROUP BY). The DP state
  refinement that makes merge-join plans findable.
- **The DP** (§5): best plan for a set of n relations = best(best plan
  for n-1) ⋈ nth. Left-deep trees only, cartesian products deferred to
  last. Complexity: the famous "n joins considered in O(2ⁿ)-ish sets".
- **Nested queries** (§6): correlated subqueries re-evaluated per row —
  the pre-decorrelation world DuckDB's deliminator escapes.

The DP, as code — best plan for a set composed from best plans of subsets:

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

Reading exercise: their example query (§5's OPTIMAL plans tables) —
follow the DP tables by hand once; it's the same table your
experiments' `reorder_joins` builds.

## 2. "The Cascades Framework for Query Optimization" (Graefe '95)

The generalization: optimization itself becomes data.

- **Memo**: groups of logically-equivalent expressions; members share
  cardinality estimates. Duplication-free search space.
- **Rules**: transformation rules (logical→logical: commute, associate)
  and implementation rules (logical→physical: Join→HashJoin). Adding an
  operator or algorithm = adding rules, not editing a search loop.
- **Top-down, goal-driven**: "optimize group G under requirement R
  (e.g. sorted by x)" spawns tasks; guidance/promise heuristics order
  rule firing; branch-and-bound pruning kills subtrees that already
  cost more than the best known plan.
- **Enforcers**: sort/exchange as operators the search inserts to meet
  required properties — how distributed engines later got shuffle
  planning for free.

vs Selinger:

| | Selinger (bottom-up) | Cascades (top-down) |
|---|---|---|
| search | DP over relation sets | memoized task recursion |
| space | joins only; rewrites separate | rewrites + physical, one space |
| pruning | none needed (small space) | branch-and-bound essential |
| extensibility | edit the enumerator | add a rule |
| shipped in | postgres, DuckDB, SQLite | SQL Server, CockroachDB, Orca |

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
