# Reading guide — "The GAP Benchmark Suite" (Beamer, Asanović, Patterson — arXiv:1508.03619) + gapbs ([`~/repos/gapbs`](https://github.com/sbeamer/gapbs))

The yardstick for graph analytics: 6 kernels × 5 graphs, plus
REFERENCE IMPLEMENTATIONS that are themselves state-of-the-art
single-node code. Read the paper for the methodology (it's topic 22's
fair-benchmarking argument specialized to graphs), read `src/` for
the algorithms — each .cc file opens with a mini-paper.

## The suite

```
  kernels: BFS  SSSP  PR  CC  BC  TC
  graphs:  twitter (skew)  web (locality)  road (diameter!)
           kron (RMAT synthetic)  urand (uniform synthetic)
                     │
   every kernel × every graph, 64 trials from random sources —
   because ONE graph shape crowns the wrong winner:
   road kills delta-stepping's parallelism (long diameter),
   urand kills direction-optimizing BFS (no hubs),
   kron/twitter kill anything O(max_degree²)
```

## Code anchors (each file's header comment = required reading)

| file | algorithm | the trick |
|---|---|---|
| `src/bfs.cc` | direction-optimizing | topic 20's guide covers it — α=15, β=18 here |
| `src/sssp.cc:87` | `DeltaStep` | thread-local bins (`:32` comment); `:44`: redundant relaxation is CHEAPER than removing stale entries — same lazy-deletion bet as our Dijkstra oracle |
| `src/bc.cc:51` | Brandes | `PBFS` records a `succ` BITMAP (:76) so backprop tests "is w my BFS successor" in one bit — no depth recheck |
| `src/cc.cc:95` | Afforest | `:106` neighbor_rounds=2 link sweeps, `:69` SampleFrequentElement (1024 samples), `:129` final sweep skips the giant component |
| `src/pr.cc:31-57` | pull PR | kDamp .85, L1-error stop; `pr_spmv.cc` is the same as one SpMV per iter — the algebraic identity made explicit |
| `src/tc.cc:52-99` | ordered TC | `OrderedCount` after `RelabelByDegree` if `WorthRelabelling` (:75 samples degree skew) |

## Methodology worth stealing for M22/M24

- **Trials from random sources, report ALL**: source choice changes
  BFS/SSSP/BC time by >10× on skewed graphs (a hub source vs a
  leaf). Our bench uses 3 fixed sources — upgrade when it matters.
- **Kernel spec ≠ implementation**: GAP specifies input/output, so
  algebraic (LAGraph runs GAP too) and frontier codes compete
  honestly. LAGr_PageRankGAP exists because GAP's PR spec (stop on
  L1 error, handle dangling like gapbs) differs from textbook PR —
  benchmark specs bind implementations.
- **The 5-graph matrix is the point**: our rmat-vs-uniform TC
  baseline (15.6M vs 5.4K triangles, same n/m) is GAP's argument in
  one row.

## Questions (answer in notes.md)

1. Why does GAP include road networks at all — which of the 6
   kernels ranks implementations DIFFERENTLY on road vs twitter,
   and what property (diameter, degree variance) drives each flip?
2. sssp.cc:44 argues redundant relaxations beat precise bucket
   removal. Under what edge-weight distribution does that bet fail?
3. bc.cc approximates with 16 sources by default. On our RMAT
   (18,844 components!), what systematic error does source sampling
   introduce and how would you stratify?
4. pr.cc vs pr_spmv.cc: same math, different memory access. Which
   wins on kron and why (hint: pull = gather = topic 20's SpMV
   16-19 GB/s lane)?
5. GAP has no Louvain/Leiden kernel. What makes community detection
   benchmark-hostile (hint: nondeterminism, tie-breaking,
   quality-vs-speed frontier)?
