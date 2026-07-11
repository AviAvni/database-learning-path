# Reading guide — wgpu compute examples

Clone: [`~/repos/wgpu`](https://github.com/gfx-rs/wgpu) (`examples/`). The portable GPU-compute stack
our experiments use: WGSL shaders → naga → Metal on this Mac,
Vulkan/DX12 elsewhere. Read three examples in order; each fixes one
naivety of the previous.

## Anchor map

| anchor | what it is |
|---|---|
| examples/standalone/01_hello_compute/ | the full plumbing, heavily commented — read FIRST |
| examples/features/src/repeated_compute/ | amortizing setup across dispatches (what our GpuCtx does) |
| examples/features/src/hello_workgroups/ | workgroup semantics + shared memory |
| examples/features/src/hello_synchronization/ | barriers + atomics |
| examples/features/src/big_compute_buffers/ | >128 MB data — chunking around limits |
| examples/standalone/01_hello_compute/src/shader.wgsl | minimal WGSL compute entry |

## 1. The object ladder (hello_compute)

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

The hello_compute doc-comment says it outright: for trivial math
"running on the gpu is slower than doing the same calculation on the
cpu... transfer/submission overhead is quite a lot higher than the
actual computation." Our gpu_bench measured that sentence: ~1.5 ms
per dispatch, no crossover for sum up to 16M elements. Question:
break down the 1.5 ms — encode, submit, Metal command-buffer
scheduling, poll — which part would a persistent command buffer
(repeated_compute) remove?

## 2. What our GpuCtx already does (repeated_compute)

Pipeline + shader compilation happen ONCE; per-call cost is buffer
create + bind + encode + submit. The example goes further: reuses
buffers across iterations too. Question: rewrite GpuCtx::sum to
take pre-uploaded input (upload once, dispatch many) — how does the
crossover table change? This is exactly Crystal's regime A → B move,
expressible in ~15 lines.

## 3. WGSL vs the CUDA you read about

| CUDA | WGSL | note |
|---|---|---|
| `__global__` kernel | `@compute @workgroup_size(N) fn` | size fixed at pipeline creation |
| blockIdx/threadIdx | `@builtin(workgroup_id / local_invocation_id)` | |
| `__shared__` | `var<workgroup>` | our sum.wgsl scratch |
| `__syncthreads()` | `workgroupBarrier()` | workgroup-scope only |
| warp shuffles | subgroup ops (feature-gated) | portable fallback: shared memory |
| atomicAdd | `atomicAdd(&x, v)` on `atomic<u32/i32>` | NO float atomics in core WGSL |

Two DB-relevant gaps: no float atomics (aggregate f32 sums via
u32-bitcast CAS loops or per-workgroup partials — our sum kernel's
design is FORCED by this) and no device-wide barrier (multi-pass
algorithms = multiple dispatches; BFS levels each need their own
submit). Question: what does the no-device-barrier rule do to the
stretch-goal BFS (frontier per dispatch — where does the frontier
size live)?

## 4. Limits that bite (big_compute_buffers)

Default `max_storage_buffer_binding_size` = 128 MB; default max
workgroups per dimension = 65535. Our sum kernel folds 4 elements
per thread partly to stay under the dispatch limit at n=2^24.
Question: at what n does the 128 MB limit break GpuCtx::sum, and
what's the fix (request higher limits at device creation vs chunked
dispatches)?

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
