//! gpu_bench — CPU vs GPU crossover, transfer time INCLUDED.
//! Run: cargo run --release --bin gpu_bench

use gpu_experiments::{cpu, gen_f32, gpu::GpuCtx};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn cpu_us(mut f: impl FnMut()) -> f64 {
    f(); // warmup
    let reps = 10;
    let t = Instant::now();
    for _ in 0..reps {
        f();
    }
    t.elapsed().as_secs_f64() * 1e6 / reps as f64
}

fn main() {
    // This is the one lane in the repo that needs hardware the machine may not
    // have: a headless Linux CI runner has no Metal and often no Vulkan driver.
    // A benchmark with no device to measure has not failed, it has nothing to
    // report — so say that and exit cleanly rather than panicking. verify.sh
    // reads the marker below and records SKIP rather than PASS, because a green
    // tick for a lane that measured nothing would be worse than a red one.
    let Some(ctx) = GpuCtx::try_new() else {
        println!(
            "[skipped — no GPU adapter on this machine]\n\n\
             This topic is the only one that needs a device: it measures where the\n\
             CPU/GPU crossover sits once transfer costs are included, and with no\n\
             adapter there is nothing to transfer to. On macOS you get Metal; on\n\
             Linux you need a Vulkan or GL driver (mesa's lavapipe works, and is\n\
             itself instructive — a software adapter makes the transfer tax look\n\
             free and the kernel look terrible).\n\n\
             The recorded reference numbers for an Apple M3 Pro are in notes.md:\n\
             no crossover up to 2^24 elements, with upload alone costing 7197 µs\n\
             at 16 M against a 2723 µs CPU total."
        );
        return;
    };
    println!("adapter: {}\n", ctx.adapter_name);

    println!("sum: CPU (8-acc autovec) vs GPU (workgroup reduce), end-to-end");
    println!(
        "  {:>10} {:>12} {:>12} {:>10} {:>10} {:>10}  winner",
        "n", "cpu µs", "gpu µs", "upload", "kernel", "readback"
    );
    let mut crossover: Option<usize> = None;
    for log2n in [14usize, 16, 18, 20, 22, 24] {
        let n = 1 << log2n;
        let vals = gen_f32(n, log2n as u64);

        let mut sink = 0.0f32;
        let c = cpu_us(|| sink += cpu::sum(&vals));
        std::hint::black_box(sink);

        // warmup + averaged GPU runs
        let _ = ctx.sum(&vals);
        let reps = 5;
        let (mut g, mut up, mut kr, mut rb) = (0.0, 0.0, 0.0, 0.0);
        for _ in 0..reps {
            let (_, t) = ctx.sum(&vals);
            g += t.total_us();
            up += t.upload_us;
            kr += t.gpu_us;
            rb += t.readback_us;
        }
        let (g, up, kr, rb) =
            (g / reps as f64, up / reps as f64, kr / reps as f64, rb / reps as f64);
        let winner = if g < c { "GPU" } else { "CPU" };
        if g < c && crossover.is_none() {
            crossover = Some(n);
        }
        println!(
            "  {:>10} {:>12.1} {:>12.1} {:>10.1} {:>10.1} {:>10.1}  {}",
            n, c, g, up, kr, rb, winner
        );
    }
    match crossover {
        Some(n) => println!("\n  crossover at n = {n} (first size where GPU total < CPU)"),
        None => println!("\n  no crossover up to 2^24 — the PCIe/transfer tax in action"),
    }

    // stubs, attempted last
    let vals = gen_f32(1 << 22, 99);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    if catch_unwind(AssertUnwindSafe(|| ctx.filter_count(&vals, 0.5))).is_err() {
        println!("\nfilter_count: (stub — implement me)");
    }
    let q = gen_f32(128, 1);
    let targets = gen_f32(128 * 100_000, 2);
    if catch_unwind(AssertUnwindSafe(|| ctx.l2_batch(&q, &targets, 128))).is_err() {
        println!("l2_batch:     (stub — implement me)");
    }
    std::panic::set_hook(prev);
}
