# usearch: HNSW with the fat trimmed

qdrant's HNSW is production plumbing; usearch is the algorithm with
the fat trimmed — same paper, ~10× less code, essentially all of it
in one header. Read it as the reference implementation for YOUR
hnsw.rs. Before the code, this chapter builds the concepts in order:
what an HNSW node must store, why layout decides hop cost, the tape
that answers it, and how a search structure stays correct under
concurrent inserts. This chapter assumes
[reading-hnsw-paper.md](reading-hnsw-paper.md) — the algorithms (Alg
1/2/4, M, M0, ef) are used here by name.

Every `file:line` below is from **unum-cloud/usearch@9fd6b01**, the
revision pinned in `resources/codebases.md`. Two files matter:
`include/usearch/index.hpp` (5033 lines — the graph) and
`include/usearch/index_plugins.hpp` (4275 lines — metrics, scalar
types, SIMD dispatch). Reproduce any snippet with
`python3 tools/pinned-source.py show usearch include/usearch/index.hpp -r A:B`.

**Terms used below, defined once.** *Slot* — usearch's internal
integer index for a member (`compressed_slot_t`, `std::uint32_t` at
`index.hpp:2128`), distinct from the user-visible *key*
(`default_key_t`, `std::uint64_t` at `:2127`). *Tape* — the single
byte buffer holding everything about one node's graph presence.
*Connectivity* — usearch's name for the paper's M. *Expansion* —
usearch's name for the paper's ef.

## The problem in one sentence

Every hop in an HNSW search is a random memory access, so if a
node's per-level neighbor lists live in separately allocated
`Vec<Vec<u32>>` structures, each visit costs 2–3 dependent cache
misses (~100 ns apiece) instead of 1 — and at ~300 node visits per
query that's the difference between ~30 µs and ~90 µs before a
single distance is computed.

Hold that against this topic's measured baseline: brute force does
**117 QPS at recall 1.000** — 8.5 ms per query — by streaming 51 MB
of contiguous f32 at 1.50 × 10⁹ MAC/s. A graph index wins only if
its pointer chase costs far less than that stream. 90 µs of pure
misses is still 90× faster than 8.5 ms, which is exactly why the
layout question is worth a whole chapter rather than a footnote: it
decides how much of that 90× margin you keep.

## The concepts, step by step

### Step 1 — what an HNSW node must store

> **In:** the paper's data model — levels, per-level neighbor lists,
> the vector. **Out:** the list of fields a node must carry, and why
> the obvious `Vec<Vec<u32>>` encoding is a layout decision disguised
> as a data-structure choice.

An HNSW index is, per node: a **key** (the caller's id), a **level**
(how high the node reaches in the hierarchy), and one neighbor list
per layer from its level down to 0 — up to M ids per upper layer,
M0 = 2M at layer 0 — plus the vector itself. usearch fixes the widths
in three `using` declarations:

```cpp
// index.hpp — the width of every field on the tape, 2127-2128, 2335-2341
  2127      using default_key_t = std::uint64_t;
  2128      using default_slot_t = std::uint32_t;
// ... 2129-2334: allocators, queues, distance types ...
  2335      using neighbors_count_t = std::uint32_t;
  2336      using level_t = std::int16_t;
  2337
  2338      /**
  2339       *  @brief  How many bytes of memory are needed to form the "head" of the node.
  2340       */
  2341      static constexpr std::size_t node_head_bytes_() { return sizeof(vector_key_t) + sizeof(level_t); }
```

So the head is `8 + 2 = 10` bytes: **key first, then level**. That
ordering matters in Step 2 and is the thing most descriptions of the
tape get wrong.

The natural first implementation is a `Vec<Vec<u32>>` per node (one
inner Vec per level): easy to grow, but every inner Vec is its own
heap allocation somewhere else in memory. Search touches nodes in
data-dependent order (topic 0's pointer chase), so layout — where
those lists physically live — is the entire performance story of an
in-RAM implementation.

### Step 2 — the node tape: one allocation, all levels adjacent

> **In:** the field list from Step 1. **Out:** the exact byte layout,
> the offset arithmetic that replaces pointer hops, and a
> byte-for-byte comparison against `Vec<Vec<u32>>`.

usearch stores everything about a node's graph presence in one
contiguous byte buffer — the "tape". The authoritative description is
the doc comment on `node_t`:

```cpp
// index.hpp — the tape layout, verbatim, 2364-2372
  2364      /**
  2365       *  @brief  A loosely-structured handle for every node. One such node is created for every member.
  2366       *          To minimize memory usage and maximize the number of entries per cache-line, it only
  2367       *          stores to pointers. The internal tape starts with a `vector_key_t` @b key, then
  2368       *          a `level_t` for the number of graph @b levels in which this member appears,
  2369       *          then the { `neighbors_count_t`, `compressed_slot_t`, `compressed_slot_t` ... } sequences
  2370       *          for @b each-level.
  2371       */
  2372      class node_t {
```

```
 node tape:  ┌─────┬───────┬────────────────────┬──────────────────┬─────┐
             │ key │ level │ L0: cnt + M0 × slot │ L1: cnt + M × slot │ ... │
             │ 8 B │  2 B  │  4 + 32×4 = 132 B  │  4 + 16×4 = 68 B │     │
             └─────┴───────┴────────────────────┴──────────────────┴─────┘
             one allocation, all levels adjacent
```

The sizes are not guesses; `precompute_` computes them once per
index and `node_bytes_` turns them into an offset:

```cpp
// index.hpp — where the tape's dimensions come from, 4147-4163
  4147      inline static precomputed_constants_t precompute_(index_config_t const& config) noexcept {
  4148          precomputed_constants_t pre;
  4149          pre.inverse_log_connectivity = 1.0 / std::log(static_cast<double>(config.connectivity));
  4150          pre.neighbors_bytes = config.connectivity * sizeof(compressed_slot_t) + sizeof(neighbors_count_t);
  4151          pre.neighbors_base_bytes = config.connectivity_base * sizeof(compressed_slot_t) + sizeof(neighbors_count_t);
  4152          return pre;
  4153      }
// ... 4154-4157: span typedef and the node_t overload ...
  4158      inline std::size_t node_bytes_(level_t level) const noexcept {
  4159          return node_head_bytes_() + node_neighbors_bytes_(level);
  4160      }
// ... 4161: the node_t overload ...
  4162      inline std::size_t node_neighbors_bytes_(level_t level) const noexcept {
  4163          return pre_.neighbors_base_bytes + pre_.neighbors_bytes * level;
  4164      }
```

Finding layer l's neighbors is then pure offset arithmetic — no
pointer hops. `neighbors_ref_t` is the view that does it:

```cpp
// index.hpp — the neighbor-slot view over raw bytes, 2404-2426
  2404      class neighbors_ref_t {
  2405          byte_t* tape_;
// ... 2406: the misaligned-load helper ...
  2407          static constexpr std::size_t shift(std::size_t i = 0) {
  2408              return sizeof(neighbors_count_t) + sizeof(compressed_slot_t) * i;
  2409          }
// ... 2410-2415: iterator typedefs ...
  2416          neighbors_ref_t(byte_t* tape) noexcept : tape_(tape) {}
```

Now do the arithmetic the paper chapter's memory formula only sketched
(M = 16, so `connectivity_base = 2M = 32`, `compressed_slot_t` = 4 B,
`neighbors_count_t` = 4 B):

```
  node_head_bytes_       = sizeof(u64 key) + sizeof(i16 level)
                         = 8 + 2                          =  10 B
  neighbors_base_bytes   = 32 × 4 + 4                     = 132 B
  neighbors_bytes        = 16 × 4 + 4                     =  68 B

  a level-0-only node    = 10 + 132                       = 142 B
  a node reaching L1     = 10 + 132 + 68                  = 210 B

  E[extra levels]        = 1/(M-1) = 1/15                 = 0.0667
       (geometric with p = 1/M, floored — Step 3)
  average bytes / node   = 142 + 68 × 0.0667              = 146.5 B

  the same node as Vec<Vec<u32>> (M=16, one inner Vec):
    outer Vec header  24 B  +  inner Vec header  24 B
  + heap block for 32 u32   128 B  +  2 allocator headers ≈ 32 B
                                                        ≈ 208 B
                                     in 2 allocations, 2 dependent
                                     misses to reach a neighbor id
```

So the tape is ~1.4× smaller *and* costs one miss instead of two per
node visit. One miss to reach the tape, then the neighbor ids stream
in sequentially — the prefetcher's favorite pattern. Compare qdrant
(per-level `Vec<Vec<RwLock<LinksContainer>>>` in the builder at
`lib/segment/src/index/hnsw_index/graph_layers_builder.rs:43`,
serialized compressed later) and neo4j's scattered records (topic 13):
usearch picks "everything about a node in one place". The cost: slots
are preallocated to the max (M or M0), so a node with 3 links pays for
16 — memory traded for predictable layout and lock-free growth
(Step 5).

### Step 3 — defaults, and the level draw that matches the paper

> **In:** the paper's parameter advice. **Out:** usearch's four
> constants, the `validate()` rule linking two of them, and the
> rounding detail that makes usearch's hierarchy differ from qdrant's.

usearch hard-codes the parameter choices the ecosystem converged on,
as source constants: `default_connectivity() = 16` (M) at
`index.hpp:1563`, `connectivity_base = default_connectivity() * 2`
= 32 (M0) at `:1591`, `default_expansion_add() = 128`
(efConstruction) at `:1568`, `default_expansion_search() = 64` (ef)
at `:1573`. `index_config_t::validate()` at `:1600-1620` is the one
that carries a rule rather than a number: `:1604` recomputes
`checked_mul(connectivity, 2)` and `:1609-1612` rejects a config
whose base connectivity is below it.

Note where these sit relative to the paper: the paper's §4.1 gives no
efConstruction default at all (100 is a figure caption, and §5's own
experiments use 500 and 40), so usearch's 128 is a *library* choice,
not a paper one. M = 16 is squarely inside §4.1's *"reasonable range
of M is from 5 to 48"*, and M0 = 2M is exactly §4.1's *"2·M is a good
choice for Mmax0"*.

The level draw is where implementations quietly diverge:

```cpp
// index.hpp — the level draw, 4336-4340
  4336      level_t choose_random_level_(std::default_random_engine& level_generator) const noexcept {
  4337          std::uniform_real_distribution<double> distribution(0.0, 1.0);
  4338          double r = -std::log(distribution(level_generator)) * pre_.inverse_log_connectivity;
  4339          return (level_t)r;
  4340      }
```

`pre_.inverse_log_connectivity` is `1/ln(connectivity)` from `:4149`
— the paper's **mL**. The C-style cast on `:4339` is a
double→integer **truncation**, i.e. a floor, which is what the
paper's Algorithm 1 line 4 specifies. qdrant, at
`graph_layers_builder.rs:391`, calls `.round()` instead. Compute the
difference at M = 16:

```
  P(level ≥ 1) with floor:   exp(-1/mL)      = exp(-ln 16) = 1/16
                                             = 6.3% promoted
  P(level ≥ 1) with round:   draw ≥ 0.5, so  = exp(-0.5·ln 16)
                                             = 16^-0.5 = 25% promoted
```

Four times as many nodes reach layer 1 in qdrant as in usearch at the
same M. Same paper, same formula, one cast apart.

### Step 4 — the three walks: the paper's algorithms as three functions

> **In:** the paper's Algorithms 1, 2, 4 and 5. **Out:** the four
> functions that implement them, with the definition sites (not the
> call sites) so you can read them.

The whole engine is a handful of traversals, each a transcription of a
paper algorithm. Definition sites, all in `index.hpp`:

| function | defined | paper | what it is |
|---|---|---|---|
| `search_for_one_` | `:4406` | Alg. 5's descent | the ef=1 greedy walk down the upper layers |
| `search_to_insert_` | `:4455` | Alg. 1's per-level beam | called from `:3234` during insert |
| `form_links_to_closest_` | `:4262` | Alg. 4 heuristic + back-links | called from `:3239` and `:3366`; shrinks overfull neighbors back to their slot limits |
| `search_to_find_in_base_` | `:4629` | Alg. 2 on layer 0 | the query path; called from `:3446` |
| `search_exact_` | `:4704` | — | the brute-force fallback, for when the graph is smaller than the query |

The mapping is the point: one paper algorithm ↔ one function, no
architecture in between. That's what "reference implementation for
your hnsw.rs" means concretely.

**Now the claim this chapter exists to correct.**
`search_to_find_in_base_` takes an optional `predicate`, and it is
tempting — and wrong — to say that a selective filter therefore
disconnects usearch's walk. Read where the predicate is actually
applied:

```cpp
// index.hpp — the neighbor loop of search_to_find_in_base_, 4681-4695
  4681              for (compressed_slot_t successor_slot : candidate_neighbors) {
  4682                  if (visits.set(successor_slot))
  4683                      continue;
  4684
  4685                  distance_t successor_dist = context.measure(query, citerator_at(successor_slot), metric);
  4686                  if (top.size() < top_limit || successor_dist < radius) {
  4687                      // This can substantially grow our priority queue:
  4688                      next.insert({-successor_dist, successor_slot});
  4689                      if (is_dummy<predicate_at>() ||
  4690                          predicate(member_cref_t{node_at_(successor_slot).ckey(), successor_slot})) {
  4691                          top.insert({successor_dist, successor_slot}, top_limit);
  4692                          radius = top.top().distance;
  4693                      }
  4694                  }
  4695              }
```

`next.insert` on `:4688` is **outside** the predicate test on
`:4689-4690`. Rejected nodes still enter the expansion frontier;
only the result list `top` is filtered. usearch filters the
**results**, not the traversal — so the walk is not disconnected and
recall does not percolate away.

What does break is the stopping rule:

```cpp
// index.hpp — the stop test that a selective predicate defeats, 4660-4666
  4660          while (!next.empty()) {
  4661
  4662              candidate_t candidate = next.top();
  4663              if ((-candidate.distance) > radius && top.size() == top_limit)
  4664                  break;
  4665
  4666              next.pop();
```

The loop exits only when `top` is **full** (`top.size() == top_limit`)
and the frontier's best is worse than `radius`. With a 1% predicate,
`top` rarely fills, the `:4663` break never fires, and the search
walks until `next` empties — degrading toward an exhaustive scan. The
failure mode is **cost, not recall**.

Contrast qdrant, which does the opposite and therefore has the
opposite problem:

```rust
// lib/segment/src/index/hnsw_index/point_scorer.rs — qdrant cuts the frontier, 231
   231          point_ids.retain(|id| self.filters.check_vector(*id));
```

qdrant drops filtered ids *before scoring*, so they never become
frontier — the walk really does disconnect, which is why qdrant needs
a cardinality planner and ACORN-1
(`graph_layers.rs:155`, walked in
[reading-qdrant-hnsw.md](reading-qdrant-hnsw.md)). usearch has
neither, and does not need them for *recall*; what it lacks is a
plan B for the cost blow-up.

### Step 5 — concurrency: striped spin locks for writers, lock-free readers

> **In:** concurrent inserts mutating neighbor lists. **Out:** the
> exact lock structure, how many stripes exist, and the invariant that
> lets readers take no lock at all.

Concurrent inserts mutate neighbor lists, so writes need exclusion —
but one global lock would serialize the build. usearch uses
**striped spin locks**, not mutexes:

```cpp
// index.hpp — the lock array, 660-680
   660  /**
   661   *  @brief  Cache-line-padded striped spin-lock array for concurrent graph mutations.
   662   *          Maps node slots to lock stripes via Fibonacci hashing, with each stripe
   663   *          occupying its own cache line to eliminate false sharing.
   664   *          The number of stripes is proportional to `threads * connectivity`, not
   665   *          graph size, keeping the lock array comfortably within L2/L3 cache.
   666   */
   667  template <typename allocator_at = std::allocator<byte_t>, std::size_t cache_line_ak = 128> //
   668  class striped_locks_gt {
// ... 669-672: allocator typedefs and a byte-size static_assert ...
   673      static constexpr std::uint64_t fibonacci_k = 0x9E3779B97F4A7C15ull;
   674
   675      using atomic_flag_t = std::atomic<std::uint8_t>;
   676      struct alignas(cache_line_ak) padded_lock_t {
   677          atomic_flag_t flag{0};
   678          char padding_[cache_line_ak - sizeof(atomic_flag_t)];
   679      };
   680      static_assert(sizeof(padded_lock_t) == cache_line_ak, "Lock stripe must be exactly one cache line");
```

An `std::atomic<uint8_t>` spun on, one per 128-byte cache line: a
single byte of state padded 128× to buy false-sharing immunity. The
stripe index is Fibonacci hashing of the slot (`:693-695`), and the
count comes from the constructor:

```cpp
// index.hpp — how many stripes exist, 716-729
   716      striped_locks_gt(std::size_t threads, std::size_t connectivity) noexcept {
   717          checked_size_result_t desired = checked_mul(threads, connectivity);
   718          desired = desired ? checked_mul(desired.value, std::size_t{4}) : desired;
// ... 719-723: overflow bail-out ...
   724          checked_size_result_t count = checked_ceil2((std::max<std::size_t>)(desired.value, 256));
// ... 725-728: overflow bail-out ...
   729          count_ = count.value;
```

Work the size on this topic's machine (Apple M3 Pro, 12 threads,
M = 16):

```
  desired  = threads × connectivity × 4 = 12 × 16 × 4  = 768
  count    = ceil2(max(768, 256))                      = 1024 stripes
  bytes    = 1024 × 128 B (one cache line each)        = 128 KiB
```

128 KiB of lock array — sized by *thread count*, never by graph size,
exactly as the doc comment on `:664-665` claims. A billion-node index
uses the same 128 KiB.

Searches take **no locks at all**: `search_to_find_in_base_`
(`:4629-4699`) contains no lock acquisition anywhere. It can do that
because Step 2's preallocation means a published tape never moves —
growth writes into slots that already exist. Simpler than qdrant's
`RwLock`-per-node builder
(`graph_layers_builder.rs:43`); the cost is update-vs-read races,
handled by slot versioning in `index_dense.hpp`.

### Step 6 — metrics and the SIMD that isn't there by default

> **In:** the belief that usearch is "the SIMD one". **Out:** the
> actual metric set, and the preprocessor gate that decides whether
> any hand-written SIMD runs at all.

The metric set is finite and enumerated — ten real metrics, plus a
sentinel:

```cpp
// index_plugins.hpp — every metric usearch knows, 114-133
   114  enum class metric_kind_t : std::uint8_t {
   115      unknown_k = 0,
   116      // Classics:
   117      ip_k = 'i',
   118      cos_k = 'c',
   119      l2sq_k = 'e',
   120
   121      // Custom:
   122      pearson_k = 'p',
   123      haversine_k = 'h',
   124      divergence_k = 'd',
   125
   126      // Sets:
   127      hamming_k = 'b',
   128      tanimoto_k = 't',
   129      sorensen_k = 's',
   130  };
```

(The comment groups them: three classic vector metrics, three custom
ones — including `haversine_k`, for lat/long — and three set metrics
over bit vectors.) The scalar types are broader:
`scalar_kind_t` at `:139-164` spans `b1x8` bit vectors through
`f64`, including `bf16` and four minifloat formats.

Now the dispatch. Check the gate before believing anything about
vectorization:

```cpp
// index_plugins.hpp — the SIMD backend is OPT-IN, 25-27
    25  #if !defined(USEARCH_USE_NUMKONG)
    26  #define USEARCH_USE_NUMKONG 0
    27  #endif
```

The external kernel library at this pin is **NumKong** (included at
`:59`), and it defaults to **off**. So the fallback path in
`metric_punned_t::builtin` is the one a stock header-only build takes:

```cpp
// index_plugins.hpp — metric_punned_t::builtin, 2916-2930
  2916      inline static metric_punned_t builtin(std::size_t dimensions, metric_kind_t metric_kind = metric_kind_t::l2sq_k,
  2917                                            scalar_kind_t scalar_kind = scalar_kind_t::f32_k) noexcept {
// ... 2918-2925: fill in the routed function pointer, dimensions, kinds ...
  2927          if (!metric.configure_with_numkong())
  2928              metric.configure_with_autovec();
  2929
  2930          return metric;
```

`configure_with_numkong()` returns false when the macro is 0, so
`configure_with_autovec()` runs — plain C++ loops handed to the
compiler's auto-vectorizer. This is the topic-11 argument in
miniature: usearch **templates** the metric so the compiler
specializes it (compiled), where qdrant enum-dispatches into
`unsafe extern "C"` hand-written kernels (vectorized). Neither is
free: usearch pays in compile time and binary size and depends on
the auto-vectorizer's judgment; qdrant pays a dispatch per batch and
maintains the intrinsics by hand.

## Where each step lives in the code

All paths relative to the `unum-cloud/usearch@9fd6b01` clone.

| step | file | lines | what |
|---|---|---|---|
| 1 | `include/usearch/index.hpp` | `:2127-2128` | `default_key_t` = u64, `default_slot_t` = u32 |
| 1 | `include/usearch/index.hpp` | `:2335-2341` | `neighbors_count_t` = u32, `level_t` = i16, `node_head_bytes_()` = 10 |
| 2 | `include/usearch/index.hpp` | `:2364-2371` | the tape doc comment — key, then level, then per-level slots |
| 2 | `include/usearch/index.hpp` | `:2372-2392` | `class node_t`, `tape_`, `neighbors_tape()`, key/level accessors |
| 2 | `include/usearch/index.hpp` | `:2404-2435` | `neighbors_ref_t` — `shift()`, `size()`, `push_back` |
| 2 | `include/usearch/index.hpp` | `:4147-4164` | `precompute_`, `node_bytes_`, `node_neighbors_bytes_` |
| 2 | `include/usearch/index.hpp` | `:4166-4182` | `node_malloc_` / `node_make_` — where a tape is born |
| 3 | `include/usearch/index.hpp` | `:1563`, `:1568`, `:1573`, `:1591` | connectivity 16, expansion_add 128, expansion_search 64, connectivity_base 2M |
| 3 | `include/usearch/index.hpp` | `:1600-1620` | `index_config_t::validate()` |
| 3 | `include/usearch/index.hpp` | `:4336-4340` | `choose_random_level_` — mL and the truncating cast |
| 4 | `include/usearch/index.hpp` | `:4406`, `:4455`, `:4262`, `:4629`, `:4704` | the five walks, at their definitions |
| 4 | `include/usearch/index.hpp` | `:4681-4695` | the predicate filters `top`, not `next` |
| 4 | `include/usearch/index.hpp` | `:4660-4664` | the stop test a selective predicate defeats |
| 5 | `include/usearch/index.hpp` | `:660-695` | `striped_locks_gt` — spin flags, Fibonacci hashing |
| 5 | `include/usearch/index.hpp` | `:716-730` | the stripe count: `ceil2(max(threads × M × 4, 256))` |
| 6 | `include/usearch/index_plugins.hpp` | `:114-133`, `:139-164` | `metric_kind_t` (10 metrics), `scalar_kind_t` |
| 6 | `include/usearch/index_plugins.hpp` | `:25-27`, `:2916-2931` | `USEARCH_USE_NUMKONG` defaults to 0; `builtin` falls back to autovec |

## Questions (answer in notes.md)

1. Bytes per node for M=16, M0=32, avg 1.06 levels, u32 slots — tape
   vs qdrant-builder Vec-of-Vecs (count headers, capacity slack,
   allocator overhead).
2. Why preallocate link slots to the max instead of growing? What
   does it cost in memory, and what does it buy under concurrent
   insert?
3. Filter-during-traversal with a 1% predicate on usearch: what
   happens, and which qdrant mechanism was built to fix exactly this?
4. usearch templates the metric; qdrant enum-dispatches scorers. Map
   this to topic 11's compiled-vs-vectorized argument — who wins
   where?
5. For YOUR hnsw.rs: steal the tape or use `Vec<Vec<u32>>` per level?
   Decide, justify with expected access pattern, and note what M17's
   SIMD needs.

## Done when

Answer each before unfolding it.

- [ ] You can list what an HNSW node must store and compute bytes per node for M=16, M0=32.
  <details><summary>Answer</summary>

  Key (u64), level (i16), and one `{count, slots…}` block per layer
  from the node's level down to 0 — `index.hpp:2364-2371`. Widths at
  `:2127-2128` and `:2335-2336`; `node_head_bytes_()` at `:2341` is
  `8 + 2 = 10`. From `precompute_` at `:4149-4151`:
  `neighbors_base_bytes = 32 × 4 + 4 = 132`,
  `neighbors_bytes = 16 × 4 + 4 = 68`. `node_bytes_(level)` at
  `:4158-4163` is `10 + 132 + 68 × level`, so a level-0-only node is
  **142 B** and a level-1 node is **210 B**. At the floored geometric
  draw's `E[extra levels] = 1/(M−1) = 0.0667`, the average is
  `142 + 68 × 0.0667 = 146.5 B`. The vector itself is stored
  separately — the tape is graph structure only.
  </details>

- [ ] You can explain what the node tape buys over `Vec<Vec<u32>>` per level, in allocations and in locality.
  <details><summary>Answer</summary>

  Allocations: one per node instead of `1 + levels`. Misses: one to
  reach the tape, after which the count and every neighbor id stream
  in from the same cache lines; `Vec<Vec<u32>>` costs a dependent miss
  to the outer Vec's buffer and another to the inner Vec's heap block
  before any id is visible. Bytes: 146.5 B on the tape versus roughly
  208 B for the Vec version (24 B outer header + 24 B inner header +
  128 B for 32 u32 + ~32 B of allocator headers). And the second-order
  win is Step 5's: a tape that never reallocates is a tape readers can
  walk without a lock.
  </details>

- [ ] You can say why link slots are preallocated to the maximum rather than grown.
  <details><summary>Answer</summary>

  Because `node_bytes_(level)` at `:4158-4163` must be a *pure
  function of the level* for two separate reasons. First, offsets:
  layer l's block is at a computable distance from the tape start
  only if every earlier block has its maximum width — that is what
  `neighbors_base_bytes + neighbors_bytes * level` at `:4163` means.
  Second, concurrency: growth would mean reallocation, reallocation
  would mean a moving tape, and a moving tape would force readers to
  take a lock. The price is slack — a node with 3 links occupies
  slots for 16 — paid in bytes to buy both O(1) addressing and
  lock-free reads.
  </details>

- [ ] You can describe the concurrency scheme — striped writer locks, lock-free readers — and what it assumes about readers.
  <details><summary>Answer</summary>

  Writers take one stripe of a `striped_locks_gt` array
  (`:667-680`): a `std::atomic<uint8_t>` **spin flag** — not a mutex —
  padded to a full 128-byte cache line so stripes cannot false-share.
  The slot picks its stripe by Fibonacci hashing (`:693-695`), and the
  array holds `ceil2(max(threads × connectivity × 4, 256))` stripes
  (`:716-729`) — 1024 stripes, 128 KiB, at 12 threads and M=16, and
  the same 128 KiB no matter how large the graph grows. Readers take
  nothing: `search_to_find_in_base_` at `:4629-4699` acquires no lock
  anywhere. The assumption that makes that sound is Step 2's — a
  published tape never moves, so a reader can at worst observe a stale
  neighbor count, never a dangling pointer. Cross-checking a
  concurrent *update* (as opposed to insert) is what `index_dense.hpp`'s
  slot versioning is for.
  </details>

- [ ] You can say where usearch applies a search predicate, and why that makes its filtered-search failure mode the opposite of qdrant's.
  <details><summary>Answer</summary>

  At `index.hpp:4689-4690`, guarding only `top.insert` on `:4691`.
  `next.insert` on `:4688` is *outside* that guard, so rejected nodes
  still expand the frontier: usearch filters the **result list**, not
  the traversal. Recall therefore does not percolate away. What breaks
  is termination — the loop's exit at `:4663` requires
  `top.size() == top_limit`, and with a 1% predicate `top` rarely
  fills, so the walk continues until `next` empties and the query
  degrades toward exhaustive. **Cost blow-up, not recall collapse.**
  qdrant does the reverse: `point_scorer.rs:231` `retain`s ids before
  scoring, cutting the frontier itself, which is genuine percolation —
  and precisely why qdrant needs a cardinality planner and ACORN-1
  (`graph_layers.rs:155`) and usearch does not.
  </details>

- [ ] You wrote answers to all five questions in notes.md, including your own tape-or-vec decision for `hnsw.rs`.
  <details><summary>Answer</summary>

  For question 4, check the gate before answering: `index_plugins.hpp:25-27`
  defaults `USEARCH_USE_NUMKONG` to 0, so `builtin` at `:2927-2928`
  falls through to `configure_with_autovec()` — a stock build has no
  hand-written SIMD at all, only what the compiler finds in templated
  loops. For question 5, the honest answer depends on whether your
  `hnsw.rs` will ever be read concurrently with writes: if not,
  `Vec<Vec<u32>>` is ~60 B/node more and one extra miss per visit, and
  is far easier to get right; if yes, the tape's
  never-reallocate invariant is doing work no amount of tuning
  recovers. Whichever you choose, write down the M you fixed it at —
  `node_bytes_` shows the tape's size is frozen the moment M is.
  </details>

## References

**Papers**
- Malkov, Yashunin — the HNSW paper
  ([arXiv:1603.09320](https://arxiv.org/abs/1603.09320)) — gets its
  own chapter: [reading-hnsw-paper.md](reading-hnsw-paper.md).
  §4.1's *"2·M is a good choice for Mmax0"* and *"reasonable range of
  M is from 5 to 48"* are the two lines usearch's defaults implement;
  Algorithm 1 line 4's floor is what `:4339`'s cast reproduces.

**Code** — `unum-cloud/usearch@9fd6b01`, pinned in
`resources/codebases.md`
- `include/usearch/index.hpp` (5033 lines) — the entire graph:
  config and defaults (`:1563-1620`), the tape (`:2335-2435`,
  `:4147-4182`), the walks (`:4262`, `:4406`, `:4455`, `:4629`,
  `:4704`), the locks (`:660-730`)
- `include/usearch/index_plugins.hpp` (4275 lines) — `metric_kind_t`
  (`:114-133`), `scalar_kind_t` (`:139-164`), the NumKong gate
  (`:25-27`) and `metric_punned_t::builtin` (`:2916-2931`)
- `index_dense.hpp` — the type-erased/quantized wrapper and the slot
  versioning that makes concurrent *updates* safe (not read here;
  named because Step 5's answer depends on it)
- qdrant, for the two contrasts drawn above:
  `lib/segment/src/index/hnsw_index/point_scorer.rs:231` (filter
  before scoring) and `graph_layers_builder.rs:391` (`.round()` on the
  level draw)
