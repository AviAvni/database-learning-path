# wgpu compute: the 1.5 ms tax before your first FLOP

The portable GPU-compute stack our experiments use: WGSL shaders →
naga → Metal on this Mac, Vulkan/DX12 elsewhere. Before you open the
examples, this chapter builds the concepts one at a time — what a
dispatch actually consists of, where the fixed ~1.5 ms goes, what
WGSL can and cannot express, and which hard limits bite — each step
fixing one naivety of the previous. Then it maps every step to the
example directory that demonstrates it.

## The problem in one sentence

On this machine, summing 16K floats takes the CPU **2 µs** and the
GPU **1619 µs** — the GPU spends ~1.5 ms on plumbing before the
first FLOP, so the only interesting question is which operators ever
amortize that tax.

## The concepts, step by step

### Step 1 — a dispatch is a ladder of objects, not a function call

Running code on a GPU is not `f(x)` — it is building a chain of
objects that describe the device, the code, the data, and the
submission, then waiting for an asynchronous queue. wgpu's ladder,
top to bottom:

```
 Instance  — loads Metal/Vulkan/DX12
   └ Adapter  — one physical GPU; limits + features live here
      └ Device — the logical connection; creates ALL resources
        Queue  — where encoded work is submitted
 Buffer(STORAGE)          — GPU-side data
 Buffer(MAP_READ|COPY_DST)— the ONLY way back to the host
 ShaderModule (WGSL) → ComputePipeline (entry point + layout)
 BindGroup — binds buffers to @group/@binding slots
 CommandEncoder → ComputePass → dispatch_workgroups(x,y,z)
 submit → poll → map_async → read
```

Vocabulary for the rest of the chapter: a **shader** is the GPU
program (written in **WGSL**, WebGPU's shading language); a
**pipeline** is the compiled shader plus its resource layout; a
**dispatch** launches the shader over a 3D grid of **workgroups**
(blocks of threads that share fast scratch memory — the WebGPU name
for CUDA's thread block); results come back only by copying into a
mappable buffer and polling. Why it matters: every rung of this
ladder has a cost, and only some rungs can be paid once instead of
per call — that split is Steps 2 and 3.

### Step 2 — the fixed tax: ~1.5 ms per dispatch before any work

Encoding commands, submitting to the queue, Metal's command-buffer
scheduling, and the completion poll together cost about 1.5 ms on
this Mac — *independent of data size*. The hello_compute doc-comment
says it outright: for trivial math "running on the gpu is slower
than doing the same calculation on the cpu... transfer/submission
overhead is quite a lot higher than the actual computation." Our
gpu_bench measured that sentence:

```
 sum of n f32 — CPU 8-acc autovec vs GPU workgroup reduction:
 n=16K    CPU     2 µs   GPU  1619 µs   ← ~1.5 ms FIXED dispatch cost
 n=4M     CPU   589 µs   GPU  4555 µs
 n=16M    CPU  2258 µs   GPU 14333 µs   ← no crossover, ever
```

A memory-bound operator on unified memory never wins: CPU and GPU
see the same ~150–400 GB/s pool, so the GPU's only edge is FLOPs a
sum doesn't need. The tax means any candidate operator needs either
high arithmetic intensity (FLOPs per byte) or a huge batch.
Question: break down the 1.5 ms — encode, submit, Metal
command-buffer scheduling, poll — which part would a persistent
command buffer (repeated_compute) remove?

### Step 3 — amortize what you can: setup once, dispatch many

Of the Step 1 ladder, the expensive top rungs — instance/adapter/
device creation and shader compilation into a pipeline — are
one-time costs, and our `GpuCtx` already hoists them: per-call cost
is only buffer create + bind + encode + submit. The
repeated_compute example goes further and reuses *buffers* across
iterations too — which is exactly Crystal's regime A → regime B
move (data resident on the device, only the dispatch per call).
Question: rewrite GpuCtx::sum to take pre-uploaded input (upload
once, dispatch many) — how does the crossover table change? This is
expressible in ~15 lines.

### Step 4 — WGSL is CUDA with the sharp edges filed off

Every CUDA concept from the papers has a WGSL name, and two have no
WGSL equivalent at all:

| CUDA | WGSL | note |
|---|---|---|
| `__global__` kernel | `@compute @workgroup_size(N) fn` | size fixed at pipeline creation |
| blockIdx/threadIdx | `@builtin(workgroup_id / local_invocation_id)` | |
| `__shared__` | `var<workgroup>` | our sum.wgsl scratch |
| `__syncthreads()` | `workgroupBarrier()` | workgroup-scope only |
| warp shuffles | subgroup ops (feature-gated) | portable fallback: shared memory |
| atomicAdd | `atomicAdd(&x, v)` on `atomic<u32/i32>` | NO float atomics in core WGSL |

The two DB-relevant gaps: **no float atomics** (you cannot
`atomicAdd` an f32 — aggregate via u32-bitcast CAS loops or
per-workgroup partials) and **no device-wide barrier** (threads in
different workgroups can never synchronize inside one dispatch).
Steps 5 and 6 show what each gap forces.

### Step 5 — the reduction shape forced by "no float atomics"

To sum n floats without a float `atomicAdd`, each thread folds a
strided slice into a register, the workgroup tree-reduces those
partials in shared memory (`var<workgroup>`), and exactly one
thread writes one partial per workgroup — our sum.wgsl:

```rust
// sum.wgsl's shape: fold in registers, tree-reduce in shared memory,
// ONE partial per workgroup — because WGSL has no float atomicAdd
var<workgroup> scratch: array<f32, WG>;

@compute @workgroup_size(WG)
fn sum(gid: u32, lid: u32) {
    var acc = 0.0;
    for (var i = gid; i < n; i += stride) { acc += input[i]; }  // coalesced
    scratch[lid] = acc;
    workgroupBarrier();
    for (var s = WG / 2u; s > 0u; s >>= 1u) {   // tree reduction
        if (lid < s) { scratch[lid] += scratch[lid + s]; }
        workgroupBarrier();
    }
    if (lid == 0u) { partials[workgroup_id] = scratch[0]; }
}   // second dispatch (or CPU) folds the partials — no device barrier
```

The strided load (`i += stride` where stride = total thread count)
keeps adjacent threads on adjacent addresses — coalesced. The design
is not a style choice; it is the only shape the language permits,
and it is also the right shape (one atomic-free partial per group =
Crystal's "amortize atomics" rule).

### Step 6 — no device-wide barrier: multi-pass = multiple dispatches

Because workgroups cannot synchronize with each other, any algorithm
with a global "everyone finished phase 1" point must end the
dispatch and start another — the folding of sum.wgsl's partials is a
second dispatch (or the CPU), and a BFS runs one dispatch per level,
each paying Step 2's submission cost. Question: what does the
no-device-barrier rule do to the stretch-goal BFS (frontier per
dispatch — where does the frontier size live)?

### Step 7 — the limits that bite at real data sizes

Two defaults matter for database-sized inputs: a single storage
buffer binding maxes out at **128 MB**
(`max_storage_buffer_binding_size`), and a dispatch allows at most
**65535 workgroups per dimension**. At n = 2²⁴ f32 (64 MB) with
256-thread workgroups you'd already need 65536 groups — our sum
kernel folds 4 elements per thread partly to stay under that limit.
Question: at what n does the 128 MB limit break GpuCtx::sum, and
what's the fix (request higher limits at device creation vs chunked
dispatches)?

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| examples/standalone/01_hello_compute/ | the full plumbing, heavily commented — read FIRST | 1–2 |
| examples/standalone/01_hello_compute/src/shader.wgsl | minimal WGSL compute entry | 1, 4 |
| examples/features/src/repeated_compute/ | amortizing setup across dispatches (what our GpuCtx does) | 3 |
| examples/features/src/hello_workgroups/ | workgroup semantics + shared memory | 4–5 |
| examples/features/src/hello_synchronization/ | barriers + atomics | 4–6 |
| examples/features/src/big_compute_buffers/ | >128 MB data — chunking around limits | 7 |

Read in that order: hello_compute end to end (every rung of Step 1's
ladder appears once, commented), then repeated_compute (diff it
against hello_compute — what moved out of the loop is exactly
Step 3's amortizable set), then the workgroups/synchronization pair
next to sum.wgsl, and big_compute_buffers only when Step 7 bites.

## Questions for notes.md

1. Measure: GpuCtx::sum with upload hoisted out (regime B). Does
   the GPU beat 2258 µs CPU at n=16M now? Predict first.
2. Why does WGSL make workgroup_size a compile-time pipeline
   constant while CUDA takes it at launch (hint: what can the
   compiler do with a known size — our scratch array)?
3. The readback in our sum is 3-19 µs — tiny. Why is upload so much
   worse (staging copy through a private buffer even on unified
   memory — find the wgpu buffer-mapping discussion)?
4. Subgroup (warp) ops vs shared-memory reduction: rewrite
   sum.wgsl's tree loop with subgroupAdd — how many barriers
   disappear?
5. For M18: the feature flag should gate at the operator boundary.
   Which signature do you expose: `sum(&[f32])` (per-call upload,
   regime A) or `upload(&[f32]) -> GpuVec` + `sum(&GpuVec)` (regime
   B)? Justify from this guide's measurements.

## References

**Code**
- [wgpu](https://github.com/gfx-rs/wgpu) — `examples/` — read in
  order: `standalone/01_hello_compute/` (the full plumbing, heavily
  commented — its doc-comment admits the overhead out loud),
  `features/src/repeated_compute/` (amortizing setup — what our
  GpuCtx does), then `hello_workgroups` / `hello_synchronization` /
  `big_compute_buffers` as needed
