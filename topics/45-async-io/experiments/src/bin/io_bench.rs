//! io_bench — queue depth, batching, and write amplification.
//!
//! cargo run --release --bin io_bench
//!
//! Lane 1 (provided): random 4 KiB reads from a 1 GiB file written and read
//! around the page cache — at queue depth 1..64 bought with threads, through
//! POSIX AIO's batched `lio_listio`, and against warm cache to price a syscall.
//! Lane 2 (exercise, Linux only): the same reads through io_uring.
//! Lane 3 (exercise): in-place vs out-of-place writes on a simulated FTL.
//!
//! Set DLP_IO_FILE=/path/on/the/device to measure a particular disk.

use async_io_experiments::{
    aio, file,
    ftl::{SsdConfig, Waf},
    gen, inplace, oop, qd,
    stats::Run,
    PAGE,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        if msg.contains("not yet implemented") {
            STUBBED.store(true, Ordering::Relaxed);
        } else {
            eprintln!("{msg}");
        }
    }));
}

/// 1 GiB: big enough that random reads land all over the device, small
/// enough to write in about a second.
const FILE_PAGES: u64 = 1 << 18;
const READS: usize = 20_000;

fn main() {
    quiet_stubs();
    println!("=== io_bench ===\n");
    let path = file::test_path();
    let tf = file::create(&path, FILE_PAGES, gen::SEED).expect("create test file");
    let (cold, mode) = file::open_cold(&tf.path).expect("open cold");
    println!(
        "file: {} — {} MiB, {} pages of 4 KiB, seeded contents",
        tf.path.display(),
        FILE_PAGES * PAGE as u64 >> 20,
        FILE_PAGES
    );
    println!("cold reads via: {}\n", mode.describe());

    let qd1 = lane1a(&cold, mode);
    lane1b(&cold, &qd1);
    lane1c(&tf.path);
    lane2(&tf.path);
    lane3();
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/uring.rs (Linux) and src/oop.rs to unlock the lanes marked STUB]");
    }
}

fn lane1a(cold: &std::fs::File, mode: file::ColdMode) -> Run {
    println!("-- lane 1a: random 4 KiB reads, queue depth bought with threads --");
    println!("   (each row reads {READS} fresh seeded offsets; prediction = Little's law, QD / mean latency at QD 1)\n");
    println!(
        "   {:>3} {:>9} {:>8} {:>8} {:>8} {:>8} {:>11} {:>9}",
        "QD", "IOPS", "MiB/s", "p50 µs", "p99 µs", "mean µs", "predicted", "achieved"
    );
    let mut base_mean = 0.0;
    let mut first = None;
    let mut peak = 0.0f64;
    for q in [1usize, 2, 4, 8, 16, 32, 64] {
        let offs = gen::random_offsets(gen::SEED + q as u64, READS, FILE_PAGES);
        let mut r = qd::thread_reads(cold, &offs, q, PAGE);
        if q == 1 {
            base_mean = r.mean_us();
        }
        let pred = q as f64 / (base_mean * 1e-6);
        peak = peak.max(r.mib_s(PAGE));
        println!(
            "   {:>3} {:>9.0} {:>8.0} {:>8.1} {:>8.1} {:>8.1} {:>11.0} {:>8.0}%",
            q,
            r.iops(),
            r.mib_s(PAGE),
            r.pct_us(50.0),
            r.pct_us(99.0),
            r.mean_us(),
            pred,
            100.0 * r.iops() / pred
        );
        if q == 1 {
            first = Some(r);
        }
    }
    let mut first = first.unwrap();

    // Sanity 1: a "cold" read faster than the floor was a cache hit.
    let p50 = first.pct_us(50.0);
    if p50 < file::COLD_FLOOR_US {
        println!(
            "\n   !! QD-1 p50 is {p50:.1} µs, below the {} µs floor for a flash read: these were \
             page-cache hits ({}), and the table above measures memcpy, not the SSD. \
             Point DLP_IO_FILE at a real device.",
            file::COLD_FLOOR_US,
            mode.describe()
        );
    } else {
        println!(
            "\n   sanity: QD-1 p50 {p50:.1} µs ≥ {} µs floor — device reads, not page-cache hits",
            file::COLD_FLOOR_US
        );
    }
    // Sanity 2: no random-read figure may exceed what the device streams.
    let seq: Vec<u64> = (0..1024u64)
        .map(|i| (i * 256 * PAGE as u64) % (FILE_PAGES * PAGE as u64))
        .collect();
    let s = qd::thread_reads(cold, &seq, 8, 1 << 20);
    let ceiling = s.iops(); // 1 MiB per op, so ops/s == MiB/s
    println!(
        "   sanity: device ceiling (sequential 1 MiB reads, 8 threads) {:.0} MiB/s; \
         best random-4K row {:.0} MiB/s = {:.0}% of it",
        ceiling,
        peak,
        100.0 * peak / ceiling
    );
    // Sanity 3: every path reads the same bytes.
    let shared = gen::random_offsets(gen::SEED ^ 0xC0FFEE, 2_000, FILE_PAGES);
    let a = qd::sync_reads(cold, &shared).checksum;
    let b = qd::thread_reads(cold, &shared, 8, PAGE).checksum;
    let c = aio::lio_reads(cold, &shared, aio::MAX_BATCH).checksum;
    println!(
        "   sanity: checksum over 2,000 shared offsets — pread {a:016x}, 8 threads {b:016x}, lio_listio {c:016x}: {}\n",
        if a == b && b == c { "agree" } else { "DISAGREE — a read path is broken" }
    );
    first
}

/// Expected batch time if all `b` reads of a batch run in parallel and each
/// takes a latency drawn from the QD-1 distribution: the batch ends when its
/// slowest read does, so it is the mean of the max of `b` draws.
fn max_of_b_us(sample: &[u64], b: usize) -> f64 {
    let mut rng = ChaCha8Rng::seed_from_u64(gen::SEED);
    let trials = 20_000;
    let mut total = 0u64;
    for _ in 0..trials {
        total += (0..b).map(|_| sample[rng.gen_range(0..sample.len())]).max().unwrap();
    }
    total as f64 / trials as f64 / 1000.0
}

fn lane1b(cold: &std::fs::File, qd1: &Run) {
    println!("-- lane 1b: POSIX AIO lio_listio(LIO_WAIT) — one syscall submits b reads, then waits for all b --\n");
    println!(
        "   {:>5} {:>9} {:>12} {:>14} {:>16} {:>11}",
        "batch", "IOPS", "syscalls/IO", "batch p50 µs", "pred. batch µs", "pred. IOPS"
    );
    for b in [1usize, 4, 16] {
        let offs = gen::random_offsets(gen::SEED + 100 + b as u64, 8_000, FILE_PAGES);
        let mut r = aio::lio_reads(cold, &offs, b);
        let t = max_of_b_us(&qd1.lat_ns, b);
        println!(
            "   {:>5} {:>9.0} {:>12.3} {:>14.1} {:>16.1} {:>11.0}",
            b,
            r.iops(),
            r.syscalls as f64 / r.ops as f64,
            r.pct_us(50.0),
            t,
            b as f64 / (t * 1e-6)
        );
    }
    println!("   (prediction: all b reads in flight at once, each with a QD-1 latency; the barrier waits for the slowest)\n");
}

fn lane1c(path: &std::path::Path) {
    println!("-- lane 1c: warm page cache — 4 KiB reads with the device taken out --\n");
    let warm = file::open_warm(path).expect("open warm");
    let region = 16_384u64; // 64 MiB, resident after one pass
    qd::warm_up(&warm, region);
    println!("   {:>10} {:>14} {:>12}", "pages/call", "ns per page", "vs 1/call");
    let mut one = 0.0;
    for b in [1usize, 2, 4, 8, 16, 32, 64] {
        let ns = (0..3)
            .map(|_| qd::warm_syscall_cost(&warm, region, b, 400_000))
            .fold(f64::MAX, f64::min);
        if b == 1 {
            one = ns;
        }
        println!("   {:>10} {:>14.0} {:>11.2}x", b, ns, one / ns);
    }
    println!("   (1 = pread per page; b > 1 = one preadv over b contiguous pages; best of 3)\n");
}

#[cfg(not(target_os = "linux"))]
fn lane2(_path: &std::path::Path) {
    println!("-- lane 2: io_uring, batched submission (exercise, Linux only) --");
    println!(
        "   [stub — io_uring is a Linux interface and this OS ({}) has none; implement src/uring.rs and run on Linux]\n",
        std::env::consts::OS
    );
}

#[cfg(target_os = "linux")]
fn lane2(path: &std::path::Path) {
    use async_io_experiments::uring;
    println!("-- lane 2: io_uring, batched submission (exercise, Linux only) --\n");
    if let Err(e) = uring::probe() {
        println!(
            "   [stub — io_uring_setup refused here ({e}); seccomp (Docker's default profile), \
             kernel.io_uring_disabled, or an old kernel — lane 2 cannot run on this machine]\n"
        );
        return;
    }
    let (cold, _) = file::open_cold(path).expect("open cold");
    println!(
        "   {:>3} {:>5} {:>9} {:>12} {:>8} {:>8}",
        "QD", "batch", "IOPS", "enters/IO", "p50 µs", "p99 µs"
    );
    for (q, b) in [(1usize, 1usize), (8, 1), (8, 8), (32, 1), (32, 8), (32, 32)] {
        let offs = gen::random_offsets(gen::SEED + 200 + (q * 100 + b) as u64, READS, FILE_PAGES);
        match catch_unwind(AssertUnwindSafe(|| uring::uring_reads(&cold, &offs, q, b))) {
            Ok(mut r) => println!(
                "   {:>3} {:>5} {:>9.0} {:>12.3} {:>8.1} {:>8.1}",
                q,
                b,
                r.iops(),
                r.syscalls as f64 / r.ops as f64,
                r.pct_us(50.0),
                r.pct_us(99.0)
            ),
            Err(_) => {
                println!("   {:>3} {:>5}   STUB", q, b);
            }
        }
    }
    println!();
}

fn waf_row(label: &str, w: &Waf) {
    println!(
        "   {:<46} {:>7.2} {:>8.2} {:>9.2}",
        label,
        w.db_waf(),
        w.ssd_waf(),
        w.total()
    );
}

fn lane3() {
    let cfg = SsdConfig::lane3();
    let fill = 0.9;
    let pages = (cfg.lbas() as f64 * fill) as u32 - inplace::DWB_SLOTS;
    println!("-- lane 3: write amplification on a simulated FTL (a model, not your SSD) --");
    println!(
        "   device: {} erase blocks x {} pages addressable + {} over-provisioned; greedy GC; one write stream",
        cfg.logical_blocks, cfg.pages_per_block, cfg.op_blocks
    );
    println!(
        "   database: {} pages = {:.0}% of the addressable space; {} user page writes, first half uncounted\n",
        pages,
        fill * 100.0,
        3 * cfg.lbas()
    );
    for theta in [0.0, 0.8] {
        let s = gen::page_stream(gen::SEED, 3 * cfg.lbas() as usize, pages, theta);
        let warm = s.len() / 2;
        if theta == 0.0 {
            println!("   uniform updates");
        } else {
            println!("   Zipf(0.8) updates — the paper's YCSB-A skew");
        }
        println!("   {:<46} {:>7} {:>8} {:>9}", "", "DB WAF", "SSD WAF", "total");
        waf_row("in-place + doublewrite buffer", &inplace::run_inplace(cfg, fill, &s, warm));
        for (zp, k, what) in [
            (256u32, 1u32, "zone = erase block, 1 open"),
            (128, 1, "zone = 1/2 erase block, 1 open"),
            (256, 2, "zone = erase block, 2 open"),
            (64, 4, "zone = 1/4 erase block, 4 open"),
        ] {
            let oc = oop::OopConfig {
                zone_pages: zp,
                open_zones: k,
            };
            let label = format!("out-of-place, {what}");
            match catch_unwind(AssertUnwindSafe(|| oop::run_out_of_place(cfg, fill, oc, &s, warm))) {
                Ok((w, _)) => waf_row(&label, &w),
                Err(_) => println!("   {label:<46}    STUB"),
            }
        }
        println!();
    }
}
