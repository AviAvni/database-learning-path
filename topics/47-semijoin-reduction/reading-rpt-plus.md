# RPT+: predicate transfer that knows when to stop

[reading-yannakakis-predicate-transfer.md](reading-yannakakis-predicate-transfer.md)
ended with a bill: the reduction is linear, but linear in the *whole input*,
and on a query whose plan was already good it costs more than it saves. This
paper is about paying that bill in a production engine.

**"Robust Predicate Transfer with Dynamic Execution"** — Yiming Qiao, Peter
Boncz, Huanchen Zhang. PVLDB 19(6), VLDB 2026, pp. 1278–1290
([pdf](https://www.vldb.org/pvldb/vol19/p1278-qiao.pdf)). Its starting point
is **RPT** (Robust Predicate Transfer, Zhao et al., SIGMOD'25), which runs
Yannakakis' two passes with Bloom filters and showed that join-order
robustness is reachable in a real system. RPT+ keeps the guarantee and
attacks the constant factor with three changes — a cheaper transfer
schedule, a filter that can skip whole blocks, and run-time checks that
abandon filters that do not filter — implemented in DuckDB v1.3.0. The
paper's own summary of its goal is the right frame: "the goal of RPT+ is to
avoid the regressions seen in prior approaches; performance gains are a
secondary benefit" (§6.3).

Section and figure numbers are the VLDB'26 PDF's. Lane numbers refer to
`semijoin_bench`; lane-2 figures are the reference implementation recorded in
[notes.md](notes.md).

## The problem in one sentence

RPT makes bad join orders harmless, but on SQLStorm's 13,308 queries it made
at least 28% of them more than 10% *slower* than plain DuckDB (§6.1), because
it builds and probes filters whether or not they filter anything.

## The concepts, step by step

### Step 1 — RPT: Yannakakis with Bloom filters on a maximum spanning tree

> **In:** the forward/backward passes and Bloom filters from the previous
> chapter. **Out:** RPT's plan — which tree, which root, which filters.

RPT models the query as a **join graph**: one vertex per table, one edge per
pair of tables joined, weighted by how many join attributes they share. It
builds a **maximum spanning tree** — a tree over all vertices whose total
edge weight is as large as possible — "rooted at the table with the highest
cardinality by repeatedly selecting the highest-weight edge that connects a
node in the tree to a node outside (break ties by choosing the larger
table)" (§2.2). That is the **LargestRoot** algorithm. The tree then drives a
leaves-to-root forward pass and a root-to-leaves backward pass of Bloom
filters, exactly as in Yannakakis, and after both "all the irrelevant tuples
from the base tables are eliminated" (§1) — up to Bloom false positives.

This topic's crate uses the same root rule (`JoinTree::largest_root`,
`experiments/src/yannakakis.rs`), which on lane 1 picks knows (200,000 rows).

### Step 2 — equivalence classes, and the two ways a symmetric plan wastes work

> **In:** Step 1's single tree. **Out:** the two inefficiencies RPT+ exists to
> remove, on the paper's JOB 3a example.

An **equivalence class** is a set of columns from different tables that are
all equal because of the join conditions (§2.2) — e.g. `movie_keyword.movie_id
= title.id = movie_info.movie_id` is one class. RPT ignores them, and §2.2
shows two costs on JOB query 3a (Figure 2):

- a **redundant filter**: in Plan 1, the filter built from movie_keyword in
  step 6 carries the same equivalence class as step 4's, so step 4's filter
  could have been applied to `title` directly;
- an **oversized filter**: in Plan 2, step 3 builds a Bloom filter from
  movie_keyword (4.5 M rows) *before* that table has been reduced by the
  filter from title, so the filter holds keys that a moment later would have
  been dropped.

Plan 1's forward pass and Plan 2's backward pass would avoid both — but RPT
uses one tree for both passes, a **symmetric** transfer plan.

### Step 3 — forward collects, backward distributes: the asymmetric plan

> **In:** Step 2's two wastes. **Out:** ATP — a deep chain for the forward
> pass, a wide star for the backward pass — and the cost argument for each.

§3.2's observation: the forward pass *collects* filtering information into
the root, the backward pass *distributes* it. Collecting is cheapest along a
**chain**, where each table is filtered before it builds the next filter, so
no filter is oversized; distributing is cheapest as a **broadcast**, where
the root's final filter is built once and probed by every other table. The
**Asymmetric Transfer Plan** (ATP, Algorithm 1) inserts tables in the same
order as LargestRoot but breaks ties differently per pass: forward attaches a
new table to the candidate parent of *greatest* depth, backward to the one of
*smallest* depth.

The forward argument (§3.4.1) prices a table T_i with N_i rows, K_i distinct
keys and child set C_i as

L(T_i) = |K(S_i)| / |K_i| · N_i · C_build + |C_i| · N_i · C_probe

— build cost proportional to the rows that survive the subtree's key
intersection K(S_i), probe cost one probe per row per child filter. Merging a
second child chain B onto the leaf T_a of chain A (Figure 4) costs T_a one
more probe pass and saves its parent T_p one: the change in probe cost is
(N_a − N_p) · C_probe. With N_p = 1,000,000 (the larger table, chosen first
because Algorithm 1 inserts by size), N_a = 100,000 and C_probe = 1, that is
100,000 − 1,000,000 = **−900,000** probe units, while every build term in the
merged chain can only shrink (more filters above it, smaller key
intersection). Repeat until the tree is a chain. The backward argument
(§3.5.1) is shorter: a star needs one filter build for n − 1 probes; a tree
of depth d needs d − 2 more builds for the same probes.

Correctness survives because both trees share the root (§3.3): the forward
pass collects the surviving keys there, the backward pass distributes them
from there.

### Step 4 — restricting the forward pass to tables that filter

> **In:** ATP from Step 3. **Out:** the first deliberate departure from the
> full-reduction guarantee, and why.

§3.6: "A PK-derived BF cannot reduce the FK side due to referential
integrity" — a **primary key** (PK) is a column whose values are unique, a
**foreign key** (FK) references it, and **referential integrity** means every
FK value exists as a PK. So a filter built from an unfiltered PK table passes
every FK row, and "PK–FK joins without selective predicates gain negligible
benefit from predicate transfer". RPT+ therefore restricts the forward pass
to tables with predicates plus the **bridge tables** that connect equivalence
classes (§5.1), which "trades the theoretical guarantee of full semi-join
reduction for significantly reduced overhead".

Lane 1 has the same structure in miniature. likes and has_tag are not PK–FK,
but filters built from their unfiltered sides are useless in the same way:
in lane 3's good-order cascade, the bitmap built from all 200,000 knows rows
keeps 99,995 of likes' 100,000 rows.

### Step 5 — cascade filters: min–max first, Bloom second

> **In:** a Bloom filter, and storage organised in blocks. **Out:** the
> cascade filter, and why its Bloom half must be accurate across hops.

A **row group** (or block) is a run of rows stored together, and a
**zone map** or **min–max filter** keeps each group's minimum and maximum
value of a column, so a group whose range misses the wanted values can be
skipped unread. A Bloom filter works per tuple and cannot skip anything
(§1). A **cascade filter** applies a min–max range test first, then the Bloom
filter on what is left (§4.1); creating one is a **pipeline breaker** — the
producing pipeline must finish before any consumer can probe — and "the
inter-pipeline materialization cost significantly outweighs the overhead of
filter construction".

The trap is in §4.2. The min–max range sent onward is computed from the
rows that *passed* the previous filter, false positives included. Model a
domain {1, …, n} in which values inside the true range always pass and values
outside pass with false-positive probability p. For n ≫ 1/p the expected
range of the survivors is ≈ **n − 2/p**: one stray survivor near each end is
enough to stretch the range. Worked: n = 1,000,000 and p = 0.01 give
1,000,000 − 200 = **999,800** — the range covers 99.98% of the domain even if
the true survivor set is empty, and min–max skipping is dead. The paper's
measurement of this (§6.6.2, Figure 17): on JOB 07c, Bloom filter 6 holds
**14** items at 24 bits per key and **378** at 16, and the accurate setting
runs about **4×** faster because cast_info's row groups can be skipped.

Lane 2 sees the same compounding without any min–max: at 8 bits per key the
knows relation keeps 3,340 rows as its own filter's false positives and
28,648 more inherited from upstream false positives.

### Step 6 — the filter itself: cache-sectorized Bloom filters

> **In:** Step 5's accuracy requirement. **Out:** the filter RPT+ chose, its
> parameters, and how lane 1b's two layouts compare.

A **blocked** Bloom filter confines each key's bits to one cache line (or one
machine word, **register-blocked**), so a probe is one memory access instead
of k. The cost is accuracy: keys that land in the same block crowd it. RPT+
uses the **cache-sectorized** Bloom filter (CSBF) of Lang et al. (VLDB'19),
which aligns blocks to 64-byte cache lines and spreads bits across 32-bit
sectors, because it "achiev[es] throughput comparable to register-blocked BFs
… while maintaining an FPR close to classic BFs" (§4.2). Parameters (§5.4):
**20 bits per key, 7 hash functions**, probe latency **2.48 cycles/tuple**,
FPR **6.1 × 10⁻⁵**.

Lane 1b measures the two ends of that trade on this machine, at equal bit
budgets and one million absent keys:

```
      keys filter                                 bits  bits/key   k  measured FPR    formula
    100000 classic Bloom                       2097152     20.97  15       0.00006    0.00004
    100000 register-blocked (DuckDB layout)    2097152     20.97   4       0.00249          —
```

The classic filter at 21 bits per key lands on the paper's CSBF figure
(6 × 10⁻⁵); the register-blocked layout DuckDB v2.0.0 (`d54cbbe3df`) uses is
**40×** worse at the same memory. That is the accuracy RPT+ is protecting.

### Step 7 — dynamic pipelines: measure the filter before paying for it

> **In:** Steps 4–6. **Out:** Algorithm 2's abandon rule and the probe-skip
> rule, each worked on numbers.

Estimates of selectivity are what made the plan bad in the first place, so
RPT+ measures instead (§5). A **selectivity checker** sits before each
cascade-filter creator and counts N_scan (rows scanned) and N_recv (rows that
passed every upstream filter). Once γ = **100K** rows have arrived (§5.4) it
estimates λ = N_recv / N_scan and the materialised size S = λ · N_total ·
avg_tuple_size, and abandons the filter — merging the two pipelines back
into one — if λ > τ_sel = **0.35** while N_scan / N_total < τ_prog = **0.6**,
or if S > M_avail = **64 GB** (Algorithm 2, lines 9–12).

Worked, on a 10 M-row table with 32-byte tuples:

- 100K rows passed after 400K scanned: λ = 0.25 ≤ 0.35 — **build the
  filter**; S = 0.25 × 10 M × 32 B = 80 MB, far under 64 GB.
- 100K rows passed after 200K scanned: λ = 0.5 > 0.35, and 200K / 10 M = 0.02
  < 0.6 — **abandon**, early enough that little was materialised.
- the same λ = 0.5 discovered only at 7 M scanned (0.7 ≥ 0.6) — keep going;
  most of the materialisation is already paid.

The probe side gets the same treatment (§5.3, Figure 9): each Bloom probe
counts rows in and out, and once sampled, the i-th filter is skipped if
λ_i / λ_(i−1) exceeds τ_stop. Figure 9's example: λ₀ = 80% after min–max,
λ₁ = 20% after BF 1 (ratio 0.25 — keep), λ₂ = 20% after BF 2 (ratio 1.00 —
skip). The figure and §5.3's prose use τ_stop = 0.95; §5.4 and §7.2 set
**0.9**. A small inconsistency in the paper, not a contradiction.

### Step 8 — what it measured, including the regressions

> **In:** the whole design. **Out:** the paper's numbers, in the order that
> matters for a planner author: regressions first.

**Regressions (§6.1, Tables 1–2).** SQLStorm: of 18,251 queries, 13,308
finish, 2,837 fail and 2,106 time out at 10 s. Under RPT, 555 (4.2%) run at
0–0.7× and 3,185 (23.9%) at 0.7–0.9× of DuckDB — the prose's "at least 28%"
is 28.1%. Under RPT+, 24 (0.2%) and 247 (1.9%) — the prose's 2.1%. The worst
RPT+ query is SQLStorm 13615 at **0.37×** (§6.6.3), the dynamic pipeline
having judged its predicate selective when it was not.

**Averages (§6.2, Figure 11)**, geometric mean speedup over DuckDB v1.3.0:
**1.47× on JOB, 1.28× on SQLStorm, 1.17× on TPC-H, 1.01× on Appian.** The
1.01× is the negative result worth keeping: Appian "is dominated by
non-selective predicates" (§6.2), there is nothing to transfer, and the best
the dynamic pipeline can do is cost nothing.

**Upside.** "reaching up to 500×" (§6.1) is prose about Figure 10; the table
it can be checked against, Table 2, gives the >100× bucket as 12 queries at
an average 226.29×.

The baseline matters for reading these. The paper's DuckDB v1.3.0 (DDB in
Figure 11) "supports min-max filter transfer … but does not support Bloom
filters" (§6). DuckDB v2.0.0 (`d54cbbe3df`)
*does* build Bloom filters and range bitmaps (exact when the key span fits) for join keys — see
[reading-duckdb-join-filters.md](reading-duckdb-join-filters.md) — so the
1.17×–1.47× are against an engine without them.

## How to read the paper (with the concepts in hand)

1. **§1 and Figure 1** — the three overheads RPT+ attacks.
2. **§2.2 and Figure 2** — JOB 3a, Step 2's two wastes.
3. **§3.2–§3.5** — ATP. Read Algorithm 1 against Step 3; skim the
   multi-class proofs on first pass.
4. **§3.6** — the PK–FK argument; it is the honest limit of the technique.
5. **§4** — cascade filters; work §4.2's n − 2/p before the case study.
6. **§5** — Algorithm 2 and Figure 9 with Step 7's numbers.
7. **§6.1 first, then §6.2** — regressions, then averages. Tables 1–2 are the
   checkable version of Figure 10.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 1 | `experiments/src/yannakakis.rs` (`JoinTree::largest_root`) | RPT's root rule |
| 4 | lane 3, good order | unfiltered builds: knows' bitmap keeps 99,995 of likes' 100,000 rows |
| 5 | lane 2 reference (notes.md) | filter FP vs inherited at 2–16 bits/key |
| 6 | lane 1b | classic vs register-blocked at equal bits |
| 7 | [reading-duckdb-join-filters.md](reading-duckdb-join-filters.md) Step 6 | DuckDB's own run-time pause rule |

## Questions (answer in notes.md)

1. ATP's forward chain filters each table before it builds the next filter.
   On lane 1 the chain is forced. Construct a three-table single-class query
   where LargestRoot's tie-break produces an oversized filter and ATP's does
   not.
2. Step 4's restriction gives up full reduction. On lane 1, which relations
   have a predicate or are bridge tables? Would restricting the forward pass
   change any lane-1 count?
3. §4.2's n − 2/p assumes values outside the range survive independently.
   Lane 2's survivors are keys that share a row with a false positive. Is the
   independence assumption better or worse than reality there?
4. Algorithm 2 measures selectivity on a *sequential* prefix of the scan
   (§5.4). Give a data layout for which the first 100K rows have λ = 0.9 and
   the table as a whole has λ = 0.1.

## Done when

Answer each before unfolding it.

- [ ] You can describe LargestRoot and say what RPT+ changes about it.

  <details><summary>Answer</summary>

  LargestRoot builds a maximum spanning tree over the join graph (edges
  weighted by shared join attributes), rooted at the largest table, adding the
  highest-weight connecting edge each time and breaking ties toward larger
  tables (§2.2); both passes use that one tree. ATP (Algorithm 1) keeps the
  same insertion order and root but builds two trees: forward attaches a new
  table to the candidate parent of greatest depth (a chain), backward to the
  one of smallest depth (a broadcast star). Sharing the root keeps full
  reduction (§3.3).

  </details>

- [ ] You can explain the two wastes of a symmetric plan on JOB 3a.

  <details><summary>Answer</summary>

  Redundant filters: two filters over the same equivalence class built in
  different steps when one would do (Plan 1, steps 4 and 6). Oversized
  filters: a filter built from a table before that table has been reduced by
  a filter it was about to receive (Plan 2, step 3 builds from the 4.5 M-row
  movie_keyword before title's filter has shrunk it). A chain forward pass
  avoids the second, a broadcast backward pass the first (§2.2, §3.2).

  </details>

- [ ] You can work §4.2's min–max inflation formula and connect it to JOB 07c.

  <details><summary>Answer</summary>

  With in-range values always kept and out-of-range ones surviving with
  probability p, the expected survivor range is ≈ n − 2/p for n ≫ 1/p. For
  n = 10⁶ and p = 0.01 that is 999,800 — 99.98% of the domain — so a min–max
  filter derived after a leaky Bloom filter skips almost nothing. On JOB 07c,
  BF 6 held 14 items at 24 bits per key and 378 at 16; the accurate setting
  let cast_info's row groups be skipped and ran about 4× faster (§6.6.2,
  Figure 17).

  </details>

- [ ] You can state Algorithm 2's abandon conditions with RPT+'s constants and apply them to a case.

  <details><summary>Answer</summary>

  After γ = 100K rows reach the filter creator: abandon if λ = N_recv/N_scan
  > τ_sel = 0.35 *and* N_scan/N_total < τ_prog = 0.6, or if the estimated
  materialised size λ · N_total · avg_tuple_size exceeds M_avail = 64 GB
  (§5.4). On a 10 M-row table, 100K passed after 200K scanned gives λ = 0.5 at
  2% progress — abandon; after 400K scanned, λ = 0.25 — build (80 MB at 32 B
  per tuple).

  </details>

- [ ] You can give the regression numbers for RPT and RPT+ on SQLStorm and say where they come from.

  <details><summary>Answer</summary>

  §6.1, Tables 1–2, over the 13,308 SQLStorm queries that finish. RPT: 555
  queries (4.2%) at 0–0.7× and 3,185 (23.9%) at 0.7–0.9× — 28.1% below 0.9×.
  RPT+: 24 (0.2%) and 247 (1.9%) — 2.1%. Worst RPT+ query: 13615 at 0.37×
  (§6.6.3).

  </details>

- [ ] You can say why Appian gains 1.01× and what that says about the technique.

  <details><summary>Answer</summary>

  Appian is dominated by non-selective predicates (§6.2, Figure 12), so few
  base-table rows can be removed and transfer is pure overhead; with dynamic
  pipelines RPT+ abandons those filters and lands at 1.01× — no gain, but no
  regression (RPT regressed there). Predicate transfer needs a selective
  predicate somewhere in the join graph; PK–FK joins without one gain
  "negligible benefit" (§3.6). The technique is insurance for selective,
  badly-ordered queries, not a general speed-up.

  </details>

- [ ] You can compare RPT+'s Bloom filter with the register-blocked layout using lane 1b.

  <details><summary>Answer</summary>

  RPT+ uses a cache-sectorized filter at 20 bits/key and 7 hashes, FPR
  6.1 × 10⁻⁵ at 2.48 cycles/tuple (§5.4). Lane 1b at 100,000 keys and
  20.97 bits/key: a classic filter (k = 15) measures 0.00006 — the same as
  the paper's CSBF — and the register-blocked DuckDB layout (4 bits in one
  64-bit word) measures 0.00249, about 40× more false positives for the same
  memory. Over multiple hops (Step 5) that difference compounds.

  </details>

## References

- Yiming Qiao, Peter Boncz, Huanchen Zhang, **"Robust Predicate Transfer
  with Dynamic Execution"**, PVLDB 19(6), 2026 — §2.2 (RPT, LargestRoot,
  JOB 3a), §3 (ATP, Algorithm 1, cost proofs), §3.6 (PK–FK), §4 (cascade
  filters, n − 2/p, CSBF), §5 (Algorithm 2, τ values in §5.4), §6.1
  (Tables 1–2), §6.2 (Figure 11), §6.6 (JOB 10a, JOB 07c, SQLStorm 15651 and
  13615). Artifact: github.com/embryo-labs/dynamic-predicate-transfer.
- Junyi Zhao, Kai Su, Yifei Yang, Xiangyao Yu, Paraschos Koutris, Huanchen
  Zhang, **"Debunking the Myth of Join Ordering: Toward Robust SQL
  Analytics"**, SIGMOD 2025 — RPT.
- Harald Lang, Thomas Neumann, Alfons Kemper, Peter Boncz,
  **"Performance-Optimal Filtering: Bloom overtakes Cuckoo at
  High-Throughput"**, VLDB 2019 — the CSBF.
- Previous: [reading-yannakakis-predicate-transfer.md](reading-yannakakis-predicate-transfer.md).
  Next: [reading-sqlserver-bitmaps.md](reading-sqlserver-bitmaps.md), which
  gets the same guarantee with one pass and no separate phase.
- Topic 10's [README](../10-query-planning/README.md) §4 (cardinality
  estimation), for the one-paragraph version of this paper.
