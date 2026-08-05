# Compiled vs vectorized: the fair fight ends in a near-tie

Kersten et al. (VLDB '18) built BOTH engines — Typer (HyPer-style
data-centric compilation) and Tectorwise (X100-style vectorization) —
sharing everything else, then raced them. Fair benchmarking (topic 0
discipline) applied to the execution-model war. Before the paper, this
chapter builds the two contenders and the two hardware effects that
decide every round — registers versus intermediates, and cache-miss
overlap — step by step; the residual differences, not the headline
winner, are what decide M11 and M19.

There is no headline winner. The paper's own summary opens "To our
surprise, the performance of vectorized and data-centric compiled query
execution is quite similar in OLAP workloads" (§10), and every number
below is checked against the section, table or figure it came from. Two
figures this guide used to carry were not in the paper at all; both
corrections are called out where they occur.

## The problem in one sentence

By 2018 both modern execution models claimed to have killed
interpretation overhead — the question is what separates them once that
tax is gone, and the answer is second-order hardware effects that leave
the two within a factor of 1.74 in the *worst* case (Typer 74% faster on
Q1, Tectorwise 32% faster on Q9, §4.1), against a HyPer-vs-Postgres gap
of one to two orders of magnitude — plus a set of operational
differences (§8) that are not about rows per second at all.

## The concepts, step by step

### Step 1 — the common enemy: interpretation overhead

> **In:** two engine designs that look nothing alike, and a reason to
> care which one you build.
> **Out:** the single cost both were invented to remove, sized on this
> repo's own measurement — so that Step 4's near-tie reads as "the war
> is over", not "the difference is small because nothing matters".

A classic **Volcano** (**iterator**) engine composes operators as a tree
in which each exposes `next()` returning **one tuple**, and evaluates
`f < 50` by walking plan-time expression objects. Per row it pays an
indirect call per operator plus that walk, while the useful work is a
single compare.

This repo has the tax on a machine you can hold:

```
 FINDINGS.md row 11 / notes.md — exec_bench volcano lane,
 Apple M3 Pro, 50M rows, scan -> filter -> group-by-sum:

   5%  selectivity   0.386 s   129.4 M rows/s    7.72 ns per scanned row
   50%               0.484 s   103.3 M rows/s    9.68 ns
   95%               0.669 s    74.7 M rows/s   13.38 ns

 at ~4 GHz:      7.72 ns = ~31 cycles to move one row through
                 two dyn calls and one accumulate
```

Two things to carry forward. First the size: ~31 cycles for work that is
a compare and an add. X100's profile of MySQL puts a single addition at
49 cycles ([reading-x100.md](reading-x100.md)); the paper here notes the
resulting whole-system gap between HyPer and Postgres is "between one and
two orders of magnitude" (§4.1). That is the war both contenders won.

Second, the direction: the lane gets **slower as selectivity rises**.
Surviving the filter is what costs — a rejected row costs one `next()`
call inside the filter's own loop, a survivor pays a second `next()` up
the chain plus the aggregate's hash and accumulate. "High selectivity =
less work" is exactly backwards for a tuple-at-a-time engine, and the
95% column is the number to keep in mind when Step 5 explains why the
*probe* is where the two models actually separate.

### Step 2 — vectorized execution: interpret once per thousand rows

> **In:** the per-tuple tax from Step 1.
> **Out:** the first cure — keep the interpreter, enlarge its unit —
> and the two hard constraints it imposes on every line of engine code,
> which are what generate all of Tectorwise's losses later.

The vectorized model (X100 lineage — see
[reading-x100.md](reading-x100.md)) keeps Volcano's tree, but each
operator call processes a **vector**: an array of ~1,000-2,048 values of
one column. §2 states the goal as "to amortize the DBMS's interpretation
decisions by performing as much as possible inside the data manipulation
methods… hash 1000s of values, compare 1000s of string pairs, update a
1000 aggregates". The work is done by **primitives** — precompiled,
type-specialized, branch-light loops.

The amortization is not a hope, it is measured:

```
 §4.2, profiler over the query set at SF=10:
   interpreted part of runtime          < 1.5%
   time inside primitives                 98.5%
```

**Correction:** this guide previously estimated the residue as "~100 ns
of dispatch ÷ 2048 rows ≈ 0.05 ns/row". The estimate was invented; the
paper measured the thing directly, and 1.5% is the number to quote.

§4.2 then goes further, and this is the sentence the rest of the guide
hangs on. Tectorwise executes more instructions per tuple than Typer —
but since 98.5% of time is inside primitives, and "primitives know all
involved types at compile time", the extra instructions **are not
interpretation**. They "are rather due to the load/store instructions for
materializing primitive results into vectors". Vectorization did not
leave a little interpretation behind; it traded interpretation for
memory traffic.

The trade is forced by two constraints §2.1 spells out. A vectorized
function (i) can only work on **one data type** — "the number of
combinations grows exponentially" otherwise — and (ii) must process
multiple tuples. Figure 1 shows what that costs on `color = 'green' AND
tires = 4`:

```
 Figure 1a, generated code — one loop, both predicates in one if:
    for i in 0..n:  if col[i]=="green" && tir[i]==4:  res.append(i)

 Figure 1b, vectorized — constraint (i) forbids the mixed-type if,
 so it must become two primitives with a selection vector between:
    s   = sel_eq_string(col, "green")     // writes positions
    res = sel_eq_int(tir, 4, s)           // reads positions

 cost: one intermediate array written and read that the fused
       version kept in a register — "The resulting materialization
       of intermediates makes fast caches very important for
       vectorized engines" (§2.1)
```

### Step 3 — compiled execution: fuse the pipeline into one loop

> **In:** the same per-tuple tax, attacked from the other side.
> **Out:** the second cure — delete the interpreter — and the resource
> it spends instead of memory: registers, of which there are sixteen.

The compiled model (HyPer lineage) generates machine code at query time
that "fuses all adjacent non-blocking operators of a query pipeline into
a single, tight loop" (§2). A **pipeline** is a chain of operators
between materialization points (scan → filter → aggregate); a
**pipeline breaker** is an operator that must consume its whole input
before producing output (a hash-join build, a sort), and it ends the
pipeline. Within the loop a row's values live in CPU **registers** — of
which x86-64 has 16 general-purpose — from scan to sink. No calls, no
intermediate arrays, no dispatch.

Both models on `SELECT k, SUM(v) FROM t WHERE f < 50 GROUP BY k`:

```
 Typer (compiled)                    Tectorwise (vectorized)
 ─ one fused loop, JIT-compiled ─    ─ interpreted per vector ─
 for each row:                       sel = filter_lt(f_vec, 50)     // loop 1
   if (f < 50)                       h   = hash(k_vec, sel)         // loop 2
     ht[k] += v                      g   = ht_lookup(h, sel)        // loop 3
                                     agg_add(states, g, v_vec, sel) // loop 4
 tuple stays in REGISTERS            vector stays in L1; each loop is
 across all operators                simple, branch-free, SIMD-able
```

**JIT** is just-in-time compilation: emitting machine code (HyPer emits
LLVM IR) after the query arrives. The bill arrives before the first row
moves, and Step 7 has what the paper does and does not say about its
size.

### Step 4 — the fair fight: build both, share everything else

> **In:** two designs, and a literature of comparisons between whole
> systems where storage format, hash table, parallelization framework
> and compiler all differ at once.
> **Out:** one variable changed, five queries measured, and a result
> that is a range rather than a winner.

Prior comparisons raced HyPer against VectorWise, where attribution is
impossible. §3's method: implement Typer and Tectorwise in **one test
system**, with "the same algorithms and data structures" and "the same
physical query plans", so that "the only difference between Tectorwise
and Typer is the query execution method". Both were even given the same
parallelization framework — morsel-driven (§6.1) — specifically to
remove it as a variable. That is topic 0's discipline: change one thing.

Two caveats the paper is explicit about, both of which shape how you may
quote it. §3: "We do not include query parsing, optimization, code
generation, and compilation time in our measurements" — so every runtime
below is *execution only*, and compilation is free by construction.
§3.3: the workload is five representative TPC-H queries, not the suite —
Q1 (fixed-point arithmetic, 4-group aggregation), Q6 (selective filters),
Q3 (join, 147 K build / 3.2 M probe), Q9 (join, 320 K build / 1.5 M
probe), Q18 (high-cardinality aggregation, 1.5 M groups).

The result (§4.1, Figure 3, SF=1, 1 thread):

```
 relative single-thread performance, per query:
   Q1     Typer faster by 74%     (arithmetic, in-cache aggregation)
   Q18    Typer faster            (Table 1: 30 vs 48 cycles/tuple = 60%)
   Q6     tie                     (Table 1: 11 vs 11 cycles/tuple)
   Q3     Tectorwise faster by  4% (join)
   Q9     Tectorwise faster by 32% (join)

 the paper's framing of that spread:
   "these are not large differences… the difference between HyPer and
    PostgreSQL is between one and two orders of magnitude"
   "neither paradigm is clearly dominated by the other"
```

**Correction:** this guide previously reported "TPC-H geometric mean
within ~10-20%". No such figure appears in the paper — there is no
geometric mean over TPC-H in it, and the honest summary is the range
above: 1.74× the worst way for Tectorwise, 1.32× the worst way for
Typer, direction depending on the query. Reporting a mean would also
have destroyed the finding, since the two halves of the spread point
opposite ways for opposite reasons. Those reasons are Steps 5 and 6.

### Step 5 — memory-level parallelism: why vectorized wins hash probes

> **In:** Q3 and Q9, where "both engines use exactly the same hash table
> layout and therefore also have an almost identical number of last
> level cache misses" (§4.1) — so the difference cannot be the algorithm.
> **Out:** a hardware effect that turns identical miss *counts* into
> different miss *costs*, and the counter that proves it.

**Memory-level parallelism** (MLP) is a core's ability to have several
cache misses outstanding at once, so that overlapped misses cost far
less than their sum. §4.1 explains the mechanism in both directions:

- Tectorwise's "hash table probing code is only a simple loop. It
  executes only hash table probes thus the CPU's out-of-order engine can
  speculate far ahead and generate many outstanding loads."
- Typer's "code has more complex loops. Each loop can contain code for a
  scan, selection, hash-table probe, aggregation and more. The
  out-of-order window of each CPU fills up more quickly with complex
  loops thus they generate less outstanding loads."

**Correction:** this guide previously said the fused loop has "ONE miss
in flight". The paper's claim is *fewer*, not one — the out-of-order
window fills faster because each iteration carries more instructions.
The distinction matters: the cure is not "batch or lose", it is anything
that keeps the reorder window free, which is why software prefetching
(group prefetching / AMAC) works for compiled probes at the cost of
contorting the loop.

The counter that settles it is memory stall cycles, and the SSB table in
§4.4 (1 thread, SF=30, per tuple) shows it as arithmetic:

```
              cycles  IPC  instr  L1miss  branchmiss  mem stall
   Q3.1 Typer    55   0.7    40     1.1      0.24       27.95   = 51% stalled
   Q3.1 TW       53   1.3    71     1.7      0.41       15.68   = 30% stalled
   Q4.1 Typer    78   0.5    39     1.8      0.38       45.91   = 59% stalled
   Q4.1 TW       59   1.0    61     2.5      0.63       19.48   = 33% stalled

 read Q4.1 as the whole thesis in one row:
   Tectorwise runs 61/39 = 1.56x the instructions
   and takes 2.5/1.8 = 1.39x the L1 misses
   and still finishes in 59/78 = 0.76x the cycles,
   because it waits 45.91 - 19.48 = 26.4 fewer cycles per tuple
```

§4.1 adds that the advantage grows with the hash table: "Tectorwise's
join advantage increases up to 40% for larger data (and hash table)
sizes". Same lesson as topic 0's `lookup_shootout` — the miss count is
not the cost; the miss *schedule* is.

### Step 6 — registers vs intermediates: why compiled wins expressions

> **In:** Q1, the opposite regime — fixed-point arithmetic over a
> four-group aggregation that never leaves cache.
> **Out:** the cost of Step 2's constraint (i) in instructions per
> tuple, plus a warning about the metric you would naturally reach for
> to measure it.

When there are no misses to hide, MLP buys nothing and Step 2's
materialization is pure cost. Table 1 (TPC-H SF=1, 1 thread, per tuple):

```
              cycles  IPC  instr  L1miss  LLCmiss  branchmiss
   Q1  Typer     34   2.0    68     0.6     0.57      0.01
   Q1  TW        59   2.8   162     2.0     0.57      0.03
   Q6  Typer     11   1.8    20     0.3     0.35      0.06
   Q6  TW        11   1.4    15     0.2     0.29      0.01
   Q3  Typer     25   0.8    21     0.5     0.16      0.27
   Q3  TW        24   1.8    42     0.9     0.16      0.08
   Q9  Typer     74   0.6    42     1.7     0.46      0.34
   Q9  TW        56   1.3    76     2.1     0.47      0.39
   Q18 Typer     30   1.6    46     0.8     0.19      0.16
   Q18 TW        48   2.1   102     1.9     0.18      0.37

 Q1, the extremes the paper quotes as "up to 2.4x" and "up to 3.3x":
   instructions   162 / 68 = 2.4x
   L1 misses      2.0 / 0.6 = 3.3x
   LLC misses     0.57 = 0.57 — identical, so this is not about DRAM
   result         59 / 34 = 1.74x slower
```

The LLC row is the tell: both engines miss last-level cache equally
often on Q1, so Tectorwise's 94 extra instructions and 1.4 extra L1
misses per tuple are entirely the write-and-reread of intermediates
between primitives. §4.1: "In Tectorwise intermediate results must be
materialized, which is similarly expensive as the computation itself."

Now the warning, because it is the most reusable thing in the paper.
Tectorwise's **IPC on Q1 is 40% higher** — 2.8 against 2.0 — while being
74% slower. §4.1: "having a higher IPC is not always better… one has to
be cautious when using IPC to compare database systems' performance. It
is a valid measure of the amount of free processing resources, but should
not be used as the sole proxy for overall query processing performance."
A model that executes 2.4× the instructions can retire them beautifully
and still lose.

Two more results belong here, both of which kill plausible stories.

**Instruction cache is not the differentiator.** Generated code is
bigger, so you would expect Typer to thrash L1i; recent work found i-cache
misses to be a real problem for OLTP [43]. §4.2 measured it and found
"instruction cache misses are negligible, thus not a performance
bottleneck for OLAP queries. For all queries measured, the L1 instruction
cache (32 KB) was large enough to contain all hot code." One 32 KB
number retires the whole hypothesis for this workload — and note it is
workload-specific, not a law.

**Branch misses do not line up with the winner either.** Read the
branch-miss column above: Tectorwise wins it on Q3 (0.08 vs 0.27) and
loses it on Q9 (0.39 vs 0.34) and Q18 (0.37 vs 0.16) — yet Tectorwise
wins Q3 *and* Q9 and loses Q18. The correlation that does hold across
every row is the mem-stall column of §4.4. Prefer the counter that
tracks the outcome.

**SIMD does not rescue the vectorized side either** (§5). Primitives are
tight typed loops, so they are the natural home for **SIMD** (one
instruction over many values), and the micro-benchmarks deliver: up to
8.4× in isolation, 2.3× for hashing. But gather instructions give only
1.1× "because the memory system of the test machine can perform at most
two load operations per cycle — regardless of whether SIMD gather or
scalar loads are used", the full probe primitive gains 1.4×, and
end-to-end the gains "almost vanish", landing "around 10% for join
queries" — even though 55-65% of runtime is inside SIMD-optimized
primitives. Figure 9's sweep says why: SIMD helps while the working set
is in cache and stops helping once it is not. §5.4's conclusion is that
"SIMD does not shift the balance in favor of vectorization much".

Auto-vectorization is worse news, and worth knowing before you reach for
it: of GCC 7.2, Clang 5.0 and ICC 18, only ICC vectorized a fair share of
primitives, and only with AVX-512; it cut instructions per tuple by
20-60% and produced **no significant runtime improvement**, sometimes
running slower (§5.3).

### Step 7 — the operational column: everything that isn't rows/second

> **In:** a performance comparison that came out a tie, which is
> precisely what makes §8 the deciding section.
> **Out:** five dimensions on which the models are *not* tied, and the
> one place this guide previously invented a number.

§8's opening states the situation: "The performance differences are not
large enough to make a general recommendation whether to use
vectorization or compilation. Therefore, as a practical matter, other
factors… may be of greater importance."

- **OLTP (§8.1)** — compilation wins outright. A vectorized engine needs
  many vectors of values to be efficient, and "for OLTP workloads,
  vectorization has little benefit over traditional Volcano-style
  iteration", while compilation can fuse an entire stored procedure into
  one machine-code fragment. The evidence offered is organizational:
  Microsoft SQL Server already had a vectorized engine (Apollo) and the
  team "felt compelled to additionally integrate the compilation-based
  engine Hekaton".
- **Compile time (§8.2)** — vectorization wins, because primitives are
  precompiled. **Correction:** this guide previously priced Typer's
  compilation at "100s of ms of LLVM per query". That figure is not in
  the paper, which excludes compilation time from every measurement
  (§3). What §8.2 does say is the *shape*: LLVM compile time is "often
  super-linear to code size", and code size grows with operator count —
  or with column count, since "a small SQL query such as `SELECT * FROM
  T` can produce a lot of code if table T has thousands of columns". The
  mitigations are the interesting part: HyPer switches off LLVM passes
  including register allocation for its own more scalable algorithm, and
  ships an LLVM IR interpreter that runs the first morsels — "if that is
  enough to answer the query, full LLVM compilation is omitted". Spark
  falls back to tuple-at-a-time interpretation above 8 KB of generated
  Java bytecode.
- **Profiling (§8.3)** — vectorization wins. Per-primitive cycle counts
  "adds only marginal overhead, as each call to the function works on a
  thousand values". For compiled code "it is currently not possible in
  Spark SQL to know the individual contributions to execution time of
  relational operators, since the system can only measure performance on
  a per-pipeline basis".
- **Adaptivity (§8.4)** — vectorization wins, "the idea of adaptive
  execution works best in systems that interpret a query". The worked
  example is why VectorWise beat Tectorwise on Q1 (Table 2): during
  aggregation it tries to partition a vector's tuples into one selection
  vector per group-by key, backing off exponentially if there are too
  many groups; when it succeeds, hash aggregation becomes ordered
  aggregation with the running sum in a register, so "the aggregates are
  just updated once per vector".
- **Implementation (§8.5)** — a wash, differently shaped. Compiled
  systems are "code that generates code, thus … harder to comprehend and
  debug"; vectorized systems must keep control logic out of primitives
  and live with constraint (i). The paper's own example of that
  constraint biting: composite sort keys, where a multi-column
  comparison must be decomposed into several primitives communicating
  through a boolean vector — extra materialization that a compiled sort
  specialized to the record format avoids entirely.

The scorecard, restated to match §8.6's table and the sections above:

| dimension | compiled (Typer) | vectorized (Tectorwise) | evidence |
|---|---|---|---|
| computation-heavy, in cache | **wins** — registers, 2.4× fewer instr | loses — materialization | Table 1 Q1: 34 vs 59 cycles/tuple |
| memory-bound probes | loses — window fills, fewer loads in flight | **wins** — miss overlap | Q9 74 vs 56; §4.4 mem stalls |
| SIMD headroom | — | small: ~10% on joins, 8.4× only in isolation | §5.2, §5.4 |
| compile latency | super-linear in code size; mitigations required | **zero** — primitives precompiled | §8.2 (not measured here) |
| OLTP / stored procedures | **wins** | little benefit over Volcano | §8.1 |
| profiling | per-pipeline only | **per-primitive**, marginal overhead | §8.3 |
| adaptivity | recompile | **swap primitives mid-flight** | §8.4 |
| implementation | codegen indirection | constraints on every primitive | §8.5 |

One last result that shrinks the performance column further: with
morsel-driven parallelism on 20 hyper-threads (Table 3, SF=100), "for all
but one query, the performance gap between the two systems becomes
smaller… For the join queries Q3 and Q9, the performance benefit of
Tectorwise is cut in half". Q1's ratio moves from 0.56 to 0.66, Q18's
from 0.75 to 0.97. Hyper-threading hides microarchitecturally
sub-optimal code — so the more cores you have, the less the choice
costs you.

Topic 19 revisits compilation; M11 goes vectorized.

## How to read the paper (with the concepts in hand)

~1.5 h. The scorecard sections matter more than the aggregate runtimes —
which is fortunate, since there are no aggregate runtimes.

| Section | What is there | Step |
|---|---|---|
| §1-2 | the two models, and Figure 1's multi-predicate example — the cheapest illustration of constraint (i) in the paper | 2, 3 |
| §2.1-2.2 | why a primitive can only handle one type, and the hash join / group by pseudo-code for both engines | 2 |
| §3 | the fairness argument. Read §3 itself for the two caveats: compilation time excluded, five queries not the suite | 4 |
| §4.1 + Table 1 | **read carefully.** The 74%/32% spread; instructions and L1 misses "up to 2.4×/3.3×"; the out-of-order-window explanation; the IPC warning | 4, 5, 6 |
| §4.2 | interpretation is <1.5% of runtime, and the extra instructions are load/stores not dispatch; the 32 KB i-cache non-result | 2, 6 |
| §4.3 + Figure 5 | Tectorwise's own vector-size sweep — 1,000 default, degradation below 64 and above 64 K. X100's U-curve, re-measured 13 years later | 2 |
| §4.4 | the SSB table, with the mem-stall column that actually tracks the winner | 5 |
| §5 | SIMD: 8.4× in isolation, ~10% end-to-end, and §5.3's auto-vectorization result | 6 |
| §6 | both engines given morsel-driven parallelism; HyPer 11.7× vs VectorWise 7.2× is *exchange vs morsel*, not compiled vs vectorized | 4 |
| §8 | **don't skip.** The whole operational column; §8.4's adaptive-aggregation example is the best concrete thing in it | 7 |
| §9-10 | hybrid models (Figure 13's design space) and the five-bullet summary; read §10 last and check it against your own notes | — |

## Takeaway

The interesting result is the *shape* of the tie. Tectorwise runs up to
2.4× the instructions and 3.3× the L1 misses of Typer and still wins the
join queries, because the instructions it wastes are load/stores it
issues while the memory system is busy anyway, and its simple loops keep
the out-of-order window free to overlap misses. Typer wins wherever
there is nothing to overlap: Q1's in-cache arithmetic, where identical
LLC-miss counts prove the gap is pure materialization.

So the question to ask of your own workload is not "which model" but
"which regime": does this query stall on memory, or compute in cache?
Graph traversal — probes and expands over a large adjacency structure —
lives in the stalled column, which is the M11 argument. And if the answer
is genuinely mixed, note that morsel parallelism and hyper-threading each
shrink the gap (Table 3), while §8's operational column does not shrink
at all. That is why a project with no LLVM budget and a need for
per-operator profiling can choose vectorization without losing an
argument about rows per second.

## Questions for notes.md

1. Why does vectorized probing overlap misses but the compiled loop
   doesn't? Connect to lookup_shootout (topic 0): what did MLP do for
   HashMap throughput there? (§4.1's out-of-order-window sentence is the
   mechanism; §4.4's mem-stall column is the proof.)
2. Software prefetching rescues compiled probes (they cite group
   prefetching / AMAC). Why is prefetching EASY in a vectorized kernel
   (you have the whole vector of hashes) and CONTORTED in a fused loop?
3. The "wide pipeline" case: 10 carried columns through 3 operators —
   count the loads/stores per row for each model. Check your count
   against Table 1's Q1 row: 94 extra instructions and 1.4 extra L1
   misses per tuple, at identical LLC misses. §4.2 says where they go.
4. Your kernels.rs is a HAND-compiled Typer pipeline for one fixed query.
   Predict from the paper: will it beat your vectorized.rs on the
   filter+sum workload (compute-bound, k dense)? By how much? (Q1 is the
   closest analogue: 1.74×. Q6 — a pure selective filter — is a tie.)
5. M11 (and topic 19's JIT milestone): FalkorDB queries are
   pattern-matching heavy — probes and expands, memory-bound. Which
   column of the scorecard do graph workloads live in, and what does
   that say about JIT priority for M19?

## Done when

Answer each before unfolding it.

- [ ] You can state the paper's result as a range with directions, not as a winner or a mean — and say why a mean would have been the wrong summary.

  <details><summary>Answer</summary>

  §4.1, single-threaded, five TPC-H queries: Typer faster by 74% on Q1
  and by ~60% on Q18 (Table 1: 30 vs 48 cycles/tuple); a tie on Q6 (11 vs
  11); Tectorwise faster by 4% on Q3 and 32% on Q9. The paper's own
  framing: "neither paradigm is clearly dominated by the other", against
  a HyPer-vs-Postgres gap of one to two orders of magnitude.

  A mean is the wrong summary because the two halves of the spread have
  *opposite causes* — register residency on compute-bound queries,
  miss overlap on memory-bound ones — so averaging them reports a number
  that predicts nothing about the next query. It would also imply a
  ranking the paper spent twenty pages refusing to produce.

  </details>

- [ ] You can explain what Tectorwise's extra instructions actually are, with the measurement that rules out the obvious answer.

  <details><summary>Answer</summary>

  They are load/store instructions materializing each primitive's result
  into a vector — not interpretation. §4.2 rules interpretation out by
  measurement: a profiler puts the interpreted part at "less than 1.5% of
  the query runtime" at SF=10, so 98.5% of time is inside primitives, and
  primitives "know all involved types at compile time". Whatever the
  extra instructions are, they are executing inside typed loops.

  Table 1's Q1 row confirms the mechanism: 162 vs 68 instructions per
  tuple and 2.0 vs 0.6 L1 misses, at an *identical* 0.57 LLC misses. The
  extra traffic never reaches DRAM — it is the write-and-reread of
  intermediates through L1, exactly what §2.1's Figure 1 predicts when
  constraint (i) forces one `if` into two primitives.

  </details>

- [ ] You can say why Tectorwise wins Q9 despite running more instructions and taking more cache misses, and name the counter that shows it.

  <details><summary>Answer</summary>

  Because identical miss counts do not mean identical miss costs. Both
  engines use the same hash table and take almost the same number of LLC
  misses (Table 1 Q9: 0.47 vs 0.46), but §4.1 explains that Tectorwise's
  probe loop "is only a simple loop… the CPU's out-of-order engine can
  speculate far ahead and generate many outstanding loads", while Typer's
  fused loop carries scan, selection, probe and aggregation, so the
  out-of-order window "fills up more quickly… thus they generate less
  outstanding loads".

  The counter is memory stall cycles. §4.4's SSB Q4.1 row is the cleanest
  case: Tectorwise runs 1.56× the instructions and takes 1.39× the L1
  misses, and finishes in 0.76× the cycles, because it stalls 19.48
  cycles per tuple against Typer's 45.91. The advantage grows with the
  table — §4.1 measures it "up to 40% for larger data (and hash table)
  sizes".

  </details>

- [ ] You can explain why a higher IPC does not mean a faster engine, using the paper's own example.

  <details><summary>Answer</summary>

  Table 1, Q1: Tectorwise's IPC is 2.8 against Typer's 2.0 — 40% higher —
  and it is 74% slower, because it executes 162 instructions per tuple
  against 68. Retiring more instructions per cycle is worthless if the
  extra instructions are load/stores you would not have issued in the
  other design.

  §4.1's own conclusion: IPC "is a valid measure of the amount of free
  processing resources, but should not be used as the sole proxy for
  overall query processing performance". Two neighbouring guides make the
  same point from other directions — [reading-x100.md](reading-x100.md),
  where a low IPC is diagnostic of interpretation, and this repo's
  FINDINGS row 17, where the branchless filter wins on work done, not on
  instructions retired.

  </details>

- [ ] You can list the operational dimensions that do *not* shrink with better hardware, and say which one decides M11.

  <details><summary>Answer</summary>

  §8's five: OLTP/stored procedures and multi-language support (both to
  compilation); compile time, profiling and adaptivity (all three to
  vectorization); implementation effort a wash with different shapes —
  codegen indirection versus per-primitive constraints. These are
  structural consequences of the architecture, so unlike the performance
  column they do not move when you add cores. The performance column
  does: Table 3 shows the gap narrowing at 20 hyper-threads for four of
  five queries, Q9's Tectorwise advantage cut roughly in half.

  For M11 the deciding pair is the memory-bound row and compile time.
  Graph pattern-matching is probes and expands over a structure larger
  than cache, which is the regime where §4.1 puts vectorization ahead;
  and vectorization's zero compile time plus §8.3's per-primitive
  profiling are worth more to a project without an LLVM budget than a
  ≤32% win on the queries that would have gone the other way. Topic 19
  reopens the question with JIT in hand.

  </details>

## References

**Papers**
- Kersten, Leis, Kemper, Neumann, Pavlo, Boncz — "Everything You Always
  Wanted to Know About Compiled and Vectorized Queries But Were Afraid
  to Ask" (VLDB 2018) — ~1.5 h. §4.1 with Table 1 and §8 are the two
  sections to internalize; §3's two caveats (compilation time excluded,
  five queries) govern how you may quote everything else

**In this repo**
- [reading-x100.md](reading-x100.md) — the vectorized contender's origin,
  and the 2.3× it left on the table that compilation went after
- [reading-morsel-parallelism.md](reading-morsel-parallelism.md) — the
  parallelization framework §6 gives to both engines so it stops being a
  variable
- [reading-duckdb-execution.md](reading-duckdb-execution.md) — a
  production vectorized engine, choosing the §8 column deliberately
- [FINDINGS.md](../../FINDINGS.md) row 11 — the interpretation tax both
  models exist to remove, measured here; row 17 for the branchless-filter
  counterpart to Step 6's IPC warning
