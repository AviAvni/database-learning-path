# Cardinality is the whole ballgame: the JOB audit

The humbling paper. Leis et al. (VLDB '15) built the Join Order Benchmark
(JOB) — 113 queries over IMDB, REAL correlated data instead of TPC-H's
synthetic uniformity — and audited every layer of the classical optimizer
stack. The verdict reorders this whole topic: cardinality error dwarfs
cost-model error dwarfs search-space limits.

## The experimental design (worth copying forever)

Factor the optimizer into its three claims and test each in isolation:

1. cardinality estimates — compare against TRUE cardinalities (computed
   offline) for every subplan;
2. cost model — feed it TRUE cardinalities, see if better cost = faster;
3. plan space — with perfect estimates, how much do bushy trees /
   exhaustive search matter?

Injecting ground truth at each layer isolates the blame. (This is the
fair-benchmarking discipline of topic 0, applied to a brain.)

## The findings to internalize

- **Cardinality is the whole ballgame.** Estimates degrade EXPONENTIALLY
  with join count: median q-error at 6 joins reaches 10²–10⁴ across all
  tested systems (postgres, and commercial A/B/C); underestimation
  dominates (independence assumption multiplies toward zero).

```rust
// the estimator every audited system runs, and why it under-shoots
fn estimate_join_card(tables: &[Table], preds: &[EquiPred]) -> f64 {
    let mut card: f64 = tables.iter().map(|t| t.rows as f64).product();
    for p in preds {
        card /= p.ndv_left.max(p.ndv_right) as f64;  // uniformity: 1/NDV
    }   // each predicate applied INDEPENDENTLY — on correlated data the
    card // true overlap is larger, so factors compound toward zero
}
```

- TPC-H hides this: uniform, independent, synthetic → estimates look
  fine. JOB's correlated real data (actors↔genres↔years) breaks them.
  **Benchmark data distribution is part of the benchmark.**
- **The cost model barely matters**: with true cardinalities, even a
  trivial cost model (they use Cout = sum of intermediate cardinalities)
  picks plans within ~2× of optimal. Cost-model tuning is polishing the
  wrong layer.
- **Plan space matters at the margins**: exhaustive beats greedy/quickpick
  meaningfully; bushy beats left-deep-only by ~10–40% on some queries.
  But all of it is noise next to cardinality error.
- Their pragmatic mitigations: prefer plans robust to misestimation
  (hash over nested-loop when unsure) — postgres's nested-loop
  catastrophes come from underestimates of 10⁴ feeding "it's only 3
  rows" decisions.

```
 error source        typical impact on runtime
 cardinality (6-way) 10×–1000× (catastrophic plans)
 cost model          ~2×
 search space        ~1.1×–1.4×
```

## Questions for notes.md

1. Why does independence UNDERestimate join sizes on correlated data?
   Construct a 2-table example where sel(a)×sel(b) is 100× low.
2. Cout (sum of intermediate sizes) as the whole cost model: which of
   your engines' knobs does that validate (DuckDB cost_model.cpp:40 is
   literally this)?
3. "Robust plans": hash join degrades linearly with a bad estimate,
   nested-loop quadratically. Frame it as a minimax decision — what's
   the regret matrix?
4. Design JOB-for-graphs: what's the correlated-data equivalent for
   Cypher patterns (degree skew × label correlation × triangle
   density)? Sketch 3 queries where independence-based nnz estimation
   (matrix-product size) blows up the same way. This is the M10/M22
   benchmark seed — write it down properly.

## Done when

You can rank cardinality/cost/search by measured impact, explain WHY
independence fails low, and have the graph-JOB sketch in notes.md.

## References

**Papers**
- Leis, Gubichev, Mirchev, Boncz, Kemper, Neumann — "How Good Are Query
  Optimizers, Really?" (VLDB 2015) — ~1.5 h; the methodology (§2–3) is
  worth as much as the findings — injecting ground truth per layer
  isolates the blame
