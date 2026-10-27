//! join_bench — topic 54. The no-partitioning hash join across build sizes
//! from L2-resident to far beyond any cache, uniform and Zipf probes, and
//! the hardware probe that says what those sizes mean on this machine.
//!
//! cargo run --release --bin join_bench

use join_experiments::{
    gen::{generate, Workload},
    hw,
    npj::{self, Table},
    radix::{radix_join, RadixConfig},
    sortmerge::sort_merge_join,
    timing::{prefer_performance_cores, Stopwatch},
    tuple::{JoinOutput, Tuple},
};
use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
}

/// Best of `n` runs in milliseconds of this thread's CPU time (equal to wall
/// time on an idle machine, immune to other processes' slices on a busy one),
/// and the last result. For single-threaded lanes.
fn best<R>(n: usize, mut f: impl FnMut() -> R) -> (f64, R) {
    let mut b = f64::MAX;
    let mut out = None;
    for _ in 0..n {
        let t = Stopwatch::start();
        let r = black_box(f());
        b = b.min(t.secs() * 1e3);
        out = Some(r);
    }
    (b, out.unwrap())
}

/// Best of `n` wall-clock runs in milliseconds, for multi-threaded lanes.
fn best_wall<R>(n: usize, mut f: impl FnMut() -> R) -> (f64, R) {
    let mut b = f64::MAX;
    let mut out = None;
    for _ in 0..n {
        let t = Instant::now();
        let r = black_box(f());
        b = b.min(t.elapsed().as_secs_f64() * 1e3);
        out = Some(r);
    }
    (b, out.unwrap())
}

fn load_average() -> String {
    let mut l = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 doubles into the array we pass.
    let n = unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
    if n == 3 {
        format!("{:.2} {:.2} {:.2}", l[0], l[1], l[2])
    } else {
        "unavailable".into()
    }
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1 << 20) as f64
}

const PROBE: usize = 1 << 25; // 33,554,432 probe tuples = 256 MiB

fn main() {
    quiet_stubs();
    prefer_performance_cores();
    println!("=== join_bench — topic 54 ===");
    println!("load average (1/5/15 min) at start: {}\n", load_average());

    let bw = lane0();
    lane1a();
    lane1(bw);
    lane1d();
    lane1b(bw);
    lane1c();
    lane2();
    lane3();

    println!("\nload average (1/5/15 min) at end: {}", load_average());
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/radix.rs (lane 2) and src/sortmerge.rs (lane 3) to unlock the lanes marked STUB]");
    }
}

/// Lane 0: the machine. Returns the best measured read bandwidth (GB/s).
fn lane0() -> f64 {
    println!("-- lane 0: the machine, measured --");
    println!("   dependent-add period: {:.3} ns (a larger figure than a quiet run means a slower core)", hw::add_chain_ns());
    println!("   dependent-load latency (random cycle, one node per 128-byte line):");
    println!("   {:>12} {:>10}", "working set", "ns/load");
    for kib in [32usize, 64, 128, 512, 2048, 8192, 16384, 32768, 65536, 262144, 1048576] {
        let ns = hw::chase_ns(kib << 10);
        let label = if kib >= 1024 { format!("{} MiB", kib / 1024) } else { format!("{kib} KiB") };
        println!("   {:>12} {:>10.2}", label, ns);
    }
    println!("\n   dependent-load latency by number of 16 KiB pages touched (one 128 B node per page):");
    println!("   {:>8} {:>10} {:>9}", "pages", "reach", "ns/load");
    for pages in [32usize, 128, 256, 512, 1024, 2048, 4096, 16384, 65536] {
        let ns = hw::chase_pages_ns(pages);
        let reach = pages * 16 / 1024;
        let label = if reach >= 1 { format!("{reach} MiB") } else { format!("{} KiB", pages * 16) };
        println!("   {:>8} {:>10} {:>9.2}", pages, label, ns);
    }
    println!("\n   sequential read bandwidth over 1 GiB:");
    let mut top = 0f64;
    for th in [1usize, 5, 11] {
        let g = hw::read_gbps(1 << 30, th);
        top = top.max(g);
        println!("   {:>2} thread(s) {:>8.1} GB/s", th, g);
    }
    println!();
    top
}

/// Lane 1a: the probe loop's floor — tables small enough to live in L1/L2, so
/// every nanosecond is the loop itself, not the memory system.
fn lane1a() {
    const P: usize = 1 << 24;
    println!("-- lane 1a: the probe loop with no cache misses — |S| = {P}, uniform --");
    println!("   {:>9} {:>10} {:>12} {:>12} {:>12}", "|R|", "table KiB", "scalar ns", "bucket ns", "batched ns");
    for lg in [4u32, 8, 12, 14] {
        let n = 1usize << lg;
        let (r, s) = generate(&Workload::uniform(n, P));
        let t = Table::build(&r);
        let (a, oa) = best(3, || t.probe_scalar(black_box(&s)));
        let (b, ob) = best(3, || t.probe(black_box(&s)));
        let (c, oc) = best(3, || t.probe_batched(black_box(&s)));
        assert!(oa == ob && ob == oc && ob.matches == P as u64);
        println!(
            "   {:>9} {:>10.1} {:>12.2} {:>12.2} {:>12.2}",
            format!("2^{lg}"),
            t.bytes() as f64 / 1024.0,
            a * 1e6 / P as f64,
            b * 1e6 / P as f64,
            c * 1e6 / P as f64
        );
    }
    println!("   (scalar: one slot per step, a branch per slot; bucket: four slots per step, a branch only on a full bucket)");
    println!();
}

/// Lane 1: the no-partitioning join across build sizes, uniform probes.
fn lane1(bw: f64) {
    println!("-- lane 1: no-partitioning hash join, uniform FK probes — |S| = {PROBE} (256 MiB), 8-byte tuples --");
    println!(
        "   {:>9} {:>9} {:>10} {:>9} {:>10} {:>9} {:>9} {:>9} {:>10}",
        "|R|", "R MiB", "table MiB", "build ms", "scalar ms", "probe ms", "batch ms", "ns/probe", "Mtup/s"
    );
    for lg in [14u32, 16, 18, 20, 22, 24] {
        let n = 1usize << lg;
        let (r, s) = generate(&Workload::uniform(n, PROBE));
        let (b_ms, t) = best(3, || Table::build(black_box(&r)));
        let (sc_ms, outs) = best(3, || t.probe_scalar(black_box(&s)));
        let (p_ms, out) = best(3, || t.probe(black_box(&s)));
        let (q_ms, outb) = best(3, || t.probe_batched(black_box(&s)));
        let ok = out == outb && out == outs && out.matches == PROBE as u64;
        let ns = q_ms * 1e6 / PROBE as f64;
        let tput_b = (n + PROBE) as f64 / (b_ms + q_ms) / 1e3;
        println!(
            "   {:>9} {:>9.2} {:>10.2} {:>9.2} {:>10.2} {:>9.2} {:>9.2} {:>9.2} {:>10.1}{}",
            format!("2^{lg}"),
            mib(n * 8),
            mib(t.bytes()),
            b_ms,
            sc_ms,
            p_ms,
            q_ms,
            ns,
            tput_b,
            if ok { "" } else { "   DISAGREE" }
        );
        // sanity: bytes moved per second must be below the machine's measured bandwidth
        let gbps = (PROBE * 8) as f64 / (p_ms / 1e3) / 1e9;
        if gbps > bw * 1.5 {
            println!("   sanity FAIL: streaming S alone at {gbps:.0} GB/s exceeds measured {bw:.0} GB/s");
        }
    }
    println!("   (times are this thread's CPU time; ns/probe and Mtup/s = (|R|+|S|) / (build + batched probe) use the batched probe; 'scalar' is the one-slot-per-step loop, 'probe' the bucket loop)");
    println!("   sanity: every probe hits, matches == |S| and scalar == bucket == batched checksum on every row; S is read at <= 1.5 x the measured read bandwidth");
    println!();
}

/// Lane 1d: the same probes in a different order. Sorting S by the table
/// address its keys hash to (outside the timer) makes consecutive probes touch
/// consecutive memory and repeats the same bucket pattern, which is what
/// partitioning S buys the join phase — here without paying for it.
fn lane1d() {
    const P: usize = 1 << 24;
    println!("-- lane 1d: probe order — S random vs S sorted by home bucket (sort not timed), |S| = {P} --");
    println!("   {:>9} {:>10} {:>14} {:>14} {:>9}", "|R|", "table MiB", "random ns", "sorted ns", "ratio");
    for lg in [14u32, 20, 22, 24] {
        let n = 1usize << lg;
        let (r, s) = generate(&Workload::uniform(n, P));
        let t = Table::build(&r);
        let mut sorted = s.clone();
        sorted.sort_unstable_by_key(|x| t.home_bucket(x.key));
        let (a, oa) = best(3, || t.probe(black_box(&s)));
        let (b, ob) = best(3, || t.probe(black_box(&sorted)));
        assert_eq!(oa, ob);
        println!(
            "   {:>9} {:>10.2} {:>14.2} {:>14.2} {:>8.2}x",
            format!("2^{lg}"),
            mib(t.bytes()),
            a * 1e6 / P as f64,
            b * 1e6 / P as f64,
            a / b
        );
    }
    println!();
}

/// Lane 1b: skew. The probe keys of S follow Zipf(theta) over R's keys.
fn lane1b(_bw: f64) {
    println!("-- lane 1b: skew — the same join with Zipf(theta) probe keys, |S| = {PROBE} --");
    println!("   {:>9} {:>6} {:>9} {:>9} {:>9}", "|R|", "theta", "probe ms", "ns/probe", "vs uniform");
    for lg in [20u32, 24] {
        let n = 1usize << lg;
        let mut base = 0f64;
        for theta in [0.0f64, 0.75, 0.99] {
            let (r, s) = generate(&Workload { n_build: n, n_probe: PROBE, theta, hit_pct: 100 });
            let t = Table::build(&r);
            let (p_ms, out) = best(3, || t.probe(black_box(&s)));
            assert_eq!(out.matches, PROBE as u64);
            if theta == 0.0 {
                base = p_ms;
            }
            println!(
                "   {:>9} {:>6.2} {:>9.2} {:>9.2} {:>8.2}x",
                format!("2^{lg}"),
                theta,
                p_ms,
                p_ms * 1e6 / PROBE as f64,
                p_ms / base
            );
        }
    }
    println!();
}

/// Lane 1c: extra threads on the probe (read-only shared table).
fn lane1c() {
    let n = 1usize << 24;
    println!("-- lane 1c: threads on the probe — |R| = 2^24 (256 MiB table), |S| = {PROBE} --");
    let (r, s) = generate(&Workload::uniform(n, PROBE));
    let t = Table::build(&r);
    let want = t.probe(&s);
    println!("   {:>8} {:>9} {:>10} {:>9}", "threads", "probe ms", "Mprobe/s", "speedup");
    let mut base = 0f64;
    for th in [1usize, 2, 5, 11] {
        let (ms, out) = best_wall(3, || t.probe_parallel(black_box(&s), th));
        assert_eq!(out, want, "parallel probe disagrees");
        if th == 1 {
            base = ms;
        }
        println!("   {:>8} {:>9.2} {:>10.1} {:>8.2}x", th, ms, PROBE as f64 / ms / 1e3, base / ms);
    }
    println!();
}

/// Lane 2: radix join — a stub until implemented.
fn lane2() {
    println!("-- lane 2: radix-partitioned hash join (exercise) --");
    let n = 1usize << 20;
    let (r, s) = generate(&Workload::uniform(n, 1 << 22));
    let want = npj::npj(&r, &s);
    println!("   |R| = 2^20, |S| = 2^22; no-partitioning reference: {} matches, checksum {:016x}", want.matches, want.checksum);
    println!("   {:>10} {:>9} {:>9}", "bits (p1+p2)", "ms", "agrees");
    let mut any = false;
    for (b1, b2) in [(4u32, 0u32), (8, 0), (10, 0), (6, 6)] {
        let cfg = RadixConfig { bits_pass1: b1, bits_pass2: b2 };
        match catch_unwind(AssertUnwindSafe(|| best(3, || radix_join(black_box(&r), black_box(&s), &cfg)))) {
            Ok((ms, out)) => {
                any = true;
                println!("   {:>10} {:>9.2} {:>9}", format!("{b1}+{b2}"), ms, if out == want { "yes" } else { "NO" });
            }
            Err(_) => {
                println!("   {:>10} STUB", format!("{b1}+{b2}"));
            }
        }
    }
    let _ = any;
    println!();
}

/// Lane 3: sort-merge join — a stub until implemented.
fn lane3() {
    println!("-- lane 3: sort-merge join (exercise) --");
    let n = 1usize << 20;
    let (r, s): (Vec<Tuple>, Vec<Tuple>) = generate(&Workload::uniform(n, 1 << 22));
    let want: JoinOutput = npj::npj(&r, &s);
    match catch_unwind(AssertUnwindSafe(|| best(3, || sort_merge_join(black_box(&r), black_box(&s))))) {
        Ok((ms, out)) => println!(
            "   |R| = 2^20, |S| = 2^22: {:.2} ms, {}",
            ms,
            if out == want { "agrees with the hash join" } else { "DISAGREES with the hash join" }
        ),
        Err(_) => println!("   STUB"),
    }
}
