# libcudf: GPU kernels can't push

RAPIDS' GPU DataFrame engine: Arrow-layout columns (topic 12) with every
operator rewritten under GPU constraints — no resizable output, atomics that
must be amortised, and a memory hierarchy you manage by hand. This chapter
builds those constraints one at a time and then points at the exact file that
implements each.

Read it as architecture, not as a lab. There is no NVIDIA device on this
machine, so nothing here can be built or run; every claim below is a claim about
source you can read, and it is anchored to a line you can check. That is also
why the anchors matter more than usual — a wrong `file:line` in a guide you
cannot compile is a wrong belief that never gets corrected.

Every anchor is [rapidsai/cudf@2f082a7](https://github.com/rapidsai/cudf), the
revision pinned in `resources/codebases.md`. Verify with
`python3 tools/pinned-source.py show cudf <path> -r A:B`. cudf's hash tables are
all instances of **cuco** (CUDA Collections, NVIDIA's GPU hash-table library),
which is *not* pinned here — so where cuco's own semantics matter this guide
quotes cudf's usage and cudf's comments, and says when the two disagree.

## The problem in one sentence

A join's output size is unknown until you compute it, but a GPU kernel's output
buffer must be allocated *before* launch and is shared by ~100,000 threads with
no `Vec::push` and no cheap lock — so every variable-output operator needs an
explicit plan for where each thread writes.

## The concepts, step by step

### Step 1 — the executor, and the three rules it imposes

> **In:** a kernel launched over a grid of blocks.
> **Out:** three constraints that decide the shape of every operator below —
> pre-sized output, amortised atomics, coalesced layout.

Vocabulary. A **kernel** is a device function launched over a grid; a **block**
(CTA) is a group of threads scheduled onto one SM, sharing a scratchpad and able
to synchronise; a **warp** is the 32 threads inside a block that issue together.
cudf fixes its block sizes as constants you can read: `DEFAULT_JOIN_BLOCK_SIZE
= 128` (`cpp/src/join/join_common_utils.hpp:21`) and `GROUPBY_BLOCK_SIZE = 128`
(`cpp/src/groupby/hash/helpers.cuh:25`). Four warps per block, everywhere.

The three rules:

- **Output must be pre-sized.** There is no allocator you would want to call
  from 100 K threads mid-kernel and no `push`. Either the size is known before
  launch (Step 2) or a device-scope cursor hands out ranges (Step 4).
- **Atomics must be amortised.** One global atomic per element serialises every
  thread on one cache line. The idiom is to reduce locally first and take one
  atomic per warp or per block (Step 4).
- **Layout is destiny.** A warp's 32 loads coalesce into one transaction only if
  the addresses are adjacent; the read granularity is 128 B on a GPU against
  64 B on a CPU (Crystal §4.3), so a row-store wastes most of every fetch
  (Step 6).

### Step 2 — size, then retrieve — and the pass you can skip

> **In:** a build-side hash table and a probe table of `left_table_num_rows`
> rows.
> **Out:** an exact output size, then two `device_uvector`s of exactly that
> length — at the cost of probing twice, unless the caller already knows the
> size.

The two entry points are one-function files. `inner_join_size.cu` is 20 lines
and `inner_join_retrieve.cu` is 28; both just instantiate templates from
`size_impl.cuh` / `retrieve_impl.cuh`. The counting pass is not a hand-written
kernel — it is cuco's own `count`:

```cpp
// cpp/src/join/hash_join/size_impl.cuh:52-61 — the whole size pass, inside
// dispatch_join_comparator's lambda. `hash_table` is the cuco::static_multiset.
    52      [&](auto equality, auto d_hasher) {
    53        auto const iter = cudf::detail::make_counting_transform_iterator(0, pair_fn{d_hasher});
    54        if constexpr (Join == join_kind::LEFT_JOIN) {
    55          return hash_table.count_outer(
    56            iter, iter + left_table_num_rows, equality, hash_table.hash_function(), stream.value());
    57        } else {
    58          return hash_table.count(
    59            iter, iter + left_table_num_rows, equality, hash_table.hash_function(), stream.value());
    60        }
    61      });
```

Note what is *not* there: no per-thread `count[]` array, no exclusive prefix scan
over it, no second kernel to turn counts into offsets. Older GPU joins (and this
guide's previous version) described exactly that three-step shape; at this pin it
lives inside cuco, and cudf only asks for a total.

The retrieve pass then makes the size pass optional:

```cpp
// cpp/src/join/hash_join/retrieve_impl.cuh:49-58 and 71-85, elided in the middle
// (the zero-size early return and the two allocations).
    49    std::size_t const join_size = output_size
    50                                    ? *output_size
    51                                    : compute_join_output_size<size_join>(right_table,
    52                                                                          left_table,
    ...
    71    auto const out_probe_begin =
    72      thrust::make_transform_output_iterator(left_indices->begin(), output_fn{});
    73    auto const out_build_begin =
    74      thrust::make_transform_output_iterator(right_indices->begin(), output_fn{});
    76    auto retrieve_results = [&](auto equality, auto d_hasher) {
    78      if constexpr (Join == join_kind::INNER_JOIN) {
    79        hash_table.retrieve(iter,
    80                            iter + left_table_num_rows,
    81                            equality,
    82                            hash_table.hash_function(),
    83                            out_probe_begin,
    84                            out_build_begin,
    85                            stream.value());
```

So the double probe is a *default*, not a law: pass `output_size` and the count
disappears. That is the whole reason `hash_join::inner_join_size` is public API
— a caller who joins the same tables repeatedly, or who has a cardinality
estimate it is willing to be wrong about, pays for one probe instead of two. The
`thrust::transform_output_iterator` pair is the other half of the trick: cuco
writes pairs, and the iterator splits them into two columns as they land, so no
intermediate array of pairs is ever materialised.

Now the arithmetic that makes this concrete, using the only dispatch floor this
repo has measured. One `inner_join` costs at least three device operations:
build (`insert_async`), count, retrieve. On a real CUDA device a launch costs a
few microseconds, so this is nothing — but our runnable lane's floor is
**1544 µs per dispatch** (`notes.md:11-14`), and the M18 question is what this
pattern would cost if you ported it to wgpu:

```
  3 dispatches x 1544 us          = 4632 us of pure submission
  to keep that under 10% overhead: total work >= 46.3 ms
  at the 12.5 GB/s our device achieved on a streaming kernel
  (notes.md:16, floor subtracted):  46.3e-3 s x 12.5e9 B/s = 579 MB
  at 8 B per probe row (4 B key + 4 B index):  ~72 M rows
```

Seventy-two million rows before the join's *plumbing* falls below a tenth of its
runtime. That number is the honest reason M18 offloads dense distance scoring
and not joins.

### Step 3 — how a probe is actually parallelised (and by how many threads)

> **In:** one probe key and a hash table in global memory.
> **Out:** the number of threads that cooperate on it, and the number of slots
> they touch per step — both of which are template parameters you can read.

Two numbers govern a cuco probe: the **cooperative-group size** (how many
threads work one key together) and the **bucket/storage size** (how many slots
sit contiguously and are examined per probing step). cudf sets them per table
type, and the values are small:

| table | probing scheme | storage | source |
|---|---|---|---|
| hash join (`static_multiset`) | `double_hashing<DEFAULT_JOIN_CG_SIZE, h1, h2>` = **2** | `cuco::storage<2>` | `cpp/src/join/hash_join/hash_join_impl.cuh:50-57`, `cpp/include/cudf/detail/join/join.hpp:12` |
| distinct join (`static_set`) | `linear_probing<1, hasher>` | `cuco::storage<1>` | `cpp/include/cudf/detail/join/distinct_hash_join.cuh:147-157` |
| group-by set | `GROUPBY_CG_SIZE = 1` | `GROUPBY_BUCKET_SIZE = 1` | `cpp/src/groupby/hash/helpers.cuh:19,22` |
| filtered join, primitive rows | `linear_probing<1, …>` | `bucket_storage<key, 1, …>` | `cpp/include/cudf/detail/join/filtered_join.cuh:165-175` |
| filtered join, nested rows | `linear_probing<4, …>` | as above | `filtered_join.cuh:182-183` |

Read that table before you repeat the folklore. There is no 4-to-8-thread
cooperative window anywhere in cudf's join path at this pin: the equi-join
probes with **two** threads and a two-slot bucket, and the distinct join —
the newer, faster path — probes with **one**. The only `4` is
`nested_probing_scheme`, for rows with nested columns, whose comparisons are
expensive enough to be worth spreading.

A naming trap worth knowing, because it will otherwise cost you an hour: cudf's
own comments call `linear_probing`'s first template parameter *"bucket size"*
(`filtered_join.cuh:174, 182, 184`) while `join.hpp:12` names the same position
`DEFAULT_JOIN_CG_SIZE`. cuco is not pinned in this repo, so this guide does not
adjudicate; what it can tell you is what the values are and where they come
from, and that the two files disagree about what to call them.

The arithmetic for why the numbers are small anyway:

```
  key stored by the hash join = cuco::pair<hash_value_type, size_type>
                              = 4 B + 4 B = 8 B
  GPU global-memory read granularity        = 128 B   (Crystal §4.3)
  slots delivered by one transaction        = 16
  slots a storage<2> bucket examines        = 2
                                              ---
  useful fraction of a random probe's fetch = 16 B / 128 B = 12.5 %
```

A wider bucket would use more of each fetched line — and would also make every
probe step compare more keys it does not want. That trade is the tuning knob the
question below asks you to find, and it is the same one hashbrown makes when it
picks a SIMD group width.

### Step 4 — amortising atomics: one `fetch_add` per warp, not per row

> **In:** a kernel whose threads each produce an unpredictable number of output
> rows.
> **Out:** a contiguous output range per warp, claimed with a single
> device-scope atomic — the pattern to copy when you cannot pre-size.

The clean example of "no push" solved without a size pass is the *conditional*
join, where output size cannot be counted cheaply. Its host side allocates from
either a caller-supplied size or a counting kernel, then creates one device-wide
cursor:

```cpp
// cpp/src/join/conditional_join.cu:74-97 — the size decision and the cursor,
// with the has_nulls branch of the launch elided.
    74    std::size_t join_size;
    75    if (output_size.has_value()) {
    76      join_size = *output_size;
    77    } else {
    79      cudf::detail::device_scalar<std::size_t> size(0, stream, mr);
    86        compute_conditional_join_output_size<DEFAULT_JOIN_BLOCK_SIZE, false>
    87          <<<config.num_blocks, config.num_threads_per_block, shmem_size_per_block, stream.value()>>>(
    88            *left_table, *right_table, join_type, parser.device_expression_data, false, size.data());
    91      join_size = size.value(stream);
    92    }
    94    cudf::detail::device_scalar<std::size_t> write_index(
    95      0, stream, cudf::get_current_device_resource_ref());
    97    auto left_indices = std::make_unique<rmm::device_uvector<size_type>>(join_size, stream, mr);
```

The kernel then buffers matches in shared memory with **block-scope** atomics —
cheap, because they never leave the SM:

```cpp
// cpp/src/join/conditional_join_kernels.cuh:41-45 — add_pair_to_cache, the
// per-warp staging step.
    41    cuda::atomic_ref<std::size_t, cuda::thread_scope_block> ref{*(current_idx_shared + warp_id)};
    42    std::size_t my_current_idx = ref.fetch_add(1, cuda::memory_order_relaxed);
    43    // It's guaranteed to fit into the shared cache
    44    joined_shared_l[my_current_idx] = first;
    45    joined_shared_r[my_current_idx] = second;
```

and flushes with exactly one **device-scope** atomic per warp, broadcast to the
other lanes by a shuffle:

```cpp
// cpp/src/join/conditional_join_kernels.cuh:74-93 — flush_output_cache, elided
// between the shuffle and the copy loop.
    74    if (0 == lane_id) {
    75      cuda::atomic_ref<std::size_t, cuda::thread_scope_device> ref{*current_idx};
    76      output_offset = ref.fetch_add(current_idx_shared[warp_id], cuda::memory_order_relaxed);
    77    }
    84    output_offset = cub::ShuffleIndex<detail::warp_size>(output_offset, 0, activemask);
    86    for (std::size_t shared_out_idx = static_cast<std::size_t>(lane_id);
    87         shared_out_idx < current_idx_shared[warp_id];
    88         shared_out_idx += num_threads) {
    89      std::size_t thread_offset = output_offset + shared_out_idx;
    90      if (thread_offset < max_size) {
    91        join_output_l[thread_offset] = join_shared_l[warp_id][shared_out_idx];
    92        join_output_r[thread_offset] = join_shared_r[warp_id][shared_out_idx];
```

Count the atomics. Per output row, the naive kernel takes one device-scope
`fetch_add`; this one takes one *block*-scope `fetch_add` (staying in the SM)
plus one device-scope `fetch_add` per warp-flush. If a warp stages `C` rows per
flush, device-scope traffic drops by a factor of `C`. It is the same rule as
Crystal's one-atomic-per-tile (`reading-crystal-sigmod20.md`, Step 6) and the
same rule the `filter_count` stub asks you to implement in WGSL
(`experiments/src/gpu.rs:150-153`) — three engines, one arithmetic.

### Step 5 — group-by: two tiers, and a fallback that restarts everything

> **In:** N rows and an unknown number of distinct keys.
> **Out:** either a shared-memory aggregation, or — if any single block sees too
> many distinct keys — the *entire* input re-aggregated through global memory.

The shared-memory tier is sized by four constants and one multiplication
(`cpp/src/groupby/hash/helpers.cuh:19-46`):

```
  GROUPBY_BLOCK_SIZE            = 128     threads per block         (:25)
  GROUPBY_CARDINALITY_THRESHOLD = 128     distinct keys a block may hold (:29)
  GROUPBY_SHM_MAX_ELEMENTS      = 128 + 128 = 256                   (:39-40)
      (threshold + block_size: after crossing the threshold every thread
       in the block can still land one more insert)
  shmem_extent_t                = 256 x 1.43 = 366 slots            (:44-46)
      load factor at the cap:  256 / 366 = 0.70   ← the comment's "0.7 occupancy"
  GROUPBY_CG_SIZE = 1, GROUPBY_BUCKET_SIZE = 1                      (:19,22)
```

A first kernel, `mapping_indices_kernel`, holds that 366-slot cuco set in
`__shared__` storage and maps each row to a local slot
(`cpp/src/groupby/hash/compute_mapping_indices.cuh:101-113`). If a block's
distinct-key count crosses the threshold it stops using the shared set and sets a
flag. The aggregation kernels then split by tier: `compute_shared_memory_aggs.cu`
for blocks under the threshold, `compute_global_memory_aggs.cu` for the rest,
with the shared tier's dynamic allocation coming from
`cudaOccupancyAvailableDynamicSMemPerBlock` — asked for the grid's actual blocks
per SM — and then **halved**, `0.5 * dynamic_shmem_size`, before being rounded
down to `ALIGNMENT` (`compute_shared_memory_aggs.cu:268-277`). cudf asks the
occupancy API what is available and then takes half of the answer; the margin is
not explained in the code, and guessing at its reason is the kind of thing this
guide is trying to stop you doing.

The part that is usually described wrongly — including by this guide's previous
version — is what happens when the flag is set. It is not a per-block spill:

```cpp
// cpp/src/groupby/hash/compute_single_pass_aggs.cuh:111-122 — the host reads
// the flag back and, if any block set it, discards the shared-memory plan.
   111    auto const needs_fallback = [&] {
   112      cuda::std::atomic_flag h_needs_fallback;
   115      CUDF_CUDA_TRY(cudf::detail::memcpy_async(&h_needs_fallback,
   116                                               needs_global_memory_fallback.data(),
   117                                               sizeof(cuda::std::atomic_flag),
   118                                               stream));
   119      stream.synchronize();
   120      return h_needs_fallback.test(cuda::std::memory_order_relaxed);
   121    }();
   122    if (needs_fallback) { return run_aggs_by_global_mem_kernel(); }
```

One block over 128 distinct keys makes the *whole* aggregation re-run in global
memory, after a host round trip and a `stream.synchronize()`. The decision is
all-or-nothing and it is taken on the host. For a high-cardinality group-by the
consequence is that the shared-memory pass is pure loss, which is why the
classical answer — partition by key hash first, so each partition's cardinality
fits — is what the question below asks you to work out. That is topic 13's radix
partitioning, applied to occupancy instead of to cache.

### Step 6 — layout, strings, and the one thing that is JIT-compiled

> **In:** an Arrow column.
> **Out:** coalesced loads for free, null handling as bit operations rather than
> branches, and — for one specific operator — a kernel compiled at runtime.

A `column_view` is Arrow by contract: *"Because column_view is non-owning, and
its data layout conforms to the Arrow Physical Memory Layout specification…"*
(`cpp/include/cudf/column/column_view.hpp:33-35`), which is a dense value buffer
plus an optional **validity bitmap** (one bit per row). Two GPU consequences: a
warp reading `col[i]` for adjacent `i` coalesces by construction, and nulls are
processed with bit operations rather than per-row branches — branches diverge
warps (`reading-crystal-sigmod20.md`, Step 1), bit twiddling does not. Slicing is
free because the offset lives in the view, not in the data
(`column_view.hpp:40-42`).

Strings are where the regularity ends: a `strings_column_view` is an offsets
child at index 0 plus a character buffer (`cpp/include/cudf/strings/
strings_column_view.hpp:53, 73-113`), so work per element is *variable* — the
same ragged-frontier problem Gunrock solves with load-balancing strategies
(`reading-gunrock.md`, Step 4).

Two anchor corrections worth carrying, because the plausible-sounding versions
are wrong:

- Validity-bitmap kernels are **not** a directory of many files. `cpp/src/
  bitmask/` contains exactly two: `null_mask.cu` and `is_element_valid.cpp`.
- cudf does **not** JIT-compile join predicates in general.
  `cpp/src/join/jit/` holds two files, `filter_join_kernel.cu` and
  `filter_join_kernel.cuh`, used by the filter-join path
  (`cpp/src/join/filter_join_indices/filter_join_indices_jit.cu:6-7`, which pulls
  in `jit/cache.hpp`, `jit/parser.hpp`, `jit/row_ir.hpp`). The *conditional* join
  — the non-equi nested loop — does not JIT at all: it parses the predicate into
  an AST on the host and ships the parsed form to the device
  (`cpp/src/join/conditional_join.cu:61-62`,
  `ast::detail::expression_parser{binary_predicate, left, right, …}`), then
  interprets it per pair, sizing shared memory from
  `parser.shmem_per_thread × threads_per_block` (`conditional_join.cu:70`).

Both halves of that are the topic 19 preview: an interpreted AST per pair is
this topic's cardinal sin, and the JIT path is what removing it looks like — for
one operator, so far.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| `cpp/src/join/join_common_utils.hpp:21` | `DEFAULT_JOIN_BLOCK_SIZE = 128` | 1 |
| `cpp/src/join/hash_join/size_impl.cuh:52-61` | the size pass = one cuco `count` | 2 |
| `cpp/src/join/hash_join/retrieve_impl.cuh:49-58, 71-85` | the retrieve pass, and the size pass being optional | 2 |
| `cpp/src/join/hash_join/inner_join_size.cu` / `inner_join_retrieve.cu` | 20 and 28 lines of instantiation — read them to see how little is here | 2 |
| `cpp/src/join/hash_join/hash_join_impl.cuh:50-57` | the multiset type: `double_hashing<2>` + `storage<2>` | 3 |
| `cpp/include/cudf/detail/join/join.hpp:12` | `DEFAULT_JOIN_CG_SIZE = 2` | 3 |
| `cpp/include/cudf/detail/join/distinct_hash_join.cuh:147-157` | the distinct path: `linear_probing<1>` + `storage<1>` | 3 |
| `cpp/include/cudf/detail/join/filtered_join.cuh:165-185` | bucket 1 for primitive rows, 4 for nested | 3 |
| `cpp/src/join/conditional_join_kernels.cuh:34-95` | shared-memory staging + one device atomic per warp | 4 |
| `cpp/src/join/conditional_join.cu:61-97` | AST parse, optional size kernel, `write_index` cursor | 4, 6 |
| `cpp/src/groupby/hash/helpers.cuh:19-46` | every group-by constant in Step 5 | 5 |
| `cpp/src/groupby/hash/compute_mapping_indices.cuh:50, 85-113` | the shared cuco set and the bail-out | 5 |
| `cpp/src/groupby/hash/compute_single_pass_aggs.cuh:111-122` | the host-side all-or-nothing fallback | 5 |
| `cpp/src/groupby/hash/compute_shared_memory_aggs.cu:268-277` | the shared-memory budget: occupancy API result, halved | 5 |
| `cpp/include/cudf/column/column_view.hpp:33-42` | Arrow conformance and the zero-copy slice offset | 6 |
| `cpp/src/bitmask/null_mask.cu` | the validity-bitmask kernels (there are only two files) | 6 |
| `cpp/src/join/jit/filter_join_kernel.cuh` | the one JIT'd join path | 6 |

Reading order: `inner_join_size.cu` and `inner_join_retrieve.cu` first — they are
tiny, and their emptiness is the lesson — then their two `_impl.cuh` headers,
then `hash_join_impl.cuh` and `distinct_hash_join.cuh` side by side for Step 3's
table. Then `conditional_join_kernels.cuh` end to end, which is the most
instructive single file here. `groupby/hash/` last, starting from `helpers.cuh`.

## Questions for notes.md

1. Count the device operations in one `inner_join`: build, size, retrieve (and
   the mapping pass if you go through the group-by path). At the 1544 µs floor
   this repo measured on Metal, what is the minimum probe-side row count that
   amortises them to under 10 %? Step 2 does the arithmetic for three
   dispatches — redo it for your count, and say what the same number would be on
   a CUDA device where a launch costs ~5 µs.
2. The size/retrieve pair probes twice. On Crystal's roofline, when is the second
   probe nearly free? (What does the V100's 6 MB L2 do for a hash table of
   1-4 MB — and what does §4.3's 14.5× regime tell you about that?) Then: what
   does `output_size` being an `optional` let a caller do about it?
3. Why does `conditional_join` interpret a device AST rather than hash, and why
   does the *filter* join get a JIT path when the conditional join does not?
4. cudf JIT-compiles that one kernel at runtime. What is the WGSL analogue for
   our engine — where in the wgpu ladder does "compile a shader specialised to
   this predicate" happen, and what does it cost per distinct predicate?
5. For M18: our `filter_count` stub's one-atomic-per-workgroup is only pass 1 of
   this pattern. Sketch pass 2 (compact the values, not just count them) using a
   workgroup prefix scan — and say which of Step 4's two atomic scopes WGSL can
   express.

## Done when

Answer each before unfolding it.

- [ ] You can explain the no-push rule and say precisely which pass cudf runs to get around it — including when it does not run.

  <details><summary>Answer</summary>

  Output buffers are allocated before launch, so the size must be known. cudf
  gets it from cuco's `count` / `count_outer` (`size_impl.cuh:52-61`) and then
  calls `retrieve` into a pair of `thrust::transform_output_iterator`s
  (`retrieve_impl.cuh:71-85`).

  But the count is skipped whenever the caller passes `output_size`
  (`retrieve_impl.cuh:49-58`) — which is why `hash_join::inner_join_size` is
  public. There is no hand-rolled per-thread count array or prefix scan at this
  pin; that shape is what cuco does internally.

  </details>

- [ ] You can state how many threads cooperate on one equi-join probe, and how many slots they examine per step.

  <details><summary>Answer</summary>

  Two and two: `cuco::double_hashing<DEFAULT_JOIN_CG_SIZE, h1, h2>` with
  `DEFAULT_JOIN_CG_SIZE = 2` (`join.hpp:12`) and `cuco::storage<2>`
  (`hash_join_impl.cuh:50-57`).

  The `distinct_hash_join` path uses `linear_probing<1>` and `storage<1>`
  (`distinct_hash_join.cuh:147-157`), and group-by uses 1 and 1
  (`helpers.cuh:19,22`). The only 4 in the join code is
  `nested_probing_scheme` for nested-typed rows (`filtered_join.cuh:183`).
  "A cooperative group of 4-8 threads" is not what this code does.

  </details>

- [ ] You can explain how `conditional_join` avoids one device atomic per output row, and quantify the saving.

  <details><summary>Answer</summary>

  Each warp stages its matches into a shared-memory cache using a **block**-scope
  `atomic_ref::fetch_add` (`conditional_join_kernels.cuh:41-45`), which never
  leaves the SM. On flush, lane 0 alone does a **device**-scope `fetch_add` of
  the whole staged count (`:74-77`) and shuffles the returned base to the other
  lanes (`:84`), which then write their rows at `base + i` (`:86-93`).

  Device-scope atomics therefore drop by the number of rows staged per flush.
  Same rule as Crystal's one-atomic-per-tile and the WGSL stub's
  one-`atomicAdd`-per-workgroup.

  </details>

- [ ] You can say what happens when a group-by block exceeds its cardinality threshold — and what it does *not* do.

  <details><summary>Answer</summary>

  A block that passes `GROUPBY_CARDINALITY_THRESHOLD = 128` distinct keys stops
  using its shared 366-slot cuco set and sets a device flag
  (`compute_mapping_indices.cuh:50`). The host copies that flag back,
  synchronises, and — if it is set — throws the shared-memory plan away and
  re-runs the *entire* aggregation with the global-memory kernel
  (`compute_single_pass_aggs.cuh:111-122`).

  It does not spill that block only, and it does not fall back per key. One bad
  block costs everyone, which is exactly why a high-cardinality group-by wants
  hash partitioning first.

  </details>

- [ ] You can name what is actually JIT-compiled in cudf's join code, and what the conditional join does instead.

  <details><summary>Answer</summary>

  JIT: the **filter** join only — `cpp/src/join/jit/filter_join_kernel.{cu,cuh}`,
  driven from `filter_join_indices/filter_join_indices_jit.cu`, which pulls in
  `jit/cache.hpp`, `jit/parser.hpp` and `jit/row_ir.hpp`.

  The conditional (non-equi) join parses its predicate into an AST on the host
  (`conditional_join.cu:61-62`) and interprets that AST per candidate pair on the
  device, sizing shared memory from `parser.shmem_per_thread × threads_per_block`
  (`:70`). Interpreted, not compiled.

  </details>

- [ ] You can explain why Arrow layout is not a portability choice here but a performance one.

  <details><summary>Answer</summary>

  `column_view` conforms to the Arrow Physical Memory Layout
  (`column_view.hpp:33-35`): dense values plus a validity bitmap. Dense values
  mean a warp's 32 loads land in one 128 B transaction; a row-store would strand
  most of every fetch. The bitmap means nulls are bit operations rather than
  per-row branches, and branches diverge warps. And the layout makes a slice
  zero-copy — the offset lives in the view (`:40-42`).

  Strings are the exception that proves it: offsets + chars means variable work
  per element, which is the ragged-work problem Gunrock's load-balancing
  strategies exist to solve.

  </details>

- [ ] You wrote answers to all five questions in `notes.md`, including the pass-2 compaction sketch.

  <details><summary>Answer</summary>

  The slots are `notes.md:71-77`. Question 1's answer needs a launch count *you*
  derived from the files, not the one in Step 2 — Step 2 counts three, and the
  group-by path is different.

  </details>

## References

**Code**

- [cudf](https://github.com/rapidsai/cudf) @ `2f082a7` (the pin). Route:
  `cpp/src/join/hash_join/` (`inner_join_size.cu`, `inner_join_retrieve.cu`, then
  `size_impl.cuh` and `retrieve_impl.cuh`) → `hash_join_impl.cuh` and
  `cpp/include/cudf/detail/join/distinct_hash_join.cuh` for the probing
  parameters → `cpp/src/join/conditional_join_kernels.cuh` for atomic
  amortisation → `cpp/src/groupby/hash/helpers.cuh` and its neighbours for the
  two-tier aggregation → `cpp/include/cudf/column/column_view.hpp` for the
  layout contract.
- cuco (CUDA Collections) is where `count`, `retrieve`, `insert_async` and the
  probing schemes actually live. It is **not** pinned in this repo, so no line in
  this guide points inside it; if you need its semantics, pin it first.

**Papers**

- Crystal (SIGMOD 2020) §3.2 and §4.3 for the two numbers this guide borrows:
  one atomic per tile, and the 128 B vs 64 B read granularity that makes random
  probes twice as expensive on a GPU. See `reading-crystal-sigmod20.md`.

**Measurements in this repo**

- `topics/18-gpu/notes.md:11-16` — the 1544 µs dispatch floor and the 12.5 GB/s
  streaming figure Step 2's arithmetic uses. Both are wgpu/Metal numbers; a CUDA
  launch is orders of magnitude cheaper, and the guide says so where it matters.
