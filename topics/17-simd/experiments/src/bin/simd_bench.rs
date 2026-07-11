//! simd_bench — GB/s per rung per kernel + the selectivity sweep.
//!
//! PROVIDED rungs run first and print real numbers; stub rungs are
//! attempted last inside catch_unwind so a todo!() panic doesn't
//! hide the baselines. Run with: cargo run --release --bin simd_bench

use simd_experiments::{dot, filter, unpack};
use simd_experiments::{gen_bytes, gen_f32, threshold_for_selectivity};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

const N: usize = 1 << 22; // 4M f32 = 16 MB per input (out of L2)
const REPS: usize = 20;

fn bench(name: &str, bytes_per_rep: usize, mut f: impl FnMut()) {
    // warmup
    f();
    let start = Instant::now();
    for _ in 0..REPS {
        f();
    }
    let secs = start.elapsed().as_secs_f64() / REPS as f64;
    let gbs = bytes_per_rep as f64 / secs / 1e9;
    println!("  {name:<28} {:>8.2} GB/s   {:>8.3} ms", gbs, secs * 1e3);
}

fn try_stub(label: &str, f: impl FnOnce()) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    if r.is_err() {
        println!("  {label:<28} (stub — implement me)");
    }
}

fn main() {
    println!("simd_bench: N = {N} f32 ({} MB/input), {REPS} reps\n", N * 4 >> 20);

    // ---- dot ----------------------------------------------------
    let a = gen_f32(N, 1);
    let b = gen_f32(N, 2);
    let bytes = N * 4 * 2; // two input streams
    println!("dot product (f32 · f32):");
    let mut sink = 0.0f32;
    bench("naive (1 chain)", bytes, || {
        sink += dot::dot_naive(&a, &b);
    });
    bench("unrolled-8 (autovec)", bytes, || {
        sink += dot::dot_unrolled8(&a, &b);
    });
    try_stub("wide f32x4 ×4 acc", || {
        sink += dot::dot_wide(&a, &b);
    });
    #[cfg(target_arch = "aarch64")]
    try_stub("neon vfmaq ×4 acc", || {
        sink += dot::dot_neon(&a, &b);
    });
    std::hint::black_box(sink);

    // ---- filter: selectivity sweep ------------------------------
    let vals = gen_f32(N, 3);
    println!("\nfilter compact (GB/s in, per selectivity):");
    println!("  {:<12} {:>10} {:>12}", "rung", "sel%", "GB/s");
    let mut out = Vec::new();
    for pct in [1u32, 25, 50, 75, 99] {
        let t = threshold_for_selectivity(pct);
        for (name, f) in [
            ("branchy", filter::compact_branchy as fn(&[f32], f32, &mut Vec<f32>)),
            ("branchless", filter::compact_branchless),
        ] {
            let start = Instant::now();
            for _ in 0..REPS {
                f(&vals, t, &mut out);
            }
            let secs = start.elapsed().as_secs_f64() / REPS as f64;
            println!("  {:<12} {:>9}% {:>12.2}", name, pct, N as f64 * 4.0 / secs / 1e9);
        }
        #[cfg(target_arch = "aarch64")]
        try_stub(&format!("neon-compress @ {pct}%"), || {
            let mut o = Vec::new();
            filter::compact_neon(&vals, t, &mut o);
        });
    }
    std::hint::black_box(&out);

    // ---- unpack --------------------------------------------------
    let packed = gen_bytes(N / 2, 4); // N nibble values
    println!("\n4-bit unpack → u32 (GB/s of OUTPUT):");
    let mut uout = Vec::new();
    bench("scalar", N * 4, || {
        unpack::unpack4_scalar(&packed, &mut uout);
    });
    #[cfg(target_arch = "aarch64")]
    try_stub("neon shift/mask", || {
        let mut o = Vec::new();
        unpack::unpack4_neon(&packed, &mut o);
    });
    std::hint::black_box(&uout);
}
