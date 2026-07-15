# Compiled vs vectorized: the fair fight ends in a near-tie

Kersten et al. (VLDB '18) built BOTH engines — Typer (HyPer-style
data-centric compilation) and Tectorwise (X100-style vectorization) —
sharing everything else, then raced them. Fair benchmarking (topic 0
discipline) applied to the execution-model war. Before the paper, this
chapter builds the two contenders and the two hardware effects that
decide every round — registers versus intermediates, and cache-miss
overlap — step by step; the residual differences, not the headline
winner, are what decide M11 and M19.

## The problem in one sentence

By 2018 both modern execution models claimed to have killed
interpretation overhead — the question is what separates them once the
100× interpretation tax is gone, and the answer turns out to be
second-order hardware effects worth "only" 2–3× per operator, plus
everything operational.

## The concepts, step by step

### Step 1 — the common enemy: interpretation overhead

Both models exist to kill the same cost. A classic (Volcano-style)
engine processes one row at a time through a tree of operators, paying
per row: an indirect function call per operator, plus walking the
expression tree (`f < 50` evaluated by recursing over plan-time
objects). That overhead is ~20–100 ns per row while the useful work (a
compare, an add) is ~1 ns — the engine spends >90% of its time deciding
what to do rather than doing it. Both contenders eliminate this, by
opposite means: amortize it over a batch, or compile it away entirely.

### Step 2 — vectorized execution: interpret once per thousand rows

The vectorized model (X100 lineage — see reading-x100.md) keeps an
interpreter, but each operator call processes a **vector** (a batch of
~1000–2048 values of one column, stored as a plain array) instead of one
row. The query becomes a sequence of **primitives** — precompiled,
branch-free loops like `filter_lt(f_vec, 50)` — each doing one simple
operation over the whole vector. Interpretation still happens, but once
per vector: the ~100 ns dispatch cost divides by 2048 rows ≈ 0.05 ns/row,
while the loops themselves are simple enough for the compiler to
auto-vectorize (emit SIMD — single instructions operating on multiple
values at once). The price: each primitive writes its result to an
intermediate array for the next primitive to read — memory traffic that
Step 3's model avoids.

### Step 3 — compiled execution: fuse the pipeline into one loop

The compiled model (HyPer lineage) deletes the interpreter: at query
time, generate machine code (**JIT** — just-in-time compilation) that
fuses each **pipeline** (a chain of operators between materialization
points, e.g. scan→filter→aggregate) into ONE loop, in which the row's
values live in CPU **registers** (the ~16 named storage slots inside the
core — zero-latency, but scarce) from scan to sink. No calls, no
intermediates, no dispatch. Here are both models on one query,
`SELECT k, SUM(v) FROM t WHERE f < 50 GROUP BY k`:

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

The price of compilation: generating and compiling that loop takes
100s of milliseconds (LLVM), paid before the first row moves.

### Step 4 — the fair fight: build both, share everything else

Prior comparisons raced whole systems (HyPer vs Vectorwise), where
storage formats, hash tables, and compilers all differ — attribution
impossible. This paper's method: implement Typer and Tectorwise with the
**same algorithms and same data structures**, differing ONLY in loop
structure (Step 2's four loops vs Step 3's one), then run TPC-H. That's
what makes the comparison fair — the topic 0 discipline of changing one
variable. Headline result: **nearly tied** — TPC-H geometric mean within
~10–20%. The 100× war of X100-vs-MySQL is over; both models kill
interpretation (Step 1). Everything interesting is in where they
*differ* — Steps 5–7.

### Step 5 — memory-level parallelism: why vectorized wins hash probes

**Memory-level parallelism** (MLP — a modern core's ability to have ~10
cache misses in flight simultaneously, making 10 overlapped misses cost
about as much as one) is the deciding hardware effect for memory-bound
operators. A vectorized probe hashes 2048 keys in loop 2, then issues
2048 independent hash-table lookups in loop 3 — the out-of-order core
overlaps many misses at once. The compiled fused loop handles one row
end-to-end: its single probe miss must resolve before the row finishes,
so it has ONE miss in flight — unless you contort the loop with software
prefetching (manually issuing "fetch this address" hints ahead of use;
they cite group prefetching / AMAC). **Hash join probe is the great
equalizer**: both models end up memory-bound on the HT's random
accesses, with Tectorwise slightly ahead because batching misses is its
natural shape. This is the same MLP lesson as topic 0's
lookup_shootout.

### Step 6 — registers vs intermediates: why compiled wins expressions

The opposite case: compute-heavy work. In the fused loop a value loaded
once stays in registers through every operator that touches it; in the
vectorized engine every primitive boundary is a store of the whole
result vector + a load by the next primitive. For an expression-heavy
query (or a "wide" pipeline carrying 10 columns through 3 operators),
that's dozens of extra loads/stores per row — Tectorwise's registers
went to array bookkeeping (question 3 below). So **compilation wins**:
expression-heavy work, wide pipelines, and OLTP-style point work (no
per-vector setup cost amortizable over 3 rows). A related sobering
finding: explicit SIMD gained less than hoped — most operators are
memory-bound (Step 5's regime), and SIMD only helps compute-bound
primitives.

### Step 7 — the operational column: everything that isn't rows/second

The differences that decide real deployments are not in the inner loop:

- **compile latency** — Tectorwise starts in ~0 ms; Typer pays 100s of
  ms of LLVM per query (deadly for short queries and for interactive
  use).
- **profiling** — perf on Tectorwise shows time per named primitive;
  compiled code is one opaque JIT blob.
- **adaptivity** — a vectorized engine can swap a primitive mid-query
  (e.g. switch filter implementation when selectivity shifts); compiled
  code must recompile.
- **engineering** — no LLVM dependency vs hundreds of hand-written
  kernels. DuckDB chose vectors partly on exactly these grounds.

The scorecard:

| dimension | compiled (Typer) | vectorized (Tectorwise) |
|---|---|---|
| computation-heavy | **wins** (registers) | loses (intermediates) |
| memory-bound (probes) | loses (1 miss in flight) | **wins** (miss overlap) |
| compile latency | 100s of ms (LLVM) | **zero** |
| profiling/debugging | opaque blob | **per-primitive** |
| adaptivity | recompile | **swap primitives** |
| implementation effort | LLVM dependency, codegen bugs | 100s of kernels |

Topic 19 revisits compilation; M11 goes vectorized.

## How to read the paper (with the concepts in hand)

~1.5 h. The scorecard sections matter more than the geometric means.

- **§1–2** — the two models (Steps 2–3) and the shared-everything-else
  methodology (Step 4). Verify the fairness claims: same hash table,
  same storage.
- **§3 (micro-architectural analysis) — read carefully.** This is
  Steps 5–6 measured: cache misses in flight, instructions per cycle,
  loads/stores per row. The hash-probe and expression subsections are
  the paper's core.
- **§4 (SIMD)** — the smaller-than-hoped gains; note *which* primitives
  benefit (compute-bound only).
- **§5 (other factors) — don't skip.** Step 7 lives here: compile time,
  profiling, adaptivity. For choosing an architecture, this section
  outweighs the benchmarks.
- **§6–7** — related work and summary; skim, then re-read the scorecard
  and argue with it.

## Questions for notes.md

1. Why does vectorized probing overlap misses but the compiled loop
   doesn't? Connect to lookup_shootout (topic 0): what did MLP do for
   HashMap throughput there?
2. Software prefetching rescues compiled probes (they cite group
   prefetching / AMAC). Why is prefetching EASY in a vectorized kernel
   (you have the whole vector of hashes) and CONTORTED in a fused loop?
3. The "wide pipeline" case: 10 carried columns through 3 operators —
   count the loads/stores per row for each model. Where did Tectorwise's
   registers go?
4. Your kernels.rs is a HAND-compiled Typer pipeline for one fixed query.
   Predict from the paper: will it beat your vectorized.rs on the
   filter+sum workload (compute-bound, k dense)? By how much?
5. M11 (and topic 19's JIT milestone): FalkorDB queries are
   pattern-matching heavy — probes and expands, memory-bound. Which
   column of the scorecard do graph workloads live in, and what does
   that say about JIT priority for M19?

## Done when

You can argue BOTH sides for a graph engine in 3 sentences each, then
commit to one (spoiler: the scorecard's memory-bound row + operational
column point vectorized for M11; revisit at topic 19).

## References

**Papers**
- Kersten, Leis, Kemper, Neumann, Pavlo, Boncz — "Everything You Always
  Wanted to Know About Compiled and Vectorized Queries But Were Afraid
  to Ask" (VLDB 2018) — ~1.5 h; the scorecard sections matter more than
  the geometric means
