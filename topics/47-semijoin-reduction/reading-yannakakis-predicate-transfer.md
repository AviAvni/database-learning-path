# Yannakakis and predicate transfer: two passes of semi-joins, then any order

A planner that picks a bad join order is not a rare failure; topic 10's
[reading-how-good-optimizers.md](../10-query-planning/reading-how-good-optimizers.md)
measured estimates off by orders of magnitude on ordinary queries. Most of
the field's answer has been "estimate better". This chapter is about the
other answer, which is forty-five years old: arrange things so the order
stops mattering. Mihalis Yannakakis showed in 1981 that for a large class of
queries — the *acyclic* ones — a cheap preprocessing step makes **every**
join order produce intermediate results no larger than the final answer.
It was then ignored by engines for four decades, and the paper that brought
it back is the second source here:

**"Predicate Transfer: Efficient Pre-Filtering on Multi-Join Queries"** —
Yifei Yang, Hangdong Zhao, Xiangyao Yu, Paraschos Koutris, CIDR 2024. It
keeps Yannakakis' schedule and replaces its expensive step with Bloom
filters.

A note on sources, because CLAUDE.md requires one. The 1981 paper
("Algorithms for Acyclic Database Schemes", VLDB'81, pp. 82–94) could not be
obtained for this chapter: the VLDB Endowment's 1981 archive returned HTTP
403 and no other copy was found. Every statement below about what Yannakakis'
algorithm *guarantees* is therefore taken from the three 2024–2026 papers
that restate it — this one (§2.2), RPT+ (VLDB'26, §2.1) and the SQL Server
paper (CIDR'26, §2.1) — and from this topic's own implementation, which the
bench checks against the guarantee on every run. No number in this chapter
is attributed to the 1981 paper.

Every measured figure is from `semijoin_bench` lane 1 (provided) or from the
lane-2 reference implementation recorded in [notes.md](notes.md); every
crate line number is the file as committed in this topic.

## The problem in one sentence

The same three-hop query emits 23,246 intermediate tuples in one join order
and 3,020,210 in another — 130× apart, for the same 20,210-row answer — and
the planner has to choose between them using estimates that are often wrong;
can we instead make the bad order cheap?

## The concepts, step by step

### Step 1 — join and semi-join

> **In:** nothing yet. **Out:** the two operators every later step is built
> from, each worked on a three-row example.

A **relation** here is a table of rows; a **join** R ⋈ S on a shared column
pairs every row of R with every row of S that agrees on it. A **semi-join**
R ⋉ S keeps *the rows of R* that have at least one partner in S, and adds
nothing from S: R ⋉ S = π_attr(R)(R ⋈ S), the definition the PT paper gives
in §2.2. The difference that matters is size: a join can be larger than
both inputs, a semi-join can only shrink R.

The crate's smallest test (`experiments/src/join.rs`, test `tiny`) has
R(a,b) = {(1,10), (2,10), (3,20)}, S(b,c) = {(10,100), (10,101), (30,102)}
and T(c) = {100}:

```
R ⋈ S   = (1,10,100) (1,10,101) (2,10,100) (2,10,101)     4 rows: 2 R-rows × 2 S-rows on b = 10
R ⋉ S   = (1,10) (2,10)                                   2 rows: (3,20) has no partner
S ⋉ T   = (10,100)                                        1 row
```

The semi-join is computed the way the crate computes it — build a hash set
of S's values on the shared variable, probe it with each row of R:

```rust
// experiments/src/yannakakis.rs:28–35 — semijoin()
28  pub fn semijoin(r: &Relation, s: &Relation, cost: &mut SemiJoinCost) -> Relation {
29      let v = r.shared_var(s).expect("semi-join of relations that share no variable");
30      let keys: FastSet<u32> = s.col(v).iter().copied().collect();
31      cost.inserts += s.len();
32      cost.probes += r.len();
33      let col = r.col(v);
34      r.filter(|row| keys.contains(&col[row]))
35  }
```

The line to focus on is 34: the output is a subset of `r`'s own rows,
filtered in place. Lines 31–32 are the cost the bench reports: one insert
per row of S, one probe per row of R.

### Step 2 — intermediate results, and why order decides them

> **In:** the join from Step 1. **Out:** the quantity this whole topic
> measures — tuples emitted by each join — computed by hand for lane 1's two
> orders.

A **left-deep plan** joins relations one at a time: start from one relation,
join the next onto the running result, and so on. Each join's output is an
**intermediate result**, materialised and then probed by the next join. The
**output** OUT is the last one. Lane 1's query is the Cypher pattern
`(a)-[:KNOWS]->(b)-[:LIKES]->(m)-[:HAS_TAG]->(t:Tag {class:'rare'})` as the
chain knows(a,b) ⋈ likes(b,m) ⋈ has_tag(m,t) ⋈ rare_tag(t), generated with
20,000 persons × 10 KNOWS each, 5 LIKES each into 50,000 posts, 2 tags per
post out of 1,000 tags, and 10 tags rare (1%).

The bad order starts at knows (200,000 rows). Every person likes exactly 5
posts, so knows ⋈ likes is exactly 200,000 × 5 = **1,000,000** rows; every
post has exactly 2 tags, so the next join is exactly 1,000,000 × 2 =
**2,000,000**; only then does rare_tag cut it to OUT = 20,210. The bench
prints exactly these:

```
   left-deep plan                            join 1    join 2    join 3  intermediates    × OUT
   good  rare_tag→has_tag→likes→knows          1000      2036     20210          23246     1.15
   bad   knows→likes→has_tag→rare_tag       1000000   2000000     20210        3020210   149.44
```

The good order starts at the predicate. 10 rare tags × an expected 100 posts
each (100,000 has_tag rows ÷ 1,000 tags) = 1,000; × 2 likers per post
(100,000 likes ÷ 50,000 posts) = 2,000 expected, 2,036 measured; × 10 knowers
per person ≈ 20,360 expected, 20,210 measured. 3,020,210 ÷ 23,246 = **129.9×**.
A planner chooses between these with an estimate of how selective `rare` is
and how the joins fan out. Get it wrong and it pays 130×.

### Step 3 — acyclic queries and join trees

> **In:** a query as a set of relations sharing variables. **Out:** the
> condition under which Yannakakis' algorithm applies, and the tree it walks.

Draw a query as a **hypergraph**: one vertex per variable, one hyperedge per
relation covering the variables it binds. A **join tree** arranges the
relations as the nodes of a tree such that, for every variable, the nodes
that bind it form a connected subtree — the **running intersection
property**. A query is **acyclic** (formally **α-acyclic**, the PT paper's
term in §2.2) when some join tree exists.

Lane 1's chain is acyclic: the tree is the chain itself, knows — likes —
has_tag — rare_tag, and each variable (b, m, t) sits on exactly two adjacent
nodes. The triangle `(a)->(b)->(c)->(a)`, as R(a,b) ⋈ S(b,c) ⋈ T(c,a), is
the smallest **cyclic** query: any tree over three nodes leaves out one
edge, and the variable on that edge then sits on two non-adjacent nodes.

The tree needs a **root**. The crate picks the largest relation — knows —
following RPT's "LargestRoot" rule (RPT+ §2.2). Any root gives the guarantee;
the choice moves cost around, which is what RPT+ is about
([reading-rpt-plus.md](reading-rpt-plus.md)).

### Step 4 — the forward pass

> **In:** the base relations and Step 3's tree rooted at knows. **Out:** four
> relations, each reduced by everything below it in the tree — the
> after-forward sizes the bench prints.

The **forward** (bottom-up) pass visits the tree in **post-order** —
children before parents — and replaces each relation by its semi-join with
each child (PT §2.2: "At each vertex, we filter the table by a sequence of
semi-joins with its children"):

```rust
// experiments/src/yannakakis.rs:99–116 — full_reduce(), both passes
 99  pub fn full_reduce(rels: &[Relation], tree: &JoinTree) -> Reduction {
100      let mut cur: Vec<Relation> = rels.to_vec();
101      let mut forward = SemiJoinCost::default();
102      for &u in &tree.post_order {
103          for c in tree.children(u) {
104              cur[u] = semijoin(&cur[u], &cur[c], &mut forward);
105          }
106      }
107      let after_forward = cur.iter().map(|r| r.len()).collect();
        // ... 108: blank ...
109      let mut backward = SemiJoinCost::default();
110      for &u in tree.post_order.iter().rev() {
111          if let Some(p) = tree.parent[u] {
112              cur[u] = semijoin(&cur[u], &cur[p], &mut backward);
113          }
114      }
115      Reduction { rels: cur, after_forward, forward, backward }
116  }
```

Line 104 is the forward pass. On lane 1 it runs three semi-joins:
has_tag ⋉ rare_tag (100,000 → 1,000), likes ⋉ has_tag' (100,000 → 2,025),
knows ⋉ likes' (200,000 → 19,340). The predicate has travelled three hops to
the relation farthest from it.

The 19,340 is checkable by hand. A person survives if at least one of the 5
posts they like carries a rare tag. A post has 2 tags, each rare with
probability 0.01, so it is rare-tagged with probability 1 − 0.99² = 0.0199;
a person escapes with probability (1 − 0.0199)⁵ = 0.9045, so 9.55% of
persons survive and knows' should hold 200,000 × 0.0955 ≈ **19,100** rows.
Measured: 19,340.

### Step 5 — the backward pass

> **In:** Step 4's after-forward relations. **Out:** the **fully reduced**
> relations — every remaining row takes part in at least one output tuple.

After the forward pass the root is exact: every knows' row reaches a rare
tag. The leaves are not. A has_tag row survived because its tag is rare,
but nothing in the forward pass asked whether anyone *likes* that post. The
**backward** (top-down) pass, line 112, walks the tree the other way and
semi-joins each relation with its parent.

On lane 1 it changes one number: has_tag goes **1,000 → 868**. The 132 rows
removed are rare-tagged posts nobody likes, and that count is predictable
too: a post receives each of 100,000 likes with probability 1/50,000, so it
has no likes with probability (1 − 1/50,000)^100,000 ≈ e⁻² = 0.1353, and
1,000 × (1 − 0.1353) = **865** should survive. Measured: 868.

The two passes together are what the 2024–2026 papers credit to Yannakakis:
"all tuples that will not contribute to the output are removed" (PT §2.2).
The crate's test `reduced_relations_keep_exactly_the_participating_rows`
checks exactly that against a brute-force enumeration, row id by row id.

### Step 6 — after the reduction, every order is safe

> **In:** the fully reduced relations from Step 5. **Out:** the guarantee —
> every join along the tree emits at most OUT tuples — and the measurement
> that it held.

Take any left-deep order that follows tree edges (each new relation shares
a variable with what is already joined). Because every reduced row has a
partner in every neighbour, **every probe finds at least one match**, so the
running result never shrinks; and since it ends at OUT, it can never have
been above OUT. That is the argument the SQL Server paper writes out in §3.3
("the intermediate join results can only grow monotonically along the join
tree until reaching the final output"), and why the algorithm runs in
**O(N + OUT)** — input size plus output size — which RPT+ §2.1 calls
**instance-optimal**: no algorithm can avoid reading the input or writing
the output.

```
   left-deep plan                            join 1    join 2    join 3  intermediates    × OUT
   good, after full reduction                   868      2036     20210          23114     1.14
   bad,  after full reduction                 20084     20210     20210          60504     2.99
```

The bad order's first join is 20,084 ≤ 20,210, its second 20,210 = OUT. The
bad ÷ good ratio falls from 129.9× to **2.62×**, and 2.99 × OUT is the most
any three-join plan can pay once every join is bounded by OUT. The bench
asserts the bound on every run (`sanity: after reduction every join emits ≤
OUT`).

### Step 7 — the price: hash tables in the semi-join phase

> **In:** the cost counters from Step 1, summed over Steps 4–5. **Out:** what
> the reduction cost, and the reason engines did not adopt it.

```
   reduction (6 semi-joins): forward 3035 inserts + 400000 probes, backward 22233 + 3035; total 428303 hash ops = 1.07 × N
```

The forward pass probes every base row of the three large relations once
(100,000 + 100,000 + 200,000 = 400,000) and inserts the reduced children
(10 + 1,000 + 2,025 = 3,035). The backward pass is cheap here because the
forward pass already shrank everything. Total **428,303 hash operations, 1.07
× the 400,010 input rows** — linear, as promised.

Linear is not free. Put the honest comparison side by side: the good order
alone emits 23,246 intermediates; the bad order *with* the reduction does
428,303 hash operations plus 60,504 intermediates. The reduction is
insurance against a bad plan, and when the plan was already good the
premium exceeds the payout. The PT paper's diagnosis (§1) is exactly this:
"The main obstacles are the costly hash table accesses and high memory
consumption in the semi-join phase."

### Step 8 — predicate transfer: Bloom filters for semi-joins

> **In:** Step 7's cost problem. **Out:** PT's substitution, its cost model
> worked on lane-2 numbers, and the term the model leaves out.

A **Bloom filter** is a bit array plus k hash functions: inserting a key
sets k bits; a lookup answers "maybe present" if all k are set and "absent"
otherwise. It has **no false negatives** — a present key is never rejected —
but a **false positive rate** (FPR) ε > 0: an absent key passes with
probability ε. **Predicate transfer** (PT) replaces each semi-join R ⋉ S with
"build a Bloom filter over S's key values, drop the rows of R it rejects"
(§3.1), along a forward and a backward pass "similar to the Yannakakis
algorithm" (§3.2). Because filters are small and cache-resident, a probe
costs β ≪ 1 of a hash-table access (§3.5).

The price is false positives, and §3.5 models how they inflate the join
phase. A table of T_k rows whose exact reduction keeps T*_k passes
T*_k + (T_k − T*_k)·ε rows. Writing ε′ = (1/Sel − 1)·ε for the worst table's
selectivity Sel = T*/T, the join-phase inflation is at most (1 + ε′)^t ≈
1 + ε′t for t tables.

Worked on lane 1 at 8 bits per key, where the textbook rate is ε = 0.0215
(the lane-2 reference, [notes.md](notes.md)). The most selective table is
knows: Sel = 19,340 / 200,000 = 0.0967, so ε′ = (10.341 − 1) × 0.0215 =
**0.2008**, and (1.2008)⁴ = **2.08** for t = 4. Per table, knows should pass
19,340 + 180,660 × 0.0215 = 19,340 + **3,884** rows.

The reference implementation measured, for knows at 8 bits/key: 3,340 rows
admitted as a filter false positive — the model's 3,884 is the right size —
but **51,328** survivors in total, not 23,224. The other 28,648 are
**inherited**: rows that passed honestly, because the likes rows they match
were themselves false positives of the previous hop and so their keys were
inserted into the filter. The §3.5 model treats each table's error as
independent; a chain of filters compounds them. RPT+ §4.2 names this
("our multi-hop transfer can accumulate the filters' one-sided errors");
lane 2 is where you measure it.

### Step 9 — cyclic queries, where the exact algorithm does worse

> **In:** Step 3's definition of acyclic, Step 8's filters. **Out:** the one
> place PT pre-filters *more* than Yannakakis, and why.

TPC-H Q5 joins six tables in a cycle. Yannakakis needs a join tree, so the
paper's baseline "break[s] the cycle in the join graph by randomly picking a
root node and, performing a BFS search from the root" (§4.1) — dropping an
edge, and the filtering that edge would have done. PT does not need a tree:
its transfer graph keeps every edge, pointed from the smaller table to the
larger (§3.2). Result (§4.3, Tables 1–2): PT's join inputs are **64%** smaller
than Yannakakis' at SF1 and 67% at SF10, and §4.3 says Yannakakis' pre-filtering
power is "3.3× and 4.7× less". The exact algorithm loses to the approximate
one because it was run on a query outside its guarantee.

## How to read the paper (with the concepts in hand)

1. **§1–§2** — ten minutes. §2.2 is the cleanest short statement of
   Yannakakis in the recent literature; read it against Steps 4–6.
2. **§3.1–§3.2** — the design space. Figure 2's filter transformation is
   Step 8's "build from S's keys": a table receives filters on one column and
   emits one on another, in a single scan.
3. **§3.5** — the cost model. Redo Step 8's arithmetic before reading on.
4. **§4.2** — the averages. Note the scale factor of each number (below).
5. **§4.3** — Q5, Tables 1–2 and Figure 6. Step 9.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | `experiments/src/yannakakis.rs:28-35`, `join.rs` test `tiny` | the semi-join and its cost counters |
| 2 | lane 1, first two rows | 1,000,000 and 2,000,000 — exact products |
| 3–5 | `yannakakis.rs:99-116`; lane 1 "after fwd / after both" | 19,340 and the 1,000 → 868 backward step |
| 6 | lane 1 last two plan rows; sanity line | every join ≤ OUT |
| 7 | lane 1 "reduction" line | 428,303 hash ops = 1.07 × N |
| 8 | lane 2 (stub); `experiments/src/transfer.rs` | survivors split into filter FP and inherited |

## Questions (answer in notes.md)

1. The forward pass alone already makes the root (knows) exact. Which plans
   are already bounded by OUT after the forward pass only, and which need the
   backward pass? (Hint: lane 3, and the SQL Server paper's §3.3.)
2. Why does the backward pass touch so few rows on lane 1 (22,233 inserts,
   3,035 probes) when the forward pass probed 400,000?
3. PT's §3.2 heuristic points every edge from the smaller table to the
   larger. Draw lane 1's transfer graph under that rule. Is it the same as
   the forward pass rooted at knows?
4. What would the inherited term in Step 8 look like on a *star* query —
   every dimension joined directly to one fact table? Is there a hop to
   inherit from?

## Done when

Answer each before unfolding it.

- [ ] You can define a semi-join and say why it can only shrink its left input.

  <details><summary>Answer</summary>

  R ⋉ S keeps the rows of R that have at least one partner in S on the shared
  variables, and adds no columns from S: π_attr(R)(R ⋈ S), PT §2.2. Each row
  of R either appears once in the output or not at all — the crate's
  `semijoin` returns `r.filter(...)` (`yannakakis.rs:34`) — so the output is a
  subset of R. A join can multiply rows (R ⋈ S on the tiny example is 4 rows
  from 3 and 3); a semi-join cannot.

  </details>

- [ ] You can compute lane 1's bad-order intermediates by hand and explain why they are exact, not expected.

  <details><summary>Answer</summary>

  The generator gives every person exactly 5 LIKES and every post exactly 2
  tags (`gen.rs`, `likes_per_person`, `tags_per_post`). So knows ⋈ likes is
  exactly 200,000 × 5 = 1,000,000, and joining has_tag doubles it to exactly
  2,000,000, whatever the random targets were. Only the last join depends on
  the random draws: 20,210. Total 3,020,210, as printed.

  </details>

- [ ] You can state the running intersection property and say whether the triangle query has a join tree.

  <details><summary>Answer</summary>

  For every variable, the tree nodes (relations) that bind it must form a
  connected subtree. The triangle R(a,b), S(b,c), T(c,a) has no join tree:
  any tree on three nodes is a path, say R—S—T, and then variable a is bound
  by R and T, which are not adjacent. So it is cyclic, and Yannakakis'
  guarantee does not apply — which is why PT's Q5 baseline had to drop an
  edge (Step 9).

  </details>

- [ ] You can explain what the backward pass removed on lane 1 and predict the count.

  <details><summary>Answer</summary>

  132 has_tag rows (1,000 → 868): posts that carry a rare tag but that nobody
  likes. The forward pass only looks *down* the tree (toward rare_tag), so it
  cannot see that a post has no likers; the backward pass semi-joins has_tag
  with its parent likes'. A post gets each of 100,000 likes with probability
  1/50,000, so P(no likes) ≈ e⁻² = 0.1353 and 1,000 × 0.8647 ≈ 865 should
  survive; 868 did.

  </details>

- [ ] You can give the argument that every join is ≤ OUT after full reduction, and the measured evidence.

  <details><summary>Answer</summary>

  After both passes every row of every relation has a partner in each
  neighbour. In a plan that follows tree edges, each probe therefore finds at
  least one match, so the running result never shrinks from one join to the
  next; it ends at OUT, so no earlier join can exceed OUT. Lane 1's bad order
  after reduction emits 20,084, 20,210, 20,210 against OUT = 20,210, and the
  bench asserts the bound every run.

  </details>

- [ ] You can say what the reduction cost on lane 1, and why it is not a win when the order is already good.

  <details><summary>Answer</summary>

  428,303 hash operations (forward 3,035 inserts + 400,000 probes; backward
  22,233 + 3,035), 1.07 × the 400,010 input rows. The good order alone emits
  only 23,246 intermediates, so adding the reduction to a good plan adds work;
  it pays only against the bad order, which it cuts from 3,020,210
  intermediates to 428,303 + 60,504 = 488,807 units — 6.2× less. Insurance,
  not a free speed-up; PT §1 names the semi-join phase's hash-table cost as
  the reason engines skipped it.

  </details>

- [ ] You can work PT's §3.5 inflation bound on lane-1 numbers and name the term it leaves out.

  <details><summary>Answer</summary>

  With ε = 0.0215 (8 bits/key) and knows as the most selective table (Sel =
  19,340/200,000 = 0.0967): ε′ = (1/0.0967 − 1) × 0.0215 = 0.2008, and
  (1 + ε′)⁴ = 2.08. Per table the model predicts knows passes 19,340 +
  180,660 × 0.0215 = 23,224 rows; the lane-2 reference measured 3,340 own
  false positives (close to the model's 3,884) but 51,328 survivors in total.
  The missing 28,648 are inherited: rows whose keys really were inserted, but
  were inserted only because an upstream filter let a false positive through.
  The model assumes independent per-table errors; a chain compounds them.

  </details>

- [ ] You can explain why PT pre-filtered more than Yannakakis on TPC-H Q5.

  <details><summary>Answer</summary>

  Q5's join graph is cyclic, so Yannakakis needs a spanning tree, and the
  paper's baseline builds one by BFS from a random root (§4.1), dropping the
  cycle-closing edge and the filtering it carried. PT's transfer graph keeps
  every edge (§3.2). PT's join inputs were 64% (SF1) and 67% (SF10) smaller
  than Yannakakis' (§4.3, Tables 1–2). The exact algorithm lost because it
  was used outside its acyclic guarantee.

  </details>

## References

- Yifei Yang, Hangdong Zhao, Xiangyao Yu, Paraschos Koutris, **"Predicate
  Transfer: Efficient Pre-Filtering on Multi-Join Queries"**, CIDR 2024 —
  §2.2 (Yannakakis), §3.2 (transfer graph and schedule), §3.5 (cost model),
  §4.2 (averages: vs NoPredTrans 4.1×/4.0×, BloomJoin 3.3×/3.2×, Yannakakis
  4.2×/4.8× at SF1/SF10), §4.3 (Q5: Tables 1–2, Figure 6, join-order
  variance up to 12% vs up to 45×). The introduction's "3.3× (up to 61×) …
  4.8× (up to 47×)" pairs an SF1 average with an SF10 one, and the only 47×
  in §4.2 is Q2 against NoPredTrans, not Yannakakis; see notes.md.
- Mihalis Yannakakis, **"Algorithms for Acyclic Database Schemes"**, VLDB
  1981, pp. 82–94 — not obtained (see the note at the top).
- Junyi Zhao et al., **"Debunking the Myth of Join Ordering: Toward Robust
  SQL Analytics"**, SIGMOD 2025 — RPT, the version with LargestRoot; cited by
  [reading-rpt-plus.md](reading-rpt-plus.md).
- Next: [reading-rpt-plus.md](reading-rpt-plus.md) for what it costs to run
  this in a real engine, and [reading-sqlserver-bitmaps.md](reading-sqlserver-bitmaps.md)
  for the engine that was already doing half of it.
- Topic 44's [reading-free-join.md](../44-egraphs-egglog/reading-free-join.md)
  Step 1, for where acyclic queries sit beside worst-case optimal joins.
