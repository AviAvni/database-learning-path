# Reading guide — CAGRA ("Highly Parallel Graph Construction for GPU ANN", ICDE '24) + cuVS code

Clone: [`~/repos/cuvs`](https://github.com/rapidsai/cuvs) (`cpp/src/neighbors/detail/cagra/`). Topic
14's HNSW rebuilt from GPU-first principles: what does a
graph-traversal index look like when the executor is 32-wide warps
instead of one pointer-chasing core?

## Anchor map

| anchor | what it is |
|---|---|
| cpp/src/neighbors/detail/cagra/cagra_build.cuh | build: NN-descent → rank-based pruning |
| detail/cagra/graph_core.cuh | graph optimization (detour counting, reverse edges) |
| detail/cagra/search_single_cta_kernel.cuh:30-34 | the search kernel params: itopk, hashmap ptr |
| detail/cagra/search_single_cta.cuh:127-143 | shared-memory budget assembly (dataset ws + topk scratch) |
| detail/cagra/hashmap.hpp | the visited set: open-addressing table IN SHARED MEMORY |
| detail/cagra/topk_by_radix.cuh + bitonic.hpp | k-select without sorting everything |
| detail/cagra/search_multi_cta.cuh | many CTAs per query for large k / low QPS |
| detail/cagra/compute_distance_vpq-impl.cuh | PQ-compressed distance (topic 14's ADC on device) |

## 1. The index: HNSW minus everything SIMT hates

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

Fixed degree means one warp loads a neighbor list in exactly one
coalesced pass, and thread i always has lane-i work. Question: what
does fixed degree cost in graph quality, and how does build
compensate (rank-based pruning + detour counting in graph_core.cuh
— keeping the edges that SHORTCUT most 2-hop paths)?

## 2. Build: NN-descent instead of insert-one-at-a-time

HNSW builds incrementally — inherently serial (topic 14's build
took minutes). CAGRA builds the whole graph at once: NN-descent
(everyone's neighbors' neighbors are candidate neighbors — a
fixpoint of local refinement, embarrassingly parallel), then prune
to fixed degree. Paper's headline: build is ~10× faster than HNSW
at equal recall. Question: NN-descent is itself a graph algorithm
with ragged intermediate state — how does the paper make ITS
memory usage bounded (fixed-size candidate lists again)?

## 3. Search: one CTA per query

search_single_cta_kernel.cuh — a whole thread block cooperates on
ONE query:

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
is WITHIN each step (32-64 distance computations at once) plus
ACROSS queries (one CTA each, thousands resident). Question: batch
size 1 uses a fraction of the device; batch 10K saturates it — how
does that reshape M14's "QPS at recall" curve axes (GPU ANN is a
THROUGHPUT device: latency per query barely improves, queries per
second explode)?

## 4. The visited hashmap (hashmap.hpp)

Open addressing in shared memory, sized by hashmap_min_bitlen /
max_fill_rate params (search_single_cta.cuh:57-59). Collisions →
false "already visited" is acceptable (skip a node, lose a bit of
recall) but the reverse isn't tracked... check: is it lossy or
exact? Question: compare topic 14's visited-set choices (bitmap vs
hash set per query) — why does shared-memory capacity (~100 KB)
force the hash here, and what happens to recall when the table
saturates on a long search?

## 5. What transfers to M18

Vector distance scoring is our engine's most GPU-shaped op (dense,
regular, high arithmetic intensity — the l2_batch stub is its
kernel). CAGRA says: if you also want the TRAVERSAL on device, you
must first make the graph regular. FalkorDB's CSR adjacency is not
— which is why M18's flag gates distance scoring, not traversal.

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
