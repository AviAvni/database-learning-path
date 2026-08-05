# X100: the vectorization manifesto

MonetDB/X100 (CIDR '05) is where vectorized execution was born — from a
profiler, not a whiteboard. Twenty years old and it reads like the DuckDB
design doc — because it is (Boncz co-authored both; DuckDB came out of
the same CWI group). Before the paper, this chapter builds its argument
step by step: the profile that started it, the two failure modes it
threads between, the vector, the primitive, and the health metric — then
routes you through the sections.

Every number below is checked against the paper — Boncz, Zukowski, Nes,
*MonetDB/X100: Hyper-Pipelining Query Execution*, CIDR 2005 — and cited
to the section, table or figure it came from. Several figures this guide
used to carry did not survive that check; the corrections are called out
where they occur, because being wrong in a memorable way is how these
numbers propagate.

## The problem in one sentence

In 2005 a database evaluating TPC-H Q1 — a plain scan + filter +
arithmetic + group-by, no join — ran **121× slower than a hand-written C
loop over the same data** (Table 1: MySQL 4.1 at 26.6 s against
hand-coded at 0.22 s, same AthlonMP), and only 10% of that time was
computation: §3.1 finds the five operations doing the actual work account
for 10% of execution time, the rest being record navigation and
hash-table machinery.

## The concepts, step by step

### Step 1 — the profile: databases ran below 10% of the hardware

> **In:** a query so simple that no system can blame its optimizer —
> one scan, a 98%-selective filter, seven arithmetic expressions, and a
> group-by with four groups (§3).
> **Out:** four measured runtimes on one machine spanning a factor of
> 121, and a profile saying where the missing time went. Everything after
> this step is an attempt to close that gap.

The paper opens with measurement, not design. Q1 was chosen because "all
database systems operate on a level playing field and mainly expose their
expression evaluation efficiency" (§3): it is a scan of SF×6M `lineitem`
rows, selecting SF×5.9M of them, computing two column-to-constant
subtractions, one addition, three column-to-column multiplications, and
eight aggregates over just four group combinations — small enough that
"accessing the hash-table" costs no cache misses.

That 98% is not incidental, and this repo has measured why. `exec_bench`'s
Volcano lane ([FINDINGS.md](../../FINDINGS.md) row 11) sweeps selectivity
over 50M rows and finds the model *slowest* at the high end — 74.7 M
rows/s at 95% selectivity against 129.4 M at 5% — because a rejected row
costs one `next()` call inside the filter loop while a survivor pays a
second `next()` up the chain plus the aggregate's work
([notes.md](notes.md)). Q1 is therefore near the worst case for a
tuple-at-a-time engine, which is precisely what makes it a good
microbenchmark: the per-tuple tax is at full strength.

Table 1, restricted to the one machine that makes the comparison honest
(AthlonMP 1533 MHz, SF=1, 1 CPU):

```
 Table 1 (AthlonMP 1533MHz, SF=1):
   hand-coded C UDF (§3.3)     0.22 s   <- the roofline
   MonetDB/X100                0.50 s   <- 2.3× off it
   MonetDB/MIL                 3.7  s   <- full-column materialization
   MySQL 4.1                  26.6  s   <- tuple-at-a-time interpretation
   "DBMS X" (commercial)      28.1  s

 the gaps the paper's argument turns on:
   26.6 / 0.22 = 121×   engine tax over hand-written C
   26.6 / 3.7  =  7.2×  what column-at-a-time alone buys
   0.50 / 0.22 =  2.3×  what X100 still leaves on the table
```

**Correction:** this guide previously printed ~0.6 s for both the
hand-coded loop and X100, and called the gap 45×. All three are wrong.
The hand-coded UDF is 0.22 s (§3.3, "a stunning 0.22 seconds"), X100 is
0.50 s, and §3.3's own summary of the gap is that X100 "is able to get
within a factor 2 of this hand-coded implementation" — it does *not*
reach the roofline. The abstract's claim is "between one and two orders
of magnitude higher than previous technology", which the 121× and 7.2×
above bracket.

A **roofline** is the hardware's actual capacity for a given
computation — here, a hand-written loop over the same arrays. Everything
above it is engine tax, and §3.1's gprof trace of MySQL says what the tax
is spent on:

```
 Table 2 — MySQL 4.1 gprof trace of Q1, SF=1 (MIPS R12000):
   the five "work" operations (+, -, *, SUM, AVG)   10% of time
   creation and lookup in the aggregation hash table 28%
   record navigation (rec_get_nth_field and friends) 62%

 Item_func_plus::val:  38 instructions per addition, IPC 0.80
   the paper's own division:  38 / 0.8            = 49 cycles per add
   what the same machine can do:                  =  3 cycles per multiply
                                                    (3 int/fp + 1 ld/st per cycle)
   ratio                                          = 16×
```

§3.1 then explains the 49 cycles rather than merely reporting them: a
double addition is four dependent RISC instructions (two loads, an add, a
store) at ~5 cycles of latency each, and because the routine performs
exactly one addition per call, the compiler cannot pipeline the loop —
"empty pipeline slots must be generated (stalls) to wait for the
instruction latencies, such that the cost of the loop becomes 20 instead
of 3 cycles". The remaining ~29 cycles are the call itself: "the cost of
the routine call (in the ballpark of 20 cycles) must be amortized over
only one operation, which effectively doubles the operation cost."

That last sentence is the whole thesis in one line. Both halves of the
cost — the un-pipelined dependent chain *and* the unamortized call — are
consequences of the same decision, which Step 2 names.

The health metric is **IPC** (instructions per cycle: how many
instructions the core actually retires per clock). §2 reports that
"query execution in commercial DBMS systems get an IPC of only 0.7",
against scientific computation extracting "average IPCs of up to 2".
**Correction:** this guide previously said a superscalar core of that era
could sustain "3+" and that X100 achieved ~2 IPC. Neither is in the
paper. Three is the R12000's *issue width* for int/fp ops (§3.1), not a
sustained rate; and the paper reports no IPC figure for X100 at all — its
X100 measurements are in cycles per tuple (Step 6).

### Step 2 — failure mode one: tuple-at-a-time (Volcano)

> **In:** the 26.6 s and the 49-cycle addition from Step 1, which need a
> cause rather than a scapegoat.
> **Out:** the cause — an expression interpreter whose granularity is one
> tuple — and the two independent penalties that follow from it.

The **Volcano** or **iterator model** composes operators as a tree in
which each exposes `next()`, and each call returns **one tuple**
(see [reading-postgres-executor.md](reading-postgres-executor.md) for
this model still in production). §3.1 derives the cost from the model's
generality rather than from any implementation flaw: a `ScanSelect(R, b, P)`
learns the shape of `R`, the predicate `b` and the projections `P` only
at query time, so "DBMS implementors must in fact implement an expression
interpreter that can handle expressions of arbitrary complexity", and
"one of the dangers of such an interpreter, especially if the granularity
of interpretation is a tuple, is that the cost of the 'real work' … is
only a tiny fraction of total query execution cost."

The two penalties §3.1 lists are worth keeping separate, because
vectorization fixes them by different mechanisms:

```
 penalty 1 — no loop pipelining.   One addition per call means the
   compiler cannot software-pipeline; four dependent instructions at
   ~5 cycles latency stall into ~20 cycles instead of ~3.
 penalty 2 — unamortized call.     ~20 cycles of call overhead divided
   by one operation.
 total                             ~49 cycles per addition (measured,
                                   Table 2: 38 instructions at IPC 0.80)
```

Penalty 2 is the one everybody quotes; penalty 1 is the larger surprise,
because it is not overhead at all — it is the *same* arithmetic, run
badly, because the compiler was denied the loop it needed.

### Step 3 — failure mode two: full-column-at-a-time (old MonetDB)

> **In:** the obvious cure for Step 2 — stop interpreting per tuple by
> making the unit an entire column.
> **Out:** MonetDB/MIL, which does exactly that, has no interpretation
> problem at all, and is still 17× off the roofline — because it moved
> the bottleneck from the CPU to memory rather than removing it.

MonetDB — the authors' own previous system — stores each column as a
BAT (Binary Association Table) and evaluates in a column algebra, MIL,
whose operators "always consume a number of materialized input BATs and
materialize a single output BAT" (§3.2). **Materialization** is that
last step: writing a complete intermediate column to memory for the next
operator to read back.

§3.2 establishes the diagnosis by a beautiful experiment: rerun the same
plan at SF=0.001, so every column and intermediate fits in cache.
"MonetDB/MIL then becomes almost twice as fast" — the work did not
change, so the missing time was memory traffic. Table 3's own columns
make it arithmetic:

```
 Table 3 (20 MIL invocations spanning >99% of Q1, AthlonMP, SF=1):
   total measured time                                    3724 ms
   sum of the per-operator MB column (inputs + outputs)   1361 MB
   sustained bandwidth the paper reports MIL stuck at      500 MB/s
      "the maximum bandwidth sustainable on this hardware"

   1361 MB / 500 MB/s                                     = 2.72 s
   2.72 s / 3.724 s                                       = 73% of the query

 in cache at SF=0.001 the same operators exceed          1.5 GB/s
```

**Correction:** this guide previously described "~10 intermediates" and
"hundreds of MB". The trace shows 20 MIL invocations and 1361 MB — more
than a gigabyte of DRAM traffic to answer a query whose result is four
rows.

The single multiply makes the failure vivid. §3.2 works it out: at
500 MB/s, `[*]()` moving 16 bytes in and 8 out manages 20M tuples/s,
"thus 75 cycles per multiplication on our 1533MHz CPU, which is even
worse than MySQL" — worse than the 49 cycles of the model this design was
supposed to beat. Interpretation was cured and bandwidth killed it
instead.

### Step 4 — the vector: small enough for cache, big enough to amortize

> **In:** two failure modes at opposite extremes of one dial — the unit
> of work, at 1 tuple and at a whole column.
> **Out:** the dial turned to ~1000, and the two constraints that pin it
> there from opposite sides. The paper measures the whole dial, which is
> what makes this a result rather than a preference.

X100 keeps Volcano's pipelining but changes the payload: each `next()`
returns a **vector** — a plain array of values of one column, "e.g. 1000
values" (§4.1.1). Two constraints, in §5.1.1's own words: "Preferably,
all vectors together should comfortably fit the CPU cache size, hence
they should not be too big. However, with really small vector sizes, the
possibility of exploiting CPU parallelism disappears. Also, in that case,
the impact of interpretation overhead in the X100 Algebra `next()`
methods will grow."

Both ends are measured (Figure 10, Q1 on Itanium2 and AthlonMP, vector
size swept from 1 to 4M). The paper's findings, quoted rather than
paraphrased: the default is **1024**; "the optimal vector size seems to
be 1000, but all values between 128 and 8K actually work well"; and at
the far end, "at the extreme vector size of 4M tuples, MonetDB/X100
behaves very similar to MonetDB/MIL". The curve is U-shaped, and its two
walls are exactly Steps 2 and 3.

The best part is that §5.1.1 tells you *where* the right-hand wall is and
the arithmetic checks out:

```
 §5.1.1: "The total width of all vectors used in Query 1 is
          just over 40 bytes."

 AthlonMP, combined L1+L2 = 320 KB (the paper's figure):
   8K  × 40 B = 327,680 B = 320 KB   ← exactly where degradation starts
   4K  × 40 B = 163,840 B = 160 KB   ← comfortably inside
 Itanium2, 16 KB L1 / 256 KB L2 / 3 MB L3:
   256 × 40 B =  10,240 B  = 10 KB   ← inside L1
   64K × 40 B = 2,621,440 B = 2.5 MB ← the edge of L3, and §5.1.1 says
                                       the decline runs "until data does
                                       not fit even in L3 (after 64K × 40 bytes)"
```

Vector size is therefore a **cache parameter**, not a tuning constant:
the right value is whatever makes (vector length × total vector width in
flight) fit the cache you have. That is why X100's 1024 and DuckDB's 2048
([reading-duckdb-execution.md](reading-duckdb-execution.md)) are the same
decision on different hardware — DuckDB's chunks are wider per row, and
its L1 is larger. Sweep it in `exec_bench` at 1 / 64 / 1024 / 64K and the
shape should reappear.

And the left-hand wall is Step 2 measured on X100 itself: "Just like
MySQL, interpretation overhead also hits MonetDB/X100 strongly if it uses
tuple-at-a-time processing (i.e. a vector size of 1)." The model is not
magic; it is a dial, and 1 is the setting that makes it MySQL.

### Step 5 — primitives: interpretation happens per vector, work per value

> **In:** an operator that has been handed a vector and must now do
> arithmetic on it.
> **Out:** a **primitive** — a precompiled, type-specialized loop chosen
> once at plan time — plus the two design consequences that come with it:
> a selection-vector convention that avoids copying, and a combinatorial
> explosion handled by code generation.

Inside `next()`, the work is done by primitives. §4.2 prints one, and it
is short enough to read whole:

```
 §4.2, the generated code for vectorized floating-point addition:

   map_plus_double_col_double_col(int n,
       double*__restrict__ res,
       double*__restrict__ col1, double*__restrict__ col2,
       int*__restrict__ sel)
   {
     if (sel) {
       for(int j=0;j<n; j++) { int i = sel[j];
                               res[i] = col1[i] + col2[i]; }
     } else {
       for(int i=0;i<n; i++)  res[i] = col1[i] + col2[i];
     }
   }
```

**Correction:** this guide previously named the primitive
`map_add_int_vec_int_vec`. The real naming scheme is
`map_<op>_<type>_<col|val>_<type>_<col|val>`, as the trace in Table 5
confirms (`map_mul_flt_col_flt_col`, `map_sub_flt_val_flt_col`,
`select_lt_date_col_date_val`).

Three things to take from those ten lines.

**`__restrict__` is load-bearing.** §3.3 notes that the hand-coded
baseline passes `__restrict__` pointers "such that the C compiler knows
that they are non-overlapping. Only then can it apply loop-pipelining!"
The primitives get the same treatment, which is how they recover Step 2's
penalty 1 — not by removing overhead but by giving the compiler back the
loop it needs.

**The `sel` parameter is where selection vectors enter the world.** "All
X100 vectorized primitives allow passing such selection vectors. The
rationale is that after a selection, leaving the vectors delivered by the
child operator intact is often quicker than copying all selected data
into new (contiguous) vectors" (§4.2). Note precisely what the loop does
with it: it reads `col1[i]` and writes `res[i]` at the *same* index —
§4.1.1 says the results are written "at the same positions in the output
vector as they were in the input one", and the selection vector is then
propagated onward to the aggregate. It does **not** compact. This guide's
earlier Rust sketch wrote survivors to `out[0..n]`, which is the opposite
convention and would have forced exactly the copy §4.2 is avoiding.

**The cost is combinatorics, paid by a generator.** "X100 contains
hundreds of vectorized primitives. These are not written (and maintained)
by hand, but are generated from primitive patterns" (§4.2) — a pattern
like `any::1 +(any::1 x, any::1 y) plus = x + y` plus a file of requested
signatures (`+(double*, double*)`, `+(double, double*)`, …). That is the
C++ template trick with a makefile instead of a compiler, and question 3
asks what the Rust equivalent costs.

§4.2 also names the ceiling that Step 1's 2.3× gap sits against, and this
is the paper's most under-quoted paragraph. A simple binary primitive is
**load/store bound**: "for simple 2-ary calculations, each vectorized
instruction requires loading two parameters and storing one result (1
work instruction, 3 memory instructions). Modern CPUs can typically only
perform 1 or 2 load/store operations per cycle."

```
 per output value, a 2-ary primitive issues:
   1 arithmetic instruction + 3 memory instructions
 at 2 load/stores per cycle, memory is the binding constraint:
   3 / 2 = 1.5 cycles per value, whatever the ALU could have done

 compound primitives (e.g. /(square(-(double*,double*)), double*))
   keep intermediates in registers, loading and storing only at the
   edges of the expression graph — §4.2 measures them "often … twice as
   fast", and notes "this factor 2 is similar to the difference between
   MonetDB/X100 and the hand-coded implementation" (0.50 / 0.22 = 2.3)
```

So the hand-coded loop's remaining advantage is not mystery: it is one
fused expression, and X100's per-primitive boundaries force a store and
two loads that fusion would have kept in registers. That is the same
argument the 2018 shootout re-runs at book length
([reading-compiled-vs-vectorized.md](reading-compiled-vs-vectorized.md)).

### Step 6 — the discipline: measure cycles per tuple, not just seconds

> **In:** a runtime, which tells you *that* you are slow.
> **Out:** a per-primitive cycles-per-tuple trace, which tells you
> *which wall* you are against — and the specific numbers X100 hits, so
> you have something to compare your own kernels to.

X100 "implements detailed tracing and profiling support using low-level
CPU counters" (§5.1), and Table 5 is the output for Q1 on the Itanium2.
Read it against MySQL's Table 2 and MIL's Table 3:

```
 cycles per tuple for the same multiply, three systems, same query:
   MonetDB/MIL   75 cycles   (§3.2, derived from 500 MB/s)
   MySQL         49 cycles   (§3.1, 38 instructions at IPC 0.80)
   MonetDB/X100 2.2 cycles   (Table 5, map_mul_flt_col_flt_col)

 the rest of Table 5's range (Itanium2, SF=1):
   map_fetch (enum fetch-joins)   1.9 cycles/tuple
   select_lt                      3.0
   map_sub / map_add              2.3 / 2.4
   aggr_sum                       6.1-6.6
   aggr_count                     4.3

 bandwidth on the same multiply operator:
   MonetDB/MIL   500 MB/s (RAM-bound)
   MonetDB/X100  >7.5 GB/s on Itanium2, ~5 GB/s on AthlonMP (§5.1)
```

**Correction:** this guide previously claimed "X100 runs at ~2 IPC where
MySQL managed 0.7". The 0.7 is real (Table 2, §2). The 2 is not an X100
measurement — §2 offers it as what *scientific computing* achieves.
Replace the comparison with the one the paper actually makes: 2.2 cycles
per multiply against 49, which §5.1 states as "way better than the 49
cycles per tuple achieved by MySQL".

The methodological lesson survives the correction intact, and is the
transferable part: a runtime is a scalar with no diagnosis in it. Cycles
per tuple, IPC, cache misses and branch misses tell you which wall you
are against — interpretation (low IPC, high branch count), bandwidth (low
IPC, high miss count, and the SF=0.001 experiment as the confirming
test), or genuinely compute-bound (high IPC: stop optimizing dispatch).
§3.2's cache-resident rerun is the cleanest example of the discipline in
the paper: change only the working-set size, and the hypothesis proves
itself.

## How to read the paper (with the concepts in hand)

~1 h. The TPC-H Q1 profile (Tables 1-3) and the vector-size sweep
(Figure 10 with §5.1.1's cache arithmetic) are the two things to
internalize.

| Section | What is there | Step |
|---|---|---|
| §1-2 | the problem, and a 2005 super-scalar CPU tutorial. Dated in constants, current in structure — skim if topic 0 is fresh. The IPC 0.7-vs-2 framing is at the end of §2 | 1 |
| §3 intro | why Q1: 6M rows, 98% selective, 4 groups, no join, so systems expose only expression evaluation | 1 |
| §3.1 + Table 1, 2 | **read carefully.** The four runtimes; the 10 / 28 / 62% breakdown; 38 instructions and 49 cycles per addition, and *why* | 1, 2 |
| §3.2 + Table 3 | MIL's bandwidth wall; the SF=0.001 cache-resident rerun; 500 MB/s; 75 cycles per multiply | 3 |
| §3.3 + Figure 4 | the hand-coded UDF: 0.22 s, `__restrict__`, and "within a factor 2" | 1, 5 |
| §4.1.1 + Figure 6 | the worked pipeline — watch the selection vector propagate from Select to Aggr without the data being copied | 5 |
| §4.2 | the generated primitive, the `sel` convention, the pattern generator, and the load/store ceiling on 2-ary primitives | 5 |
| §5.1 + Table 5 | cycles per tuple per primitive; 2.2 for multiply; >7.5 GB/s | 6 |
| §5.1.1 + Figure 10 | **read carefully.** The U-curve: default 1024, 128-8K all fine, 40 bytes of vector width per tuple, 8K × 40 B = the AthlonMP's 320 KB | 4 |
| §6 | related work; the DSM/NSM storage discussion feeds topic 12 | — |

## Takeaway

The paper is an argument by elimination on a single dial. Turn the unit
of work to one tuple and you get MySQL: 26.6 s, 49 cycles per addition,
90% of the time spent deciding what to do. Turn it to a whole column and
you get MonetDB/MIL: 3.7 s, no interpretation problem at all, and 1361 MB
of DRAM traffic pinned at the machine's 500 MB/s ceiling — 75 cycles per
multiply, worse than the model it replaced. Turn it to about a thousand
and both terms disappear at once, because the call amortizes *and* the
compiler gets a loop it can pipeline: 0.50 s, 2.2 cycles per multiply.

Two things are worth carrying past the constants. First, the right vector
size is derived, not chosen — §5.1.1's "just over 40 bytes" times 8K is
the AthlonMP's 320 KB of cache, and that is the whole rule. Second, X100
did not reach the roofline, and §4.2 says why in a sentence about
load/store ports: a per-primitive boundary costs a store and two loads
that a fused loop keeps in registers. That unfinished 2.3× is the opening
the compiled-execution literature walks through thirteen years later.

## Questions for notes.md

1. Reproduce the arithmetic: 8-col chunk of 8-byte values — what vector
   length keeps 3 operators' intermediates inside your M-series L1
   (128 KB data)? Does DuckDB's 2048 fit? (§5.1.1 does the same sum with
   40 bytes of width against 320 KB.)
2. Full-column MonetDB dies of bandwidth. Table 3 measures 1361 MB moved
   and 500 MB/s sustained = 2.72 s of the 3.724 s query. Redo it for your
   Mac: same 1361 MB against topic 12's measured ~50 GB/s — how many
   seconds, and what does that say about whether materialization is still
   the failure mode it was in 2005?
3. Primitives are monomorphized per type combination — the C++ template
   trick, or in X100's case a pattern file and a makefile (§4.2). What's
   the Rust equivalent, and what does it do to compile time / binary
   size? (You'll hit this writing kernels.rs.)
4. X100 pre-dates SIMD-everywhere: which of its wins does the compiler
   now deliver FREE via autovectorization of the primitive loops, and
   what still needs explicit `std::simd`? (Answer after writing
   kernels.rs — compare autovec asm vs your manual version. §4.2's
   load/store ceiling is the thing to check first.)

## Done when

Answer each before unfolding it.

- [ ] You can draw the U-curve from memory with both failure modes labelled, and say where the right-hand wall is *and why it is there*.

  <details><summary>Answer</summary>

  Figure 10 sweeps vector size from 1 to 4M for Q1. At size 1 the curve is
  at its worst — §5.1.1: "Just like MySQL, interpretation overhead also
  hits MonetDB/X100 strongly if it uses tuple-at-a-time processing" — and
  at 4M "MonetDB/X100 behaves very similar to MonetDB/MIL", the
  materialization wall. In between, "the optimal vector size seems to be
  1000, but all values between 128 and 8K actually work well"; the default
  is 1024.

  The right-hand wall is at a *computable* place, not an empirical one.
  Q1's vectors are "just over 40 bytes" of total width per tuple, so 8K
  vectors are 8192 × 40 B = 320 KB, which is exactly the AthlonMP's
  combined L1+L2. On the Itanium2 (16 KB L1, 256 KB L2, 3 MB L3) the
  decline starts earlier and runs "until data does not fit even in L3
  (after 64K × 40 bytes)" = 2.5 MB.

  </details>

- [ ] You can explain why vector size is a cache parameter rather than a tuning constant, and say what that implies about DuckDB's 2048.

  <details><summary>Answer</summary>

  Because the constraint that sets it is `vector length × total width of
  all vectors in flight ≤ cache`. Neither side of that inequality is a
  property of the query engine: the width comes from the query's columns,
  the cache from the chip. §5.1.1 derives 8K from 40 bytes and 320 KB, and
  had either number differed the answer would have moved with it.

  So X100's 1024 and DuckDB's 2048 are the *same* decision evaluated on
  different hardware, not a disagreement — DuckDB's chunks carry more
  bytes per row and its target L1 is larger (2048 × 64 B = 128 KB for
  eight 8-byte columns, against the AthlonMP's 320 KB shared between L1
  and L2). Anyone porting either constant without redoing the sum is
  copying an answer rather than a method.

  </details>

- [ ] You can state the two independent penalties tuple-at-a-time pays, and say which one is *not* overhead.

  <details><summary>Answer</summary>

  §3.1 separates them for the 49-cycle addition. Penalty 2 is the obvious
  one: the routine call costs "in the ballpark of 20 cycles" and is
  amortized over a single operation, which "effectively doubles the
  operation cost". Penalty 1 is the interesting one and is not overhead at
  all — with one addition per call the compiler cannot software-pipeline
  the loop, so the four dependent instructions (two loads, an add, a
  store) stall on ~5-cycle latencies and the arithmetic itself costs "20
  instead of 3 cycles".

  It matters because the two are fixed by different things. Batching fixes
  penalty 2 arithmetically. Penalty 1 is fixed only if the batched code is
  a loop the compiler can pipeline — which is why §3.3 and §4.2 both make
  a point of `__restrict__`.

  </details>

- [ ] You can say why MonetDB/MIL, which has no interpretation problem, was still 17× off the roofline — with the number that proves it.

  <details><summary>Answer</summary>

  Because every MIL operator materializes its output: it "always consume[s]
  a number of materialized input BATs and materialize[s] a single output
  BAT" (§3.2). Table 3's own MB column sums to 1361 MB of traffic for a
  query returning four rows, and MIL is "stuck at 500 MB/s, which is the
  maximum bandwidth sustainable on this hardware" — so 1361/500 = 2.72 s
  of the measured 3.724 s, about 73%, is memory movement.

  The proof is §3.2's control experiment rather than the arithmetic:
  rerunning the identical plan at SF=0.001, where everything fits in
  cache, makes MonetDB/MIL "almost twice as fast" and lifts the operators
  above 1.5 GB/s. Same instructions, less memory, large speedup — the
  bottleneck is located, not guessed. The single worst case is the
  multiply at 75 cycles per tuple, which §3.2 notes is "even worse than
  MySQL".

  </details>

- [ ] You can explain the `sel` argument every X100 primitive takes, and what the primitive does *not* do with it.

  <details><summary>Answer</summary>

  `sel` is a selection vector: an array of `n` selected positions produced
  by a Select operator. Every X100 primitive accepts one, and when it is
  non-NULL the loop iterates `sel` instead of `0..n`. §4.2 gives the
  rationale: "after a selection, leaving the vectors delivered by the
  child operator intact is often quicker than copying all selected data
  into new (contiguous) vectors."

  What it does *not* do is compact. The printed loop reads `col1[i]` and
  writes `res[i]` at the same index `i = sel[j]`, and §4.1.1 confirms the
  results are written "at the same positions in the output vector as they
  were in the input one", with the selection vector propagated onward to
  the aggregate. A primitive that wrote survivors densely into `res[0..n]`
  would have performed exactly the copy the convention exists to avoid.

  </details>

## References

**Papers**
- Boncz, Zukowski, Nes — "MonetDB/X100: Hyper-Pipelining Query
  Execution" (CIDR 2005) — ~1 h. Tables 1-3 (the three failure profiles),
  §4.2 (the primitive and its `sel` convention), Table 5 and Figure 10
  with §5.1.1 (cycles per tuple, and the vector-size U-curve derived from
  cache size) are the parts to internalize

**In this repo**
- [reading-duckdb-execution.md](reading-duckdb-execution.md) — the same
  design twenty years later, with `STANDARD_VECTOR_SIZE` where X100 has
  1024
- [reading-postgres-executor.md](reading-postgres-executor.md) — Step 2's
  failure mode, still shipping
- [reading-compiled-vs-vectorized.md](reading-compiled-vs-vectorized.md)
  — the 2.3× X100 left on the table, re-measured in 2018
- [FINDINGS.md](../../FINDINGS.md) row 11 — this repo's own
  tuple-at-a-time ceiling, and the direction it moves in
