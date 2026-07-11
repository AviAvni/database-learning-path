# Reading guide — "A Faster Algorithm for Betweenness Centrality" (Brandes, J. Math. Sociology 2001)

The paper that turned BC from O(n³) bookkeeping into O(V·E) — and the
cleanest example of "restructure the sum, not the data structure".
Our `bc::brandes` stub implements it against the O(n³) definitional
oracle; gapbs `bc.cc` and LAGraph `LAGr_Betweenness.c` show the two
production shapes.

## The restructuring

```
  definition:  bc(v) = Σ_{s≠v≠t}  σ_st(v) / σ_st
               (our bc_brute: all-pairs BFS + triple loop, O(n³))

  Brandes' observation: fix s and define the DEPENDENCY
               δ_s(v) = Σ_t σ_st(v)/σ_st
  then δ_s satisfies a recurrence over the BFS DAG, deepest first:

               δ_s(v) =  Σ_{w : v ∈ pred_s(w)}  (σ_sv / σ_sw) · (1 + δ_s(w))

  so per source: one forward BFS (depths + σ) + one backward sweep.
  bc(v) = Σ_s δ_s(v).   n sources × O(E) each = O(V·E).
```

The recurrence is the entire paper — derive it once by hand
(partition shortest s→t paths through v by v's DAG successor w; the
1 accounts for t=w itself).

## The two production shapes

| | gapbs bc.cc | LAGraph LAGr_Betweenness.c |
|---|---|---|
| forward | `PBFS` (:51): CAS on depths, records `succ` BITMAP (:76) — "is (u,v) a DAG edge" = one bit | `frontier`/`paths` are ns×n MATRICES (:110-164) — a BATCH of sources advances as one masked mxm |
| σ | `path_counts` accumulated at depth boundaries (`depth_index` slices the BFS queue by level) | `paths += frontier` per level, FP64 semiring |
| backward | deepest-first over `depth_index`, reads `succ` | transposed mxm per level with `bc_update` matrix |
| sampling | k sources, scores scaled | `sources` array — batch size = ns |
| wins | per-edge constants, one bitmap read per edge | no atomics; 4-32 sources amortize each matrix pass |

The batched-matrix trick is the one to remember for M24: BC over 32
sampled sources = the SAME number of graph passes as one source,
just with 32-row frontier matrices — SpGEMM amortizes what frontier
code cannot (it would need 32 separate BFS queues).

## Traps for the stub

1. σ must be accumulated ONLY along depth+1 edges (BFS DAG), and
   backprop must iterate strictly deepest-first — bucket vertices by
   depth after `bfs_sigma`, don't re-walk the queue out of order.
2. σ overflows u64 fast on dense graphs (σ multiplies along
   diamonds) — that's why everyone (gapbs `CountT`, LAGraph FP64,
   us) uses floats for path COUNTS. Exactness of the RATIO survives.
3. Disconnected sources: unreachable v has depth -1 — contribute
   nothing, don't divide by σ=0 (our RMAT has 18,844 components;
   the test will catch you).
4. Convention check: directed-sum over ordered (s,t) on a symmetric
   graph double-counts undirected pairs. Fine — but halve if you
   ever compare against NetworkX's undirected numbers.

## Questions (answer in notes.md)

1. Derive the recurrence from the definition (the partition-by-
   successor argument). Where does the "+1" come from?
2. bc_brute is O(n³) time but also O(n²) MEMORY (all-pairs depths+σ).
   Brandes is O(V·E) time, O(V) extra memory per source. At what
   n/m does the brute oracle stop fitting in LLC, and does that
   matter for a CORRECTNESS oracle?
3. gapbs's succ bitmap vs re-checking depth[w]==depth[v]+1: count
   memory touches per backprop edge for both. Why does the bitmap
   win despite costing a bit per EDGE?
4. LAGraph batches ns sources into one matrix. What limits ns
   (memory = ns×n FP64 dense rows in `paths`) and where's the sweet
   spot on our 65K-node RMAT?
5. FalkorDB has `proc_betweenness.c` calling LAGraph. M24: what
   should `CALL algo.betweenness(samples: 32)` return when the graph
   changed under a delta matrix that hasn't been flushed (topic 20's
   wait) — flush first, or compute on the stale main matrix?
