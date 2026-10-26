//! compress_bench — bytes per edge and scan time as functions of vertex ids.
//!
//! cargo run --release --bin compress_bench

use graph_compression_experiments::{
    bisect::bisection_order,
    csr::Csr,
    gapcode::{gamma_bits, log_gap, simulate_gather_misses, VarintGraph},
    gen::{community_graph, shuffle_perm, GenParams},
    measure::{bfs, scan_csr},
    order::{bfs_order, degree_order, invert, is_permutation, random_order},
    refcode::encode,
};
use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
}

fn best_ms<R>(reps: usize, mut f: impl FnMut() -> R) -> (f64, R) {
    let mut best = f64::MAX;
    let mut out = None;
    for _ in 0..reps {
        let t = Instant::now();
        let r = black_box(f());
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        out = Some(r);
    }
    (best, out.unwrap())
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

struct Row {
    name: String,
    order_ms: f64,
    log_gap: f64,
    gamma: f64,
    varint: f64,
    varint_all: f64,
    miss_pct: f64,
    csr_ns: f64,
    var_ns: f64,
    bfs_ms: f64,
    checks: (u64, (usize, usize, usize)),
}

const CACHE_BYTES: usize = 4 << 20;

fn measure(name: &str, order_ms: f64, base: &Csr, vals: &[u64], srcs: &[usize], new_id: &[u32]) -> Row {
    assert!(is_permutation(new_id), "{name}: not a permutation");
    let g = base.permute(new_id);
    let mut v2 = vec![0u64; g.n];
    for (old, &nw) in new_id.iter().enumerate() {
        v2[nw as usize] = vals[old];
    }
    let m = g.m() as f64;
    let vg = VarintGraph::encode(&g);
    let (a, b) = vg.bits_per_edge(g.m());
    let (_, miss) = simulate_gather_misses(&g, CACHE_BYTES);
    let (csr_ms, c1) = best_ms(5, || scan_csr(&g, &v2));
    let (var_ms, c2) = best_ms(5, || vg.scan(&v2));
    assert_eq!(c1, c2, "{name}: varint scan disagrees with CSR scan");
    let (bfs_ms, tri) = best_ms(2, || {
        let mut t = (0, 0, 0);
        for &s in srcs {
            let r = bfs(&g, new_id[s] as usize);
            t = (t.0 + r.0, t.1 + r.1, t.2 + r.2);
        }
        t
    });
    Row {
        name: name.to_string(),
        order_ms,
        log_gap: log_gap(&g),
        gamma: gamma_bits(&g) as f64 / m,
        varint: a,
        varint_all: b,
        miss_pct: 100.0 * miss as f64 / m,
        csr_ns: csr_ms * 1e6 / m,
        var_ns: var_ms * 1e6 / m,
        bfs_ms,
        checks: (c1, tri),
    }
}

fn print_header() {
    println!(
        "   {:<22} {:>9} {:>7} {:>7} {:>7} {:>8} {:>7} {:>8} {:>8} {:>8}",
        "ordering", "order ms", "LogGap", "gamma", "varint", "+vertex", "miss %", "CSR ns/e", "var ns/e", "BFS ms"
    );
}

fn print_row(r: &Row) {
    println!(
        "   {:<22} {:>9.0} {:>7.2} {:>7.2} {:>7.2} {:>8.2} {:>7.1} {:>8.2} {:>8.2} {:>8.1}",
        r.name, r.order_ms, r.log_gap, r.gamma, r.varint, r.varint_all, r.miss_pct, r.csr_ns, r.var_ns, r.bfs_ms
    );
}

/// Returns false if the lane is still a stub.
fn lane3(title: &str, base: &Csr, vals: &[u64], candidates: &[(&str, Vec<u32>)]) -> bool {
    println!("   {title}: n = {}, m = {}", base.n, base.m());
    println!(
        "   {:<22} {:>10} {:>9} {:>9} {:>8} {:>10} {:>11} {:>9}",
        "ordering", "gaps-only", "varint", "BV coded", "enc ms", "BV ns/e", "varint ns/e", "CSR ns/e"
    );
    for (name, p) in candidates {
        let g = base.permute(p);
        let mut v2 = vec![0u64; g.n];
        for (old, &nw) in p.iter().enumerate() {
            v2[nw as usize] = vals[old];
        }
        let t = Instant::now();
        let enc = catch_unwind(AssertUnwindSafe(|| (encode(&g, 0, 0, usize::MAX), encode(&g, 7, 3, 4))));
        let enc_ms = t.elapsed().as_secs_f64() * 1e3 / 2.0;
        match enc {
            Ok((gaps, e)) => {
                let vg = VarintGraph::encode(&g);
                let m = g.m() as f64;
                let (bv_ms, s) = best_ms(3, || e.decode_scan(&v2));
                let (vr_ms, _) = best_ms(3, || vg.scan(&v2));
                let (cs_ms, c) = best_ms(3, || scan_csr(&g, &v2));
                assert_eq!(s, c, "{name}: decode_scan disagrees with CSR scan");
                println!(
                    "   {:<22} {:>10.2} {:>9.2} {:>9.2} {:>8.0} {:>10.2} {:>11.2} {:>9.2}",
                    name,
                    gaps.bits_per_edge(),
                    vg.bits_per_edge(g.m()).0,
                    e.bits_per_edge(),
                    enc_ms,
                    bv_ms * 1e6 / m,
                    vr_ms * 1e6 / m,
                    cs_ms * 1e6 / m
                );
            }
            Err(_) => {
                println!("   STUB — refcode::encode is todo!()");
                return false;
            }
        }
    }
    true
}

fn main() {
    quiet_stubs();
    println!("=== compress_bench — topic 52 ===");
    println!("load average (1/5/15 min) at start: {}\n", load_average());

    let t = Instant::now();
    let planted = community_graph(GenParams::LANE1);
    let gen_ms = t.elapsed().as_secs_f64() * 1e3;
    let load = shuffle_perm(planted.g.n, 1); // planted id -> loaded id
    let base = planted.g.permute(&load);
    let n = base.n;
    let m = base.m();
    let maxd = (0..n).map(|v| base.degree(v)).max().unwrap();
    let (plain, plain_all) = base.plain_bits_per_edge();
    println!("-- lane 1: the same graph under different vertex ids --");
    println!(
        "   n = {n}, m = {m} edges (mean out-degree {:.2}, max {maxd}), {} planted communities, generated in {gen_ms:.0} ms",
        m as f64 / n as f64,
        planted.community.iter().max().unwrap() + 1
    );
    println!("   loaded with random ids; plain CSR = {plain:.0} bits/edge ({plain_all:.2} with the offset array)");
    println!("   values: one u64 per vertex ({} MB); simulated cache: 8-way LRU, {} MiB, 64 B lines\n", n * 8 >> 20, CACHE_BYTES >> 20);

    let vals: Vec<u64> = (0..n as u64).map(|x| (x ^ 0x5bd1e995).wrapping_mul(0x9E3779B97F4A7C15)).collect();
    let mut by_deg: Vec<usize> = (0..n).collect();
    by_deg.sort_by_key(|&v| (std::cmp::Reverse(base.degree(v)), v));
    let srcs: Vec<usize> = by_deg[..3].to_vec();

    let mut rows: Vec<Row> = Vec::new();
    let timed = |f: &dyn Fn() -> Vec<u32>| {
        let t = Instant::now();
        let p = f();
        (p, t.elapsed().as_secs_f64() * 1e3)
    };
    let (p, ms) = timed(&|| random_order(n, 2));
    rows.push(measure("random (as loaded)", ms, &base, &vals, &srcs, &p));
    let (p, ms) = timed(&|| degree_order(&base));
    rows.push(measure("in-degree sorted", ms, &base, &vals, &srcs, &p));
    let (p, ms) = timed(&|| bfs_order(&base));
    rows.push(measure("BFS", ms, &base, &vals, &srcs, &p));
    let oracle = invert(&load);
    rows.push(measure("planted (oracle)", 0.0, &base, &vals, &srcs, &oracle));

    print_header();
    for r in &rows {
        print_row(r);
    }

    let first = &rows[0].checks;
    let same = rows.iter().all(|r| r.checks == *first);
    let (reached, edges, levels) = first.1;
    println!(
        "\n   sanity: scan checksum {:016x} and BFS (reached, edges, depth) = ({reached}, {edges}, {levels}) summed over 3 sources are identical under every labelling: {}",
        first.0,
        if same { "ok" } else { "MISMATCH" }
    );
    let fastest = rows.iter().map(|r| r.csr_ns).fold(f64::MAX, f64::min);
    println!(
        "   sanity: fastest CSR scan {:.2} ns/edge = {:.1} G edges/s — {} 20 G/s, so the loop did real work: {}",
        fastest,
        1.0 / fastest,
        "below",
        if 1.0 / fastest < 20.0 { "ok" } else { "IMPLAUSIBLE" }
    );
    let r0 = &rows[0];
    let rp = &rows[3];
    println!(
        "   planted vs random: varint {:.2} vs {:.2} bits/edge ({:.1}x smaller), CSR scan {:.2} vs {:.2} ns/edge ({:.1}x faster), simulated misses {:.1}% vs {:.1}%",
        rp.varint, r0.varint, r0.varint / rp.varint, rp.csr_ns, r0.csr_ns, r0.csr_ns / rp.csr_ns, rp.miss_pct, r0.miss_pct
    );

    // ---- lane 2 ----
    println!("\n-- lane 2: recursive graph bisection (leaf 32, 20 iterations) --");
    let t = Instant::now();
    let bp = catch_unwind(AssertUnwindSafe(|| bisection_order(&base, 32, 20, 1)));
    let bp_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut bp_perm: Option<Vec<u32>> = None;
    match bp {
        Ok(p) => {
            let row = measure("bisection (BP)", bp_ms, &base, &vals, &srcs, &p);
            print_header();
            print_row(&row);
            bp_perm = Some(p);
        }
        Err(_) => println!("   STUB — bisect::bisection_order is todo!()"),
    }

    // ---- lane 3 ----
    println!("\n-- lane 3: WebGraph-style reference + interval + residual coding (W=7, R=3, Lmin=4) --");
    let mut candidates: Vec<(&str, Vec<u32>)> = vec![
        ("random (as loaded)", random_order(n, 2)),
        ("BFS", bfs_order(&base)),
        ("planted (oracle)", oracle.clone()),
    ];
    if let Some(p) = bp_perm {
        candidates.push(("bisection (BP)", p));
    }
    let mut ok = lane3("lane-1 graph (independent lists)", &base, &vals, &candidates);
    if ok {
        drop(candidates);
        let web = community_graph(GenParams::WEBLIKE);
        let wbase = web.g.permute(&load);
        let wc: Vec<(&str, Vec<u32>)> =
            vec![("random (as loaded)", random_order(n, 2)), ("planted (oracle)", oracle.clone())];
        ok = lane3("web-like graph (40% of lists are noisy copies of the previous one)", &wbase, &vals, &wc);
    }
    let _ = ok;

    println!("\nload average (1/5/15 min) at end: {}", load_average());
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/bisect.rs (lane 2) and src/refcode.rs (lane 3) to unlock the lanes marked STUB]");
    }
}
