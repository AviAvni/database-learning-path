# CAGRA: HNSW rebuilt for warps

Topic 14's HNSW asked "what index makes a pointer-chasing core fast?" CAGRA asks
the same question of a machine that executes 32 lanes in lockstep and answers
differently at every level: no hierarchy, fixed out-degree, a visited set that
fits in shared memory and is periodically *thrown away* on purpose. This chapter
builds those choices in order and shows each one in the code that implements it.

You cannot run any of it here. cuVS is CUDA-only and this machine has no CUDA
device — the topic's runnable lane is wgpu/Metal, and its measured numbers
(`notes.md:9-16`) are the transfer tax, not ANN throughput. Every performance
figure below is quoted from the paper with the hardware it was measured on, and
every structural claim is quoted from source. Nothing here is a local
measurement, and nothing here should be repeated as one.

Code anchors are [rapidsai/cuvs@8b97b61](https://github.com/rapidsai/cuvs);
check any of them with `python3 tools/pinned-source.py show cuvs <path> -r A:B`.
Paper citations are to [arXiv:2308.15136v2](https://arxiv.org/abs/2308.15136)
(9 Jul 2024) — the version that matches the ICDE 2024 paper. The v1 preprint
(Aug 2023) reports different numbers; quote the one you actually read.

## The problem in one sentence

HNSW's greedy walk is a chain of dependent random loads over variable-degree
nodes — the exact shape a 32-lane lockstep warp executes worst — and CAGRA's
answer is to delete every irregularity at build time, then spend the search
budget on a shared-memory hash table it can afford to forget.

## The concepts, step by step

### Step 1 — the greedy walk, and the two structures it needs

> **In:** a query vector and a proximity graph over N vectors.
> **Out:** k approximate nearest neighbours, plus the two auxiliary structures
> every graph ANN search must maintain.

Graph ANN search starts at some entry node, scores that node's neighbours, moves
to the best unvisited one, and repeats until nothing improves. Two structures
ride along:

- a **candidate list / internal top-M** — the best M nodes seen so far, M ≥ k
  (HNSW calls its width `ef`); wider means better recall and more work;
- a **visited set**, so a node reachable from three different directions is
  scored once, not three times.

CAGRA's buffer is both, adjacent in memory: *"a sequential memory buffer
consisting of an internal top-M list … and its candidate list … The length of
the internal top-M list is M (≥ k), and the candidate list is p × d"* (§IV-A),
where *p* is the number of parents expanded per iteration and *d* the graph's
fixed degree. That is exactly the code:

```cpp
// cpp/src/neighbors/detail/cagra/search_single_cta.cuh:106-115, the head of
// set_params — the paper's M is itopk_size, p is search_width, d is
// graph_degree.
   106    inline void set_params(raft::resources const& res)
   107    {
   108      num_itopk_candidates = search_width * graph_degree;
   109      result_buffer_size   = itopk_size + num_itopk_candidates;
   111      typedef raft::Pow2<32> AlignBytes;
   112      unsigned result_buffer_size_32 = AlignBytes::roundUp(result_buffer_size);
   114      constexpr unsigned max_itopk = 512;
   115      RAFT_EXPECTS(itopk_size <= max_itopk, "itopk_size cannot be larger than %u", max_itopk);
```

HNSW additionally stacks sparse upper levels to find a good entry point in
O(log N) hops. CAGRA does not, and says why: *"in the case of GPU, we can obtain
compatible initial nodes by randomly picking some nodes and comparing their
distances to the query, thus employing the high parallelism and memory bandwidth
of GPU"* (§III). A hierarchy is a way to spend few distance computations; a GPU
would rather spend many in parallel.

### Step 2 — what SIMT hates, and what CAGRA deletes

> **In:** HNSW's structure.
> **Out:** the same structure with every irregularity removed — and the defaults
> that say how far.

A warp is 32 lanes issuing one instruction; it is efficient when all lanes do
identical work on adjacent addresses. Score HNSW feature by feature:

```
  HNSW (topic 14)                CAGRA
  multi-level skip list          one flat graph, random entry points (§III)
  variable degree <= M           FIXED out-degree d, every list identical
  1 candidate expanded/iter      p parents expanded per iteration
  visited: heap-allocated set    visited: hash table in shared memory (Step 6)
```

Fixed degree is the load-bearing deletion, and it is worth naming what it buys
in this repo's vocabulary: it deletes Gunrock's entire research problem. There is
no ragged frontier, no `thread_mapped`-vs-`merge_path` choice, no prefix scan to
size the output — every parent contributes exactly *d* children, so the output
size is `p × d` before you look (`search_single_cta.cuh:108`). Regularity bought
at build time, spent at every search.

The defaults are bigger than most descriptions of CAGRA (including this guide's
previous version, which said "e.g. 32") suggest:

```cpp
// cpp/include/cuvs/neighbors/cagra.hpp:149-153 — build defaults.
   149  struct index_params : cuvs::neighbors::index_params {
   150    /** Degree of input graph for pruning. */
   151    size_t intermediate_graph_degree = 128;
   152    /** Degree of output graph. */
   153    size_t graph_degree = 64;
```

```cpp
// cpp/include/cuvs/neighbors/cagra.hpp:291-318 — search defaults, elided.
   291    size_t itopk_size = 64;
   294    size_t max_iterations = 0;
   300    search_algo algo = search_algo::AUTO;
   303    size_t team_size = 0;
   307    size_t search_width = 1;
   312    size_t thread_block_size = 0;
   314    hash_mode hashmap_mode = hash_mode::AUTO;
   316    size_t hashmap_min_bitlen = 0;
   318    float hashmap_max_fill_rate = 0.5;
```

The paper's experiments sweep d ∈ {32, 48, 64, 80} with the initial graph at
`d_init = 3d` (Fig. 3 caption), which is where the library's 64/128 pair comes
from. Note `search_width = 1`: the default expands **one** parent per iteration,
and the paper confirms the intent — *"we typically set p = 1 to maximize the
throughput of single-CTA"* (§IV-C2). The parallelism is across queries, not
across parents.

### Step 3 — build: NN-descent, then two graph surgeries

> **In:** N raw vectors.
> **Out:** a fixed-degree graph that is strongly connected — built 2.2–27×
> faster than HNSW's, on the hardware named below.

HNSW builds by inserting one vector at a time, each insert a search into the
graph built so far: sequential by construction. CAGRA builds the whole k-NN
graph at once by **NN-descent** (*"my neighbours' neighbours are probably my
neighbours"* — a fixpoint of independent local refinements, §III), then
*optimises* it in two passes.

**Pass 1, rank-based reordering.** An edge X→Y is redundant if some Z gives a
two-hop route that is short enough — NGT's criterion, quoted as Eq. 3 in §III-A.
Counting *detourable routes* per edge and keeping the least-detourable edges is
the pruning rule. CAGRA's twist is to rank by **position in the neighbour list**
rather than by distance: *"we approximate the distance by the initial rank. This
approximation allows us not to compute the impractical amount of distance
computations and not to store the large size of the distance table in memory"*
(§III-A). The cost, stated in the same section: distance-based reordering needs
`N × d_init × (d_init − 1)` distance computations or an `N × d_init` table; both
reorderings are O(N d³). Measured payoff: rank-based is *"faster than the
distance-based for all datasets by as much as 1.9×"*, and distance-based ran out
of memory on DEEP-100M where rank-based did not (§V-A, Q-A2, Fig. 4).

The triple loop is right there in the kernel, and note what it compares:

```cpp
// cpp/src/neighbors/detail/cagra/graph_core.cuh:259-277, inside
// kern_fused_prune — counting A->D->B detours. No distances are read.
   259    // count number of detours (A->D->B)
   260    for (uint32_t kAD = 0; kAD < knn_graph_degree - 1; kAD++) {
   261      const uint64_t iD = smem_indices[kAD];
   262      if (iD >= graph_size) { continue; }
   263      for (uint32_t kDB = lane_id; kDB < knn_graph_degree; kDB += raft::WarpSize) {
   264        const uint64_t iB_candidate = knn_graph(iD, kDB);
   265        for (uint32_t kAB = kAD + 1; kAB < knn_graph_degree; kAB++) {
   267          {
   268            const uint64_t iB = smem_indices[kAB];
   269            if (iB == iB_candidate) {
   270              atomicAdd(smem_num_detour + kAB, 1);
   271              break;
   272            }
   273          }
   274        }
   275      }
   276      warp.sync();
   277    }
```

**Pass 2, reverse edges.** Pruning leaves a directed graph in which some nodes
are unreachable. CAGRA reverses the pruned graph — *"Someone who considers you
are more important is also more important to you"* — at O(N d), then merges:
*"we basically take d/2 children for each parent node from each graph and
interleave them"* (§III-A; `kern_merge_graph`, `graph_core.cuh:375`). Fig. 3
measures which pass does what: the raw k-NN graphs have between 90 and 105,333
strongly connected components, and adding reverse edges brings every dataset to
between 1 and 26 — reordering alone never does. Reordering raises the 2-hop
count; reverse edges make the graph strongly connected. Two surgeries, two
different jobs.

**The build headline, correctly stated.** The abstract's number is *"2.2–27×
faster than HNSW, which is one of the CPU SOTA implementations"* — measured on a
DGX A100 with an AMD EPYC 7742 (64 cores) and an A100 80 GB, with dataset and
graph resident in device memory (§V-A). This guide previously said "~10×"; that
figure is in neither the abstract nor §V. And read the residency clause the way
`reading-crystal-sigmod20.md` teaches: a GPU number quoted without saying where
the data lives is not a number.

### Step 4 — search: one CTA per query, and warps split into teams

> **In:** a batch of queries and a fixed-degree graph in device memory.
> **Out:** one thread block per query, one team of lanes per distance
> computation, and a loop whose iterations are serial but whose steps are not.

A **CTA** (cooperative thread array — CUDA's thread block, 64–1024 threads with
private shared memory) owns one query and runs the entire search in one kernel.
The paper tried the obvious alternative and rejected it: *"extensive testing
revealed that the overhead of launching multiple kernels outweighs any potential
performance gains"* (§IV-C1) — the same per-dispatch tax this topic measures at
1544 µs on its runnable lane (`notes.md:11-14`), paid per *iteration* instead of
per search.

The loop body, elided to its four phases:

```cpp
// cpp/src/neighbors/detail/cagra/jit_lto_kernels/search_single_cta_jit.cuh:
// 256-291 — one iteration: pick parents, maybe reset the hash, score children.
   256      // pick up next parents
   257      if (threadIdx.x < 32) {
   259        pickup_next_parents<TOPK_BY_BITONIC_SORT, IndexT>(
   260          terminate_flag, parent_list_buffer, result_indices_buffer, internal_topk, search_width);
   262      }
   264      // restore small-hash table by putting internal-topk indices in it
   265      if ((iter + 1) % small_hash_reset_interval == 0) {
   266        const unsigned first_tid = ((blockDim.x <= 32) ? 0 : 32);
   268        hashmap_restore(
   269          local_visited_hashmap_ptr, hash_bitlen, result_indices_buffer, internal_topk, first_tid);
   271      }
   272      __syncthreads();
   274      if (*terminate_flag && iter >= min_iteration) { break; }
   279      compute_distance_to_child_nodes_jit<IndexT, DistanceT, DataT>(
   280        result_indices_buffer + internal_topk,
   281        result_distances_buffer + internal_topk,
   291        search_width);
```

Parent selection is done by **32 threads only** (`:257`) — one warp, because
`pickup_next_parents` is a warp-level operation and the rest of the block would
only contend. The walk is still sequential across iterations; the parallelism is
*within* an iteration (`p × d` distance computations at once) and *across*
queries (thousands of CTAs resident).

**Warp splitting** is the trick that keeps lanes busy inside one distance
computation. §IV-B1 does the arithmetic explicitly for a 96-dimensional float
dataset; here it is with the inputs named:

```
  one 128-bit load instruction per lane, 32 lanes in a warp
      warp-wide load width = 32 x 128 bits          = 4096 bits

  dataset vector, dim 96, float32
      vector width        = 96 x 32 bits            = 3072 bits
                                                      ----
      waste if 1 warp = 1 distance = 4096 - 3072    = 1024 bits idle (25%)

  team of 8 lanes (team_size = 8)
      team load width     = 8 x 128 bits            = 1024 bits
      loads per vector    = 3072 / 1024             = 3 (exact)
      teams per warp      = 32 / 8                  = 4 distances in flight
```

*"Although we split the warp into teams in software, we don't encounter warp
divergence since all of the teams in each warp still execute the same
instructions"* (§IV-B1). `team_size = 0` in the defaults means "choose for me"
(`cagra.hpp:302-303`, which documents the legal values as 4, 8, 16 or 32), and
Fig. 8 shows the choice is worth a large constant factor on both DEEP-1M
(dim 96) and GIST (dim 960).

### Step 5 — top-M without a heap, and the 512 that appears twice

> **In:** a buffer holding `itopk_size + p×d` (index, distance) pairs, partially
> sorted.
> **Out:** the new top-M — chosen by a sorting network, not a priority queue.

A heap is the CPU's answer (topic 14) and the worst possible warp code: every
sift-down is a data-dependent branch, so lanes diverge and the warp serialises.
CAGRA uses **bitonic sort** — a fixed compare-exchange schedule, identical for
every lane, executable in registers — and only falls back to a radix top-k when
the buffer is too big for registers:

> *"we first sort the candidate buffer and merge it with the internal top-M
> buffer through the merge process of the bitonic sort. We use the single
> warp-level bitonic sort when the candidate buffer size is less or equal to
> 512, while we use a radix-based sort using within a single CTA when it is
> larger than 512."* (§IV-B2)

The pinned code switches at a different number. Twice, in the same function:

```cpp
// cpp/src/neighbors/detail/cagra/search_single_cta.cuh:134 and 161-169 — the
// radix decision and its block-size consequence. The paper says 512.
   134      if (num_itopk_candidates > 256) {  // radix sort
   161        if (num_itopk_candidates > 256) {  // radix sort
   162          // radix-based topk is used.
   163          block_size = min_block_size_radix;
   165          // Internal topk values per thread must be equlal to or less than 4
   166          // when radix-sort block_topk is used.
   167          while ((block_size < max_block_size) && (max_itopk / block_size > 4)) {
   168            block_size *= 2;
   169          }
   170        }
```

Report the discrepancy rather than resolving it: the paper's threshold is 512 on
the *candidate buffer*, the code's is 256 on `num_itopk_candidates =
search_width × graph_degree` (`:108`), and the code additionally forces a
256-thread minimum block when radix is chosen. With the defaults (`search_width
= 1`, `graph_degree = 64`) `num_itopk_candidates` is 64, so the bitonic path is
what runs.

512 shows up again as the itopk ceiling (`max_itopk`, `:114`) and again in the
implementation-choice rule — where the paper's recommendation and the code
differ by a factor of two:

```cpp
// cpp/src/neighbors/detail/cagra/search_plan.cuh:122-130 — algo = AUTO.
   122      } else if (algo == search_algo::AUTO) {
   123        const size_t num_sm = raft::getMultiProcessorCount();
   124        if (itopk_size <= 512 && search_params::max_queries >= num_sm * 2lu) {
   125          algo = search_algo::SINGLE_CTA;
   127        } else {
   128          algo = search_algo::MULTI_CTA;
   130        }
   131      }
```

The paper recommends *"MT = 512 and bT = 'the number of SMs on the GPU'"*
(§IV-C3); the code demands **twice** the SM count of queries before it will use
single-CTA. Both agree on the shape: too few queries to fill the device, or too
large an internal top-M, and you switch to multi-CTA, which splits one query
across many blocks and moves the hash table to device memory (Table II).

The split is concrete, and it costs a second kernel. `search_multi_cta.cuh:117-126`
pins each CTA's internal list to a hard-coded 32 entries and derives the CTA count
from the itopk you asked for — `num_cta_per_query = max(search_width,
ceildiv(global_itopk_size, 32))`, so `itopk_size = 128` becomes 4 CTAs. Those CTAs
cannot merge their lists in place, because CUDA gives you no device-wide barrier
inside a kernel. So they write `num_cta_per_query * itopk_size` candidates to
device memory and a *separate* `_cuann_find_topk` launch reduces them after the
search kernel has fully returned (`search_multi_cta.cuh:246-265`). That extra
launch is exactly the "overhead of launching multiple kernels" the paper's §IV-C1
spends the whole single-CTA design avoiding — multi-CTA pays it because with too
few queries the device would otherwise sit idle.

### Step 6 — the forgettable hash table

> **In:** a visited set that must be checked on every candidate.
> **Out:** an *exact* open-addressing table small enough for shared memory,
> periodically wiped — and a computed reset interval.

The old version of this guide asked, next to this table, "check: is it lossy or
exact?" Here is the answer, from the code:

```cpp
// cpp/src/neighbors/detail/cagra/hashmap.hpp:15 and 37-60 — insert(). Linear
// probing is unconditional: the #define at line 15 is not guarded.
    15  #define HASHMAP_LINEAR_PROBING
...
    41    // Open addressing is used for collision resolution
    42    const uint32_t size     = get_size(bitlen);
    43    const uint32_t bit_mask = size - 1;
    44  #ifdef HASHMAP_LINEAR_PROBING
    45    // Linear probing
    46    IdxT index                = (key ^ (key >> bitlen)) & bit_mask;
    47    constexpr uint32_t stride = 1;
    53    constexpr IdxT hashval_empty = ~static_cast<IdxT>(0);
    55    for (unsigned i = 0; i < size; i++) {
    56      const IdxT old = atomicCAS(&table[index], hashval_empty, key);
    57      if (old == hashval_empty) {
    58        return 1;
    59      } else if (old == key) {
    60        return 0;
```

**Exact.** Open addressing stores the full key and compares it, so a collision
costs a probe, never a wrong answer; `atomicCAS` makes concurrent inserts by
different lanes safe and returns 1 exactly once per key. The only failure mode
is a *full* table: the loop at `:55` gives up after `size` probes and returns 0
— "already visited" — so a saturated table silently starts skipping nodes.
Recall degrades; correctness of the data structure does not.

Which is precisely why the table is sized to stay unsaturated, and reset when it
cannot be:

```cpp
// cpp/src/neighbors/detail/cagra/search_plan.cuh:298-305 and 324-330 —
// small-hash sizing and the reset interval, in calc_hashmap_params.
   298        const auto max_visited_nodes = itopk_size + (search_width * graph_degree * 1);
   299        unsigned min_bitlen          = 8;   // 256
   300        unsigned max_bitlen          = 13;  // 8K
   302        hash_bitlen = min_bitlen;
   303        while (max_visited_nodes > hashmap::get_size(hash_bitlen) * max_fill_rate) {
   304          hash_bitlen += 1;
   305        }
...
   324        small_hash_reset_interval = 1;
   325        while (1) {
   326          const auto max_visited_nodes =
   327            itopk_size + (search_width * graph_degree * (small_hash_reset_interval + 1));
   328          if (max_visited_nodes > hashmap::get_size(hash_bitlen) * max_fill_rate) { break; }
   329          small_hash_reset_interval += 1;
   330        }
```

Run it on the defaults with `graph_degree = 32`:

```
  inputs: itopk_size 64, search_width 1, graph_degree 32, max_fill_rate 0.5

  max_visited_nodes (1 iteration) = 64 + 1 x 32 x 1        = 96
  bitlen 8  -> 2^8 x 0.5           = 128 capacity >= 96    -> bitlen stays 8
  table bytes                      = 2^8 x 4 B (uint32)    = 1024 B

  reset interval:
    r = 1 -> 64 + 32 x 2 = 128 <= 128   keep going
    r = 2 -> 64 + 32 x 3 = 160 >  128   stop
  small_hash_reset_interval                                = 2
```

So the table is wiped every second iteration and re-seeded with only the current
internal top-M (`hashmap_restore`, the jit kernel at `:265-271`). Nodes visited
three iterations ago are forgotten and may be re-scored — the paper's
**forgettable hash table management**: *"Although this process may increase the
number of distance computations, catastrophic recall degradation will not occur…
We set the number of entries of the hash table as 2⁸ ∼ 2¹³ and the reset
interval as typically 1 ∼ 4"* (§IV-B3). Compare topic 14's CPU choice, where a
per-query bitmap over 1 M vertices costs 125 kB and nobody minds.

When even 2¹³ is not enough, `hash_bitlen` is set to 0 and the table moves to
**global** memory, sized for the whole search and allocated per query in the
batch:

```cpp
// cpp/src/neighbors/detail/cagra/search_single_cta.cuh:200-204 — the fallback.
   200      hashmap_size = 0;
   201      if (small_hash_bitlen == 0 && !this->persistent) {
   202        hashmap_size = max_queries * hashmap::get_size(hash_bitlen);
   203        hashmap.resize(hashmap_size, raft::resource::get_cuda_stream(res));
   204      }
```

`hash_bitlen ≤ 20` there (`search_plan.cuh:346-348`), i.e. up to 4 MB per query
— which is why that path is for small batches.

### Step 7 — the shared-memory budget, computed

> **In:** every structure Steps 4-6 introduced.
> **Out:** one number per CTA, and the loop that turns it into a block size —
> the fight Question 4 asks about, with real coefficients.

Everything the CTA owns is summed in one expression:

```cpp
// cpp/src/neighbors/detail/cagra/search_single_cta.cuh:126-131 and 175-178 —
// the budget, and the rule that converts it into a thread count.
   126      const std::uint32_t topk_ws_size = 3;
   127      const std::uint32_t base_smem_size =
   128        dataset_desc.smem_ws_size_in_bytes +
   129        (sizeof(INDEX_T) + sizeof(DISTANCE_T)) * result_buffer_size_32 +
   130        sizeof(INDEX_T) * hashmap::get_size(small_hash_bitlen) + sizeof(INDEX_T) * search_width +
   131        sizeof(std::uint32_t) * topk_ws_size + sizeof(std::uint32_t);
...
   175      constexpr unsigned ulimit_smem_size_cta32 = 4096;
   176      while (smem_size > ulimit_smem_size_cta32 / 32 * block_size) {
   177        block_size *= 2;
   178      }
```

Evaluate it for the Step 6 configuration, uncompressed float data of dimension
128, `INDEX_T = uint32`, `DISTANCE_T = float`:

```
  result_buffer_size    = 64 + 1 x 32                  = 96
  result_buffer_size_32 = roundUp(96, 32)              = 96
  buffer bytes          = (4 + 4) x 96                 = 768 B
  hash table            = 2^8 x 4                      = 1024 B
  parent list           = 4 x search_width(1)          =    4 B
  topk workspace        = 4 x 3                        =   12 B
  terminate flag        = 4                            =    4 B
  dataset workspace     = sizeof(desc) + 128 x 4       ~  512 B + desc
                                                         ------
  base_smem_size                                       ~ 2324 B  (+ desc)

  block-size rule: smem must fit 4096/32 = 128 B per thread
      64 threads  -> 8192 B budget  >= 2324 B   -> the 64-thread floor binds
```

Comfortably under the paper's *"typically ≤ 4 kB"* per query (§IV-B3), with
~5.8 kB of headroom before the block size is forced to double. Now spend that
headroom on Question 4's collision. Under PQ compression the codebook is
*also* in the workspace:

```cpp
// cpp/src/neighbors/detail/cagra/compute_distance_vpq-impl.cuh:112-114 and
// 133-143 — the PQ codebook and query buffer live in the same shared-memory
// workspace the hash table is competing for.
   112    static constexpr std::uint32_t kSMemCodeBookSizeInBytes =
   113      (1 << PQ_BITS) * PQ_LEN * utils::size_of<typename smem_val_config::smem_val_pack_uint_t>() /
   114      smem_val_config::num_packed_elements;
...
   135      /* SMEM workspace layout:
   136        1. The descriptor itself
   137        2. Codebook (kSMemCodeBookSizeInBytes bytes)
   138        3. Queries (smem_query_buffer_length elems)
   139      */
   140      return sizeof(cagra_q_dataset_descriptor_t) + kSMemCodeBookSizeInBytes +
```

With the default F16 packing (`uint32_t` holding 2 elements,
`compute_distance_vpq-impl.cuh:26-29`) and `PQ_BITS = 8`:

```
  codebook bytes = 2^8 x PQ_LEN x 4 / 2 = 512 x PQ_LEN
      PQ_LEN 2  -> 1024 B        PQ_LEN 4 -> 2048 B      PQ_LEN 8 -> 4096 B

  budget at 64 threads = 8192 B
  spent by Step 6's config (excl. dataset ws)           = 1812 B
  PQ_LEN 8 codebook                                     = 4096 B
  remaining for hash table + buffer + query staging     = 2284 B
```

That is the fight: the codebook and the visited table draw on one 8192-byte
budget, and losing it does not fail — it doubles `block_size` (`:176-178`), which
halves how many queries an SM can hold, which shows up as throughput. Whoever
wins, you pay in occupancy.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| `cpp/include/cuvs/neighbors/cagra.hpp:149-153` | build defaults: `graph_degree = 64`, `intermediate_graph_degree = 128` | 2 |
| `cpp/include/cuvs/neighbors/cagra.hpp:286-318` | search params: `itopk_size = 64`, `search_width = 1`, `team_size`, hash knobs | 2, 4-6 |
| `cpp/src/neighbors/detail/cagra/graph_core.cuh:206-330` | `kern_fused_prune`: detour counting over ranks, then keep-fewest-detours | 3 |
| `cpp/src/neighbors/detail/cagra/graph_core.cuh:178-196, 375` | reverse graph, then the d/2-each interleaved merge | 3 |
| `cpp/src/neighbors/detail/cagra/search_plan.cuh:122-130` | AUTO: single-CTA iff `itopk_size ≤ 512 && max_queries ≥ 2 × #SMs` | 5 |
| `cpp/src/neighbors/detail/cagra/search_plan.cuh:289-330` | small-hash bitlen and reset interval | 6 |
| `cpp/src/neighbors/detail/cagra/search_plan.cuh:333-349` | the device-memory fallback table, `hash_bitlen ≤ 20` | 6 |
| `cpp/src/neighbors/detail/cagra/search_single_cta.cuh:106-131` | buffer sizing, `max_itopk = 512`, the shared-memory sum | 1, 5, 7 |
| `cpp/src/neighbors/detail/cagra/search_single_cta.cuh:157-197` | block size: radix minimum, the 128 B/thread rule, occupancy bump | 5, 7 |
| `cpp/src/neighbors/detail/cagra/hashmap.hpp:15-73` | linear-probing open addressing, `atomicCAS` insert, full-table behaviour | 6 |
| `cpp/src/neighbors/detail/cagra/jit_lto_kernels/search_single_cta_jit.cuh:176-300` | the search loop: parents, hash reset, child distances | 4 |
| `cpp/src/neighbors/detail/cagra/search_multi_cta.cuh:117-138` | small-batch mode: `itopk_size` forced to 32 per CTA, `num_cta_per_query` derived from the requested itopk | 5 |
| `cpp/src/neighbors/detail/cagra/search_multi_cta.cuh:246-265` | the partial lists merged by a *separate* kernel over device memory — the no-device-barrier tax | 5 |
| `cpp/src/neighbors/detail/cagra/compute_distance_vpq-impl.cuh:112-143` | PQ codebook in the shared-memory workspace | 7 |

Reading order: `cagra.hpp` for the vocabulary, then `search_single_cta.cuh`'s
`set_params` — it is 100 lines and it contains the whole resource argument —
then `hashmap.hpp` and `search_plan.cuh:289-349` as a pair, then the jit kernel
loop. `graph_core.cuh` last; it is 1811 lines and only Steps 3's two passes
matter. In the paper: §III is build, §IV-A the algorithm, §IV-B the four
elemental techniques, §IV-C the two implementations and Table II.

## Questions for notes.md

1. HNSW's levels exist to reach the right neighbourhood in O(log N) hops. CAGRA
   deletes them and picks random entry points (§III). At fixed degree, where did
   the log factor go — and what in Step 3's build is paying for it? (Fig. 3's
   2-hop counts and strong-CC numbers are the evidence; say which pass buys
   which.)
2. Why is a heap the wrong top-M structure on a warp, and what does bitonic sort
   buy instead? Then find the threshold discrepancy: §IV-B2 says 512, and
   `search_single_cta.cuh:134` says 256 — of *what*, in each case?
3. `multi_cta` splits one query across CTAs (`search_plan.cuh:124-129` says
   when). There is no device-wide barrier, so how do the partial internal-top-M
   lists merge, and where does that table have to live (Table II)? Check your
   answer against `search_multi_cta.cuh:246-265` — count the kernel launches.
4. Do the Step 7 arithmetic yourself for `graph_degree = 64`, `itopk_size = 128`,
   PQ_LEN = 4: does the 64-thread block still fit in 128 B/thread? If not, what
   does the doubling cost you in resident queries per SM?
5. For M14+M18: our rescore pipeline is exact-f32 over PQ candidates. Which half
   goes to the GPU first, and at what batch size — given that this topic measured
   no crossover at all up to 2²⁴ elements on the local lane
   (`FINDINGS.md:36`) and CAGRA's own rule needs ≥ 2 × #SMs queries before it
   will even use its throughput mode?

## Done when

Answer each before unfolding it.

- [ ] You can name the three HNSW features CAGRA deletes, and say which one deletes another topic's entire problem.

  <details><summary>Answer</summary>

  The hierarchy (random entry points instead, §III), variable degree (fixed
  out-degree, `graph_degree = 64` by default, `cagra.hpp:153`), and the
  heap-allocated visited set (shared-memory hash, Step 6).

  Fixed degree deletes Gunrock's: no ragged frontier, so no load-balancing
  strategy to choose and no prefix scan to size the output — the candidate count
  is `search_width × graph_degree`, known before the iteration starts
  (`search_single_cta.cuh:108`).

  </details>

- [ ] You can state the build speedup with its caveats, and say which paper version you are quoting.

  <details><summary>Answer</summary>

  2.2–27× faster graph construction than HNSW (abstract and §V-A of
  [arXiv:2308.15136**v2**](https://arxiv.org/abs/2308.15136), 9 Jul 2024), on a
  DGX A100 — AMD EPYC 7742, 64 cores, against an A100 80 GB — with *"both the
  dataset and graph on the device memory of the GPU"* (§V-A). Large-batch search
  is 33–77× at 90-95 % recall, and 3.8–8.8× against other GPU implementations.

  Not "~10×", which this guide previously claimed and which appears nowhere in
  the paper.

  </details>

- [ ] You can explain warp splitting with the arithmetic that motivates it.

  <details><summary>Answer</summary>

  32 lanes × 128-bit loads = 4096 bits per warp instruction, but a 96-dim float
  vector is 3072 bits, so one-warp-per-distance idles a quarter of the lanes. A
  team of 8 lanes loads 1024 bits, covers the vector in exactly 3 loads, and lets
  4 teams per warp work on 4 different candidates (§IV-B1). No divergence: all
  teams execute the same instructions.

  `team_size` defaults to 0 = auto, legal values 4/8/16/32 (`cagra.hpp:302-303`).

  </details>

- [ ] You can say whether the visited table is exact or lossy, and what actually degrades recall.

  <details><summary>Answer</summary>

  Exact: open addressing with linear probing stores full keys and compares them
  (`hashmap.hpp:15,44-47`), and `atomicCAS` returns "newly inserted" exactly once
  (`:56-60`). A collision costs a probe, not an error.

  Two things degrade recall. A *full* table — the probe loop gives up after
  `size` attempts and returns 0, i.e. "already visited" (`:55,72`). And the
  deliberate one: the small table is wiped every `small_hash_reset_interval`
  iterations and re-seeded from the internal top-M only
  (`search_single_cta_jit.cuh:265-271`), so older nodes are re-scored. The paper
  calls it forgettable hash table management and reports no catastrophic recall
  loss (§IV-B3).

  </details>

- [ ] You can compute the reset interval from the search parameters.

  <details><summary>Answer</summary>

  `max_visited_nodes = itopk_size + search_width × graph_degree × (r + 1)`, and
  `r` grows while that stays under `2^bitlen × max_fill_rate`
  (`search_plan.cuh:324-330`). With itopk 64, width 1, degree 32, fill rate 0.5,
  bitlen 8: capacity 128; r=1 → 128 (fits), r=2 → 160 (does not) → interval 2.

  The table itself is 2⁸ × 4 B = 1 kB, within the paper's stated 2⁸–2¹³ range
  (§IV-B3).

  </details>

- [ ] You can say what happens when the shared-memory budget is exceeded, and why it is a throughput cost rather than an error.

  <details><summary>Answer</summary>

  `while (smem_size > 4096 / 32 * block_size) block_size *= 2`
  (`search_single_cta.cuh:175-178`): the CTA gets more threads so that the
  per-thread shared-memory quota is met. More threads per query means fewer
  queries resident per SM, so batch throughput falls. Nothing fails.

  The competing tenants are the top-M buffer, the hash table, the parent list,
  and — under PQ — a codebook of `2^PQ_BITS × PQ_LEN × 2` bytes in the same
  workspace (`compute_distance_vpq-impl.cuh:112-114,140`).

  </details>

- [ ] You wrote answers to all five questions in `notes.md`, including the Step 7 arithmetic redone for question 4's parameters.

  <details><summary>Answer</summary>

  The slots are `notes.md:87-93`. Question 4 wants the sum evaluated, not
  described: buffer + hash + parents + workspace against 128 B × block_size.

  </details>

## References

**Papers**

- Hiroyuki Ootomo, Akira Naruse, Corey Nolet, Ray Wang, Tamas Feher, Yong Wang —
  *"CAGRA: Highly Parallel Graph Construction and Approximate Nearest Neighbor
  Search for GPUs"*, ICDE 2024;
  [arXiv:2308.15136](https://arxiv.org/abs/2308.15136). Cite **v2** (9 Jul 2024)
  for every number above — §III build, §IV-A the algorithm, §IV-B1 warp
  splitting, §IV-B2 bitonic/radix, §IV-B3 the forgettable hash, §IV-B4 the MSB
  parent flag (and its 2³¹−1 dataset limit for `uint32`), §IV-C and Table II the
  two implementations, §V the evaluation on DGX A100 / EPYC 7742.

**Code**

- [cuvs](https://github.com/rapidsai/cuvs) @ `8b97b61` — `cpp/src/neighbors/
  detail/cagra/`. CUDA only: it does not build on this machine, and the guide
  quotes it rather than running it.

**Measurements in this repo**

- `topics/18-gpu/notes.md:9-16` and `FINDINGS.md:36` — the local wgpu/Metal
  transfer tax that Step 4's kernel-launch argument and question 5 lean on. They
  are not ANN measurements and must not be quoted as any.

**Related guides**

- `reading-crystal-sigmod20.md` — why "GPU is N× faster" is meaningless without
  the residency clause the CAGRA paper states in §V-A.
- `reading-gunrock.md` — the ragged-frontier problem CAGRA's fixed degree
  deletes.
- `reading-faiss-gpu.md` — the other GPU k-select design, in registers rather
  than shared memory.
