# Gunrock: advance, filter, and the ragged-frontier problem

The GPU graph framework that reduced every graph algorithm to two
data-parallel operators over frontiers — and then spent its research
budget on the problem hiding inside: adjacency lists are RAGGED, and
warps hate ragged. This chapter builds the ideas in order — frontier
traversal, the two-operator model, why power-law degrees wreck naive
work assignment, and the three load-balancing strategies that answer
it — then maps each to the modern "Essentials" codebase. Read that
code alongside the paper; the load-balancing menu in
`operators/advance/` is the chapter's core.

## The problem in one sentence

In one BFS frontier, vertex degrees range from 1 to 10⁷ — assign one
thread per vertex and a single hub keeps one thread busy for
~10⁶ edge visits while thousands of warps sit idle, so the real
research problem is not the algorithm but dividing ragged work
evenly.

## The concepts, step by step

### Step 1 — frontier-based traversal: graph algorithms as rounds

A **frontier** is the set of vertices active in the current round of
a graph algorithm. BFS (breadth-first search — visit all vertices at
distance 1, then 2, then 3...) is the archetype: the frontier starts
as {source}, each round expands every frontier vertex's neighbors,
and the *unvisited* neighbors become the next frontier. This
round-at-a-time shape is what makes graph algorithms GPU-friendly at
all: within one round, every vertex can be processed in parallel —
the sequential dependency is only *between* rounds. The graph itself
is stored as **CSR** (compressed sparse row — one array of
concatenated adjacency lists plus an offsets array saying where each
vertex's list starts, topic 13's format).

### Step 2 — the programming model: advance + filter, and a lambda

Gunrock's claim: every frontier algorithm is a loop over just two
data-parallel operators, specialized by a user **lambda** (a small
per-edge function):

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

In code, with BFS's lambda spelled out (**CAS** = compare-and-swap,
an atomic "write only if still unset"):

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

bfs.hxx:139-145 is the whole loop: `advance::execute_runtime` then
optionally `filter::execute_runtime` to remove invalids. Question:
BFS works WITHOUT the filter (bfs.hxx:114's comment) — what grows
unbounded if you skip it, and why is that sometimes still faster
(redundant work vs a full extra pass — the "idempotent BFS" trick)?

### Step 3 — why raggedness breaks warps

A warp is 32 threads executing in lockstep, and it is fast only when
all 32 lanes have the same amount of work. Adjacency lists give them
wildly different amounts: real graphs are power-law (topic 13), so a
frontier mixes degree-1 leaves with degree-10⁷ hubs. Whatever unit
of work you assign — vertex per thread, vertex per block — some unit
gets a hub and everything else waits. This is Gunrock's actual hard
problem; the two-operator model of Step 2 is just the stage it plays
on.

### Step 4 — the load-balancing menu: thread, block, merge_path

Three ways to split a frontier's edges across the device, each dying
on a different degree distribution:

```
 thread_mapped: thread i ← vertex i     good: uniform degree
                                        dies: one hub = one thread
 block_mapped:  block ← one vertex      good: hubs
                                        dies: 1-degree leaves waste 255/256
 merge_path:    binary-search the CSR offsets so every thread gets
                the same number of EDGES regardless of which vertex
                they belong to — perfect balance, pays a search
```

merge_path works because CSR's offsets array is a sorted prefix-sum
of degrees: "which vertex does global edge number e belong to?" is
one binary search, so thread t can independently compute its slice
of exactly `total_edges / n_threads` edges. advance.hxx:111-123
dispatches on a runtime enum — because no single strategy wins;
real frontiers mix hubs and leaves. (CAGRA sidesteps this whole
problem by CONSTRUCTION: fixed-degree graph ⇒ thread_mapped is
perfect. Worth noticing.) Question: merge_path is topic 11's
morsel-stealing idea done with arithmetic instead of a queue — what
property of CSR (sorted prefix offsets) makes the binary search
sufficient?

### Step 5 — frontier representation: sparse vs dense = push vs pull

A frontier can be a **sparse** list of vertex ids
(vector_frontier) or a **dense** bitmap with one bit per vertex
(boolmap_frontier) — exactly topic 20's SpMSpV-vs-SpMV and
direction-optimizing BFS. Small frontier → sparse/push (work
proportional to frontier size); huge frontier → dense/pull (scan
everything, but no atomics and no filter needed — the bitmap
dedupes by construction, since setting a bit twice is harmless).
Question: the switch threshold on CPU is ~|frontier| > n/20; what
changes on GPU (atomics for sparse output vs full-array scans being
nearly free at 400 GB/s)?

### Step 6 — the host loop: one dispatch per BFS level

There is no device-wide barrier inside a kernel launch (the wgpu
guide's point), so each BFS level is its own dispatch, and the
"is the frontier empty?" convergence test needs the frontier size
on the host — either a round-trip copy per level or **indirect
dispatch** (the GPU writes the next launch's size into a buffer the
runtime reads). Find how Gunrock decides iteration convergence.
Three consequences for our milestones:

- The advance lambda = FalkorDB's per-edge semiring op; Gunrock is
  what GraphBLAS-on-GPU compiles down to (M20).
- Advance produces a next frontier of unknown size — the cudf
  guide's no-push problem again; Gunrock scans the input frontier's
  degrees first (same two-phase, different name).
- The stretch-goal WGSL BFS: use boolmap frontier + level array —
  dense SpMV shape, no atomics needed except the "changed" flag
  (M18/M24).

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| include/gunrock/algorithms/bfs.hxx:95-149 | the whole BFS loop: advance + optional filter | 2, 6 |
| include/gunrock/framework/operators/advance/advance.hxx:94-123 | load-balance dispatch: thread/block/merge_path | 4 |
| operators/advance/thread_mapped.hxx | 1 thread : 1 vertex — dies on power laws | 3–4 |
| operators/advance/block_mapped.hxx | 1 block : 1 vertex's edges — dies on leaves | 4 |
| operators/advance/merge_path.hxx | binary-search work split — even by EDGE count | 4 |
| framework/frontier/vector_frontier.hxx | sparse frontier (vertex list) | 5 |
| framework/frontier/experimental/boolmap_frontier.hxx | dense frontier (bitmap) | 5 |
| include/gunrock/framework/operators/filter/ | dedupe/compact the output frontier | 2, 5 |

Reading order: `algorithms/bfs.hxx` first (Step 2's loop, ~50
lines), then the three load-balance strategies in
`framework/operators/advance/` side by side (Step 4 — the diff
between them IS the research), then the two frontier
representations. In the paper: §3 is the operator model (Step 2),
§4 is load balancing (Steps 3–4).

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
