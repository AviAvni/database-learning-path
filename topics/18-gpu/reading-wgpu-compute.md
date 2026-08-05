# wgpu compute: the 1.5 ms tax before your first FLOP

This is the only guide in topic 18 whose code you can actually run. The other
five read CUDA that needs an NVIDIA device; this machine is an Apple-silicon Mac
with none, so wgpu talking to Metal is the whole of the reader's hardware. That
turns out to be enough, because the thing topic 18 is about — the cost of
crossing the host/device boundary — is *worse* on a portable API, not absent
from it, and it is measurable here to the microsecond.

The chapter builds four ideas in order: what a dispatch is made of, where the
fixed ~1.5 ms goes, what WGSL cannot express and what that forces on your kernel
shapes, and which hard limits bite at database-sized inputs. Each step fixes one
naivety of the previous one.

Every wgpu anchor below is [gfx-rs/wgpu@f945c78](https://github.com/gfx-rs/wgpu)
— the revision pinned in `resources/codebases.md`. Check any of them with
`python3 tools/pinned-source.py show wgpu <path> -r A:B`. One mismatch to know
about before you start: this topic's crate builds against the released
`wgpu = "23"` (`experiments/Cargo.toml`), so `src/gpu.rs:130` says
`device.poll(wgpu::Maintain::Wait)` while the pinned tree spells the same act
`device.poll(PollType::wait_indefinitely())`. Same rung of the ladder, renamed
between releases; the concepts are unchanged.

## The problem in one sentence

On this machine, summing 16K floats takes the CPU **2.3 µs** and the GPU
**1618.9 µs**, of which 1567.1 µs is encode/submit/poll with no data in it
(`notes.md:11`) — so the only interesting question in the whole topic is which
operators ever move enough work to amortize a fixed millisecond-and-a-half.

## The concepts, step by step

### Step 1 — a dispatch is a ladder of objects, not a function call

> **In:** a `&[f32]` on the host and a WGSL source string.
> **Out:** a `Vec<f32>` back on the host, plus roughly a dozen live objects you
> had to construct in a fixed order to get it. Nothing here is `f(x)`.

Vocabulary, defined once and used for the rest of topic 18. A **shader** is the
program the GPU runs, written in **WGSL** (WebGPU's shading language). A
**pipeline** is that shader compiled together with the layout of the resources
it reads and writes. A **dispatch** launches the shader over a 3-D grid of
**workgroups**; a workgroup is a block of **invocations** (threads) that share
one fast scratch memory and can synchronise with each other — WebGPU's name for
what CUDA calls a thread block. Results reach the host only by copying into a
buffer created with `MAP_READ`, then mapping it.

The ladder, as `01_hello_compute` climbs it. This is the shortest complete wgpu
compute program in the tree (254 lines, most of them comments), and every rung
appears exactly once:

```
Instance         main.rs:41   loads Metal/Vulkan/DX12
 └ Adapter       main.rs:48   one physical GPU; limits and features live here
    └ Device     main.rs:69   logical connection — creates ALL resources
      Queue      main.rs:69   returned with it; where work is submitted
ShaderModule     main.rs:83   WGSL parsed and validated
Buffer(STORAGE)  main.rs:91   input, filled via create_buffer_init
Buffer(STORAGE|COPY_SRC)      main.rs:98   output the kernel writes
Buffer(MAP_READ|COPY_DST)     main.rs:108  the ONLY road back to the host
BindGroupLayout  main.rs:118  what @group(0) @binding(n) will mean
BindGroup        main.rs:152  the actual buffers bound to those slots
PipelineLayout   main.rs:168
ComputePipeline  main.rs:176  shader + entry point + layout, compiled
CommandEncoder   main.rs:186
 └ ComputePass   main.rs:191  set_pipeline, set_bind_group,
                              dispatch_workgroups(...)   main.rs:207
copy_buffer_to_buffer  main.rs:214   storage → mappable
encoder.finish()       main.rs:223   → CommandBuffer
queue.submit()         main.rs:230
map_async + poll       main.rs:238, 245
get_mapped_range       main.rs:248   bytes, at last
```

Two details on that ladder are worth stopping on. The dispatch argument is a
*workgroup count*, not a thread count — `01_hello_compute` computes it as
`arguments.len().div_ceil(64)` (main.rs:206) because its shader declares
`@workgroup_size(64)`. And the device is requested with an explicit limits
struct (`required_limits: wgpu::Limits::downlevel_defaults()`, main.rs:72),
which is Step 7's subject: limits are a negotiation at device creation, not a
property of the hardware you discover later.

### Step 2 — the fixed tax: ~1.5 ms per dispatch before any work

> **In:** the ladder of Step 1, run once per call on inputs from 2¹⁴ to 2²⁴ f32.
> **Out:** a phase-split timing table in which one column does not move.

wgpu's own example says it out loud, in the file's doc comment:

```rust
// examples/standalone/01_hello_compute/src/main.rs:8-12 — the module doc comment,
// quoted whole. This is the topic's thesis, written by the API's authors.
     8  /// If you time the recording and execution of this example you will certainly see that
     9  /// running on the gpu is slower than doing the same calculation on the cpu. This is because
    10  /// floating point multiplication is a very simple operation so the transfer/submission overhead
    11  /// is quite a lot higher than the actual computation. This is normal and shows that the GPU
    12  /// needs a lot higher work/transfer ratio to come out ahead.
```

`gpu_bench` measures that sentence. From `notes.md:11-16` (Apple M3 Pro, wgpu →
Metal, 2026-07-10, 5-rep averages, µs):

```
 n      CPU      GPU total   upload   kernel+submit   readback
 16K      2.3      1618.9      48.5        1567.1        3.2
 64K      9.2      1633.5      69.6        1560.8        3.1
 256K    36.8      1701.5     151.8        1547.1        2.6
 1M     154.4      1985.5     437.3        1544.2        4.0
 4M     588.6      4554.8    1654.8        2887.2       12.8
 16M   2257.7     14332.9    7384.7        6929.1       19.1
```

Read the `kernel+submit` column downwards for 16K → 1M: 1567.1, 1560.8, 1547.1,
1544.2. The input grew **64×** and that column *fell* by 23 µs. Nothing in it is
work; it is encode, submit, Metal's command-buffer scheduling, and the
completion poll — a floor of about **1544 µs** that every dispatch pays.

Now subtract the floor to recover the actual kernel throughput at the top end.
64 MiB of f32 is 2²⁴ × 4 = 67,108,864 bytes:

```
  work time at 16M   = 6929.1 µs − 1544.2 µs floor = 5384.9 µs
  effective read BW  = 67,108,864 B / 5384.9e-6 s  = 12.5 GB/s
  CPU, same bytes    = 67,108,864 B / 2257.7e-6 s  = 29.7 GB/s
```

So even with the tax removed the GPU reads *slower* than the CPU here. That is
the second, deeper reason for "no crossover": on unified memory both processors
pull from the same pool, so there is no bandwidth ratio to win — which is
exactly the premise Crystal's 16× speedups rest on and this machine does not
have (`reading-crystal-sigmod20.md`, Step 4).

`FINDINGS.md:36` records a different run of the same lane — 7197 µs upload
against a 2723 µs CPU total at 16 M. Quote whichever file you took the number
from; they are separate runs, not a contradiction.

### Step 3 — amortize what you can: setup once, dispatch many

> **In:** the ladder of Step 1 and the floor of Step 2.
> **Out:** a partition of the ladder into rungs paid once per process and rungs
> paid once per call — and the knowledge that the per-call set is the one that
> costs 1.5 ms.

`GpuCtx` already hoists the top of the ladder. `GpuCtx::try_new`
(`experiments/src/gpu.rs:52-83`) builds the instance, adapter, device, queue,
shader module and pipeline once and stores three of them; `GpuCtx::sum`
(`gpu.rs:88-144`) then does, per call:

```
 upload phase   gpu.rs:92-110    create_buffer_init(input) + 2 create_buffer
 dispatch phase gpu.rs:112-131   bind group, encoder, pass, dispatch, copy,
                                 submit, poll        ← Step 2's ~1544 µs lives here
 readback phase gpu.rs:133-141   map_async, poll, get_mapped_range
```

Bind group and pipeline layout are cheap; the floor is the submit-and-wait pair.
Which means the amortization that matters is not "build fewer objects" but
"submit fewer times, and upload once". The wgpu example that shows the first is
`features/src/repeated_compute/`; the second is what Crystal calls moving from
regime A (data streamed per query) to regime B (data resident) — and it is
Question 1 below, which you should predict before measuring. Arithmetic for the
prediction: hoisting upload out of the 16 M row removes 7384.7 µs from 14332.9,
leaving 6948.2 µs against the CPU's 2257.7 — still 3.1× behind, because Step 2
showed the kernel itself is the slower reader. Hoisting alone cannot fix a
bandwidth deficit.

### Step 4 — WGSL is CUDA with the sharp edges filed off

> **In:** the CUDA vocabulary the other five guides use.
> **Out:** the WGSL spelling of each, and the two constructs that have no
> spelling at all — which is what Steps 5 and 6 are about.

| CUDA | WGSL | note |
|---|---|---|
| `__global__` kernel | `@compute @workgroup_size(N) fn` | N is fixed when the pipeline is created, not at launch |
| `blockIdx` / `threadIdx` | `@builtin(workgroup_id)` / `@builtin(local_invocation_id)` | `sum.wgsl:15-16` takes both |
| `__shared__` | `var<workgroup>` | `sum.wgsl:9` |
| `__syncthreads()` | `workgroupBarrier()` | workgroup scope only — see Step 6 |
| warp shuffle (`__shfl_sync`) | subgroup operations | feature-gated; portable fallback is shared memory |
| `atomicAdd(float*)` | — | **no float atomics in core WGSL**; `atomic<u32>` / `atomic<i32>` only |
| grid-wide sync (cooperative groups) | — | **does not exist**; end the dispatch instead |

The two blanks are not portability pedantry — they change what a kernel can
look like. Step 5 is what the first blank forces, Step 6 the second.

### Step 5 — the reduction shape forced by "no float atomics"

> **In:** n f32 in a storage buffer, and a language with no float `atomicAdd`.
> **Out:** one f32 partial per workgroup, and a second pass (or the host) to
> fold the partials.

Every invocation folds a strided slice into a register, the workgroup
tree-reduces those registers through `var<workgroup>` scratch, and exactly one
invocation writes one partial. Our whole kernel, with its real line numbers:

```wgsl
// experiments/src/shaders/sum.wgsl:9-39 — declarations and both loops; the
// @group/@binding lines (6-7) and the closing brace are elided.
     9  var<workgroup> scratch: array<f32, 256>;
    11  const WG: u32 = 256u;
    12  const PER_THREAD: u32 = 4u;
    14  @compute @workgroup_size(256)
    15  fn main(@builtin(local_invocation_id) lid: vec3<u32>,
    16          @builtin(workgroup_id) wid: vec3<u32>) {
    17      let n = arrayLength(&input);
    18      let base = wid.x * WG * PER_THREAD + lid.x;
    19      var v = 0.0;
    20      for (var k = 0u; k < PER_THREAD; k = k + 1u) {
    21          let i = base + k * WG;            // stride WG — see below
    22          if (i < n) {
    23              v = v + input[i];
    24          }
    25      }
    26      scratch[lid.x] = v;
    27      workgroupBarrier();
    29      var stride = WG / 2u;                 // tree reduction
    30      while (stride > 0u) {
    31          if (lid.x < stride) {
    32              scratch[lid.x] = scratch[lid.x] + scratch[lid.x + stride];
    33          }
    34          workgroupBarrier();
    35          stride = stride / 2u;
    36      }
    37      if (lid.x == 0u) {
    38          partials[wid.x] = scratch[0u];
    39      }
```

Three things to notice, each of which is a rule the other guides will restate in
CUDA. **Coalescing**: at step k, invocation `lid.x` reads `base + k*256`, so the
256 invocations of a workgroup touch 256 *adjacent* f32 — one contiguous 1 KiB
run per step, which is the access pattern every GPU memory system is built for.
The obvious alternative (`lid.x * 4 + k`, each thread taking a contiguous quad)
would have 256 threads touching 256 addresses 16 bytes apart, and is the classic
way to lose most of your bandwidth. **Barrier discipline**: the barrier at line
34 is inside the loop but outside the `if` — every invocation in the workgroup
must reach it, so it can never sit under a divergent condition. **Atomic
amortization**: one write per workgroup, not one per element. That is the same
rule libcudf applies in `conditional_join_kernels.cuh` and CAGRA applies to its
visited table, and it is the rule the `filter_count` stub asks you to implement
with a real `atomicAdd` (`gpu.rs:146-156`).

Check the shared-memory cost against the limit while you are here:
`array<f32, 256>` is 256 × 4 = **1024 bytes** per workgroup, against a default
`max_compute_workgroup_storage_size` of **16384** bytes
(`wgpu-types/src/limits.rs:451`). And `@workgroup_size(256)` is not a round
number chosen for looks — it is exactly `max_compute_invocations_per_workgroup`
and `max_compute_workgroup_size_x` (limits.rs:452-453). The kernel sits on the
ceiling of the portable defaults in one dimension while using 1/16th of the
budget in the other.

### Step 6 — no device-wide barrier: multi-pass means multiple dispatches

> **In:** an algorithm with a global "everyone has finished phase 1" point.
> **Out:** one dispatch per phase, each paying Step 2's floor again.

`workgroupBarrier()` synchronises the 256 invocations of one workgroup. Nothing
synchronises workgroup 0 with workgroup 16383: they may run concurrently, or
sequentially, or overlapped, and the language deliberately refuses to say. So a
phase boundary can only be expressed by ending the dispatch.

This is why `sum` is not finished when the kernel returns: 16384 partials come
back to the host and are folded there (`gpu.rs:137-140`). A second dispatch
would be the alternative, and at 16384 f32 it would obviously lose — 1544 µs of
floor to add 64 KiB of numbers the host adds in microseconds. The rule
generalises badly for graphs: a BFS is one dispatch per level, so a
9-level traversal pays the floor nine times — 9 × 1544 µs = 13.9 ms of pure
submission before counting a single edge. Topic 13's LDBC-shaped graphs have
diameters in that range, which is why the stretch-goal BFS in this topic's
`notes.md:48` is a warning as much as an exercise.

### Step 7 — the limits that bite at real data sizes

> **In:** `wgpu::Limits::default()` (what `DeviceDescriptor::default()` requests
> at `gpu.rs:64`) and an input of n f32.
> **Out:** the two n at which this program stops working, and which fix each one
> needs.

Two defaults decide it (`wgpu-types/src/limits.rs`, values verified at the pin):

| limit | default | line |
|---|---|---|
| `max_storage_buffer_binding_size` | 128 MiB (`128 << 20`) | limits.rs:441 |
| `max_buffer_size` | 256 MiB (`256 << 20`) | limits.rs:443 |
| `max_compute_workgroups_per_dimension` | 65535 | limits.rs:456 |
| `max_compute_invocations_per_workgroup` | 256 | limits.rs:452 |
| `max_compute_workgroup_storage_size` | 16384 B | limits.rs:451 |

Work out where each one lands, with `ELEMS_PER_GROUP = WG × PER_THREAD = 256 × 4
= 1024` (`gpu.rs:8-10`) and `n_groups = ceil(n / 1024)` (`gpu.rs:90`):

```
 storage binding:  128 MiB / 4 B  = 33,554,432 = 2^25 elements
                   n = 2^24 (64 MiB)   → fits, half the budget
                   n = 2^25 (128 MiB)  → exactly at the limit
                   n = 2^26 (256 MiB)  → rejected at bind time

 workgroup count:  65535 × 1024   = 67,107,840 elements ≈ 2^26
                   n = 2^24 → 16,384 groups.  4× under the cap.

 the same kernel WITHOUT the 4-element fold (PER_THREAD = 1):
                   65535 × 256    = 16,776,960 elements
                   n = 2^24       = 16,777,216 → 65,536 groups
                   short by 256 elements — one workgroup over the cap,
                   at exactly the size this topic's bench tops out at.
```

So `PER_THREAD = 4` is not a tuning knob, it is what keeps the largest measured
size legal; and the limit that actually ends the program is the 128 MiB storage
binding, at n > 2²⁵. The two fixes differ in kind: request a higher
`max_storage_buffer_binding_size` at device creation (portable only if the
adapter reports it — this is what `Limits` negotiation is *for*), or chunk the
input across several dispatches, which re-pays Step 2's floor per chunk. Note
that `downlevel_defaults()` — what `01_hello_compute` asks for at main.rs:72 —
keeps all of the above except workgroup storage, which drops to 16352 B
(limits.rs:521). Asking for less does not buy you a bigger buffer.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| `examples/standalone/01_hello_compute/src/main.rs:41-248` | the entire ladder, once, commented — read FIRST | 1 |
| `examples/standalone/01_hello_compute/src/main.rs:8-12` | the doc comment that admits the overhead | 2 |
| `examples/standalone/01_hello_compute/src/shader.wgsl` | minimal WGSL compute entry point | 1, 4 |
| `examples/features/src/repeated_compute/` | setup amortised across dispatches | 3 |
| `examples/features/src/hello_workgroups/` | workgroup id / shared memory semantics | 4-5 |
| `examples/features/src/hello_synchronization/` | barriers and atomics | 5-6 |
| `examples/features/src/big_compute_buffers/` | data past one binding — chunking | 7 |
| `wgpu-types/src/limits.rs:404-476` | `Limits::default()`, the numbers in Step 7 | 7 |
| this topic: `experiments/src/gpu.rs:88-144` | the measured `sum`, phase by phase | 2-3 |
| this topic: `experiments/src/shaders/sum.wgsl` | the kernel of Step 5 | 5 |

Read in that order. `01_hello_compute` end to end first — it is short and every
rung of Step 1 appears once with a comment. Then diff `repeated_compute` against
it: what moved out of the loop is precisely Step 3's amortizable set. Then the
workgroups/synchronization pair alongside `sum.wgsl`, and `big_compute_buffers`
only once Step 7 bites you.

## Questions for notes.md

1. Measure: `GpuCtx::sum` with the upload hoisted out (regime B). Does the GPU
   beat 2257.7 µs at n = 16M now? Predict first — Step 3 does the subtraction
   for you, so commit to the answer before running it.
2. Why does WGSL make `workgroup_size` a compile-time pipeline constant while
   CUDA takes the block size at launch? (Hint: `sum.wgsl:9` sizes `scratch` from
   the same constant. What can the compiler do with a workgroup size it knows,
   and what would it have to do without one?)
3. Readback at 16M is 19.1 µs against 7384.7 µs of upload — but compute both per
   byte before concluding anything, remembering that readback moves 16384
   partials (64 KiB) and upload moves 64 MiB. Then explain the *real* asymmetry:
   why does wgpu stage the upload through a private buffer even on unified
   memory?
4. Subgroup (warp) operations vs the shared-memory tree: rewrite `sum.wgsl`'s
   loop at lines 29-36 using `subgroupAdd`. How many `workgroupBarrier()` calls
   disappear, and what does the shader now require of the adapter?
5. For M18 the feature flag should gate at the operator boundary. Which
   signature do you expose — `sum(&[f32])` (per-call upload, regime A) or
   `upload(&[f32]) -> GpuVec` plus `sum(&GpuVec)` (regime B)? Justify it from
   Step 3's arithmetic, not from taste.

## Done when

Answer each before unfolding it.

- [ ] You can list the object ladder a dispatch requires and say which rungs can be hoisted out of a loop — and which hoist does *not* help.

  <details><summary>Answer</summary>

  Instance → Adapter → Device+Queue → ShaderModule → ComputePipeline are
  per-process (main.rs:41-176; `GpuCtx::try_new` hoists exactly these,
  gpu.rs:52-83). Buffers, bind group, encoder, pass, submit and map are
  per-call (gpu.rs:92-141).

  The trap: hoisting the *objects* is not where the money is. Step 2's floor
  lives in submit-and-poll, and Step 3's subtraction shows that removing the
  entire 7384.7 µs upload at 16M still leaves 6948.2 µs against a 2257.7 µs
  CPU. The only hoists that change the verdict are "submit fewer times" and
  "keep the data resident", and on unified memory even both together are not
  enough for a streaming reduction.

  </details>

- [ ] You can state the fixed per-dispatch tax and show the evidence that it is fixed rather than work.

  <details><summary>Answer</summary>

  ~1544 µs. Evidence: the `kernel+submit` column of `notes.md:11-14` reads
  1567.1 → 1560.8 → 1547.1 → 1544.2 µs while n goes 16K → 64K → 256K → 1M. A
  64× increase in data with a 23 µs *decrease* in time is not work; it is
  encode + submit + Metal command-buffer scheduling + the completion poll.
  Above 1M the column finally starts to climb (2887.2 at 4M, 6929.1 at 16M)
  because real work has at last become the larger term.

  </details>

- [ ] You can explain why the absence of float atomics forces the tree-reduction shape, and why that shape is the right one anyway.

  <details><summary>Answer</summary>

  WGSL has `atomic<u32>` and `atomic<i32>` only, so there is no way for 4 M
  invocations to accumulate into one f32. The portable route is: fold into a
  register, tree-reduce through `var<workgroup>` scratch with a barrier per
  halving (`sum.wgsl:26-36`), and have invocation 0 write one partial
  (`sum.wgsl:37-39`).

  It is also what you would write if float atomics existed: 16384 conflicting
  atomics beat 16.7 M of them by four orders of magnitude, and the tree costs
  log₂(256) = 8 barrier-separated steps on data already in scratch. Amortising
  atomics per workgroup is the same rule libcudf uses for its join output
  (`conditional_join_kernels.cuh:74-77`, one device-scope `fetch_add` per
  warp).

  </details>

- [ ] You can say why there is no device-wide barrier and what multi-pass therefore costs.

  <details><summary>Answer</summary>

  Workgroups are scheduled independently and may not be resident at the same
  time, so a grid-wide barrier could deadlock; WebGPU simply does not offer
  one. A phase boundary therefore means ending the dispatch. Cost: Step 2's
  ~1544 µs floor per phase. A 9-level BFS pays 9 × 1544 µs ≈ 13.9 ms of
  submission before any edge is counted — which is why `sum` folds its 16384
  partials on the host (`gpu.rs:137-140`) rather than launching a second
  kernel.

  </details>

- [ ] You can state, per byte, how upload and readback actually compare in the measured table — and resist the obvious misreading.

  <details><summary>Answer</summary>

  At 16M (`notes.md:16`): upload 7384.7 µs for 67,108,864 B = **0.110 ns/B**;
  readback 19.1 µs for 16384 × 4 = 65,536 B = **0.291 ns/B**. Per byte the
  readback is 2.6× *worse*. "Upload dominates" is true of the totals only
  because upload moves 1024× more bytes — the reduction's entire job is to
  shrink the return trip.

  The upload number is itself the honest one to quote for transfer cost:
  67,108,864 B / 7384.7 µs = 9.1 GB/s on a machine whose CPU reads the same
  bytes at 29.7 GB/s. `FINDINGS.md:36` records 7197 µs for the same phase on a
  different run; cite whichever file you read.

  </details>

- [ ] You can name the two limits that end this program and the n at which each does it.

  <details><summary>Answer</summary>

  `max_storage_buffer_binding_size` = 128 MiB (limits.rs:441) → 2²⁵ f32; the
  next power of two, 2²⁶, is rejected when the bind group is created.
  `max_compute_workgroups_per_dimension` = 65535 (limits.rs:456) → 65535 × 1024
  = 67,107,840 elements with the current `PER_THREAD = 4`, so it does not bind
  first. It *would* have: at `PER_THREAD = 1` the cap is 65535 × 256 =
  16,776,960, and n = 2²⁴ = 16,777,216 needs 65,536 groups — over by one, at
  exactly the largest size the bench runs.

  Fixes: negotiate a higher limit at `request_device` (adapter permitting), or
  chunk and pay the floor per chunk.

  </details>

- [ ] You wrote answers to all five questions in `notes.md`, including the regime-B rerun with upload hoisted out.

  <details><summary>Answer</summary>

  The slots are `notes.md:63-69`. Question 1's number also belongs in the
  prediction table at `notes.md:39`, where the prediction column must be filled
  in *before* the measurement.

  </details>

## References

**Code**

- [wgpu](https://github.com/gfx-rs/wgpu) @ `f945c78` (the pin) — read in order:
  `examples/standalone/01_hello_compute/src/main.rs` (the full ladder, and the
  doc comment at lines 8-12 that admits the overhead),
  `examples/features/src/repeated_compute/` (what amortising looks like),
  `examples/features/src/hello_workgroups/` and `hello_synchronization/`
  (workgroup semantics, barriers, atomics), `examples/features/src/
  big_compute_buffers/` (life past one binding), and
  `wgpu-types/src/limits.rs:404-476` for every number in Step 7.
- This topic: `experiments/src/gpu.rs` and `experiments/src/shaders/sum.wgsl` —
  the code the measurements come from.

**Spec**

- [WebGPU](https://www.w3.org/TR/webgpu/) and
  [WGSL](https://www.w3.org/TR/WGSL/) — the normative source for "no float
  atomics" and for workgroup-scope-only barriers. wgpu's limits are the spec's
  `supported limits` defaults; when an adapter and the spec disagree, the
  adapter wins and `request_device` fails.

**Measurements**

- `notes.md:9-16` — the phase-split table every number in Steps 2, 3 and 7 is
  taken from (Apple M3 Pro, wgpu → Metal, 2026-07-10).
- `FINDINGS.md:36` — the headline row, from a separate run of `./verify.sh 18`.
