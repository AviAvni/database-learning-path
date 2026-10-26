//! alloc_bench — graph-engine allocation traces under the system allocator and
//! a bump arena, with the lane-2/3 stubs degrading to a `[stub — ...]` note.
//!
//!   cargo run --release --bin alloc_bench
//!   cargo run --release --features mimalloc --bin alloc_bench   # lane 1d
//!   cargo run --release --features jemalloc --bin alloc_bench   # lane 1d
//!
//! Every measurement runs in a fresh child process (this binary re-executed
//! with `--child <phase> <heap>`) so a memory baseline can be subtracted and an
//! earlier phase cannot leave pages behind.

use alloc_experiments::heap::{Bump, Global, Heap};
use alloc_experiments::pageheap::PageHeap;
use alloc_experiments::rss::{self, mib};
use alloc_experiments::shared::SharedHeap;
use alloc_experiments::trace::{bulk_load, live_bytes as trace_bytes, query_sizes};
use alloc_experiments::workloads::{churn, live_bytes, load, query, Live};
use std::collections::HashMap;
use std::panic::catch_unwind;
use std::process::Command;
use std::sync::mpsc::sync_channel;
use std::time::Instant;

#[cfg(all(feature = "mimalloc", feature = "jemalloc"))]
compile_error!("enable at most one of the `mimalloc` and `jemalloc` features");

#[cfg(feature = "mimalloc")]
#[global_allocator]
static A: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static A: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const NODES: usize = 200_000;
const SEED: u64 = 56;
const CHURN_ROUNDS: usize = 3;
const Q_OBJS: usize = 20_000;
const Q_COUNT: usize = 400;
const XP: usize = 4; // producer threads
const XN: usize = 1_000_000; // objects per producer
const XB: usize = 1024; // batch size

fn allocator_name() -> &'static str {
    if cfg!(feature = "mimalloc") {
        "mimalloc (crate `mimalloc` 0.1, bundled C source)"
    } else if cfg!(feature = "jemalloc") {
        "jemalloc (crate `tikv-jemallocator` 0.6, bundled C source)"
    } else if cfg!(target_os = "macos") {
        "system (macOS libmalloc)"
    } else {
        "system (libc malloc)"
    }
}

// ---------------------------------------------------------------- child side

fn kv(pairs: &[(&str, String)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

fn run_with<H: Heap>(mut h: H, phase: &str) -> String {
    match phase {
        "bulk" => {
            let trace = bulk_load(NODES, SEED);
            let base = rss::now();
            let base_peak = rss::peak();
            let t = Instant::now();
            let live = load(&mut h, &trace);
            let el = t.elapsed();
            let m = rss::now();
            let meta = (live.capacity() * std::mem::size_of::<Live>()) as u64;
            kv(&[
                ("objs", trace.len().to_string()),
                ("ns", format!("{:.2}", el.as_nanos() as f64 / trace.len() as f64)),
                ("live", live_bytes(&live).to_string()),
                ("resident", m.resident.saturating_sub(base.resident + meta).to_string()),
                ("footprint", m.footprint.saturating_sub(base.footprint + meta).to_string()),
                ("peak", rss::peak().saturating_sub(base_peak.max(base.resident) + meta).to_string()),
            ])
        }
        "churn" => {
            let trace = bulk_load(NODES, SEED);
            let base = rss::now();
            let mut live = load(&mut h, &trace);
            let meta0 = (live.capacity() * std::mem::size_of::<Live>()) as u64;
            let meta = meta0 + (live.len() / 2 + 1) as u64 * 8; // plus churn's victim list
            let b = rss::now();
            let live0 = live_bytes(&live);
            let t = Instant::now();
            let n = churn(&mut h, &mut live, CHURN_ROUNDS, SEED + 1);
            let el = t.elapsed();
            let c = rss::now();
            kv(&[
                ("ops", n.to_string()),
                ("ns", format!("{:.2}", el.as_nanos() as f64 / (2 * n) as f64)),
                ("live0", live0.to_string()),
                ("foot0", b.footprint.saturating_sub(base.footprint + meta0).to_string()),
                ("res0", b.resident.saturating_sub(base.resident + meta0).to_string()),
                ("live1", live_bytes(&live).to_string()),
                ("foot1", c.footprint.saturating_sub(base.footprint + meta).to_string()),
                ("res1", c.resident.saturating_sub(base.resident + meta).to_string()),
            ])
        }
        "query" => {
            let sizes = query_sizes(Q_OBJS, SEED + 2);
            let base = rss::now();
            let base_peak = rss::peak();
            let t = Instant::now();
            let n = query(&mut h, &sizes, Q_COUNT);
            let el = t.elapsed();
            let m = rss::now();
            kv(&[
                ("objs", n.to_string()),
                ("ns", format!("{:.2}", el.as_nanos() as f64 / n as f64)),
                ("footprint", m.footprint.saturating_sub(base.footprint).to_string()),
                ("peak", rss::peak().saturating_sub(base_peak.max(base.resident)).to_string()),
            ])
        }
        _ => unreachable!("phase {phase}"),
    }
}

fn xthread<H: Heap + Send, F: Fn() -> H + Sync>(mk: &F, cross: bool) -> String {
    let base = rss::now();
    let base_peak = rss::peak();
    let t = Instant::now();
    std::thread::scope(|s| {
        for pid in 0..XP {
            let (tx, rx) = sync_channel::<Vec<(usize, u32)>>(8);
            s.spawn(move || {
                let mut h = mk();
                let sizes = query_sizes(XB, SEED + 10 + pid as u64);
                let mut sent = 0;
                while sent < XN {
                    let batch: Vec<(usize, u32)> = sizes
                        .iter()
                        .map(|&sz| unsafe {
                            let p = h.alloc(sz as usize);
                            alloc_experiments::workloads::touch(p, sz as usize);
                            (p as usize, sz)
                        })
                        .collect();
                    if cross {
                        tx.send(batch).unwrap();
                    } else {
                        for (p, sz) in batch {
                            unsafe { h.free(p as *mut u8, sz as usize) };
                        }
                    }
                    sent += XB;
                }
            });
            if cross {
                s.spawn(move || {
                    let mut h = mk();
                    for batch in rx {
                        for (p, sz) in batch {
                            unsafe { h.free(p as *mut u8, sz as usize) };
                        }
                    }
                });
            }
        }
    });
    let el = t.elapsed();
    let m = rss::now();
    kv(&[
        ("objs", (XP * XN).to_string()),
        ("ns", format!("{:.2}", el.as_nanos() as f64 * XP as f64 / (XP * XN) as f64)),
        ("mobjs", format!("{:.2}", (XP * XN) as f64 / el.as_secs_f64() / 1e6)),
        ("footprint", m.footprint.saturating_sub(base.footprint).to_string()),
        ("peak", rss::peak().saturating_sub(base_peak.max(base.resident)).to_string()),
    ])
}

fn child(phase: &str, heap: &str) {
    std::panic::set_hook(Box::new(|_| {}));
    let r = catch_unwind(|| match (phase, heap) {
        ("xthread" | "xlocal", "global") => xthread(&|| Global, phase == "xthread"),
        ("xthread" | "xlocal", "shared") => {
            let sh = SharedHeap::new();
            let out = xthread(&|| sh.handle(), phase == "xthread");
            format!("{out} pages={}", sh.pages())
        }
        (_, "global") => run_with(Global, phase),
        (_, "bump") => run_with(Bump::new(), phase),
        (_, "pageheap") => run_with(PageHeap::new(), phase),
        _ => "SKIP".into(),
    });
    match r {
        Ok(s) => println!("{s}"),
        Err(_) => println!("STUB"),
    }
}

// --------------------------------------------------------------- parent side

type Row = HashMap<String, f64>;

fn spawn(phase: &str, heap: &str) -> Option<Row> {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe).args(["--child", phase, heap]).output().expect("spawn child");
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().last().unwrap_or("");
    if line == "STUB" || line == "SKIP" || line.is_empty() {
        return None;
    }
    Some(
        line.split_whitespace()
            .filter_map(|kv| kv.split_once('='))
            .filter_map(|(k, v)| v.parse::<f64>().ok().map(|v| (k.to_string(), v)))
            .collect(),
    )
}

/// Best (lowest) `ns` of three child runs; every other column is the median of
/// the three, with `<key>_min` / `<key>_max` alongside, because resident memory
/// under libmalloc is not perfectly repeatable from process to process.
fn best3(phase: &str, heap: &str) -> Option<Row> {
    let mut runs: Vec<Row> = Vec::new();
    for _ in 0..3 {
        let r = spawn(phase, heap)?;
        assert!(r["ns"] >= 1.0, "{phase}/{heap}: {} ns per op is implausibly low, a bug in the bench", r["ns"]);
        runs.push(r);
    }
    let mut out = Row::new();
    for k in runs[0].keys() {
        let mut v: Vec<f64> = runs.iter().map(|r| r[k]).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        out.insert(k.clone(), if k == "ns" { v[0] } else { v[1] });
        out.insert(format!("{k}_min"), v[0]);
        out.insert(format!("{k}_max"), v[2]);
    }
    Some(out)
}

fn load_average() -> String {
    let mut l = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 doubles.
    let n = unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
    if n == 3 {
        format!("{:.2} {:.2} {:.2}", l[0], l[1], l[2])
    } else {
        "unavailable".into()
    }
}

fn stub(lane: &str, what: &str) {
    println!("   [stub — {lane}: {what}; implement it and re-run]");
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() == 4 && a[1] == "--child" {
        child(&a[2], &a[3]);
        return;
    }
    println!("=== alloc_bench — topic 56 ===");
    println!("global allocator: {}", allocator_name());
    println!("load average (1/5/15 min) at start: {}", load_average());
    let trace = bulk_load(NODES, SEED);
    println!(
        "trace: {NODES} nodes -> {} objects, {:.1} MiB requested (seed {SEED})\n",
        trace.len(),
        mib(trace_bytes(&trace))
    );
    let edges = trace.iter().filter(|o| o.kind == alloc_experiments::trace::Kind::Edge).count();
    println!(
        "edge records: {edges} x 40 B; an allocator whose smallest steps are 16 B rounds each to 48 B = {:.1} MiB of padding\n",
        edges as f64 * 8.0 / 1048576.0
    );
    drop(trace);

    println!("-- lane 1a: bulk load (keep everything) --");
    println!("   {:<10} {:>8} {:>10} {:>12} {:>12} {:>9}", "heap", "ns/alloc", "live MiB", "resident", "footprint", "res/live");
    for heap in ["global", "bump", "pageheap"] {
        match best3("bulk", heap) {
            Some(r) => println!(
                "   {:<10} {:>8.1} {:>10.1} {:>10.1}Mi {:>10.1}Mi {:>9.2}",
                heap, r["ns"], mib(r["live"] as u64), mib(r["resident"] as u64), mib(r["footprint"] as u64),
                r["resident"] / r["live"]
            ),
            None if heap == "pageheap" => stub("lane 2", "PageHeap is todo!()"),
            None => println!("   {heap}: failed"),
        }
    }

    println!("\n-- lane 1b: churn ({CHURN_ROUNDS} rounds: free a random 50%, insert replacements with grown properties) --");
    println!("   {:<10} {:>9} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>16}", "heap", "ns/op", "live0 MiB", "res0", "live1 MiB", "res1", "res1/live1", "footprint1", "res1/live1 range");
    for heap in ["global", "bump", "pageheap"] {
        match best3("churn", heap) {
            Some(r) => println!(
                "   {:<10} {:>9.1} {:>10.1} {:>8.1}Mi {:>10.1} {:>8.1}Mi {:>10.2} {:>8.1}Mi {:>8.2}-{:.2}",
                heap, r["ns"], mib(r["live0"] as u64), mib(r["res0"] as u64), mib(r["live1"] as u64),
                mib(r["res1"] as u64), r["res1"] / r["live1"], mib(r["foot1"] as u64),
                r["res1_min"] / r["live1"], r["res1_max"] / r["live1"]
            ),
            None if heap == "pageheap" => stub("lane 2", "PageHeap is todo!()"),
            None => println!("   {heap}: failed"),
        }
    }

    println!("\n-- lane 1c: query arena ({Q_COUNT} queries x {Q_OBJS} short-lived objects, all freed at query end) --");
    println!("   {:<10} {:>13} {:>12} {:>12}", "heap", "ns/obj (a+f)", "footprint", "peak");
    for heap in ["global", "bump", "pageheap"] {
        match best3("query", heap) {
            Some(r) => println!(
                "   {:<10} {:>13.1} {:>10.2}Mi {:>10.2}Mi",
                heap, r["ns"], mib(r["footprint"] as u64), mib(r["peak"] as u64)
            ),
            None if heap == "pageheap" => stub("lane 2", "PageHeap is todo!()"),
            None => println!("   {heap}: failed"),
        }
    }

    println!("\n-- lane 1d: cross-thread frees under the global allocator ({XP} producers x {XN} objects, batches of {XB}) --");
    println!("   {:<22} {:>9} {:>9} {:>12} {:>12}", "pattern", "ns/obj", "Mobj/s", "footprint", "peak");
    for (label, phase) in [("free on own thread", "xlocal"), ("free on other thread", "xthread")] {
        match best3(phase, "global") {
            Some(r) => println!(
                "   {:<22} {:>9.1} {:>9.1} {:>10.1}Mi {:>10.1}Mi",
                label, r["ns"], r["mobjs"], mib(r["footprint"] as u64), mib(r["peak"] as u64)
            ),
            None => println!("   {label}: failed"),
        }
    }

    println!("\n-- lane 3: SharedHeap, same producer/consumer pattern --");
    match best3("xthread", "shared") {
        Some(r) => println!(
            "   shared, cross-thread   {:>9.1} {:>9.1} {:>10.1}Mi {:>10.1}Mi  pages={}",
            r["ns"], r["mobjs"], mib(r["footprint"] as u64), mib(r["peak"] as u64), r["pages"]
        ),
        None => stub("lane 3", "SharedHeap is todo!()"),
    }

    println!("\n-- sanity --");
    println!("   bump is not a free lunch on churn: it never reclaims, so res1/live1 above shows total-ever-allocated, not live");
    println!("   every row above passed the guard in best3: >= 1 ns per operation (a hoisted loop would print ~0)");
    println!("   times are the best of 3 fresh processes; memory columns are the median of the same 3 (resident = ri_resident_size on macOS, RSS on Linux; footprint = ri_phys_footprint, which can read lower than resident when macOS compresses pages)");
}
