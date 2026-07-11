# Gunrock: advance, filter, and the ragged-frontier problem

The GPU graph framework that reduced every graph algorithm to two
data-parallel operators over frontiers — and then spent its research
budget on the problem hiding inside: adjacency lists are RAGGED, and
warps hate ragged. Read the modern "Essentials" codebase alongside
the paper; the load-balancing menu in `operators/advance/` is the
chapter's core.

## Anchor map

| anchor | what it is |
|---|---|
| include/gunrock/algorithms/bfs.hxx:95-149 | the whole BFS loop: advance + optional filter |
| include/gunrock/framework/operators/advance/advance.hxx:94-123 | load-balance dispatch: thread/block/merge_path |
| operators/advance/thread_mapped.hxx | 1 thread : 1 vertex — dies on power laws |
| operators/advance/block_mapped.hxx | 1 block : 1 vertex's edges — dies on leaves |
| operators/advance/merge_path.hxx | binary-search work split — even by EDGE count |
| framework/frontier/vector_frontier.hxx | sparse frontier (vertex list) |
| framework/frontier/experimental/boolmap_frontier.hxx | dense frontier (bitmap) |
| include/gunrock/framework/operators/filter/ | dedupe/compact the output frontier |

## 1. The programming model: two operators

```
 while frontier not empty:
   ADVANCE: frontier → all neighbors, apply user lambda
            (BFS lambda: CAS parent; return "keep?" per edge)
   FILTER:  drop invalids/duplicates → next frontier

 BFS, SSSP, PageRank, connected components = different lambdas,
 SAME two operators. GraphBLAS says the same thing with matrices:
 advance = SpMV/SpMSpV over the frontier vector, filter = the mask
 (topic 20's push/pull duality, imperative edition).
```

bfs.hxx:139-145 is the whole loop: `advance::execute_runtime` then
optionally `filter::execute_runtime` to remove invalids.

```rust
// every graph algorithm = the same two operators + a different lambda
while !frontier.is_empty() {
    let next = advance(csr, &frontier, |src, dst| {
        // BFS lambda: a LOST race is benign — any parent is a valid tree
        parent[dst].compare_exchange(INVALID, src).is_ok()
    });
    frontier = filter(next, |v| is_valid(v));   // dedupe/compact
}
// SSSP, PageRank, CC: same loop, different lambda + frontier policy
```

Question:
BFS works WITHOUT the filter (bfs.hxx:114's comment) — what grows
unbounded if you skip it, and why is that sometimes still faster
(redundant work vs a full extra pass — the "idempotent BFS" trick)?

## 2. Load balancing: the actual hard problem

A frontier's vertices have degrees from 1 to 10⁷ (topic 13's power
laws). Assign work naively and one warp does a hub while thousands
idle:

```
 thread_mapped: thread i ← vertex i     good: uniform degree
                                        dies: one hub = one thread
 block_mapped:  block ← one vertex      good: hubs
                                        dies: 1-degree leaves waste 255/256
 merge_path:    binary-search the CSR offsets so every thread gets
                the same number of EDGES regardless of which vertex
                they belong to — perfect balance, pays a search
```

advance.hxx:111-123 dispatches on a runtime enum — because no
single strategy wins; real frontiers mix hubs and leaves. (CAGRA
sidesteps this whole problem by CONSTRUCTION: fixed-degree graph ⇒
thread_mapped is perfect. Worth noticing.) Question: merge_path is
topic 11's morsel-stealing idea done with arithmetic instead of a
queue — what property of CSR (sorted prefix offsets) makes the
binary search sufficient?

## 3. Frontiers: sparse vs dense = push vs pull

vector_frontier (list of vertex ids) vs boolmap_frontier (bit per
vertex): exactly topic 20's SpMSpV-vs-SpMV and direction-optimizing
BFS. Small frontier → sparse/push; huge frontier → dense/pull (and
no filter needed — the bitmap dedupes by construction). Question:
the switch threshold on CPU is ~|frontier| > n/20; what changes on
GPU (atomics for sparse output vs full-array scans being nearly
free at 400 GB/s)?

## 4. What transfers to M18/M20/M24

- The advance lambda = FalkorDB's per-edge semiring op; Gunrock is
  what GraphBLAS-on-GPU compiles down to.
- Each BFS level = one dispatch (no device-wide barrier — the wgpu
  guide's point); the frontier size must round-trip to the host OR
  use indirect dispatch. Find how Gunrock decides iteration
  convergence.
- The stretch-goal WGSL BFS: use boolmap frontier + level array —
  dense SpMV shape, no atomics needed except the "changed" flag.

## Questions for notes.md

1. Advance produces the NEXT frontier with unknown size — cudf
   solved this with size/retrieve; what does Gunrock use (scan the
   degrees of the input frontier first — same two-phase, different
   name)?
2. BFS's lambda uses CAS on parent[] — why is a LOST race benign
   here (any parent is a valid BFS tree — idempotence again)?
3. Direction-optimizing BFS needs the REVERSE graph for pull. What
   does that double (memory), and when is it worth it (topic 13's
   CSR+CSC question resurfacing)?
4. Estimate: hub vertex, degree 10⁶, thread_mapped — how many
   microseconds does one thread take at ~10 edges/cycle/SM... vs
   merge_path spreading it over the whole device?
5. For M24: LDBC power-law graphs on GPU — which advance strategy
   per LDBC scale factor, and does the answer change with the
   frontier's hub fraction per BFS level?

## References

**Papers**
- Wang, Davidson, Pan, Wu, Riffel, Owens — "Gunrock: A
  High-Performance Graph Processing Library on the GPU" (PPoPP 2016,
  [arXiv:1501.05387](https://arxiv.org/abs/1501.05387)) — §3 the
  operator model, §4 load balancing

**Code**
- [gunrock](https://github.com/gunrock/gunrock) — the modern
  "Essentials" rewrite under `include/gunrock/` — read
  `algorithms/bfs.hxx` first, then the three load-balance strategies
  in `framework/operators/advance/`
