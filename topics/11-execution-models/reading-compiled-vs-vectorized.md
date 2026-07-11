# Reading guide — "Everything You Always Wanted to Know About Compiled and Vectorized Queries But Were Afraid to Ask" (VLDB '18) (~1.5 h)

Kersten et al. built BOTH engines — Typer (HyPer-style data-centric
compilation) and Tectorwise (X100-style vectorization) — sharing
everything else, then raced them. Fair benchmarking (topic 0 discipline)
applied to the execution-model war.

## The two models, one query

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

Same algorithms, same data structures — ONLY the loop structure differs.
That's what makes the comparison fair.

## Findings to internalize

- **Overall: nearly tied.** TPC-H geometric mean within ~10–20% of each
  other. The 100× war of X100-vs-MySQL is over; both models kill
  interpretation overhead. The remaining differences are second-order.
- **Compilation wins**: expression-heavy work (fused loop keeps
  everything in registers, no intermediate vectors), joins with many
  columns carried through ("wide" pipelines), OLTP-style point work
  (no per-vector setup cost).
- **Vectorization wins**: memory-bound operators (hash probes: vectorized
  code overlaps MANY cache misses at once — the MLP lesson from topic 0;
  compiled code's fused loop has ONE miss in flight unless you add
  software prefetching), SIMD applicability (isolated simple loops),
  and everything operational: compile time (ms vs 100s of ms per query),
  profiling (perf shows WHICH primitive; compiled code is one opaque
  blob), adaptivity (can swap primitive mid-query).
- **Hash join probe is the great equalizer**: both models end up
  memory-bound on the HT random accesses; Tectorwise slightly ahead
  because vectorized probing naturally batches misses.
- SIMD gains on modern cores were smaller than hoped: most operators are
  memory-bound; SIMD helps compute-bound primitives only.

## The scorecard

| dimension | compiled (Typer) | vectorized (Tectorwise) |
|---|---|---|
| computation-heavy | **wins** (registers) | loses (intermediates) |
| memory-bound (probes) | loses (1 miss in flight) | **wins** (miss overlap) |
| compile latency | 100s of ms (LLVM) | **zero** |
| profiling/debugging | opaque blob | **per-primitive** |
| adaptivity | recompile | **swap primitives** |
| implementation effort | LLVM dependency, codegen bugs | 100s of kernels |

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
