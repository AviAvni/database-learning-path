# Topic 24 — Advanced Graph Algorithms & Analytics

Traversal (topics 13/20) is table stakes; this topic is the analytics
layer — centrality, components, communities, triangles — and the
recurring question: when does the algebraic (LAGraph) formulation
beat the frontier-based (GAP/Ligra) one? M24 turns the answer into a
`CALL algo.*` procedure library over the sparse core.

## The map

```
             per-source                 whole-graph
  paths   │ BFS (t20), Dijkstra,      │ APSP (don't)
          │ delta-stepping (STUB)     │
  central │ Brandes BC (STUB)         │ PageRank (provided),
          │  = BFS + backprop         │  harmonic/closeness
  structure│                          │ CC: union-find (provided)
          │                           │  vs Afforest (STUB),
          │                           │ triangles (provided),
          │                           │ k-truss, Louvain→Leiden
```

```mermaid
graph LR
    subgraph frontier world — gapbs, Ligra
        F["explicit worklists,<br/>atomics (CAS on depths),<br/>direction switching"]
    end
    subgraph algebraic world — LAGraph over GraphBLAS
        A["frontier = sparse vector,<br/>step = masked mxv/mxm,<br/>semiring picks the algorithm"]
    end
    F ---|"same asymptotics,<br/>different constants +<br/>parallelism story"| A
```

The trade in one line: frontier code exploits per-vertex tricks
(Afforest skips edges, Brandes' succ bitmap) that algebra can't
express cheaply; algebra gets batching (LAGr_Betweenness runs MANY
sources as one matrix frontier), no atomics, and free format/
direction switching from the runtime (topic 20's dot3-vs-saxpy).

## Measured baselines (algo_bench, M3 Pro, single thread)

RMAT scale 16 (n=65,536, m=1.82M directed, max deg 9,751) vs uniform
(same n/m, max deg 59):

| lane | rmat | uniform |
|---|---|---|
| PageRank to 1e-4 | 8 iters, 10.8 ms, 1.35 GTEPS-ish | 6 iters, 7.9 ms |
| Triangle count | **15,645,988** in 376 ms | **5,428** in 158 ms |
| Dijkstra ×3 sources | 33.7 ms, 343K pops | — |
| CC union-find | 18,844 comps, 4.2 ms, all m edges | — |

The TC row is the whole "skew matters" lecture: same n and m, 2883×
more triangles — hub neighborhoods intersect. Any TC benchmark on
uniform data measures a different algorithm.

## The stubs and what each teaches

- **`delta_stepping`** (sssp.rs) — the Dijkstra↔Bellman-Ford dial:
  δ=1 buys ordering (no wasted relaxations, no parallelism), δ=∞ is
  one big Bellman-Ford bucket. Stats expose the trade.
- **`brandes`** (bc.rs) — dependency accumulation replaces per-pair
  path counting; oracle is the O(n³) definition, so the stub must
  reproduce exact BC, then sample sources GAP-style.
- **`afforest`** (cc.rs) — union-find was never the bottleneck;
  EDGE INSPECTIONS are. Two neighbor-rounds + frequent-component
  sampling skip the giant component's edges entirely (test demands
  <50% of m inspected).

## Reading guides

- [reading-gap.md](reading-gap.md) — the GAP suite: 6 kernels, 5 graphs, and the reference-code anchors
- [reading-delta-stepping.md](reading-delta-stepping.md) — Meyer & Sanders + gapbs's thread-local bins
- [reading-brandes.md](reading-brandes.md) — Brandes '01 + LAGraph's batched matrix formulation
- [reading-ligra.md](reading-ligra.md) — edgeMap and the direction-switch threshold, generalized
- [reading-louvain-leiden.md](reading-louvain-leiden.md) — modularity, Louvain's broken communities, Leiden's fix
- [reading-lagraph-algos.md](reading-lagraph-algos.md) — FastSV7, six triangle-count formulations, and FalkorDB's `proc_pagerank.c` already doing M24
- topic 20: `reading-lagraph.md` (BFS push/pull) and `reading-beamer-sc12.md` — direction switching's origin

## Experiments

| file | status | what it shows |
|---|---|---|
| `graph.rs` | provided | weighted CSR, RMAT (skewed) + uniform generators |
| `sssp.rs` `dijkstra` | provided | heap Dijkstra with pop counter |
| `sssp.rs` `delta_stepping` | **stub** | bucketed SSSP, work-vs-ordering dial |
| `bc.rs` `bfs_sigma`+`bc_brute` | provided | path counting + O(n³) definitional oracle |
| `bc.rs` `brandes` | **stub** | dependency accumulation, exact + sampled |
| `cc.rs` `cc_unionfind` | provided | exact baseline, all edges |
| `cc.rs` `afforest` | **stub** | sampling CC, edges_inspected ≪ m |
| `analytics.rs` | provided | pull PageRank, degree-ordered triangle count |
| `bin/algo_bench.rs` | provided | rmat-vs-uniform lanes, stubs in catch_unwind |

## M24 checklist (capstone)

- [ ] algorithm library over the M20 sparse core: PR, BFS, CC, BC,
      SSSP, TC — algebraic where it wins, frontier where it doesn't
      (document each choice)
- [ ] Cypher procedure surface: `CALL algo.pagerank(...)` — FalkorDB
      already wraps `LAGr_PageRank` in `src/procedures/proc_pagerank.c:197`;
      copy the shape, replace the engine
- [ ] GAP-style regression lanes in M22's standing suite (BFS/SSSP/
      PR/CC/BC/TC on RMAT + uniform, both formulations)
