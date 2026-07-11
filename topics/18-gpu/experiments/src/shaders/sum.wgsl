// PROVIDED: workgroup sum reduction.
// 256 invocations × 4 elements each = 1024 elements → 1 partial.
// Note the load stride: invocation lid.x reads base + k*256, so at
// each step k, adjacent threads touch ADJACENT addresses — coalesced.

@group(0) @binding(0) var<storage, read> input: array<f32>;
@group(0) @binding(1) var<storage, read_write> partials: array<f32>;

var<workgroup> scratch: array<f32, 256>;

const WG: u32 = 256u;
const PER_THREAD: u32 = 4u;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>,
        @builtin(workgroup_id) wid: vec3<u32>) {
    let n = arrayLength(&input);
    let base = wid.x * WG * PER_THREAD + lid.x;
    var v = 0.0;
    for (var k = 0u; k < PER_THREAD; k = k + 1u) {
        let i = base + k * WG;
        if (i < n) {
            v = v + input[i];
        }
    }
    scratch[lid.x] = v;
    workgroupBarrier();
    // tree reduction in shared memory
    var stride = WG / 2u;
    while (stride > 0u) {
        if (lid.x < stride) {
            scratch[lid.x] = scratch[lid.x] + scratch[lid.x + stride];
        }
        workgroupBarrier();
        stride = stride / 2u;
    }
    if (lid.x == 0u) {
        partials[wid.x] = scratch[0u];
    }
}
