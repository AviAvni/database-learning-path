# Δ-stepping: the dial between Dijkstra and Bellman-Ford

Meyer & Sanders' paper put a DIAL between Dijkstra (perfect ordering,
zero parallelism) and Bellman-Ford (perfect parallelism, wasteful
work): bucket vertices by tentative distance and relax a bucket at a
time. This chapter derives the dial, then compares the two production
readings of it — gapbs's frontier version and LAGraph's algebraic
one — which our `sssp::delta_stepping` stub sits between.

## The dial

```
  Dijkstra:      settle ONE vertex at a time, strict order
                 → zero wasted relaxations, zero parallelism
  Bellman-Ford:  relax EVERYTHING, |V| rounds
                 → embarrassing parallelism, embarrassing waste

  Δ-stepping:    buckets of width Δ by tentative distance
                 bins[i] = { v : dist(v) ∈ [iΔ, (i+1)Δ) }
                 process buckets in order; INSIDE a bucket, relax in
                 parallel and re-relax until stable (light edges
                 w < Δ can re-insert into the current bucket)

  Δ→min_weight  ⇒ Dijkstra (every bucket ≤ 1 settle-round)
  Δ→∞           ⇒ Bellman-Ford (one bucket, all rounds inside it)
```

The bucket loop, in one screen:

```rust
fn delta_stepping(g: &Csr, src: u32, delta: u64) -> Vec<u64> {
    let mut dist = vec![u64::MAX; g.n]; dist[src as usize] = 0;
    let mut bins: Vec<Vec<u32>> = vec![vec![src]];        // bins[i] = [iΔ, (i+1)Δ)
    let mut i = 0;
    while i < bins.len() {
        while let Some(u) = bins[i].pop() {               // bucket i can REFILL
            let du = dist[u as usize];
            if du / delta < i as u64 { continue; }        // stale entry — skip
            for (v, w) in g.edges(u) {                    // relax; parallel-safe:
                let nd = du + w;                          //   min is idempotent
                if nd < dist[v as usize] {
                    dist[v as usize] = nd;
                    let b = (nd / delta) as usize;        // light edge ⇒ b == i:
                    bins.resize(bins.len().max(b + 1), vec![]);
                    bins[b].push(v);                      //   re-enters this bucket
                }
            }
        }
        i += 1;                                           // bucket i settled exactly
    }
    dist
}
```

The paper's analysis: for random weights and low-diameter graphs
there's a Δ giving near-linear work AND polylog depth. On road
networks (huge diameter, few vertices per distance band) the buckets
are nearly empty — no parallelism at any Δ. That's why GAP includes
road.

## The two implementations

| | gapbs sssp.cc | LAGraph LAGr_SingleSourceShortestPath.c |
|---|---|---|
| bucket | thread-local `vector` bins (:32-44), merged at sync points | `tmasked` sparse vector = current bucket (:100-142) |
| relax | explicit `RelaxEdges` with CAS-free benign races (:69-79) | one `GrB_vxm` with MIN_PLUS semiring (:151-185) per inner iteration |
| stale entries | left in old bins; skipped when drained (:44 — redundancy beats bookkeeping) | mask + select prune them algebraically |
| light/heavy split | skipped entirely (re-relax instead) | skipped too; `Delta` is a GrB_Scalar knob |

Same lesson as topic 20's BFS: the algebraic version is ~15 lines of
semiring calls and inherits parallelism from the runtime; the
frontier version owns its memory layout and wins constants.

MIN_PLUS is the whole algorithm: dist' = dist min.+ (dist ⊗ A) —
SSSP is matrix "multiplication" over the tropical semiring; Δ-buckets
are just a sparsity filter on which rows participate per step.

## Implementation traps (for the stub)

1. A vertex drained from bucket i whose dist has since improved
   below iΔ is STALE — skip it (our Dijkstra's `d > dist[u]` check,
   bucketed edition). Without this you still get right answers, but
   the relaxation counter lies.
2. `new_dist / delta` can exceed the bins vec — grow it lazily;
   don't precompute max_dist/Δ (you don't know max_dist yet).
3. Bucket i can refill while you drain it (light edges) — loop until
   bucket i is empty before moving to i+1, or you break the ordering
   invariant that makes answers exact.

## Questions (answer in notes.md)

1. Our RMAT has weights uniform 1..=255. Predict the
   relaxations-vs-Δ curve for Δ ∈ {16, 128, 1024, 2^40} against
   Dijkstra's 343K pops (fill the notes table BEFORE implementing).
2. Δ=1 with integer weights: exactly which Dijkstra do you get, and
   why is it still cheaper than a binary heap (hint: Dial's
   algorithm, O(1) bucket ops)?
3. Why do thread-local bins + benign write races (gapbs :32) not
   corrupt distances? What property of `min` makes the race safe —
   and which GraphBLAS concept is that (idempotent monoid)?
4. LAGraph does one vxm per INNER iteration — how does the number of
   vxm calls relate to (max_dist/Δ + reinsertions)? Where does the
   algebraic version pay that gapbs doesn't?
5. M24: FalkorDB's weighted-shortest-path today is `algo.SPpaths`/
   BFS-flavored. Sketch `CALL algo.sssp(src, 'weight', delta)` over
   the M20 core: which semiring, which vector becomes the bucket,
   and where does Δ live in the API?

## References

**Papers**
- Meyer & Sanders — "Δ-Stepping: A Parallelizable Shortest Path
  Algorithm" (J. Algorithms 2003) — the dial and its analysis;
  the road-network caveat is in the analysis sections

**Code**
- [gapbs](https://github.com/sbeamer/gapbs) `src/sssp.cc` — frontier
  version, thread-local bins; the :32-44 header comment explains why
  redundancy beats bookkeeping
- [LAGraph](https://github.com/GraphBLAS/LAGraph)
  `src/algorithm/LAGr_SingleSourceShortestPath.c` — the algebraic
  version: one MIN_PLUS `GrB_vxm` per inner iteration
