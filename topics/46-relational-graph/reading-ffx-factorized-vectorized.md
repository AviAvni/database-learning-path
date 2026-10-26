# Packed factorized vectors: factorization without leaving the vectorized loop

Yasser, Dorbani & Mhedhbi (Polytechnique Montréal, SIGMOD'26) take the same
problem as Lehner & Neumann — m:n joins whose flat results dwarf their inputs —
into the other engine family: **vectorized** execution, the DuckDB/Velox model
where operators pass fixed-size column chunks. Their engine, **FFX**, keeps
every intermediate factorized *and* every vector fully packed, which the
list-based processing of Kùzu and Graphflow cannot do. This chapter builds
vectorized execution and f-trees from zero, reproduces the paper's running
example to the value (this topic's lane 1a does it in code), works the offset
arrays of Figure 4, and reads the evaluation — where the headline speedups come
with hand-picked join orders, and where Table 4's prose summary does not match
its own rows.

Everything quoted is from arXiv:2609.09002v1 (Proc. ACM Manag. Data 4(3),
Article 178). Topic 11's README §5 (hash joins and aggregation, vectorized) carries a short
note on this paper; this is the full reading.

## The problem in one sentence

A vectorized engine is fast because every operator loops over a full,
contiguous vector of values, but a factorized result is a tree whose child
lists are short and uneven — so how do you keep the tree's compactness without
falling back to tuple-at-a-time loops over tiny lists?

## The concepts, step by step

### Step 1 — vectorized execution and the selection vector

> **In:** nothing yet — this step is background.
> **Out:** the terms *vector*, *chunk*, *selection*, *interpretation
> overhead*, used by every later step.

**Vectorized execution** (MonetDB/X100, VectorWise, DuckDB — §2.1) passes data
between operators in **vectors**: fixed-size arrays (e.g. 2,048 entries) of one
column's values; a group of vectors for the columns of one batch is a
**chunk** (DuckDB's "data chunk"). Each operator is a tight loop over a whole
vector, which is cache-friendly and lets the compiler use SIMD. A filter does
not move data; it marks which positions survive, in a **selection vector**
(a list of surviving positions) or, in FFX, a **selection bitmap** with one bit
per position (§4.1). **Interpretation overhead** is the cost of the operator
dispatch itself — a virtual call per operator invocation — which a vector of
2,048 values amortises 2,048 ways and a vector of 3 values only 3 ways.

Why it matters: everything that follows is about keeping vectors *full*.
A factorized engine that hands an operator 3 values at a time has turned a
vectorized engine back into a slow interpreter.

### Step 2 — the running example: one relation, two f-trees

> **In:** the base relation `R(src, dst)` of Fig. 1a (ten edges).
> **Out:** the flat result of `Q2H = R(a1,a2) ⋈ R(a2,a3)` and its size over
> two f-trees — 39, 22 and 15 values. Steps 4–5 encode the 15-value one.

The relation has edges 0→2, 1→2, 1→3, 1→4, 2→5, 2→6, 2→7, 3→2, 3→4, 4→8.
`Q2H` is the 2-hop path query. An **f-representation** is a nested expression
of unions (alternatives) and products (independent combinations); its
**f-tree** says which attribute is grouped under which; its size is the number
of values it stores (§2.2.1). Working the three sizes the paper reports:

```text
flat: group by the middle vertex a2 and multiply in-neighbours by out-neighbours
  a2 = 2:  in {0,1,3} × out {5,6,7}   = 3 · 3 = 9 tuples
  a2 = 3:  in {1}     × out {2,4}     = 1 · 2 = 2
  a2 = 4:  in {1,3}   × out {8}       = 2 · 1 = 2
                                         13 tuples × 3 values = 39 values

T1 = a1 → a2 → a3 (the flat nesting with repeated prefixes removed)
  a1 level:  {0, 1, 3}                                  3 values
  a2 level:  0→{2}, 1→{2,3,4}, 3→{2,4}                  6 values
  a3 level:  one out-list per (a1,a2): 3 + (3+2+1) + (3+1) = 13 values
                                                         22 values

T2 = a2 → {a1, a3} (branch at the shared variable)
  a2 level:  {2, 3, 4}                                  3 values
  a1 level:  {0,1,3} | {1} | {1,3}                      6 values
  a3 level:  {5,6,7} | {2,4} | {8}                      6 values
                                                         15 values
```

Exactly the paper's 13 tuples, 39 values, 22 over T1 and 15 over T2 (§2.2.1).
This topic's lane 1a recomputes all four from the edge list and prints them
next to the paper's figures; the test `ffx_fig1_sizes_reproduce_the_paper`
(`experiments/src/fsize.rs:138`) pins them.

The **path constraint** (§2.2.1) decides which f-trees are legal: two
attributes that are *dependent* — in the same relation, or linked by a chain
of join predicates whose keys are projected out — must lie on one root-to-leaf
path. `a1` and `a3` are not dependent once `a2` is fixed, so T2 may put them
side by side. "In general, more branching and smaller height imply greater
compactness" (§2.2.1).

Why it matters: T1 is what a plan that binds `a1` first produces; T2 is what
binding `a2` first produces. The *attribute order* of the plan picks the f-tree,
and the f-tree decides the size — 22 versus 15 here, and the difference grows
with degree.

### Step 3 — list-based processing and why its vectors are sparse

> **In:** an attribute order (Step 2) executed by a pipelined engine.
> **Out:** the LBP state machine (`pos`, `size`) and its two limitations,
> which FFX's design answers.

**List-based processing** (LBP, Gupta, Mhedhbi & Salihoglu VLDB'21; used by
Kùzu and Graphflow — §2.2.3) represents each f-tree node as its own vector.
A vector's state holds `size` and a position `pos`: `pos = −1` means "this is
a list of values", `pos ≥ 0` means "this vector is bound to the single value
at `pos`". For the order `(a2, a1, a3)` (§2.2.3, Fig. 2):

```text
Scan[a2]           → a2 = [2, 3, 4], pos = −1
Join[R(a1,a2)]     → bind a2 to position 0 (a2 = 2, pos = 0); a1 = [0, 1, 3], pos = −1
Join[R(a2,a3)]     → a2 already single-valued;                 a3 = [5, 6, 7], pos = −1
                     chunk = {2} × {0,1,3} × {5,6,7} = 9 tuples
backtrack: advance a2's pos to 1 and repeat
```

Two costs follow (§2.2.3). **Sparse vectors**: child vectors are filled "at
the granularity of join matches, typically tens to hundreds of values", not to
the vector width — Fig. 2's 8-wide vectors hold 3 values. **Restricted
f-trees**: because grouping is implicit in `pos`, "at any node, at most one
child branch can extend beyond a height of 1". For the 4-edge path
`R(a1,a2) ⋈ R(a2,a3) ⋈ R(a3,a4) ⋈ R(a4,a5)`, the best f-tree branches at `a3`
into two 2-deep chains (T3, worst-case size N²); LBP can only build T4 or T5,
whose worst case is N³ (Fig. 3).

Why it matters: LBP pays interpretation overhead per *parent value* and caps
how much it can factorize — two reasons the same plan runs faster when the
grouping is made explicit.

### Step 4 — packed factorized vectors: grouping as an offset array

> **In:** an f-tree (Step 2) and a vector layout (Step 1).
> **Out:** FFX's `State` — `start`, `end`, a selection bitmap and an offset
> array — and the three vectors of Figure 4 for `Q2H`.

FFX keeps every node's values in one **packed** vector (filled contiguously,
no holes) and records the grouping explicitly in an **offset array** `off`:
"the values associated with the parent at position `i` occupy the slice
`(off[i], off[i+1])` in the child vector" (§4.1). `start` and `end` bound the
active slice of a parent vector; the selection is a bitmap so updates are
in-place and constant-size. The struct as printed in §4.1:

```text
§4.1, the packed factorized vector state (Yasser et al.)
1  struct State {
2     uint16_t start;
3     uint16_t end;
4     alignas(64) uint64_t sel[MAX_SIZE / 64]; // a single bit per position
5     alignas(64) uint16_t off[MAX_SIZE];      // offset array
6  };
```

Line 5 is the whole idea. Encoding `Q2H` over T2 (Fig. 4):

```text
a2  vals = [2, 3, 4]                 off = (none: root)
a1  vals = [0, 1, 3,  1,  1, 3]      off = [0, 3, 4, 6]    → a2=2: 0..3, a2=3: 3..4, a2=4: 4..6
a3  vals = [5, 6, 7,  2, 4,  8]      off = [0, 3, 5, 6]    → a2=2: 0..3, a2=3: 3..5, a2=4: 5..6
```

3 + 6 + 6 = 15 values — Step 2's T2 size — in three dense vectors. Compared
with LBP, "LBP would require backtracking to process the three a2 bindings and
therefore would require 3× as many operator calls" (§4.1.2). And because the
grouping lives in `off` rather than in `pos`, any valid f-tree can be encoded
(§4.1.3; Fig. 5 shows a branching f-tree).

This topic's lane 2 uses exactly this layout: `Level { vals, off }` in
`experiments/src/factorized.rs:43–58`, with `slice(i)` returning
`vals[off[i]..off[i+1]]`.

Why it matters: an operator can now scan a whole level as one tight loop and
find any parent's children with two array reads — vectorization and
factorization at once.

### Step 5 — joins: non-expanding, fanout-expanding, and where new values hang

> **In:** packed vectors over an f-tree (Step 4) and the next join in the
> attribute order.
> **Out:** the two join classes of §5.1 and the rule for choosing the
> grouping attribute.

A **non-expanding join** (1:1 or n:1) yields at most one match per key, so its
output shares the input's `State` and only the selection bitmap changes —
plain vectorized execution, which is FFX's "graceful fallback" (§5.1.1, §5.1.3).
A **fanout-expanding join** (1:n or m:n) appends each key's matches
contiguously to a new child vector and writes its `off` (§5.1.2). Which node
does the new attribute hang under? "We choose this grouping attribute as high
as possible while respecting the root-to-leaf path constraint" (§5): the
lowest join key on the path, so the new values are repeated as little as
possible. Multi-way joins run "join-at-a-time" in a worst-case-optimal style,
alternating extensions with intersections, with the order fixed at compile
time (§5.1.3).

Why it matters: "as high as possible" is the vectorized engine's version of
Lehner & Neumann's bottom insert — new values go under the attribute they
depend on, not under the whole prefix.

### Step 6 — cascade update: a filter in a tree is not local

> **In:** a factorized intermediate (Step 4) and a reducing operator — a
> filter, or a join whose key has no match.
> **Out:** the Cascade Update operator, its AGG and SCATTER steps, and its
> cost argument (§5.2).

In a flat vector, filtering position 7 affects position 7. In a factorized
result it does not: if every child under a parent is filtered out, the parent
binding has no completions and must go too, and *its other children* — in a
sibling subtree — must go with it (§5.2). FFX's **Cascade Update** propagates a
selection change along a **dependency tree** derived from the f-tree (§5.2.2):

- **AGG** (towards the root): a parent binding is cleared when all bits in its
  child slice are clear (Algorithm 2, lines 6–9).
- **SCATTER** (into siblings): every slice grouped under a newly cleared parent
  is cleared (Algorithm 2, lines 11–14).

A two-level example: `a2 = [2, 3, 4]`, `a3` as in Step 4, and a filter
`a3 > 4`. Clear bits for `a3` values 2 and 4 (positions 3, 4). AGG: `a2 = 3`'s
slice 3..5 is now all clear → clear `a2` position 1. SCATTER: clear `a1`'s
slice for that position, 3..4 (the value 1). What remains is
`{2} × {0,1,3} × {5,6,7} ∪ {4} × {1,3} × {8}` = 9 + 2 = 11 tuples — the 13 of
Step 2 minus the two that went through `a2 = 3`.

Three optimizations keep it cheap (§5.2.4): it is placed lazily (before a sink
or before a stale vector is read), fused into the preceding operator, and AGG
is driven by the *delta* of newly cleared positions, so "the cost … [is]
largely proportional to the number of newly introduced invalidations".

This topic's lane 2 asks for the same reduction done eagerly instead of with
bitmaps: `ThreeHop::retain_d` must leave no empty group, which
`ThreeHop::check` verifies (`experiments/src/factorized.rs:129`).

Why it matters: without cascading, a factorized count over-reports — the
parent still multiplies by its other siblings' sizes. Correctness, not speed,
is why this operator exists.

### Step 7 — the evaluation: what is measured, and what is chosen by hand

> **In:** FFX (Steps 4–6).
> **Out:** the measured results of §7 with the conditions they hold under,
> and three places where the prose does not match the tables.

Setup (§7.1): Google Cloud c4-standard-8 (8 vCPUs, 30 GB, Xeon Platinum
8581C), clang 15 `-O3`, median of three runs with the OS cache dropped between
queries; single-threaded unless stated.

**Packed versus unpacked, all plans (§7.2, Fig. 7).** Both layouts are
implemented inside FFX ("P" packed, "UP" LBP's unpacked) and run over *every*
attribute order of nine queries on the Google web graph — 16 + 31 + 48 + 240 +
1,440 + 16 + 72 + 72 + 16 = 1,951 plans. P wins on 1,883 and loses on 68 (all
Q5 plans under 100 ms); mean speedup 2.08×; largest 9.39× (Q2, 16.5 s →
1.76 s). The paper prints the split as 96.53% / 3.47%; 1,883 / 1,951 =
96.51% and 68 / 1,951 = 3.49% — a rounding slip, not a different result.

**Isolating vectorization and factorization (§7.2.1–7.2.2, Table 4).** Two
flat-equivalent plans (PQ1,1, PQ2,1) and two branching ones (PQ1,2, PQ2,2):

| plan | RT, UP → P | operator calls | instructions | cycles | IPC | processed tuples |
|---|---|---|---|---|---|---|
| PQ1,1 (flat order) | 27,663 → 16,671 (1.66×) | 114.13× fewer | 1.47× | 1.57× | 1.06× | 16.80× |
| PQ2,1 (flat order) | 294,089 → 200,793 (1.46×) | 108.43× | 1.52× | 1.35× | 1.11× | 17.76× |
| PQ1,2 (branching) | 2,348 → 441 (5.32×) | 687.69× | 3.63× | 3.59× | 1.02× | 80.96× |
| PQ2,2 (branching) | 16,502 → 1,757 (9.39×) | 1,378.84× | 12.84× | 8.14× | 1.58× | 122.60× |

Three things in the prose do not match these rows. The caption says run time
is "in seconds", but the values are milliseconds — §7.2 itself reports PQ2,2
as "16.5 to 1.76 seconds", which is 16,502 → 1,757 in ms. §7.2.1 says the
flat-order plans reduce operator calls "by up to 108.4×", but PQ1,1's row
shows 114.13×. And §7.2.1's "reduces total CPU cycles by 1.35–3.59×" mixes
rows: 1.35 and 1.57 are the two flat-order plans, 3.59 is a branching plan
from §7.2.2, and the other branching plan's 8.14 is left out. The finding
survives all three: with no factorization benefit, packing alone is worth
1.46–1.66×; with branching, 5.32–9.39×.

**End to end (§7.3).** LSQB at scale factor 10, single-threaded, Q1–Q6
(Table 5, seconds): FFX 1.28 vs Kùzu 591.10 vs DuckDB 18.43 on Q1; the cyclic
Q2 is a tie with DuckDB (2.71 vs 2.72); FFX never loses. But "for FFX, we pick
orderings to maximize factorization … In contrast, cyclic join queries … we
evaluate FFX under DuckDB's chosen ordering" (§7.3.3) — FFX has no optimizer,
so the comparison is an engine with a hand-chosen plan against engines with
their own optimizers. On the nine synthetic queries over Amazon, the reported
speedups span "10²×–10⁵×" (Fig. 9); for Q1, DuckDB and Kùzu "materializ[e]
2.98B tuples" where FFX's two factorized halves hold 63.2M (§7.3.5).

**Semantic operators (§6, §7.5).** Serializing the factorized intermediate
into the LLM prompt cuts input tokens by 1.70× to 15.67× at equal batch size
(Table 6), with recall within about 0.07 of the flat prompt at batch sizes
8–64 and better than flat at batch 256, where flat prompts lose 59% of their
output tuples.

Why it matters: the engine-level result (packed beats unpacked on 1,883 of
1,951 plans, same plans, same engine) is the clean one. The system-level
comparisons are real measurements of a different question — what a factorized
engine *can* do given a good order — and the paper says so.

### Step 8 — what this means for a vectorized graph engine

> **In:** Steps 3–7.
> **Out:** the design consequences for this topic's lanes and for a
> Cypher engine's intermediate representation.

- The attribute order chooses the f-tree (Step 2): binding the middle vertex of
  a path first is what makes lane 1's factorized size `Σ(in + out)` instead of
  `Σ in·out`. A planner for factorized execution optimizes f-tree size, and FFX
  does not yet have one (Step 7).
- LBP's limitation (Step 3) is a limitation of encoding grouping in vector
  *state*. Encoding it in data (`off`) is cheap: one `u16` per parent position.
- Lane 2 asks you to build Step 4's layout one level deeper and Step 6's
  reduction for one filter. The test that `count()` equals the flat join's
  2,224,823,394 at |V| = 40,000 is what keeps a cascading bug from hiding.

## How to read the paper (with the concepts in hand)

1. **§1–§1.1** and **Table 1** (Flat / FDB / LBP / FFX feature matrix).
2. **§2.2.1** with Step 2's worked sizes; **§2.2.3** with Fig. 2 and Fig. 3.
3. **§4.1** and **Fig. 4** — redo the offsets by hand before reading §4.1.2.
4. **§5.1**, then **§5.2** with Algorithm 2 open; Fig. 6's dependency tree is
   easier after Step 6's two-level example.
5. **§7.2** with Table 4 and Step 7's notes; **§7.3.3**'s paragraph on chosen
   orderings before Table 5.
6. **§6** and **§7.5** if the LLM-operator angle matters to you; skip otherwise.

## Where each step lives in the code

FFX is at `github.com/dais-polymtl/ffx` (footnote 1); it is not cloned for
this topic. The concepts map onto this topic's crate:

| step | file | what to look at |
|---|---|---|
| 2 | `experiments/src/gen.rs:89` | `ffx_fig1()` — the ten-edge relation of Fig. 1a |
| 2 | `experiments/src/fsize.rs:138` | the test asserting 13 / 39 / 22 / 15 |
| 4 | `experiments/src/factorized.rs:43–58` | `Level { vals, off }` — the offset-array layout |
| 4 | `experiments/src/factorized.rs:99` | `two_hop` — building T2 for a whole graph |
| 6 | `experiments/src/factorized.rs:167` | `retain_d` — your eager cascade (lane 2) |

## Questions (answer in notes.md)

1. Build the T1 encoding (offsets included) of `Q2H` over Fig. 1a. How many
   offset entries does it need, and how many does T2 need?
2. Redo Step 6's filter with `a3 > 6` instead of `a3 > 4`. Which `a2`
   positions does AGG clear, and how many tuples remain?
3. LBP restricts branching to "at most one child branch … beyond a height of
   1". Which of lane 1's two patterns (2-hop, 3-hop) can LBP encode in its most
   compact f-tree, and which cannot?
4. FFX's `off` is `uint16_t` (§4.1). What does that bound, and what happens in
   lane 2's layout, whose offsets are `u64` because one level spans the whole
   graph rather than one chunk?
5. Table 4's "processed tuples" column falls 16.80× even for a flat-order plan.
   If the representation is not more compact there, what is being processed
   fewer times?

## Done when

Answer each before unfolding it.

- [ ] You can compute the 39, 22 and 15 of §2.2.1 from the ten edges by hand.

  <details><summary>Answer</summary>

  Group by the middle vertex: a2 = 2 has 3 in × 3 out = 9 paths, a2 = 3 has
  1 × 2 = 2, a2 = 4 has 2 × 1 = 2; 13 tuples × 3 = 39 values. T1 (`a1 → a2 → a3`):
  3 distinct `a1`, 6 `(a1, a2)` pairs whose `a2` has an out-edge, and one
  out-list per pair, 3 + 6 + 4 = 13 values: 3 + 6 + 13 = 22. T2 (`a2 → {a1, a3}`):
  3 middles, their in-lists (3 + 1 + 2 = 6) and out-lists (3 + 2 + 1 = 6): 15.

  </details>

- [ ] You can say what LBP's `pos` field encodes and name its two consequences.

  <details><summary>Answer</summary>

  `pos = −1` means the vector is a list; `pos ≥ 0` means it is bound to the
  single value at that position. Joins bind a parent to one value at a time,
  so child vectors hold one parent's matches — sparse, with interpretation
  overhead per parent value — and the grouping can only be expressed for f-trees
  where at most one child branch goes deeper than one level (§2.2.3), which
  rules out the most compact f-tree of a 4-edge path (N² vs N³ worst case).

  </details>

- [ ] You can write the offset arrays of Fig. 4 and explain why they make any f-tree expressible.

  <details><summary>Answer</summary>

  `a1 = [0,1,3, 1, 1,3]` with `off = [0,3,4,6]`; `a3 = [5,6,7, 2,4, 8]` with
  `off = [0,3,5,6]`; parent `a2 = [2,3,4]` has no `off`. Each node's grouping
  under its parent is stored explicitly per node, so a node can have any number
  of children, each with its own `off`, at any depth — the grouping no longer
  depends on a single "current binding" in the parent's state (§4.1.3).

  </details>

- [ ] You can explain why a filter on a leaf must cascade, with the two directions.

  <details><summary>Answer</summary>

  A parent whose child slice is entirely filtered has no completions, so it
  must be cleared (AGG, towards the root); once cleared, every slice in its
  *other* subtrees grouped under it describes tuples that no longer exist, so
  those are cleared too (SCATTER). Without it, a count multiplies the cleared
  parent's surviving sibling lists into tuples that are not in the result. FFX
  keeps it cheap by driving AGG from the delta of new invalidations (§5.2.4).

  </details>

- [ ] You can state the evaluation's clean result and the caveat on its end-to-end tables.

  <details><summary>Answer</summary>

  Clean: inside one engine, over all 1,951 plans of nine queries on Google,
  packed vectors beat LBP's unpacked ones on 1,883 with a mean 2.08× and a
  maximum 9.39× (§7.2); Table 4 shows packing alone worth 1.46–1.66× when the
  f-tree is flat. Caveat: in the LSQB and synthetic comparisons FFX's orders
  are picked by hand "to maximize factorization" (§7.3.3) and FFX has no
  optimizer — DuckDB and Kùzu choose their own plans.

  </details>

- [ ] You can name the three places where §7.2's prose disagrees with Table 4.

  <details><summary>Answer</summary>

  The caption says seconds; the values are milliseconds (16,502 → 1,757 is the
  prose's "16.5 to 1.76 seconds"). "Up to 108.4×" fewer operator calls for the
  flat-order plans, but PQ1,1 shows 114.13×. "1.35–3.59×" fewer cycles mixes a
  branching plan (3.59) into the flat-order range and omits the other
  branching plan's 8.14×.

  </details>

## References

- Sunny Yasser, Anas Dorbani, Amine Mhedhbi, **"Factorized and Vectorized
  Execution: Optimizing Analytical and Semantic Queries over Relations"**,
  Proc. ACM Manag. Data 4(3), Article 178 (SIGMOD 2026),
  [arXiv:2609.09002](https://arxiv.org/abs/2609.09002). §2.1, §2.2.1 (Fig. 1),
  §2.2.3 (Figs. 2–3), §4.1 (State, Fig. 4), §5.1, §5.2 (Algorithm 2, Fig. 6),
  §7.2 (Fig. 7, Table 4), §7.3 (Table 5, Figs. 8–9), §7.5 (Table 6).
- Pranjal Gupta, Amine Mhedhbi, Semih Salihoglu, **"Columnar Storage and
  List-based Processing for Graph Database Management Systems"**, PVLDB 14(11),
  2021 — LBP, the baseline of Step 3.
- Previous chapter: [reading-lehner-neumann-factorized.md](reading-lehner-neumann-factorized.md),
  the compiled-engine version; topic 13's [reading-kuzu.md](../13-graph-engines/reading-kuzu.md)
  for the system LBP comes from.
