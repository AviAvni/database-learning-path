//! dyngraph_bench — what a CSR costs when it has to take writes.
//!
//! cargo run --release --bin dyngraph_bench

use dyngraph_experiments::{
    csr::{Csr, EagerCsr},
    delta::{CsrDelta, Tombstones},
    gen::{degree_stats, rmat, workload, EdgeList, GraphParams, Workload},
    pma::PmaCsr,
    segblocks::SegBlocks,
    store::{run_mix, scan_ns_per_edge, GraphRead, GraphWrite, MixResult},
    vecvec::VecVec,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STUBBED: AtomicBool = AtomicBool::new(false);

const BLOCK: usize = 1000;
const BLOCKS: usize = 200;
const N_STARTS: usize = 200_000;
const SCAN_STARTS: usize = 2_000;
const N_WRITES: usize = 1_000_000;
/// FalkorDB's default flush bound, `DELTA_MAX_PENDING_CHANGES_DEFAULT`.
const FALKOR_T: usize = 10_000;

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

fn main() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
    println!("=== dyngraph_bench — topic 51 ===");
    println!("load average (1/5/15 min) at start: {}\n", load_average());

    let t = Instant::now();
    let p = GraphParams::LANE1;
    let el = rmat(p);
    let wl = workload(&el, p.scale, N_STARTS, N_WRITES);
    let st = degree_stats(&el);
    println!("graph: R-MAT scale {} (a=.57 b=.19 c=.19), seed {}, {} draws/vertex, ids permuted", p.scale, p.seed, p.edge_factor);
    println!(
        "   |V| = {}   |E| = {} after de-dup (directed)   mean out-degree {:.2}   max {}   isolated {}   top 1% of vertices own {:.1}% of edges   (built in {:.1} s)",
        el.n, el.edges.len(), st.mean, st.max, st.isolated, 100.0 * st.top1pct_share, t.elapsed().as_secs_f64()
    );

    lane1a_splice(&el, &wl);
    lane1b_scan(&el, &wl);
    lane1c_mix(&el, &wl);
    lane1d_growth(&el, &wl);
    lane1e_threshold(&el, &wl);
    lane1f_memory(&el, &wl);
    lanes23(&el, &wl);

    println!("\nload average (1/5/15 min) at end: {}", load_average());
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/segblocks.rs (lane 2) and src/pma.rs (lane 3) to unlock the rows marked STUB]");
    }
}

fn csr(el: &EdgeList) -> Csr {
    Csr::from_edges(el.n, &el.edges)
}

fn lane1a_splice(el: &EdgeList, wl: &Workload) {
    println!("\n-- lane 1a: why not just splice? one write into an eager CSR --");
    let mut g = EagerCsr(csr(el));
    let k = 100;
    let t = Instant::now();
    let mut done = 0;
    for &(u, v) in wl.inserts.iter().take(k * 2) {
        done += g.insert(u, v) as usize;
        if done == k {
            break;
        }
    }
    let ins_ms = t.elapsed().as_secs_f64() * 1e3 / done as f64;
    let t = Instant::now();
    let mut gone = 0;
    for &(u, v) in wl.deletes.iter().take(k * 2) {
        gone += g.delete(u, v) as usize;
        if gone == k {
            break;
        }
    }
    let del_ms = t.elapsed().as_secs_f64() * 1e3 / gone as f64;
    let bytes = 4 * el.edges.len() + 4 * el.n as usize;
    println!("   edge array {:.1} MB + offsets {:.1} MB; a uniformly placed write moves about half of the {:.1} MB of both", 4.0 * el.edges.len() as f64 / 1e6, 4.0 * el.n as f64 / 1e6, bytes as f64 / 1e6);
    println!("   insert (random position): {:.3} ms each over {} inserts    delete: {:.3} ms each over {} deletes", ins_ms, done, del_ms, gone);
    println!("   → {:.0} inserts/s; the 10,000 edges of topic 20's write burst would take {:.1} s", 1e3 / ins_ms, ins_ms * 10_000.0 / 1e3);
    println!("   sanity: {:.3} ms per splice moved ~{:.0} MB → {:.0} GB/s, below any memmove this machine can do: {}", ins_ms, bytes as f64 / 2e6, bytes as f64 / 2e6 / ins_ms, if bytes as f64 / 2e9 / (ins_ms / 1e3) < 400.0 { "ok" } else { "IMPLAUSIBLE" });
}

fn lane1b_scan(el: &EdgeList, wl: &Workload) {
    println!("\n-- lane 1b: two-hop scan, nothing pending ({} queries from seeded starts, best of 5) --", SCAN_STARTS);
    let starts = &wl.starts[..SCAN_STARTS];
    println!("   {:<28} {:>13} {:>12} {:>18}", "store", "edges touched", "ns / edge", "checksum");
    let a = csr(el);
    let (n1, e1, c1) = scan_ns_per_edge(&a, starts);
    println!("   {:<28} {:>13} {:>12.3} {:>18x}", a.name(), e1, n1, c1);
    let d = CsrDelta::new(csr(el), FALKOR_T);
    let (n2, e2, c2) = scan_ns_per_edge(&d, starts);
    println!("   {:<28} {:>13} {:>12.3} {:>18x}", "CSR + empty delta", e2, n2, c2);
    let v = VecVec::from_edges(el);
    let (n3, e3, c3) = scan_ns_per_edge(&v, starts);
    println!("   {:<28} {:>13} {:>12.3} {:>18x}", v.name(), e3, n3, c3);
    println!("   → Vec<Vec<u32>> is {:.2}x the CSR's ns/edge; an empty delta costs {:.2}x", n3 / n1, n2 / n1);
    println!("   sanity: three stores, one answer — edges {} / {} / {}, checksums {}: {}", e1, e2, e3, if c1 == c2 && c2 == c3 { "equal" } else { "DIFFER" }, if e1 == e2 && e2 == e3 && c1 == c2 && c2 == c3 { "agree" } else { "DISAGREE" });
    println!("   sanity: {:.3} ns/edge = {:.1} GB/s of 4-byte edges, against ~150 GB/s unified-memory peak: {}", n1, 4.0 / n1, if 4.0 / n1 < 150.0 { "ok" } else { "IMPLAUSIBLE" });
}

fn row_header() {
    println!(
        "   {:<28} {:>9} {:>11} {:>12} {:>11} {:>8} {:>10} {:>18}",
        "store", "writes", "scan ns/edge", "write ns/op", "writes ok", "flushes", "max flush ms", "checksum"
    );
}

fn row(r: &MixResult, name: &str) {
    println!(
        "   {:<28} {:>9} {:>11.3} {:>12.0} {:>11} {:>8} {:>10.2} {:>18x}",
        name, r.writes_tried, r.ns_per_edge(), r.ns_per_write(), r.writes_applied, r.maint.flushes, r.maint.flush_ns_max as f64 / 1e6, r.checksum
    );
}

fn lane1c_mix(el: &EdgeList, wl: &Workload) {
    println!("\n-- lane 1c: two-hop queries interleaved with writes, {} blocks of {} ops (queries first, then the block's writes; the splice store runs at 1% only, 10% would take over a minute) --", BLOCKS, BLOCK);
    for (label, pct, del) in [("0% writes", 0usize, 0usize), ("1% writes (all inserts)", 1, 0), ("10% writes (all inserts)", 10, 0), ("10% writes (half inserts, half deletes)", 10, 50)] {
        println!("\n   {}", label);
        row_header();
        let mut sums: Vec<(String, u64, usize)> = Vec::new();
        if pct == 0 {
            let g = csr(el);
            let mut e = 0;
            let mut c = 0u64;
            let mut qn = 0u64;
            let mut s = 0usize;
            for _ in 0..BLOCKS {
                let (de, dc, ns) = dyngraph_experiments::store::run_queries(&g, &wl.starts[s..s + BLOCK]);
                s += BLOCK;
                e += de;
                c = c.wrapping_add(dc);
                qn += ns as u64;
            }
            println!("   {:<28} {:>9} {:>11.3} {:>12} {:>11} {:>8} {:>10} {:>18x}", "static CSR", 0, qn as f64 / e as f64, "—", "—", "—", "—", c);
            sums.push(("static CSR".into(), c, el.edges.len()));
        }
        {
            let mut g = CsrDelta::new(csr(el), FALKOR_T);
            let r = run_mix(&mut g, wl, BLOCKS, BLOCK, pct, del);
            row(&r, &g.name());
            sums.push((g.name(), r.checksum, r.final_edges));
        }
        if del > 0 {
            let mut g = CsrDelta::new(csr(el), FALKOR_T).with_tombstones(Tombstones::PerEdge);
            let r = run_mix(&mut g, wl, BLOCKS, BLOCK, pct, del);
            row(&r, &g.name());
            sums.push((g.name(), r.checksum, r.final_edges));
        }
        {
            let mut g = VecVec::from_edges(el);
            let r = run_mix(&mut g, wl, BLOCKS, BLOCK, pct, del);
            row(&r, &g.name());
            sums.push((g.name(), r.checksum, r.final_edges));
        }
        if pct == 1 {
            let mut g = EagerCsr(csr(el));
            let r = run_mix(&mut g, wl, BLOCKS, BLOCK, pct, del);
            row(&r, &g.name());
            sums.push((g.name(), r.checksum, r.final_edges));
        }
        let ok = sums.windows(2).all(|w| w[0].1 == w[1].1 && w[0].2 == w[1].2);
        println!("   sanity: {} stores, one logical graph — checksums and final edge counts {} (final |E| {})", sums.len(), if ok { "agree" } else { "DISAGREE" }, sums[0].2);
    }
}

fn lane1d_growth(el: &EdgeList, wl: &Workload) {
    println!("\n-- lane 1d: delta growth — scan cost as pending changes pile up (no flush; D inserts + D/10 deletes applied, then {} queries, best of 5) --", SCAN_STARTS);
    let starts = &wl.starts[..SCAN_STARTS];
    println!("   {:>9} {:>9} {:>9} {:>11} {:>9} {:>14} {:>12}", "D", "pending", "DP rows", "scan ns/edge", "× empty", "delta bytes/edge", "apply ns/op");
    let mut base = 0.0;
    for d in [0usize, 1_000, 10_000, 100_000, 1_000_000] {
        let mut g = CsrDelta::new(csr(el), usize::MAX);
        let t = Instant::now();
        for i in 0..d {
            g.insert(wl.inserts[i].0, wl.inserts[i].1);
        }
        for i in 0..d / 10 {
            g.delete(wl.deletes[i].0, wl.deletes[i].1);
        }
        let apply = t.elapsed().as_nanos() as f64 / (d + d / 10).max(1) as f64;
        let (ns, _, _) = scan_ns_per_edge(&g, starts);
        if d == 0 {
            base = ns;
        }
        let delta_bytes = g.bytes() - g.settled().bytes_used();
        println!(
            "   {:>9} {:>9} {:>9} {:>11.3} {:>9.2} {:>14.2} {:>12.0}",
            d, g.pending(), g.dp_rows(), ns, ns / base, delta_bytes as f64 / (g.pending().max(1)) as f64, apply
        );
    }
    println!("   (delta bytes/edge is per pending change, not per graph edge)");
}

fn lane1e_threshold(el: &EdgeList, wl: &Workload) {
    println!("\n-- lane 1e: the flush threshold, at 10% inserts ({} blocks of {}) --", BLOCKS, BLOCK);
    println!("   {:>10} {:>11} {:>12} {:>8} {:>12} {:>12} {:>14}", "threshold", "scan ns/edge", "write ns/op", "flushes", "max flush ms", "mean flush ms", "write ns/op ex-flush");
    for t in [1_000usize, 10_000, 100_000, usize::MAX] {
        let mut g = CsrDelta::new(csr(el), t);
        let r = run_mix(&mut g, wl, BLOCKS, BLOCK, 10, 0);
        let fl = r.maint.flush_ns_total as f64;
        let mean = if r.maint.flushes > 0 { fl / r.maint.flushes as f64 / 1e6 } else { 0.0 };
        let ex = (r.write_ns as f64 - fl) / r.writes_tried as f64;
        println!("   {:>10} {:>11.3} {:>12.0} {:>8} {:>12.2} {:>12.2} {:>14.0}", if t == usize::MAX { "never".to_string() } else { t.to_string() }, r.ns_per_edge(), r.ns_per_write(), r.maint.flushes, r.maint.flush_ns_max as f64 / 1e6, mean, ex);
    }
    println!("   ({} writes per run; the last row never flushes)", BLOCKS * BLOCK / 10);
}

fn lane1f_memory(el: &EdgeList, wl: &Workload) {
    println!("\n-- lane 1f: bytes per edge, accounted from capacities (allocator overhead excluded) --");
    let e = el.edges.len() as f64;
    let c = csr(el);
    println!("   {:<44} {:>11}", "static CSR (u32 offsets + u32 edges)", format!("{:.2} B/edge", c.bytes() as f64 / e));
    let mut d = CsrDelta::new(csr(el), usize::MAX);
    for i in 0..100_000 {
        d.insert(wl.inserts[i].0, wl.inserts[i].1);
    }
    println!("   {:<44} {:>11}", "CSR + delta, 100,000 pending inserts", format!("{:.2} B/edge", d.bytes() as f64 / e));
    let v = VecVec::from_edges(el);
    println!("   {:<44} {:>11}", "Vec<Vec<u32>> (24 B header per vertex)", format!("{:.2} B/edge", v.bytes() as f64 / e));
    println!("   (FalkorDB's GraphBLAS arrays use 8-byte indices: roughly double the CSR row above, and a transposed twin doubles it again.)");
}

fn lane_block<G: GraphWrite>(label: &str, build: impl Fn() -> G, el: &EdgeList, wl: &Workload, ref_cks: &[u64; 3]) {
    println!("\n   {label}");
    let g = build();
    let (idle, _, _) = scan_ns_per_edge(&g, &wl.starts[..SCAN_STARTS]);
    println!("      nothing pending: scan {:.3} ns/edge, {:.2} B/edge", idle, g.bytes() as f64 / el.edges.len() as f64);
    drop(g);
    row_header();
    for (i, (name, pct, del)) in [("1% inserts", 1usize, 0usize), ("10% inserts", 10, 0), ("10% half inserts, half deletes", 10, 50)].into_iter().enumerate() {
        let mut g = build();
        let r = run_mix(&mut g, wl, BLOCKS, BLOCK, pct, del);
        row(&r, name);
        println!("      checksum {} the CSR+delta run's", if r.checksum == ref_cks[i] { "matches" } else { "DIFFERS FROM" });
    }
}

fn lanes23(el: &EdgeList, wl: &Workload) {
    println!("\n-- lanes 2 and 3: the exercise layouts, same three write mixes as lane 1c --");
    let mut ref_cks = [0u64; 3];
    for (i, (pct, del)) in [(1usize, 0usize), (10, 0), (10, 50)].into_iter().enumerate() {
        let mut g = CsrDelta::new(csr(el), FALKOR_T);
        ref_cks[i] = run_mix(&mut g, wl, BLOCKS, BLOCK, pct, del).checksum;
    }
    for lane in 2..=3 {
        let res = catch_unwind(AssertUnwindSafe(|| {
            if lane == 2 {
                lane_block("lane 2 — sorted segmented blocks (block capacity 256)", || SegBlocks::from_edges(el, 256), el, wl, &ref_cks);
            } else {
                lane_block("lane 3 — packed-memory-array CSR (leaf 16)", || PmaCsr::from_edges(el, 16), el, wl, &ref_cks);
            }
        }));
        if res.is_err() {
            println!("\n   lane {lane} STUB — not implemented yet");
        }
    }
}
