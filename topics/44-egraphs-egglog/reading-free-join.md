# Free Join: the plan space that contains both hash join and generic join

The previous three chapters make a clean argument: e-matching is a
conjunctive query, generic join answers it with a worst-case optimal
bound, and egglog is built on that. This chapter is where the argument
gets complicated, and it is the reason egglog's *default* plan strategy
is not generic join.

**"Free Join: Unifying Worst-Case Optimal and Traditional Joins"** —
Yisu Remy Wang, Max Willsey and Dan Suciu, SIGMOD 2023
(arXiv:2301.10841) — starts from an uncomfortable observation. Ten
years after worst-case optimal joins were published, systems that adopt
them use them *only* for the cyclic part of a query and fall back to
binary joins for everything else, because binary joins have decades of
constant-factor engineering behind them. The paper's response is not to
pick a side. It shows the two algorithms are corners of one design
space, gives a plan language that covers all of it, and then does the
engineering — a data structure and a vectorized executor — that the
worst-case optimal side was missing.

It is the most conventionally *database* paper in this topic, and it is
where the e-graph literature pays its debt back: the ideas were
developed for query processing, and this is the query-processing paper
that came out of the e-graph group.

## The problem in one sentence

Generic join has the better asymptotic bound and binary hash join has
the better constant, and treating them as rival algorithms means every
system that wants both has to implement both and a rule for choosing —
whereas they are the same nested loop with two different settings of
"how many relations and how many attributes does one join step touch".

## The concepts, step by step

### Step 1 — why the dichotomy survived

> **In:** the AGM bound and generic join from
> [reading-relational-ematching.md](reading-relational-ematching.md)
> Steps 8–9. **Out:** the reason a provably better algorithm did not
> displace the one it beats.

The folklore the paper opens with (§1): *"WCOJ is designed for cyclic
queries"*. It has real support. On a cyclic query, generic join beats
any binary plan asymptotically. On an **acyclic** query — one whose
hypergraph admits a join tree — Yannakakis' algorithm is already
asymptotically optimal, so there is nothing left to win, and binary
joins arrive with "column-oriented layout, vectorization, and query
optimization … compounding constant-factor speedups".

So systems went hybrid: Umbra, EmptyHeaded, Graphflow all use WCOJ for
the cyclic subparts and binary joins elsewhere. The paper's complaint
about that is an engineering one, not a theoretical one: "Having two
different algorithms in the same system requires changing and
potentially duplicating existing infrastructure like the query
optimizer. This introduces complexity, and hinders the adoption of
WCOJ."

Two planners, two executors, and a rule for switching. Anyone who has
maintained a query engine knows what that costs.

### Step 2 — the two algorithms are the same loop

> **In:** Step 1's dichotomy. **Out:** the structural identity the whole
> paper is built on, stated in one sentence per algorithm.

§1, and it is worth memorising:

- **Binary join** "processes two relations at a time, and joins on all
  attributes in the join condition between these two relations."
- **Generic join** "processes one attribute at a time, and joins all
  relations that share that attribute."

Both are nested loops. A binary hash join iterates the tuples of one
relation and probes the hash table of another; a generic join level
iterates the keys of one trie and probes the others. The difference is
only in *what a single step is quantified over*:

```
                          relations per step        attributes per step
   binary hash join       2                         all shared ones
   generic join           all sharing the attribute 1
   Free Join              any number                any number
```

Which immediately suggests filling in the rest of the table. Figure 1
of the paper draws that design space and points out that the classic
multiway algorithms already live in it — Hash Teams, Generalized Hash
Teams, Eddies are all points between the two corners.

### Step 3 — one data structure for both: the GHT

> **In:** Step 2's design space. **Out:** the structure a plan in that
> space indexes over, and the two special cases it collapses to.

Before unifying the algorithms you must unify what they read. Binary
join reads a **hash table**; generic join reads a **trie**.

> **Definition 3.1 (Generalized Hash Trie).** "A GHT is a tree where
> each leaf is a vector of tuples, and each internal node is a hash map
> whose keys are tuples, and each key maps to a child node."

The **schema** of a GHT is the list `[y₀, y₁ … y_ℓ]` of the attribute
names keyed at each level. Now read off the two corners (§3.1):

- The trie used by generic join is a GHT **where each key is a tuple of
  size one**, and the last level stores empty vectors.
- The hash table used by binary join is a GHT with **only two levels**:
  level 0 the keys, level 1 vectors of tuples.

One structure, two configurations, and everything in between is legal.
Note how far this is from the previous chapters' `Trie`: our
`relational.rs::Trie` is the size-one-key case, hard-coded, because that
is all generic join needs.

### Step 4 — subatoms, and what a Free Join plan is

> **In:** the GHT of Step 3. **Out:** the plan language, its validity
> condition, and the word egglog's source uses — *cover*.

Three definitions (§3.2), each small:

- A **subatom** of an atom `R(x)` is `R(y)` for some subsequence `y` of
  `x` — the atom restricted to some of its columns.
- The subatoms of `R` used across a plan must form a **partitioning** of
  `R(x)`: every column appears in exactly one of them.
- A **Free Join plan** for a query is a list of *nodes*
  `[φ₁ … φ_m]`, each node a list of subatoms.

Each node is one loop level. Write `vs(φ_k)` for the variables of node
k, and `avs(φ_k) = ⋃_{j<k} vs(φ_j)` for the variables **available** from
earlier nodes. Then (Definition 3.7) a plan is **valid** when, in every
node, (a) no two subatoms come from the same relation, and (b) some
subatom contains all of `vs(φ_k) − avs(φ_k)` — every variable this node
is newly binding. That subatom is the node's **cover**.

The cover is the relation you *iterate*; everything else in the node you
*probe*. That is the entire execution model, and it is why the
definition insists the cover contains all the new variables: you cannot
iterate values you are not looking at.

Worked, on the paper's running "clover" query — three relations sharing
one attribute:

```
   Q♣(x, a, b, c) :- R(x, a), S(x, b), T(x, c)

   generic-join-like plan       binary-join-like plan
   [[R(x), S(x), T(x)],         [[R(x, a), S(x)],
    [R(a)],                      [S(b), T(x)],
    [S(b)],                      [T(c)]]
    [T(c)]]
   covers: R(x), R(a),          covers: R(x,a), S(b), T(c)
           S(b), T(c)
```

The left plan binds `x` by intersecting three single-column tries, then
expands `a`, `b`, `c`. The right one iterates `R` whole and probes `S`,
then probes `T`. Same language, and every hybrid in between is
expressible — which is the contribution.

### Step 5 — converting a binary plan, then factoring it

> **In:** Step 4's plan language. **Out:** how the system gets a good
> plan without a new optimizer, which is the practical crux.

The paper does not build a new query optimizer. It takes an *existing*
binary join plan — from a real optimizer, with its cardinality
estimates and decades of tuning — and converts it into a Free Join plan
that "runs as fast or faster" (§1, §4.1). A left-deep binary plan maps
directly onto a list of two-subatom nodes.

Then it **factors**: split a node whose cover carries several new
variables into two nodes, so that the more selective intersection
happens first and the remaining attributes are expanded later. Factoring
is the move that turns the binary-join-like plan of Step 4 into the
generic-join-like one, and it is applied only where the estimates say it
pays.

This is why egglog's default strategy is `MinCover` rather than `Gj`
(`core-relations/src/free_join/plan.rs:35-41`), and why the source
comment can say a Free Join plan "degenerates to a hash join" at one end
and "~ recovers generic join" at the other. Read that comment again
after this step; it is a two-line summary of this paper.

### Step 6 — COLT: pay for the index only where you probe

> **In:** the GHT of Step 3 and the plans of Steps 4–5. **Out:** the
> data structure, and the specific waste it removes from generic join.

Generic join's cost is not only the loops. Before it runs, the tries
have to be built — a cost this topic's own bench prints in its
`index µs` column, and which POPL'22 Table 1 splits its rows on.

**COLT** — Column-Oriented Lazy Trie (§4.2) — attacks it from two sides.

*Lazy*: a trie level is built only when something actually looks it up.
A COLT starts as a single leaf holding the offsets of every tuple in the
base table; on the first `get` at a level, that level is materialised.
If a subtrie is never probed, it is never built. The paper's improvement
over the earlier lazy trie of Umbra is that COLT "completely eliminates
the cost" of building at least one level per table — with the neat
special case that if a relation is only ever *iterated* (it is a cover
and nothing gets it), no auxiliary structure is built for it at all.

*Column-oriented*: the leaves are vectors of **offsets into the base
table** rather than copies of the tuples, so the trie stores integers
and the payload columns stay where they were. Topic 12's readers will
recognise the pattern and the reason: you touch only the columns the
join actually uses.

Set this against the honest number in our own lane 1: at N = 1600 the
tries cost 322.4 µs and the join itself 145.0 µs — **the index build is
69% of generic join's total time**, and it is charged in full on every
call because our matcher rebuilds it every time. COLT is the answer to
that column, and exercise 4 of this topic is a small version of it.

### Step 7 — vectorized execution

> **In:** the Free Join execution of Step 4. **Out:** the second
> constant-factor recovery, and where you have met it before.

Generic join as published is tuple-at-a-time recursion. Free Join
batches: at each node, collect a batch of bindings and probe the other
subatoms for the whole batch, "so these probes issue the same set of
relations for each tuple". This is exactly topic 11's tuple-at-a-time
versus batch-at-a-time result, arriving in a join algorithm for the same
reasons — fewer indirect branches, better cache and TLB behaviour, and
memory-level parallelism from independent probes in flight.

You can see the shape in egglog's executor, which accumulates into
`FrameUpdates` and drains in chunks (`free_join/execute.rs:1453`).

### Step 8 — the numbers, including the ones that are below 1

> **In:** Steps 5–7. **Out:** what the combination actually bought, on
> real benchmarks, with the losses stated.

Setup (§5): implemented in Rust, evaluated on the **Join Order
Benchmark** (JOB — real IMDb data, the benchmark topic 22 and topic 10
both use) and **LSQB**, against the in-memory column store DuckDB as the
binary-join baseline, its own Generic Join implementation, and Kùzu.

```
   Free Join vs.          geometric mean      maximum       minimum
   binary join (DuckDB)         2.94x          19.36x    0.85x  (a 17% slowdown)
   Generic Join                 9.61x          31.6x     2.63x
```

Two things to take from that table. First, the geometric means are the
honest summary and they are much smaller than the maxima — 2.94× is a
good result, not a revolution. Second, **the minimum against binary join
is below one**: on some queries Free Join loses, and §5 says why —
those plans are bushy and materialise a large intermediate, and "we have
not spent much effort optimizing for materialization".

The instructive single query is JOB's **Q13a** (§5.2). DuckDB takes over
10 seconds, Generic Join 7 seconds, Free Join just over 1. The plan
explains it: the first three binary joins are over four very large
tables, two of them many-to-many, "exploding the intermediate result to
contain over 100 million tuples" — and all three joins are on the *same
attribute*, which makes it the clover query of Step 4. Generic Join and
Free Join intersect on that attribute and "expand the remaining
attributes only after other more selective joins".

The paper draws the right conclusion rather than the flattering one: the
binary plan *could* have been fast had the optimizer ordered the
selective joins first. What the WCOJ-style plan bought here was
**robustness to a bad plan**, not raw speed — a claim §5.4 then tests
directly. And §5.2 notes elsewhere that performance "is not solely
determined by the cyclicity of the query; the presence of skew in the
data is another important factor", which is the folklore of Step 1
finally being stated properly.

### Step 9 — what this means back in the e-graph

> **In:** everything above, plus
> [reading-egglog-source.md](reading-egglog-source.md) Step 6.
> **Out:** why the engine that started this topic ships with generic
> join as the *option* rather than the default.

egglog's planner offers `PlanStrategy::Gj` and the Free Join strategies
`PureSize`/`MinCover`, and the rebuild rules explicitly ask for
`MinCover` (`egglog-bridge/src/lib.rs:956`). The source comment's
footnote is the trade stated plainly: Free Join "is not worst-case
optimal because it does not necessarily pick the smallest side to scan".

So the arc of this topic ends where a database course should want it to.
POPL'22: your matcher is a query, here is the optimal algorithm.
PLDI'23: then make the database primary and get incrementality free.
SIGMOD'23: and then, having won the asymptotics, spend the next paper
winning back the constants — with lazy indexes, column layout and
vectorized execution, the same three things every analytical engine in
topics 11 and 12 is made of.

## How to read the paper (with the concepts in hand)

1. **§1** whole. It is the clearest statement of the dichotomy anywhere,
   and Figure 1 is the paper's thesis in one picture.
2. **§2** if you want generic join re-derived; skip if the POPL'22
   chapter's Step 9 is fresh.
3. **§3.1** for the GHT (Step 3) and **§3.2** for the plan language
   (Step 4). Do the exercise of writing both plans for `Q♣` yourself
   before reading Example 3.6.
4. **§4.1** (conversion and factoring), then **§4.2** (COLT) — Figures
   11 and 12 together are the data structure.
5. **§5** with Step 8's table beside you; read §5.2's Q13a discussion
   and §5.4's robustness experiments, which are the paper's most useful
   pages for a practitioner.
6. **§6**, limitations, is short and worth it.

## Where each step lives in the code

Nothing in this topic's crate implements Free Join — that is exercise 6.
The production reference is egglog:

| step | file:line |
|---|---|
| 4, plan language | `core-relations/src/free_join/plan.rs:134-158` `JoinStage`, `:145` `FusedIntersect` (a cover plus the subatoms it probes) |
| 5, strategies | `plan.rs:32-41`; `PlanStrategy::MinCover` chosen at `egglog-bridge/src/lib.rs:956` |
| 5, fusion | `plan.rs:159-163` `fuse_single_scans` — merging single-scan stages onto one cover atom (the module doc at `:43` calls it `JoinStage::fuse`; the function on disk has the longer name) |
| 6, lazy index | `free_join/execute.rs:1431` (small subsets are refined, not indexed), `:1439` `get_cached_trie_node` |
| 7, vectorization | `free_join/execute.rs:1428` `FrameUpdates`, `:1453` chunked drain |
| this topic's toy | `relational.rs:78` `index_atom` builds every level eagerly — the thing COLT does not do |

## Questions (answer in notes.md)

1. Write both Free Join plans of Step 4 for lane 3's triangle
   multi-pattern, and give the cover of every node. Which is generic
   join, which is a binary plan, and what is the intermediate size of
   each on the V = 1600, E = 8000 graph?
2. Our `index µs` at N = 1600 is 69% of generic join's total. Which
   levels of the two tries does lane 1a's plan actually probe, and how
   much of that build would COLT's laziness skip? Estimate before
   measuring, then measure (exercise 4).
3. The minimum speedup against binary join is 0.85×. Construct the
   shape of a query where you would *expect* Free Join to lose, using
   §5's explanation, and say what you would change in the executor to
   fix it.
4. §5.2 claims WCOJ-style plans are more robust to bad plans. State
   that claim as a measurable property (not "more robust"), and design
   the experiment for lane 3.
5. A GHT with two levels is a hash table; with size-one keys it is a
   trie. What is a GHT with size-two keys, and which classic algorithm
   from Figure 1's design space does a plan over it correspond to?
6. egglog picks `MinCover` for its rebuild rules. Given what the rebuild
   rule's query looks like (two atoms, one of them the union-find
   table), argue whether worst-case optimality could ever matter there.

## Done when

Answer each before unfolding it.

- [ ] You can state the difference between binary join and generic join
      in one sentence each, without mentioning cyclicity.
  <details><summary>Answer</summary>

  Binary join processes **two relations at a time and joins on all the
  attributes shared between them**; generic join processes **one
  attribute at a time and joins all the relations that share it**
  (§1). They are the same nested loop with different quantifiers, which
  is why a design space parameterised by (relations per step,
  attributes per step) contains both — and contains Hash Teams and
  Eddies in between.
  </details>

- [ ] You can say what a GHT is and give its two degenerate cases.
  <details><summary>Answer</summary>

  A tree whose leaves are vectors of tuples and whose internal nodes are
  hash maps from *tuples* to child nodes (Definition 3.1); its schema
  names the attributes keyed at each level. Generic join's trie is the
  case where every key is a one-tuple and the last level holds empty
  vectors; binary join's hash table is the two-level case, keys at level
  0 and tuple vectors at level 1.
  </details>

- [ ] You can define a valid Free Join plan and identify a node's cover.
  <details><summary>Answer</summary>

  A plan is a list of nodes, each a list of subatoms (an atom restricted
  to a subsequence of its columns), such that each atom's subatoms
  partition it. With `vs(φ_k)` the node's variables and
  `avs(φ_k) = ⋃_{j<k} vs(φ_j)` the ones already available, the plan is
  valid when no two subatoms in a node share a relation and some subatom
  contains all of `vs(φ_k) − avs(φ_k)` — the newly bound variables. That
  subatom is the **cover**: the one you iterate, while the rest of the
  node is probed.
  </details>

- [ ] You can explain what COLT is lazy about and why it matters to this
      topic's own numbers.
  <details><summary>Answer</summary>

  A COLT starts as one leaf of offsets into the base table and
  materialises a trie level only on the first `get` at that level, so
  subtries that are never probed are never built — and a relation that
  is only ever iterated (always a cover, never probed) gets no auxiliary
  structure at all. Leaves hold offsets, not copied tuples, so the
  payload columns stay in the base table. It matters here because lane
  1a spends 322.4 µs building tries against 145.0 µs joining — 69% of
  the cost is index construction that is thrown away after one query.
  </details>

- [ ] You can quote the evaluation without overstating it.
  <details><summary>Answer</summary>

  Geometric means on JOB and LSQB: **2.94×** faster than binary join
  (DuckDB) and **9.61×** faster than its own Generic Join. Maxima
  19.36× and 31.6×; minima **0.85×** — a 17% slowdown against binary
  join on some queries — and 2.63×. The losing queries have bushy plans
  that materialise large intermediates, which the authors say they did
  not optimise. Q13a is the showcase: >10 s (DuckDB), 7 s (Generic
  Join), just over 1 s (Free Join), because the binary plan built an
  intermediate of over 100 million tuples across three joins on the same
  attribute.
  </details>

- [ ] You can say why egglog's default is not generic join.
  <details><summary>Answer</summary>

  Because worst-case optimality is a bound on the worst case, and the
  average case is decided by constants: index building, memory layout,
  batching. Free Join's `MinCover` strategy reaches a plan close to a
  binary plan where that is better and close to generic join where
  *that* is better, at the cost of the guarantee — egglog's own comment
  says it "is not worst-case optimal because it does not necessarily
  pick the smallest side to scan" (`plan.rs:41`). `PlanStrategy::Gj`
  remains available for when the guarantee is what you want.
  </details>

## References

- Yisu Remy Wang, Max Willsey, Dan Suciu, **"Free Join: Unifying
  Worst-Case Optimal and Traditional Joins"**, SIGMOD 2023,
  arXiv:2301.10841. §1 + Figure 1 (the design space), §3.1 (GHT,
  Definition 3.1), §3.2 (subatom, partitioning, plan, validity, cover —
  Definitions 3.4–3.8), §4.1 (conversion and factoring), §4.2 (COLT,
  Figures 11–12), §4.3 (vectorized execution, Figure 13), §5
  (evaluation, JOB and LSQB), §5.4 (robustness to bad plans), §6
  (limitations).
- Hung Q. Ngo et al., **"Worst-case Optimal Join Algorithms"** — the
  generic join this paper takes as the WCOJ representative.
- Mihalis Yannakakis, **"Algorithms for Acyclic Database Schemes"**,
  VLDB 1981 — why acyclic queries were already solved, which is half of
  Step 1.
- Viktor Leis et al., **"How Good Are Query Optimizers, Really?"**,
  VLDB 2015 — the Join Order Benchmark used here, read in
  [topic 10](../10-query-planning/reading-how-good-optimizers.md).
- Previous chapter:
  [reading-egglog-source.md](reading-egglog-source.md), whose
  `PlanStrategy` enum is this paper.
