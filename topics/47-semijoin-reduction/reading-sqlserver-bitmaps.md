# SQL Server's bitmap filters: Yannakakis by accident

The two previous chapters treat semi-join reduction as a *phase*: reduce
everything, then join. This paper reports that a production engine has been
getting the same guarantee for over a decade with no separate phase at all —
a hash join that builds a bitmap alongside its hash table and pushes it into
its probe side, chained through a pull-based executor, and costed by a
Cascades optimizer that knows the bitmap is coming.

**"I Can't Believe It's Not Yannakakis: Pragmatic Bitmap Filters in
Microsoft SQL Server"** — Hangdong Zhao, Yuanyuan Tian, Rana Alotaibi, Bailu
Ding, Nicolas Bruno, Jesús Camacho-Rodríguez, Vassilis Papadimos, Ernesto
Cervantes Juárez, Cesar Galindo-Legaria, Carlo Curino. CIDR 2026
([pdf](https://www.cidrdb.org/cidr2026/papers/p29-zhao.pdf)). Hangdong Zhao
is also an author of the predicate-transfer paper; this is the same group
looking at what industry already had.

Lane 3 of `semijoin_bench` is this paper's mechanism on lane 1's query, and
its reference numbers (from an implementation written, run, and then removed
so the lane is yours to write) are in [notes.md](notes.md).

## The problem in one sentence

A semi-join phase costs an extra pass over the inputs (lane 1: 428,303 hash
operations before any join runs); can the ordinary hash joins of a plan do
the same pre-filtering as a side effect, and does that still guarantee that
no intermediate exceeds the output?

## The concepts, step by step

### Step 1 — the bitmap-building hash join

> **In:** a hash join: build a hash table on one input, probe it with the
> other. **Out:** SQL Server's version, which also builds a filter and sends
> it down the probe side before the probe starts.

A **bitmap filter** here means any compact membership filter over the build
side's join-key values: SQL Server uses "both Bloom filters and exact
bit-vector filters (i.e., equivalent to semijoins), which we will
collectively term bitmap filters" (§3). An **exact bit-vector** is one bit
per possible key value in [min, max] — zero false positives, affordable only
when that range is compact.

The **batch-mode hash join** (HJ, §3.1) is "the sole execution primitive" for
them. Its `next()` does two things in order (Figure 2): open the build side,
fill the hash table and — if the optimizer judged the join selective —
a bitmap; then open the probe side with the bitmap pushed into it, and probe.
Which kind of bitmap and how large is decided at run time from the build's
statistics: "if the ranges of l_orderkey values are compact, SQL Server may
opt for a bit-vector for zero false positives" (§3.1). The bitmap also
carries the build side's min and max, and a scan uses them to skip whole
**rowgroups** (blocks of column data with their own min/max statistics).

The paper's single-join example, TPC-H Q12 at scale factor 100: a Bloom
filter "rejects 80% out of the 15M orders rows, handing only 2.96M rows to
the hash join — approaching a perfect semijoin pre-filtering (which would
otherwise remain 2.92M orders rows under zero false positives)" (§3.1).
Worked: 2.96 / 15 = 19.7% passed, so 80.3% rejected; the 0.04 M extra rows
are false positives, 0.04 / (15 − 2.92) = **0.33%** of the non-matching rows.

### Step 2 — pull-based execution turns bitmaps into a cascade

> **In:** Step 1's operator, several of them stacked. **Out:** the order in
> which builds happen, and the realisation that it is Yannakakis' forward
> pass.

In a **pull-based** (iterator, Volcano-style) executor each operator asks its
inputs for rows by calling `next()`. The root HJ therefore builds *its*
hash table first, then pulls from its probe side — which is another HJ, which
then builds its own hash table, from a scan that already sees the root's
bitmap. Figure 3 traces TPC-H Q3 (customer C, orders O, lineitem L):

```
root HJ:  build on C, bitmap(c_custkey) → pushed into O ⋈ L
inner HJ: build on O — the scan of O probes bitmap(c_custkey) — so it builds on O⊥ = O ⋉ C,
          and its bitmap(o_orderkey) is built from O⊥
probe:    scan L, probe bitmap(o_orderkey) → L⊥ = L ⋉ O⊥, then probe O's table, then C's
```

"The first two stages of execution are just the bottom-up pass of
Yannakakis" (§3.2). Each bitmap is built from a build side that the bitmaps
above it have already filtered, so the filtering travels the whole way down.
There is no backward pass: the last stage probes the built hash tables
directly — "a single scan over input … in contrast to the three back and
forth scans … of Yannakakis and PT" (§3.2).

### Step 3 — why one pass is enough: every probe hits

> **In:** Step 2's cascade, assuming exact bitmaps. **Out:** the
> instance-optimality proof, and the condition it needs.

After the cascade, every row of every build side has a partner in the build
side of the join above it (it passed that join's bitmap), and every probe row
has a partner in the first build it meets. So when the probe pipeline runs
upward, **every probe is a hit**: "every row in L⊥ has some matches in O⊥ …
every probe into C must also be a hit. Hence, all intermediate probes (and
outputs) are bounded by O(OUT)" (§3.3). Builds and bitmaps cost O(N); probes
cost O(OUT); total O(N + OUT), the same bound as Yannakakis.

The paper turns this into Algorithm 1: given a **post-order** of a rooted
join tree (leaves to root), make each leaf the build side of a hash join
whose probe side is everything else, recursively. The root ends up as the
probe relation at the bottom of the plan. The condition is in the plan's
shape: the bitmaps flow from build sides toward the probe, so the relation
that ends up fully reduced is the one at the root of the cascade.

### Step 4 — lane 3: the same cascade, in both orders and two pushdown modes

> **In:** Steps 2–3 and lane 1's query. **Out:** four measured rows that say
> when the cascade works and when it cannot.

Lane 3 runs lane 1's two left-deep orders with exact bitmaps (hash sets).
In lane 1's left-deep plans `order[0]` is the probe relation and every later
relation is a build side, built top-down as in Step 2. The two modes differ in
*where* a bitmap may go. **Into lower builds** lets the bitmap from join j
filter any relation below it that binds its variable — Algorithm 1's
cascade. **Probe spine only** lets it filter only `order[0]` — the shape
DuckDB's pushdown has at the pinned commit
([reading-duckdb-join-filters.md](reading-duckdb-join-filters.md), Step 3).
Reference numbers:

```
   plan                                     pushdown              scan 0   build 1   build 2   build 3  build rows bitmap probes  intermediates
   good  rare_tag→has_tag→likes→knows       probe spine only          10    100000    100000    200000      400000            10          23246
   good  rare_tag→has_tag→likes→knows       into lower builds         10     86444     99995    200000      386439        200010          23114
   bad  knows→likes→has_tag→rare_tag        probe spine only      200000    100000    100000        10      200010        200000        3020210
   bad  knows→likes→has_tag→rare_tag        into lower builds      19340      2025      1000        10        3035        400000          60504
```

Read the last row first. The bad order with the cascade builds rare_tag (10),
then has_tag filtered by bitmap(t) (1,000), then likes filtered by
bitmap(m) (2,025), then scans knows filtered by bitmap(b) (19,340) — exactly
lane 1's after-forward sizes — and emits **60,504** intermediates, the same as
the bad order after the *full* two-pass reduction. One pass sufficed, as §3.3
says it must.

The third row is the same plan with bitmaps confined to the probe scan. The
rare_tag bitmap is on t, which only has_tag binds, and has_tag is a build
side: it cannot be reached. The only bitmap that reaches knows is
bitmap(b) built from *unfiltered* likes, which passes all 200,000 rows.
Result: **3,020,210** — no help at all.

The first two rows are the surprise. In the "good" order the cascade runs the
wrong way: the top build is knows, built first, from nothing, and its bitmap
(on b) keeps 99,995 of likes' 100,000 rows. 386,439 rows go into hash tables
against the bad order's **3,035**. Under bitmap pushdown the textbook bad
order — biggest table as the probe, predicate at the top — is the cheap plan.

### Step 5 — bitmaps in the optimizer's cost model

> **In:** Step 4's lesson that bitmaps change which order is good. **Out:**
> how SQL Server's Cascades optimizer costs a plan with the bitmaps it will
> receive, and the two heuristics that keep that affordable.

**Cascades** is a top-down, memoised optimizer: it optimizes each logical
subexpression once per required context and caches the winner (topic 10's
[reading-selinger-cascades.md](../10-query-planning/reading-selinger-cascades.md)).
SQL Server adds a **context** `ctx` carrying the bitmaps that ancestor joins
will push down (§4.1, Figure 6). When costing a hash join it optimizes the
build side first, and if the join looks selective creates `ctx'` with that
bitmap's estimated selectivity, which the probe side is then optimized
under. On Q3 this is visible in the estimates: with the cascaded contexts
the optimizer estimates **3.3 M** rows for L instead of the **323 M** that
local filters alone leave (§4.1, Figure 4).

Optimizing "in a context" defeats memo reuse, so two heuristics (§4.2):
**selectivity discretization** — snap each context's selectivity to a fixed
ladder "such as 75%, 10%, 1%, 0.1%", so a 3% estimate is costed as 1% and
shares memo entries with other 1% contexts — and **early stopping** —
cost bitmap-aware plans first and tighten the cost cap to the first good one
found.

### Step 6 — what it measured

> **In:** the whole design. **Out:** Table 1's numbers, and the gaps.

TPC-H SF100, SQL Server 2025, 8-core Azure VM, median of 10 runs (§5):

- Bitmaps on vs off: **7 of 22 queries above 2×** — Q2 3.47, Q5 2.33, Q7 3.00,
  Q8 2.86, Q17 2.55, Q20 2.01, Q21 2.36 — and the top is the "up to 3.5×" of
  §1.
- **12 acyclic queries** run on instance-optimal plans (§5 prose; Table 1's
  check marks did not survive PDF text extraction, so the count is taken
  from the prose).
- Optimizer overhead: **5 queries** marked ▲ (Q5, Q7, Q8, Q18, Q19); the rest
  within ±5%.
- Pre-filtering versus a hand-picked Yannakakis/PT schedule (last two
  columns): equal on many queries, but Q3 keeps **5.23%** of its rows where
  Yannakakis keeps 1.25%, and Q8 **1.92%** against 0.16% — gaps the paper
  attributes to a single pass that does not "aggressively circulate bitmaps
  across the entire join graph", Bloom false positives, and bitmaps skipped as
  not worth building.

And the honest part (§6): telemetry shows **7%** of join queries go beyond
8-way joins (up to 1,000 tables) and **15%** mix join types; on four
adversarial production queries (Figure 7) neither SQL Server's choice nor a
Yannakakis schedule is right — sometimes conservative costing misses
pre-filtering, sometimes aggressive pre-filtering is pure overhead.

Two discrepancies in the paper, for the record: §1 says bitmaps have been
"deployed since 2012", the acknowledgments say they "shipped in SQL Server
2014"; and §3.1's rowgroup size reads "~900 rows" in the extracted text,
which is far from SQL Server's documented columnstore rowgroup size and is
probably a lost "K" — this chapter does not use it.

## How to read the paper (with the concepts in hand)

1. **§1–§2** — fifteen minutes; §2.1 restates Yannakakis on Q3 in eight lines.
2. **§3.1–§3.3 with Figures 2–4** — the core. Trace Figure 3 with Step 2's
   table; read the Q3 proof before Algorithm 1's.
3. **§3.4** — bushy plans and cyclic Q5. Read for the idea that bitmaps can
   be built from *intermediate* results; the semi-robustness bound
   O(N + OUT₁ + OUT₃ + OUT) is the takeaway.
4. **§4** — Figure 6 beside topic 10's Cascades chapter.
5. **§5 Table 1 and §6** — the numbers, then the cases where it fails.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 2–3 | lane 3, `experiments/src/bitmap.rs` (stub) | builds top-down, probe last |
| 4 | lane 3 reference rows (notes.md) | 60,504 with one pass; 3,020,210 probe-spine-only |
| 4 | lane 1 "after fwd" column | the cascade leaves exactly these sizes |
| 5 | topic 10's [reading-selinger-cascades.md](../10-query-planning/reading-selinger-cascades.md) | the memo the context breaks |

## Questions (answer in notes.md)

1. Lane 3's bad order with the cascade matches the *full* reduction's 60,504
   even though has_tag was left at 1,000 rows instead of 868. Why does the
   extra 132 not show up in any join?
2. The paper's Algorithm 1 requires a single base table on each build side.
   Lane 1's GOOD order satisfies that too, yet gets no benefit. What property
   of the order, not the shape, is missing?
3. Q12's Bloom filter passed 0.33% of non-matching rows. With DuckDB's
   register-blocked layout at ~13 bits per key (lane 1b), what would you
   predict?
4. How would `ctx` discretization have costed lane 1's bad order with
   bitmaps? Snap each bitmap's selectivity (rare_tag → has_tag 1%,
   has_tag' → likes 2%, likes' → knows 9.7%) to the ladder and multiply.

## Done when

Answer each before unfolding it.

- [ ] You can describe what one batch-mode hash join does that an ordinary hash join does not.

  <details><summary>Answer</summary>

  After building its hash table it also builds a bitmap filter over the build
  keys (Bloom or exact bit-vector, chosen at run time from the build's
  statistics; exact when the key range is compact) plus min/max, and pushes
  it into its probe subtree before opening it, so scans drop non-matching
  rows and skip rowgroups whose min/max misses the range (§3.1). Q12 SF100:
  15 M orders → 2.96 M passed, against 2.92 M for a perfect semi-join.

  </details>

- [ ] You can explain why pull-based execution makes stacked bitmaps a Yannakakis forward pass.

  <details><summary>Answer</summary>

  Each HJ builds before it pulls from its probe side, so builds happen from
  the root of the plan downward, and each lower build side is scanned after
  the bitmaps of every join above it exist. A lower build is therefore already
  filtered by the upper bitmaps when it builds its own bitmap, which carries
  the filtering one hop further — the bottom-up semi-join pass of a join tree
  rooted at the probe relation (§3.2, Figure 3).

  </details>

- [ ] You can give the instance-optimality argument and the condition it rests on.

  <details><summary>Answer</summary>

  With exact bitmaps, after the cascade every probe row matches at least one
  row in each hash table it probes, because each build side was filtered by
  the bitmap of the join above it and the probe relation by all of them. So
  intermediates grow monotonically up the plan and are bounded by OUT; builds
  are O(N). Total O(N + OUT) (§3.3). The condition: the plan's builds must
  follow a post-order of the join tree so the bitmaps flow toward the probe
  relation (Algorithm 1) — and bitmaps must be allowed into lower build sides.

  </details>

- [ ] You can read lane 3's four reference rows and say what each shows.

  <details><summary>Answer</summary>

  Bad order, into lower builds: 3,035 build rows, 60,504 intermediates —
  one pass equals lane 1's full two-pass reduction. Bad order, probe spine
  only: the rare_tag bitmap cannot reach has_tag (a build side), so nothing is
  filtered and it stays at 3,020,210. Good order, either mode: the predicate
  is already at the probe, the bitmaps flow from the big tables toward it and
  filter almost nothing (likes 99,995 of 100,000), and 386,439–400,000 rows
  are built into hash tables. With bitmaps, the "bad" order is the cheap one.

  </details>

- [ ] You can explain how SQL Server costs bitmaps and why it discretizes selectivities.

  <details><summary>Answer</summary>

  The Cascades optimizer passes a context of pending bitmaps down the plan;
  a hash join optimizes its build side, and if selective, optimizes its probe
  side under a context that includes the new bitmap's selectivity, so probe
  subplans are costed on reduced inputs (Q3: L estimated at 3.3 M instead of
  323 M). Each distinct context defeats memo reuse, so selectivities are
  snapped to a ladder (75%, 10%, 1%, 0.1%, …; 3% → 1%), and early stopping
  tightens the cost cap after the first good bitmap-aware plan (§4).

  </details>

- [ ] You can state Table 1's headline numbers and the gaps to Yannakakis.

  <details><summary>Answer</summary>

  TPC-H SF100: 7 of 22 queries above 2× with bitmaps (max Q2 3.47×), 12
  acyclic queries on instance-optimal plans, 5 queries with perceptible
  optimizer slowdown. Pre-filtering mostly matches a hand-picked Yannakakis
  schedule, but Q3 keeps 5.23% vs 1.25% and Q8 1.92% vs 0.16%, because one
  pass does not circulate bitmaps everywhere, Bloom false positives
  accumulate, and some bitmaps are judged not worth building (§5).

  </details>

## References

- Hangdong Zhao et al., **"I Can't Believe It's Not Yannakakis: Pragmatic
  Bitmap Filters in Microsoft SQL Server"**, CIDR 2026 — §3.1 (HJ, Figure 2,
  Q12), §3.2–3.3 (cascade, proofs, Algorithm 1), §3.4 (bushy plans, Q5), §4
  (Figure 6, discretization, early stopping), §5 (Table 1), §6 (Figure 7,
  telemetry).
- Bailu Ding, Surajit Chaudhuri, Vivek Narasayya, **"Bitvector-aware Query
  Optimization for Decision Support Queries"**, SIGMOD 2020 — the earlier
  paper on costing bitvectors in snowflake schemas.
- Previous: [reading-rpt-plus.md](reading-rpt-plus.md). Next:
  [reading-in-predicates.md](reading-in-predicates.md) — when `IN` looks like
  a semi-join and is not.
- The code side: [reading-duckdb-join-filters.md](reading-duckdb-join-filters.md)
  and [reading-datafusion-dynamic-filters.md](reading-datafusion-dynamic-filters.md).
