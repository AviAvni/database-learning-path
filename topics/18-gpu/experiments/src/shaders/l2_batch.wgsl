// YOURS: squared L2 distance, one query vs M targets.
//
// One invocation per TARGET: loop over dim, accumulate (q-t)².
// Think about layout before writing: targets row-major means thread
// j reads targets[j*dim + i] — adjacent threads are dim*4 bytes
// apart (UNcoalesced). Column-major (targets[i*M + j]) makes
// adjacent threads adjacent in memory at every step. Try row-major
// first, measure, then transpose — the gap IS the coalescing lesson.

@group(0) @binding(0) var<storage, read> query: array<f32>;
@group(0) @binding(1) var<storage, read> targets: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
// TODO binding 3: uniform { dim: u32, m: u32 }

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    // TODO: j = gid.x; if j >= m { return; }
    // TODO: loop dim, d = query[i] - targets[...], acc += d*d
    // TODO: out[j] = acc;
}
