// YOURS: count of input[i] < threshold.
//
// Skeleton — the shape mirrors sum.wgsl. TODO markers are the work.
// Key rule: ONE atomicAdd per workgroup, not per element. Atomics on
// the same address serialize; 4M elements = 4M-way contention if you
// do it per-element, but only n/1024 adds if you reduce first.

@group(0) @binding(0) var<storage, read> input: array<f32>;
@group(0) @binding(1) var<storage, read_write> count: atomic<u32>;
// TODO binding 2: uniform threshold (a struct with one f32)

var<workgroup> scratch: array<u32, 256>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>) {
    // TODO: fold PER_THREAD elements: c += u32(input[i] < threshold)
    // TODO: scratch[lid.x] = c; workgroupBarrier(); tree-reduce
    // TODO: if lid.x == 0 { atomicAdd(&count, scratch[0]); }
}
