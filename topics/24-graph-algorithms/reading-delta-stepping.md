# Δ-stepping: the dial between Dijkstra and Bellman-Ford

Meyer & Sanders' paper put a DIAL between Dijkstra (perfect ordering,
zero parallelism) and Bellman-Ford (perfect parallelism, wasteful
work): bucket vertices by tentative distance and relax a bucket at a
time. This chapter builds the dial step by step — relaxation, the two
extremes, the bucket machinery and its traps, and the algebraic
reading — then compares the two production implementations (gapbs's
frontier version and LAGraph's algebraic one) that our
`sssp::delta_stepping` stub sits between.

*Sources pinned (resources/codebases.md): gapbs @`b5e3e19`, LAGraph
@`e2539e2`; anchors re-checked with `tools/pinned-source.py`. The bound
and its preconditions are Meyer & Sanders (J. Algorithms 2003); the
343K-pop and timing figures are this topic's `notes.md`.*

## The problem in one sentence

Dijkstra settles exactly one vertex at a time, so on our 65,536-vertex
RMAT it is a strictly sequential chain of **343K heap pops** (measured:
342,909 pops across its 3 source runs, ~114K per source, notes.md) —
the answer is perfectly work-efficient and perfectly unparallelizable,
and Δ-stepping asks how much wasted work buys how much parallelism.

## The concepts, step by step

### Step 1 — SSSP and relaxation: the one move every algorithm shares

> **In:** a weighted graph and a source vertex.
> **Out:** the single primitive every SSSP algorithm is built from —
> *relaxing* an edge — and the definition of *wasted work* (a
> relaxation later overwritten by a better one).

Single-source shortest paths (SSSP) computes, for every vertex, the
minimum total edge weight of any path from one source vertex. Every
SSSP algorithm maintains a **tentative distance** per vertex — the
best path cost found *so far*, initialized to ∞ — and improves it by
**relaxing** edges: for an edge u→v of weight w, if
`dist[u] + w < dist[v]`, lower `dist[v]` to `dist[u] + w`. Relaxation
is the entire instruction set; algorithms differ only in *which edges
they relax, in what order*. A relaxation that later gets overwritten
by a better one is **wasted work** — that waste is the currency this
whole chapter trades in.

### Step 2 — Dijkstra: perfect order, zero parallelism

> **In:** relaxation from Step 1.
> **Out:** Dijkstra's rule (always relax out of the global-minimum
> vertex) — zero wasted relaxations, but a strictly sequential chain of
> priority-queue pops.

Dijkstra's rule is: always relax out of the unfinished vertex with the
smallest tentative distance. That vertex's distance can never improve
again (all other paths to it must pass through vertices at least as
far), so it is **settled** — final — and each edge is relaxed from a
settled source exactly once: zero wasted relaxations. The price is a
total order: the algorithm is a chain of "pop the global minimum"
operations (a priority-queue pop per vertex — our heap Dijkstra's 343K
pops), each depending on the last. Nothing can proceed in parallel,
because parallelizing would mean relaxing out of a not-yet-minimal
vertex — which Dijkstra's correctness argument forbids.

### Step 3 — Bellman-Ford: zero order, perfect parallelism

> **In:** relaxation from Step 1, with the ordering dropped.
> **Out:** Bellman-Ford's opposite trade — every edge relaxed in any
> order, |V| rounds, embarrassing parallelism bought with embarrassing
> re-relaxation waste.

Bellman-Ford drops the ordering entirely: relax *every* edge, in any
order, and repeat until no distance changes (at most |V| rounds).
Every relaxation within a round is independent — embarrassingly
parallel — but early rounds relax edges out of vertices whose
tentative distances are still wildly wrong, and all of that work is
redone in later rounds. On a graph where Dijkstra does m relaxations,
Bellman-Ford can do m × (number of rounds) — orders of magnitude of
waste, bought for the right to use every core.

### Step 4 — the dial: buckets of width Δ

> **In:** Dijkstra (Step 2) and Bellman-Ford (Step 3) as the two ends.
> **Out:** the dial between them — buckets of width Δ, Dijkstra order
> *between* buckets and Bellman-Ford freedom *within* one — plus its
> two degenerate endpoints (Δ→min = Dijkstra, Δ→∞ = Bellman-Ford).

Δ-stepping interpolates: group vertices into **buckets** by tentative
distance in bands of width Δ, process buckets in order, and inside a
bucket relax freely in parallel — accepting Bellman-Ford-style waste
*within* a Δ-band while keeping Dijkstra-style order *between* bands:

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

The dial's endpoints are the previous two steps exactly: Δ at the
minimum edge weight makes every bucket a singleton settle (Dijkstra);
Δ = ∞ makes bucket 0 the whole graph (Bellman-Ford). Everything in
between trades a measurable amount of re-relaxation for a measurable
amount of parallel width — our stub's stats expose exactly that trade.

### Step 5 — the bucket loop, and the three traps inside it

> **In:** the dial from Step 4.
> **Out:** the drain-until-empty bucket loop and its three
> implementation traps (stale entries, lazy bin growth, refill during
> drain).

The machinery fits on one screen; the subtleties are the loop
conditions. An edge of weight w < Δ (a "light" edge) can re-insert its
target *into the bucket currently being drained*, so a bucket must be
drained until empty, not iterated once:

```rust
// ILLUSTRATION — not quoted; the reader's own stub. The production
// frontier version is gapbs sssp.cc:87-116 (RelaxEdges at :69-85);
// ours lands in experiments/src/sssp.rs:45 (delta_stepping).
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

The implementation traps, numbered for the stub:

1. A vertex drained from bucket i whose dist has since improved
   below iΔ is STALE — skip it (our Dijkstra's `d > dist[u]` check,
   bucketed edition). Without this you still get right answers, but
   the relaxation counter lies.
2. `new_dist / delta` can exceed the bins vec — grow it lazily;
   don't precompute max_dist/Δ (you don't know max_dist yet).
3. Bucket i can refill while you drain it (light edges) — loop until
   bucket i is empty before moving to i+1, or you break the ordering
   invariant that makes answers exact.

Note that a vertex may sit in an out-of-date bucket rather than be
moved: leaving stale entries and skipping them on drain is *cheaper*
than precise bucket bookkeeping — the same lazy-deletion bet as a
binary-heap Dijkstra that never does decrease-key.

### Step 6 — where the dial fails: diameter

> **In:** the dial from Step 4 and the paper's analysis.
> **Out:** the exact bound Δ=Θ(1/d) achieves, the three preconditions
> it rests on, and the graph family (high diameter) that voids it.

The paper's analysis promises that for random edge weights on
low-diameter graphs (diameter = the longest shortest-path distance in
hops) there is a Δ giving near-linear total work AND polylog depth —
both ends of the trade at once. Precisely (Meyer & Sanders 2003,
Theorem 1 and Conclusions):

```
  Given max degree d (or a random graph G(n, d/n)) and RANDOM edge
  weights, the Δ = Θ(1/d)-stepping scheme runs in
      O(d·n)                 expected sequential work   (linear if d = O(1))
      O(log³n / log log n)   time (phases) whp

  It rests on THREE preconditions, all stated in the paper:
    1. weights uniform in [0,1] — used ONLY in Lemma 3: a length-ℓ
       path has total weight ≤ Δ with probability ≤ Δ^ℓ/ℓ!, so long
       light paths (the sequential-chain risk) are unlikely.
    2. bounded max degree d — Δ is tuned as Θ(1/d).
    3. low diameter — the longest settling chain is ℓ_max =
       O(log n / log log n) whp (Lemma 4); a big diameter makes ℓ_max
       large and the phase count blows up.
```

The promise dies on road networks: with a huge diameter, few vertices
share any given distance band, so the buckets are nearly empty at
*every* Δ — there is no parallelism for the dial to buy. That is
exactly why the GAP suite includes a road graph (see reading-gap.md,
Step 3): one graph family flips the SSSP ranking. (Our RMAT weights are
uniform 1..=255, not [0,1], so the paper's Δ=Θ(1/d) rescales by the
weight range — Q1 has you predict the actual relaxation curve.)

### Step 7 — the algebraic reading: SSSP is MIN_PLUS matrix multiplication

> **In:** the bucket loop from Step 5.
> **Out:** the algebraic reading — one relaxation round *is* one
> (min,+) matrix-vector product — and the two production shapes it
> maps to (gapbs's frontier bins, LAGraph's semiring vxm).

Replace ordinary (+, ×) arithmetic with (min, +) — the "tropical
semiring", where matrix multiply computes minimum path sums instead of
dot products — and one relaxation round over the whole graph becomes
one matrix-vector product: `dist' = dist min.+ (dist ⊗ A)`. In that
light, MIN_PLUS *is* the whole algorithm, and Δ-buckets are just a
sparsity filter on which entries of `dist` participate in each step.
The two production implementations are the two readings of this:

| | gapbs sssp.cc | LAGraph LAGr_SingleSourceShortestPath.c |
|---|---|---|
| bucket | thread-local `vector` bins (:32-44), merged at sync points | `tmasked` sparse vector = current bucket (:100-142) |
| relax | explicit `RelaxEdges` with a lock-free CAS retry loop (:69-85) | one `GrB_vxm` with MIN_PLUS semiring (:151-185) per inner iteration |
| stale entries | left in old bins; skipped when drained — a vertex is processed only if `dist[u] >= delta*curr_bin` (:110); the :41-44 header argues redundancy beats bookkeeping | mask + select prune them algebraically |
| light/heavy split | skipped entirely (re-relax instead) | skipped too; `Delta` is a GrB_Scalar knob |

Same lesson as topic 20's BFS: the algebraic version is ~15 lines of
semiring calls and inherits parallelism from the runtime; the
frontier version owns its memory layout and wins constants. gapbs is
not race-*free*: each write goes through `compare_and_swap`, and a
thread that loses the race rechecks `dist` and retries the loop
(sssp.cc:74-83). The algebraic reason that loop is safe — and that
re-relaxation across threads never corrupts a distance — is that `min`
is an idempotent, monotone monoid: writing a worse value, losing a
race, or relaxing twice only ever triggers one more `min`, never a
wrong answer.

## How to read the paper (with the concepts in hand)

- The algorithm definition (buckets, light/heavy edge split) is Steps
  4–5 — note that both production codes *skip* the light/heavy split
  the paper spends pages on, and re-relax instead: redundancy beats
  bookkeeping on real hardware.
- The analysis sections carry Step 6: the near-linear-work,
  polylog-depth result holds for random weights and low diameter —
  find the diameter dependence, it is the road-network caveat.
- Read the Δ endpoints (Step 4's dial) as the sanity check on every
  claim: any theorem should degenerate sensibly to Dijkstra at Δ→0
  and Bellman-Ford at Δ→∞.
- Then the two implementations: gapbs `src/sssp.cc` (the :32-44
  header comment first — it argues the lazy-deletion bet), then
  LAGraph's `LAGr_SingleSourceShortestPath.c:151-185` with Step 7's
  semiring translation in hand.

## Questions (answer in notes.md)

1. Our RMAT has weights uniform 1..=255. Predict the
   relaxations-vs-Δ curve for Δ ∈ {16, 128, 1024, 2^40} against
   Dijkstra's 343K pops (fill the notes table BEFORE implementing).
2. Δ=1 with integer weights: exactly which Dijkstra do you get, and
   why is it still cheaper than a binary heap (hint: Dial's
   algorithm, O(1) bucket ops)?
3. Why do thread-local bins + the lock-free CAS retry loop (gapbs
   sssp.cc:74-83) not corrupt distances? What property of `min` makes
   a lost or repeated relaxation safe — and which GraphBLAS concept is
   that (idempotent monoid)?
4. LAGraph does one vxm per INNER iteration — how does the number of
   vxm calls relate to (max_dist/Δ + reinsertions)? Where does the
   algebraic version pay that gapbs doesn't?
5. M24: FalkorDB's weighted-shortest-path today is `algo.SPpaths`/
   BFS-flavored. Sketch `CALL algo.sssp(src, 'weight', delta)` over
   the M20 core: which semiring, which vector becomes the bucket,
   and where does Δ live in the API?

## Done when

Answer each before unfolding it.

- [ ] You can state relaxation as the one move all SSSP algorithms share.
  <details><summary>Answer</summary>
  For an edge u→v of weight w, if dist[u]+w < dist[v] then lower dist[v]
  to dist[u]+w. Every SSSP algorithm is just a policy for *which* edges
  to relax and *in what order*; a relaxation later overwritten by a
  better one is wasted work.
  </details>
- [ ] You can explain the Dijkstra/Bellman-Ford trade as order against parallelism, and where Δ sits on that dial.
  <details><summary>Answer</summary>
  Dijkstra imposes a total order (relax out of the global minimum) — zero
  waste, zero parallelism. Bellman-Ford drops order — full parallelism,
  up to |V|× re-relaxation. Δ-stepping keeps Dijkstra order *between*
  Δ-width buckets and Bellman-Ford freedom *within* one, trading a
  measurable amount of re-relaxation for parallel width.
  </details>
- [ ] You can say exactly which algorithm Δ=1 gives you with integer weights.
  <details><summary>Answer</summary>
  Δ=1 puts each integer distance in its own bucket, so buckets are
  settled in strict increasing order — that is Dial's-bucket Dijkstra.
  It is cheaper than a binary heap because bucket insert/extract is O(1)
  (array indexing) rather than O(log n).
  </details>
- [ ] You can name the three traps inside the bucket loop and why a lost CAS is acceptable.
  <details><summary>Answer</summary>
  (1) skip stale entries whose dist has dropped below the current
  bucket's floor; (2) grow the bins vector lazily — you don't know
  max_dist/Δ in advance; (3) drain a bucket to empty, since light edges
  (w<Δ) can refill it. A lost CAS in gapbs (sssp.cc:74-83) just rechecks
  dist and retries; because `min` is an idempotent monotone monoid, the
  extra relaxation can never write a wrong final value.
  </details>
- [ ] You can write SSSP as MIN_PLUS matrix multiplication.
  <details><summary>Answer</summary>
  Over the tropical (min,+) semiring, one relaxation round is
  dist' = dist min.+ (dist ⊗ A): every vertex takes the min over its
  in-neighbours of dist[u]+w(u,v). Δ-buckets are a sparsity filter on
  which entries of dist participate in each vxm.
  </details>
- [ ] You wrote answers to all five questions in notes.md, and predicted a Δ for weights uniform in 1..=255 before running the lane against the measured Dijkstra oracle (notes.md: 33.7 ms for 3 sources, 342,909 heap pops).
  <details><summary>Answer</summary>
  Done when notes.md has your relaxations-vs-Δ prediction filled in
  *before* the run, plus the answers to Q2–Q5, and your prediction is
  compared against the measured Dijkstra baseline (33.7 ms / 342,909
  pops across 3 sources).
  </details>

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
