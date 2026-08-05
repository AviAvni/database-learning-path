# Gunrock: advance, filter, and the ragged-frontier problem

The GPU graph framework that reduced every graph algorithm to two data-parallel
operators over frontiers — and then spent its research budget on the problem
hiding inside: adjacency lists are ragged, and warps hate ragged. This chapter
builds the ideas in order (frontier traversal, the two-operator model, why
power-law degrees wreck naive work assignment, and the strategies that answer
it) and maps each to the modern "Essentials" codebase.

Two warnings before you start. First, none of this runs here: Gunrock is CUDA
(or HIP) and this machine has no such device, so every claim below is a claim
about source, anchored to a line. Second, the API moved between Gunrock 1.x and
Essentials, and most secondary descriptions of Gunrock — including this guide's
previous version — describe the old one. Where the code and the paper disagree,
this guide says so and takes the code.

Every code anchor is
[gunrock/gunrock@748f79e](https://github.com/gunrock/gunrock), the pinned
revision; check any of them with `python3 tools/pinned-source.py show gunrock
<path> -r A:B`. Paper citations are to
[arXiv:1501.05387](https://arxiv.org/abs/1501.05387) (PPoPP 2016).

## The problem in one sentence

In one BFS frontier the vertex degrees span three orders of magnitude — this
repo's own generated graph has a p50 degree of 11 and a maximum of 6565
(`topics/13-graph-engines/README.md:10-12`) — so assigning one thread per vertex
means one lane runs 6565 serial iterations while its 31 warp-mates and the rest
of the device wait.

## The concepts, step by step

### Step 1 — frontier-based traversal: graph algorithms as rounds

> **In:** a CSR graph and a set of active vertices.
> **Out:** the next set of active vertices — with all the parallelism inside a
> round and all the dependency between rounds.

A **frontier** is the set of vertices active in the current round. BFS is the
archetype: start with `{source}`, expand every frontier vertex's neighbours,
keep the newly-reached ones, repeat. The graph is **CSR** (compressed sparse
row): one array of concatenated adjacency lists plus an offsets array saying
where each vertex's list begins — topic 13's format, and the property that makes
Step 4 possible.

Gunrock's driver loop is four lines, and the convergence test is exactly what
you would guess:

```
// include/gunrock/framework/enactor.hxx:272-278 (the run loop) and 328-330
// (the default convergence test), quoted separately.
   272      prepare_frontier(get_input_frontier(), *context);
   274      while (!is_converged(*context)) {
   275        loop(*context);
   276        ++iteration;
   277      }
   278      finalize(*context);
...
   328    virtual bool is_converged(gcuda::multi_context_t& context) {
   329      return active_frontier->is_empty();
   330    }
```

`loop` is one kernel launch per round, at least — which is Step 6's problem.

### Step 2 — the programming model: advance, filter, and a lambda

> **In:** a frontier and a user lambda over `(source, neighbor, edge, weight)`.
> **Out:** a new frontier, with `false` from the lambda meaning "put an invalid
> sentinel here instead of this neighbour".

The claim is that every frontier algorithm is a loop over two operators
specialised by a lambda. Here is BFS's, whole:

```
// include/gunrock/algorithms/bfs.hxx:105-128 — the search lambda. Lines 116-122
// are commented out IN THE SOURCE; they are quoted here because they are the
// version most descriptions of Gunrock (including this guide's last one) claim
// is running.
   105      auto search = [distances, single_source, iteration] __host__ __device__(
   106                        vertex_t const& source,    // ... source
   107                        vertex_t const& neighbor,  // neighbor
   108                        edge_t const& edge,        // edge
   109                        weight_t const& weight     // weight (tuple).
   110                        ) -> bool {
   111        // If the neighbor is not visited, update the distance. Returning false
   112        // here means that the neighbor is not added to the output frontier, and
   113        // instead an invalid vertex is added in its place. These invalides (-1 in
   114        // most cases) can be removed using a filter operator or uniquify.
   116        // if (distances[neighbor] != std::numeric_limits<vertex_t>::max())
   117        //   return false;
   118        // else
   119        //   return (math::atomic::cas(
   120        //               &distances[neighbor],
   121        //               std::numeric_limits<vertex_t>::max(), iteration + 1) ==
   122        //               std::numeric_limits<vertex_t>::max());
   124        // Simpler logic for the above.
   125        auto old_distance =
   126            math::atomic::min(&distances[neighbor], iteration + 1);
   127        return (iteration + 1 < old_distance);
   128      };
```

Read lines 125-127 carefully, because the correction matters. There is **no
compare-and-swap** and there is **no `parent[]` array**: the state is
`distances[]`, the update is an atomic *min*, and the "did I win?" answer is
derived from the *old* value the atomic returns. Two threads reaching the same
unvisited neighbour in the same round both call `min`; the first sees
`old_distance = INT_MAX` and returns true, the second sees `old_distance =
iteration + 1` and returns `false` because `iteration + 1 < iteration + 1` is
false. Exactly one of them puts the neighbour in the output frontier.

That is a better primitive than CAS for the same reason `min` is a better
primitive than "test then set": it is commutative and idempotent, so any
interleaving of concurrent updates converges to the same array, and the same
lambda shape extends to SSSP where the winning condition is a genuinely smaller
distance rather than a first write.

The round body is two calls:

```
// include/gunrock/algorithms/bfs.hxx:138-146 — the entire loop body.
   138      // Execute advance operator on the provided lambda
   139      auto advance_load_balance = P->param.options.advance_load_balance;
   140      operators::advance::execute_runtime(G, E, search, advance_load_balance, context);
   142      // Execute filter operator to remove the invalids (if enabled via options).
   143      if (P->param.options.enable_filter) {
   144        auto filter_algorithm = P->param.options.filter_algorithm;
   145        operators::filter::execute_runtime(G, E, remove_invalids, filter_algorithm, context);
   146      }
```

The filter is **optional** (`options.enable_filter`), and the comment at
bfs.hxx:111-114 says why it can be: a rejected neighbour is not removed, it is
replaced by an invalid sentinel, so the output frontier is correct but padded.
The paper calls this the *idempotent* advance and is explicit about what the
filter buys — it *"can perform a series of inexpensive heuristics to reduce, but
not eliminate, redundant entries"*, and the non-idempotent variant *"internally
uses atomic operations to guarantee each element appears only once"* (§4.5).
Skipping the filter trades a growing, sentinel-padded frontier against one fewer
full pass per round.

### Step 3 — why raggedness breaks warps, in this repo's own numbers

> **In:** a frontier whose vertices have power-law degrees.
> **Out:** a makespan set by the largest list, not by the average — quantified
> below.

A warp is 32 lanes issuing one instruction together, so a warp finishes when its
*slowest* lane finishes. `thread_mapped` gives each thread one vertex and a
serial loop over its neighbours:

```
// include/gunrock/framework/operators/advance/thread_mapped.hxx:58-80 —
// one thread per frontier element, elided at the invalid-vertex guard.
    58    auto thread_mapped = [=] __device__(int const& tid, int const& bid) {
    59      auto v = (input_type == advance_io_type_t::graph)
    60                   ? type_t(tid)
    61                   : input.get_element_at(tid);
    66      auto total_edges = G.get_number_of_neighbors(v);
    68      for (auto i = 0; i < total_edges; ++i) {
    69        auto starting_edge = G.get_starting_edge(v);
    70        auto e = i + starting_edge;            // edge id
    71        auto n = G.get_destination_vertex(e);  // neighbor id
    73        bool cond = op(v, n, e, w);
    75        if (output_type != advance_io_type_t::none) {
    76          std::size_t out_idx = segments_ptr[tid] + i;
    77          type_t element = cond ? n : gunrock::numeric_limits<type_t>::invalid();
    78          output.set_element_at(element, out_idx);
    79        }
    80      }
    81    };
```

`total_edges` is the loop bound, and it is the vertex's degree. Put this repo's
measured graph through it — 1 M nodes, 16 M directed edges, p50 degree 11, max
degree 6565 (`topics/13-graph-engines/README.md:9-12`) — with a frontier of
100,000 vertices that happens to contain the top-degree node:

```
  mean degree              = 16e6 / 1e6            = 16 edges
  edges in the frontier E  ~ 100,000 x 16          = 1.6e6 edge visits
  perfectly balanced work  = 1.6e6 / 100,000 thr   = 16 steps per thread
  thread_mapped makespan   = max degree in frontier = 6565 steps
                                                      ----
  makespan / balanced      = 6565 / 16             = 410x
```

410× is the *load-balance* term alone, before any memory effect. And topic 13
measured the consequence end to end on the CPU: the same two-hop query is
**101× slower** from supernodes than from random nodes (4914 ns vs 495,378 ns,
`topics/13-graph-engines/README.md:15-19`). Skew is not a GPU problem that CPUs
avoid; it is a graph problem that the GPU's lockstep execution amplifies.

### Step 4 — the load-balancing menu, as it is actually spelled

> **In:** the choice of how to map a frontier's edges onto threads.
> **Out:** a `load_balance_t` enum value — of which fewer are usable than the
> enum suggests.

The menu is seven entries and three of them are marked work-in-progress:

```
// include/gunrock/framework/operators/configs.hxx:52-60 — verbatim, comments
// included, because the comments are the point.
    52  enum load_balance_t {
    53    thread_mapped,  ///< 1 element per thread
    54    warp_mapped,    ///< (wip) Equal # of elements per warp
    55    block_mapped,   ///< Equal # of elements per block
    56    bucketing,      ///< (wip) Davidson et al. (SSSP)
    57    merge_path,     ///< Merrill & Garland (SpMV):: DEPRECATED (use merge_path_v2)
    58    merge_path_v2,  ///< Merrill & Garland (SpMV):: CUSTOM
    59    work_stealing,  ///< (wip) <cite>
    60  };
```

So the strategies you can actually select are `thread_mapped`, `block_mapped`,
`merge_path` (deprecated) and `merge_path_v2` — and the runtime dispatch
confirms it, with `merge_path_v2` additionally guarded by
`#if __HIP_PLATFORM_NVIDIA__` (`advance.hxx:111-127` for the compile-time
dispatch, `advance.hxx:254-274` for `execute_runtime`, which is what
`bfs.hxx:140` calls).

What each does, and the degree distribution that kills it:

```
  thread_mapped   thread i <- frontier element i, serial loop over its edges
                  good: uniform degrees      dies: one hub stalls a whole warp
                  thread_mapped.hxx:58-81, launch box dim3_t<256> at :90

  block_mapped    equal number of elements per block
                  good: hubs                 dies: degree-1 leaves waste a block

  merge_path      every thread gets the same number of EDGES, found by a
                  diagonal search over (segments, atoms)
                  good: power laws           dies: pays an O(n) scan per round
```

The merge-path file states its own trade-off, which is exactly the arithmetic of
Step 3 turned into a design note:

```
// include/gunrock/framework/operators/advance/merge_path.hxx:17-29 — the
// algorithm and trade-off comment at the top of the file.
    17  * ALGORITHM:
    18  * 1. Compute prefix sum of segment sizes (compute_output_offsets)
    19  * 2. For each tile, use merge-path search to find tile boundaries
    20  * 3. Load segment offsets into shared memory
    21  * 4. Each thread uses merge-path to find its starting position
    22  * 5. Serial merge: walk the merge path processing items
    24  * TRADE-OFFS vs block_mapped:
    25  * - Requires O(n) prefix scan per iteration (vs O(n) reduce for block_mapped)
    26  * - Better load balancing for power-law graphs with hub vertices
    27  * - Worse performance for uniform-degree graphs (road networks, meshes)
    28  * - Each tile processes exactly merge_tile_size work items
    29  * - Hub vertices spanning multiple tiles are handled gracefully
```

`merge_tile_size = threads_per_block * items_per_thread` (`merge_path.hxx:131`),
and the tile boundaries are cached in shared memory (`:136-137`). Put the Step 3
frontier through it: every thread gets `E / T` = 16 edges whatever the degrees
are, so the 410× load-balance penalty goes to 1× and the price is one exclusive
scan per round (Step 5).

The paper describes the same territory with different names and one number worth
keeping: its hybrid *"set[s] a static threshold. When the frontier size is
smaller than the threshold, we use coarse-grained load-balance over nodes,
otherwise coarse-grained load-balance over edges… setting this threshold to 4096
yields consistent high performance"* (§4.4). The paper's per-warp/per-CTA
strategy sorts neighbour lists into three size classes — larger than a CTA;
larger than a warp but smaller than a CTA; smaller than a warp — and processes
each class with its own pass, *"at the cost of higher overhead due to the
sequential processing of the three different sizes"* (§4.4). None of those class
names appear in the Essentials enum. Cite the paper for the idea and the code
for the API.

Worth noticing while you are here: CAGRA deletes this entire problem by
construction. A fixed out-degree graph makes `thread_mapped` optimal, because
every list is the same length (`reading-cagra.md`, Step 2). Regularity bought at
build time, spent at search time.

### Step 5 — the unknown output size, and the scan that answers it

> **In:** a frontier of vertices with different degrees, and an output buffer
> that must be allocated before the kernel launches.
> **Out:** a per-element write offset, computed by an exclusive scan over the
> degrees — the same two-phase shape as libcudf's size/retrieve.

Advance cannot know how many neighbours it will emit until it looks. Gunrock's
answer is not an atomic cursor: it is a prefix scan over the input frontier's
degrees, taken before the kernel runs.

```
// include/gunrock/framework/operators/advance/helpers.hxx:58-79 — the degree
// functor and the scan, inside compute_output_offsets.
    58    auto segment_sizes = [=] __host__ __device__(std::size_t const& i) {
    59      if (i == total_elems)  // XXX: this is a weird exc. scan.
    60        return edge_t(0);
    62      auto v = graph_as_frontier ? vertex_t(i) : input_data[i];
    63      // if item is invalid, segment size is 0.
    64      if (!gunrock::util::limits::is_valid(v))
    65        return edge_t(0);
    66      else
    67        return G.get_number_of_neighbors(v);
    68    };
    70    auto new_length = thrust::transform_exclusive_scan(
    71        context.execution_policy(),  // execution policy
    72        thrust::make_counting_iterator<std::size_t>(0),  // input iterator: first
    73        thrust::make_counting_iterator<std::size_t>(total_elems +
    74                                                    1),  // input iterator: last
    75        segments.begin(),                                // output iterator
    76        segment_sizes,                                   // unary operation
    77        edge_t(0),                                       // initial value
    78        thrust::plus<edge_t>()                           // binary operation
    79    );
```

The scan's last element is the total output size, and element *i* is the write
base for frontier element *i* — which is precisely what `thread_mapped` uses at
`thread_mapped.hxx:76` (`segments_ptr[tid] + i`). One scan buys both the
allocation size and a contention-free write plan; no thread ever contends with
another for an output slot, because each thread's range was computed before the
kernel started.

That same `segments` array is what merge-path binary-searches (`merge_path.hxx:
139-140`, *"segments is the exclusive prefix scan of degrees"*). This is why CSR
makes merge-path possible at all: the offsets array is already a sorted
prefix-sum of degrees, so "which vertex owns global edge *e*?" is one binary
search, computable by each thread independently with no communication.

### Step 6 — the frontier's shape, and one dispatch per round

> **In:** the round structure of Step 1 and a GPU with no device-wide barrier.
> **Out:** one launch per level, a host-visible convergence test, and a choice
> of frontier representation that is really a choice of push vs pull.

A frontier can be a **sparse** list of vertex ids
(`include/gunrock/framework/frontier/vector_frontier.hxx`) or a **dense** bitmap
with one bit per vertex
(`include/gunrock/framework/frontier/experimental/boolmap_frontier.hxx`). That
is topic 20's SpMSpV-vs-SpMV distinction and direction-optimising BFS's push-vs-
pull, and Gunrock names the correspondence itself:

```
// include/gunrock/framework/operators/configs.hxx:78-82
    78  enum advance_direction_t {
    79    forward,   ///< Push-based approach
    80    backward,  ///< Pull-based approach
    81    optimized  ///< Push-pull optimized
    82  };
```

Small frontier → sparse and push, work proportional to the frontier. Huge
frontier → dense and pull, which scans everything but needs no atomics and
dedupes by construction, because setting a bit twice is harmless.

The round boundary itself is not free. There is no device-wide barrier inside a
launch (`reading-wgpu-compute.md`, Step 6), so each round is at least one
dispatch, and `is_converged` reads a host-visible emptiness flag
(`enactor.hxx:328-330`). On the lane you can actually run, that boundary costs
1544 µs (`notes.md:11-14`): a 9-level traversal spends 13.9 ms in submission
before counting an edge. On CUDA it is microseconds — but the *structure* is the
same, and it is why the stretch-goal WGSL BFS in this topic should use a boolmap
frontier and a level array (dense SpMV shape, one atomic-free dispatch per
level) rather than a sparse frontier whose size the host must learn every round.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| `include/gunrock/framework/enactor.hxx:272-278, 328-330` | the run loop and the convergence test | 1, 6 |
| `include/gunrock/algorithms/bfs.hxx:105-128` | the BFS lambda: atomic **min**, not CAS (the CAS is commented out at 116-122) | 2 |
| `include/gunrock/algorithms/bfs.hxx:138-146` | advance, then optional filter | 2 |
| `include/gunrock/framework/operators/advance/thread_mapped.hxx:58-93` | 1 thread : 1 element, serial edge loop, `dim3_t<256>` launch box | 3-4 |
| `include/gunrock/framework/operators/configs.hxx:52-60` | the seven-entry `load_balance_t`, three `(wip)`, `merge_path` DEPRECATED | 4 |
| `include/gunrock/framework/operators/advance/advance.hxx:111-127` | compile-time dispatch on the strategy | 4 |
| `include/gunrock/framework/operators/advance/advance.hxx:254-274` | `execute_runtime` — what `bfs.hxx:140` actually calls | 4 |
| `include/gunrock/framework/operators/advance/merge_path.hxx:9-29` | the algorithm and its stated trade-offs | 4 |
| `include/gunrock/framework/operators/advance/merge_path.hxx:120-140` | the kernel, `merge_tile_size`, shared-memory tile offsets | 4 |
| `include/gunrock/framework/operators/advance/helpers.hxx:41-79` | `compute_output_offsets` — the degree scan | 5 |
| `include/gunrock/framework/frontier/vector_frontier.hxx` | sparse frontier (vertex list) | 6 |
| `include/gunrock/framework/frontier/experimental/boolmap_frontier.hxx` | dense frontier (bitmap) | 6 |
| `include/gunrock/framework/operators/filter/` | `bypass`, `compact`, `predicated`, `remove` — the filter menu | 2, 6 |

Reading order: `bfs.hxx` first (the lambda and the two-call loop are the whole
programming model), then `helpers.hxx` for the scan that makes advance possible,
then `thread_mapped.hxx` and `merge_path.hxx` side by side — the diff between
those two files *is* the research. `configs.hxx` whenever a name confuses you.
In the paper: §3 is the operator model, §4.4 is load balancing, §4.5 is
idempotence and push/pull.

## Questions for notes.md

1. Advance produces the next frontier with unknown size; libcudf solved the same
   problem with size-then-retrieve. Name Gunrock's answer, the function it lives
   in, and the one property of CSR that makes it cheaper than cudf's second
   probe.
2. This guide's previous version said BFS does a CAS on `parent[]`. The pinned
   code does `math::atomic::min` on `distances[]` (`bfs.hxx:125-127`). Work out
   why a lost race is benign in *either* formulation — and then why the `min`
   version is the one that also works for SSSP.
3. Direction-optimising BFS needs the reverse graph (CSC) for its pull phase.
   What does that double, and when is it worth it? (Topic 13's CSR+CSC question,
   resurfacing.)
4. Hub vertex of degree 10⁶ in a frontier of 100,000 degree-10 vertices: compute
   `thread_mapped`'s makespan against merge-path's `E/T`, the way Step 3 does it
   for the measured graph. Then say what the scan in Step 5 costs you per round,
   and at what frontier size it stops being worth paying.
5. For M24: LDBC power-law graphs on a GPU — which advance strategy per scale
   factor, and does the answer change per BFS level as the frontier's hub
   fraction changes? (Note that Gunrock picks this per *call*, not per level:
   `bfs.hxx:139` reads it from options once.)

## Done when

Answer each before unfolding it.

- [ ] You can express a graph algorithm as rounds of advance and filter, and say what the filter is allowed *not* to do.

  <details><summary>Answer</summary>

  `while (!is_converged) { advance; maybe filter; }` (`enactor.hxx:274-277`,
  `bfs.hxx:138-146`). Advance applies the lambda to every edge out of the
  frontier and writes the neighbour — or an invalid sentinel when the lambda
  returns false (`bfs.hxx:111-114`, `thread_mapped.hxx:77`).

  The filter is optional (`bfs.hxx:143`) and, in the idempotent formulation,
  only *reduces* duplicates: the paper says its heuristics *"reduce, but not
  eliminate, redundant entries"*, and only the non-idempotent advance
  guarantees uniqueness, using atomics to do it (§4.5).

  </details>

- [ ] You can explain why frontier raggedness breaks warps, using this repo's own measured degree skew.

  <details><summary>Answer</summary>

  A warp retires when its slowest lane does, and `thread_mapped`'s loop bound is
  the vertex's degree (`thread_mapped.hxx:66-68`). On the topic 13 graph — 1 M
  nodes, 16 M edges, p50 degree 11, max 6565
  (`topics/13-graph-engines/README.md:9-12`) — a 100,000-vertex frontier
  containing the top node has ~1.6 M edge visits, i.e. 16 per thread if
  balanced, against a makespan of 6565: **410×**.

  The end-to-end consequence was measured on the CPU in that same topic: 101×
  slower two-hop queries from supernodes than from random nodes.

  </details>

- [ ] You can name the load-balancing strategies that are actually selectable, and say which one a degree-10⁶ hub demands.

  <details><summary>Answer</summary>

  `thread_mapped`, `block_mapped`, `merge_path` (marked DEPRECATED in favour of
  `merge_path_v2`) and `merge_path_v2` (NVIDIA-only, `advance.hxx:114-118`).
  `warp_mapped`, `bucketing` and `work_stealing` are all `(wip)`
  (`configs.hxx:52-60`).

  A 10⁶-degree hub demands merge-path: it is the only strategy that splits by
  *edge* count rather than by vertex, giving every thread `num_atoms /
  num_threads` work regardless of which vertex those edges belong to
  (`merge_path.hxx:9-12`). The price is stated in the file: an O(n) prefix scan
  per iteration, and worse performance than `block_mapped` on uniform-degree
  graphs (`merge_path.hxx:24-27`).

  </details>

- [ ] You can explain what a lost race costs in the BFS lambda — using the code that is actually compiled.

  <details><summary>Answer</summary>

  `auto old_distance = math::atomic::min(&distances[neighbor], iteration + 1);
  return (iteration + 1 < old_distance);` (`bfs.hxx:125-127`). Two threads
  reaching the same unvisited neighbour in one round: the first gets
  `old_distance = INT_MAX` and returns true; the second gets `iteration + 1` and
  returns false. Exactly one emission, no CAS loop, and the array converges to
  the same contents under any interleaving because `min` is commutative and
  idempotent.

  Benign, because at a given level every candidate parent is equally valid — any
  of them yields a correct BFS tree. The CAS version is right there at
  `bfs.hxx:116-122`, commented out, labelled by the author as the more
  complicated way to get the same result.

  </details>

- [ ] You can say how advance sizes its output, and why that is the same problem cudf solves differently.

  <details><summary>Answer</summary>

  `compute_output_offsets` runs a `thrust::transform_exclusive_scan` over the
  frontier's degrees (`helpers.hxx:58-79`); element *i* is frontier element
  *i*'s write base and the last element is the total. `thread_mapped` writes at
  `segments_ptr[tid] + i` (`:76`), so no thread ever contends for a slot.

  cudf faces the identical constraint (no `push`, pre-sized buffers) and answers
  with a counting pass through cuco plus a retrieve pass
  (`reading-libcudf.md`, Step 2). Gunrock can be cheaper because a vertex's
  output count is its degree, which CSR already stores — no probe required.

  </details>

- [ ] You can explain why sparse-vs-dense frontier is the same choice as push-vs-pull, and what a round boundary costs.

  <details><summary>Answer</summary>

  Sparse (`vector_frontier.hxx`) means work proportional to the frontier and
  atomics on output — push. Dense (`boolmap_frontier.hxx`) means scanning every
  vertex but no atomics and free deduplication — pull. Gunrock names them
  directly: `forward` = push, `backward` = pull, `optimized` = both
  (`configs.hxx:78-82`).

  A round is at least one dispatch, because there is no device-wide barrier, and
  convergence is a host-visible emptiness test (`enactor.hxx:328-330`). On this
  repo's runnable lane that boundary is 1544 µs (`notes.md:11-14`) — 13.9 ms for
  a 9-level traversal before a single edge is examined.

  </details>

- [ ] You wrote answers to all five questions in `notes.md`, including the hub arithmetic.

  <details><summary>Answer</summary>

  The slots are `notes.md:79-85`. Question 4 wants the arithmetic done, not
  described: `E / T` against `max degree`, for the numbers in the question.

  </details>

## References

**Papers**

- Yangzihao Wang, Andrew Davidson, Yuechao Pan, Yuduo Wu, Andy Riffel, John D.
  Owens — *"Gunrock: A High-Performance Graph Processing Library on the GPU"*,
  PPoPP 2016, [arXiv:1501.05387](https://arxiv.org/abs/1501.05387). §3 for the
  operator model, §4.4 for the load-balancing strategies and the 4096 threshold,
  §4.5 for idempotent vs non-idempotent advance and push vs pull. Read it after
  the code, not before: the names in §4.4 are not the names in `configs.hxx`.

**Code**

- [gunrock](https://github.com/gunrock/gunrock) @ `748f79e` — the "Essentials"
  rewrite under `include/gunrock/`. Route: `algorithms/bfs.hxx` →
  `framework/operators/advance/helpers.hxx` → `thread_mapped.hxx` and
  `merge_path.hxx` → `framework/operators/configs.hxx` →
  `framework/frontier/`.

**Measurements in this repo**

- `topics/13-graph-engines/README.md:9-19` — the degree distribution and the
  101× supernode penalty Step 3 computes with.
- `topics/18-gpu/notes.md:11-14` — the 1544 µs per-dispatch floor Step 6 uses.
