# CAGRA: HNSW rebuilt for warps

Topic 14's HNSW rebuilt from GPU-first principles: what does a
graph-traversal index look like when the executor is 32-wide warps
instead of one pointer-chasing core? This chapter builds the answer
step by step — what a proximity-graph search is, what SIMT hates
about HNSW, and the three fixes (flatten the levels, fix the degree,
move the visited set into shared memory) — a case study in making an
irregular algorithm regular enough for SIMT. The cuVS implementation
is the code half of this chapter.

## The problem in one sentence

HNSW's greedy walk is one long chain of dependent random loads with
variable-degree nodes — the exact shape a 32-lane lockstep warp
executes worst — yet CAGRA reaches the same recall with ~10× faster
index build and an order-of-magnitude more queries per second.

## The concepts, step by step

### Step 1 — proximity-graph ANN: search is a greedy walk

ANN (approximate nearest neighbor) search finds the k vectors
closest to a query without scanning all of them. Graph-based indexes
like HNSW (topic 14) connect each vector to a few dozen near
neighbors; search starts at an entry vertex and **greedily walks**:
compute distances to the current vertex's neighbors, move to the
closest unvisited one, repeat until no neighbor beats what you have.
Two supporting structures make it work: a **candidate list** of the
best vertices seen so far (HNSW's beam of width `ef` — wider beam,
better recall, more work), and a **visited set** so the walk never
scores the same vertex twice. HNSW additionally stacks sparse upper
**levels** (a skip-list-like hierarchy) to find a good entry point
in O(log n) hops.

### Step 2 — what SIMT hates about that walk

A warp is 32 threads executing one instruction in lockstep; it is
efficient only when all lanes do identical work on adjacent data.
Score HNSW against that executor, feature by feature — and CAGRA's
index is HNSW with each offending feature deleted:

```
 HNSW (topic 14)               CAGRA
 multi-level skip list         SINGLE flat level
 variable degree ≤ M           FIXED degree (e.g. 32) — no ragged
                               adjacency, no load balancing needed
                               (Gunrock's whole problem, deleted
                               by construction)
 greedy walk, 1 candidate      parallel walk, itopk candidate list,
 beam ef                       search_width parents expanded/iter
 visited: hash set on heap     visited: hashmap in SHARED MEMORY
```

Fixed degree is the load-bearing change: one warp loads a neighbor
list in exactly one coalesced pass (adjacent lanes, adjacent
addresses, one memory transaction), and lane i always has lane-i
work. Question: what does fixed degree cost in graph quality, and
how does build compensate (rank-based pruning + detour counting in
graph_core.cuh — keeping the edges that SHORTCUT most 2-hop paths)?

### Step 3 — build by NN-descent, not insert-one-at-a-time

HNSW builds incrementally, one insert at a time — inherently serial
(topic 14's build took minutes). CAGRA builds the whole graph at
once with **NN-descent**: start every vertex with random candidate
neighbors, then iterate "my neighbors' neighbors are probably my
neighbors too" — a fixpoint of local refinement where every vertex
improves its list independently, embarrassingly parallel. Then prune
each list to the fixed degree, preferring edges that shortcut many
2-hop paths (detour counting). Paper's headline: build is ~10×
faster than HNSW at equal recall. Question: NN-descent is itself a
graph algorithm with ragged intermediate state — how does the paper
make ITS memory usage bounded (fixed-size candidate lists again)?

### Step 4 — search: one CTA per query, parallel within each step

A **CTA** (cooperative thread array — CUDA's thread block, a few
hundred threads sharing ~100 KB of fast scratch **shared memory**)
cooperates on ONE query in search_single_cta_kernel.cuh:

```
 shared memory holds: itopk candidate list + visited hashmap
                      + distance scratch  (:127-143 budgets this)
 loop until itopk stable:
   pick search_width best unvisited parents   (bitonic/radix topk)
   ALL threads: load their fixed-degree neighbors, compute
                distances in parallel (one lane ≈ one neighbor)
   dedupe via shared hashmap, merge into itopk
```

The greedy walk is still SEQUENTIAL across iterations — parallelism
is WITHIN each step (32–64 distance computations at once, expanding
`search_width` parents per iteration instead of HNSW's one) plus
ACROSS queries (one CTA each, thousands resident):

```rust
// one CTA per query; the walk is sequential, each STEP is parallel
while !itopk.stable() {
    let parents = itopk.best_unvisited(SEARCH_WIDTH);   // bitonic/radix topk
    par_for lane in 0..(SEARCH_WIDTH * DEGREE) {        // one lane ≈ one neighbor
        let v = graph[parents[lane / DEGREE]][lane % DEGREE];
        // FIXED degree ⇒ this load is one coalesced pass, no load balancing
        if visited.insert(v) {                          // shared-memory hashmap
            dist[lane] = l2(query, data[v]);
        }
    }
    itopk.merge(dist);                                  // shared-memory topk
}
```

The itopk list is maintained by bitonic/radix top-k — sorting
networks with a fixed compare-exchange schedule, not a heap, because
data-dependent branching diverges the warp. Question: batch size 1
uses a fraction of the device; batch 10K saturates it — how does
that reshape M14's "QPS at recall" curve axes (GPU ANN is a
THROUGHPUT device: latency per query barely improves, queries per
second explode)?

### Step 5 — the visited set becomes a shared-memory hashmap

The visited set is queried on every candidate, so it must live in
the fastest memory the CTA owns — but shared memory is ~100 KB,
shared with the itopk list and distance scratch, so a bitmap over a
million vertices (125 KB) doesn't fit. hashmap.hpp packs an
open-addressing hash table into that budget, sized by
hashmap_min_bitlen / max_fill_rate (search_single_cta.cuh:57-59).
Collisions → a false "already visited" is acceptable (skip a node,
lose a bit of recall) but the reverse isn't tracked... check: is it
lossy or exact? Question: compare topic 14's visited-set choices
(bitmap vs hash set per query) — why does shared-memory capacity
force the hash here, and what happens to recall when the table
saturates on a long search?

### Step 6 — what transfers to M18

Vector distance scoring is our engine's most GPU-shaped op (dense,
regular, high arithmetic intensity — the l2_batch stub is its
kernel). CAGRA's lesson: if you also want the TRAVERSAL on device,
you must first make the graph regular — fixed degree, flat levels,
bounded scratch. FalkorDB's CSR adjacency is not regular — which is
why M18's flag gates distance scoring, not traversal.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| cpp/src/neighbors/detail/cagra/cagra_build.cuh | build: NN-descent → rank-based pruning | 3 |
| detail/cagra/graph_core.cuh | graph optimization (detour counting, reverse edges) | 2–3 |
| detail/cagra/search_single_cta_kernel.cuh:30-34 | the search kernel params: itopk, hashmap ptr | 4 |
| detail/cagra/search_single_cta.cuh:127-143 | shared-memory budget assembly (dataset ws + topk scratch) | 4–5 |
| detail/cagra/hashmap.hpp | the visited set: open-addressing table IN SHARED MEMORY | 5 |
| detail/cagra/topk_by_radix.cuh + bitonic.hpp | k-select without sorting everything | 4 |
| detail/cagra/search_multi_cta.cuh | many CTAs per query for large k / low QPS | 4, Q3 |
| detail/cagra/compute_distance_vpq-impl.cuh | PQ-compressed distance (topic 14's ADC on device) | 6, Q4 |

Start from `search_single_cta_kernel.cuh` (Step 4's loop), with
`search_single_cta.cuh:127-143` open beside it to see the
shared-memory budget being assembled; then `hashmap.hpp`, then the
build side. In the paper: §III is build (Steps 2–3), §IV is the
single-CTA search (Steps 4–5).

## Questions for notes.md

1. Fixed degree 32 vs HNSW's M=16-64 with levels: derive expected
   hops for 1M vectors (paper reports ~same recall at similar
   memory — where did the levels' log-factor go?).
2. itopk lives in shared memory and is maintained by
   bitonic/radix-topk — why is a HEAP (topic 14's CPU choice)
   wrong on a warp?
3. search_multi_cta splits one query across CTAs — when (large k,
   small batch)? What synchronizes the partial itopks (global
   memory + separate merge kernel — the no-device-barrier tax
   again)?
4. compute_distance_vpq: PQ codes unpacked per lane — topic 14's
   ADC table lives where (shared memory — budget collision with
   the hashmap: find who wins)?
5. For M14+M18: our rescore pipeline is exact-f32 over PQ
   candidates. Which half goes to GPU first, and what's the batch
   size per the crossover table you'll measure with l2_batch?

## References

**Papers**
- Ootomo, Naruse, Nolet, Wang, Feher, Wang — "CAGRA: Highly Parallel
  Graph Construction and Approximate Nearest Neighbor Search for
  GPUs" (ICDE 2024,
  [arXiv:2308.15136](https://arxiv.org/abs/2308.15136)) — §III for
  build (NN-descent + pruning), §IV for the single-CTA search

**Code**
- [cuvs](https://github.com/rapidsai/cuvs) —
  `cpp/src/neighbors/detail/cagra/` — the anchor map above is the
  reading order; start from `search_single_cta_kernel.cuh`
