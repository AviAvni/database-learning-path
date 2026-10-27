//! Topic 57 bench. Lane 1 (provided) runs today; lanes 2 and 3 print STUB until
//! you implement `normalize.rs`/`radix.rs` and `collation::sort_key`.
//!
//! `SORT_SCALE=10 cargo run --release --bin sort_bench` divides every row
//! count by 10 for a quick look. Counts and checksums are seeded and exact;
//! timings are the best of three runs of thread CPU time, each on a fresh copy of the input.

use sorting_experiments::baseline::*;
use sorting_experiments::collation::sort_by_sort_keys;
use sorting_experiments::gen::*;
use sorting_experiments::radix;
use sorting_experiments::{cpu_ns, IntStr, Row64, StrSet};
use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|_| STUBBED.store(true, Ordering::Relaxed)));
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

/// Best of three: `setup` makes a fresh input (untimed), `run` sorts it (timed).
fn best3<I, R>(mut setup: impl FnMut() -> I, mut run: impl FnMut(&mut I) -> R) -> (f64, I) {
    let mut best = f64::MAX;
    let mut last = None;
    for _ in 0..3 {
        let mut input = setup();
        let t = cpu_ns();
        black_box(run(&mut input));
        let ms = (cpu_ns() - t) as f64 / 1e6;
        best = best.min(ms);
        last = Some(input);
    }
    (best, last.unwrap())
}

fn ns_per_nlogn(ms: f64, n: usize) -> f64 {
    ms * 1e6 / (n as f64 * (n as f64).log2())
}

fn scaled(n: usize) -> usize {
    let s: usize = std::env::var("SORT_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    (n / s.max(1)).max(1000)
}

fn commas(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn plausible(name: &str, ms: f64, n: usize) {
    let v = ns_per_nlogn(ms, n);
    // Below ~0.05 ns per n·log2(n) is faster than one cycle per comparison on 16 cores' worth of
    // work; above 500 ns means the timer saw something other than a sort.
    let ok = v > 0.05 && v < 500.0;
    println!("   sanity: {name} {v:.2} ns/(n·log2 n) — {}", if ok { "ok" } else { "IMPLAUSIBLE" });
}

fn main() {
    quiet_stubs();
    println!("=== sort_bench — topic 57 ===");
    println!("load average (1/5/15 min) at start: {}\n", load_average());

    let na = scaled(10_000_000);
    let nb = scaled(5_000_000);
    let nc = scaled(2_000_000);

    lane1a(na);
    lane1b(nb);
    lane1c(nc);
    lane2(na, nb);
    lane3(nc);

    println!("\nload average (1/5/15 min) at end: {}", load_average());
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/normalize.rs + src/radix.rs (lane 2) and collation::sort_key / sort_by_sort_keys (lane 3) to unlock the lanes marked STUB]");
    }
}

fn lane1a(n: usize) {
    println!("-- lane 1a: u64 keys, n = {} (Row64 = 16 B: key + payload) --", commas(n));
    let base = gen_u64(n, SEED);
    let cs = checksum_u64(&base);
    let keys: Vec<u64> = base.iter().map(|r| r.key).collect();

    let (floor, _) = best3(|| keys.clone(), |v| v.sort_unstable());
    let (cmp, sorted) = best3(|| base.clone(), |v| sort_u64_comparator(v));
    let (stable, sorted2) = best3(|| base.clone(), |v| sort_u64_stable_comparator(v));
    println!("   {:<52} {:>9.1} ms  {:.2} ns/(n·log2 n)", "sort_unstable on bare u64 (ipnsort, no payload)", floor, ns_per_nlogn(floor, n));
    println!("   {:<52} {:>9.1} ms  {:.2} ns/(n·log2 n)", "sort_unstable_by comparator over Row64", cmp, ns_per_nlogn(cmp, n));
    println!("   {:<52} {:>9.1} ms  {:.2} ns/(n·log2 n)", "sort_by (stable, driftsort) comparator over Row64", stable, ns_per_nlogn(stable, n));
    println!("   rows/s: {:.1} M (comparator)   sorted bytes/s: {:.2} GB/s", n as f64 / cmp / 1e3, n as f64 * 16.0 / cmp / 1e6);
    println!("   sanity: sorted + checksum {:016x} preserved: {}", cs, if is_sorted_u64(&sorted) && is_sorted_u64(&sorted2) && checksum_u64(&sorted) == cs && checksum_u64(&sorted2) == cs { "ok" } else { "FAIL" });
    plausible("comparator", cmp, n);
    println!("\n   n sweep, sort_unstable_by over Row64 (best of 3):");
    println!("   {:>12} {:>10} {:>16}", "n", "ms", "ns/(n·log2 n)");
    for m in [1_000usize, 10_000, 100_000, 1_000_000] {
        let b = gen_u64(m, SEED);
        // repeat small sorts so the timer sees at least ~1M elements
        let reps = (1_000_000 / m).max(1);
        let (ms, _) = best3(|| (0..reps).map(|_| b.clone()).collect::<Vec<_>>(), |vs| {
            for v in vs.iter_mut() {
                sort_u64_comparator(v);
            }
        });
        let per = ms / reps as f64;
        println!("   {:>12} {:>10.3} {:>16.2}", commas(m), per, ns_per_nlogn(per, m));
    }
}

struct IsCase {
    name: &'static str,
    distinct: u32,
    shape: StrShape,
}

const IS_CASES: [IsCase; 3] = [
    IsCase { name: "b1  a in 1,000 values, random strings", distinct: 1000, shape: StrShape::Random },
    IsCase { name: "b2  a over all of i32, random strings", distinct: 0, shape: StrShape::Random },
    IsCase { name: "b3  a in 1,000 values, 20-byte shared prefix", distinct: 1000, shape: StrShape::SharedPrefix20 },
];

fn lane1b(n: usize) {
    println!("\n-- lane 1b: (int, string) keys, n = {}, IntStr = 12 B + out-of-line string --", commas(n));
    for c in &IS_CASES {
        let d = gen_int_str(n, c.distinct, c.shape, SEED);
        let cs = checksum_int_str(&d.rows);
        let ties = {
            let mut a: Vec<i32> = d.rows.iter().map(|r| r.a).collect();
            a.sort_unstable();
            a.windows(2).filter(|w| w[0] == w[1]).count()
        };
        let (ms, sorted) = best3(|| d.rows.clone(), |v| sort_int_str_comparator(v, &d.arena));
        let ok = is_sorted_int_str(&sorted, &d.arena) && checksum_int_str(&sorted) == cs;
        println!("   {:<48} {:>9.1} ms  {:.2} ns/(n·log2 n)  ties on a: {:>9}  checksum {:016x} {}", c.name, ms, ns_per_nlogn(ms, n), commas(ties), cs, if ok { "ok" } else { "FAIL" });
        plausible("comparator", ms, n);
    }
}

fn lane1c(n: usize) {
    println!("\n-- lane 1c: strings under a case/accent-insensitive collation, collating inside the comparator, n = {} --", commas(n));
    let set = gen_collation_strings(n, SEED);
    let avg = set.bytes().len() as f64 / n as f64;
    let (ms, order) = best3(|| (), |_| sort_collating_in_comparator(&set));
    let _ = order;
    let order = sort_collating_in_comparator(&set);
    let classes = collation_classes(&set, &order);
    println!("   {} strings, {:.1} bytes each on average, {} collation classes (equal-collating groups)", commas(n), avg, commas(classes));
    println!("   {:<52} {:>9.1} ms  {:.2} ns/(n·log2 n)", "sort_unstable_by(collate_cmp) over indices", ms, ns_per_nlogn(ms, n));
    println!("   sanity: order is a permutation and collation-sorted: {}", if is_collation_sorted(&set, &order) { "ok" } else { "FAIL" });
    plausible("collating comparator", ms, n);
}

fn lane2(na: usize, nb: usize) {
    println!("\n-- lane 2: radix sort on normalized keys --");
    let base = gen_u64(na, SEED);
    let cs = checksum_u64(&base);
    match catch_unwind(AssertUnwindSafe(|| best3(|| base.clone(), |v: &mut Vec<Row64>| radix::sort_u64(v)))) {
        Ok((ms, sorted)) => {
            let (cmp, _) = best3(|| base.clone(), |v| sort_u64_comparator(v));
            let ok = is_sorted_u64(&sorted) && checksum_u64(&sorted) == cs;
            println!("   u64, n = {}: radix {:.1} ms vs comparator {:.1} ms ({:.2}x)  {}", commas(na), ms, cmp, cmp / ms, if ok { "ok" } else { "FAIL" });
        }
        Err(_) => println!("   u64: STUB"),
    }
    for c in &IS_CASES {
        let d = gen_int_str(nb, c.distinct, c.shape, SEED);
        let cs = checksum_int_str(&d.rows);
        match catch_unwind(AssertUnwindSafe(|| best3(|| d.rows.clone(), |v: &mut Vec<IntStr>| radix::sort_int_str(v, &d.arena)))) {
            Ok((ms, sorted)) => {
                let (cmp, _) = best3(|| d.rows.clone(), |v| sort_int_str_comparator(v, &d.arena));
                let ok = is_sorted_int_str(&sorted, &d.arena) && checksum_int_str(&sorted) == cs;
                println!("   {:<48} radix {:.1} ms vs comparator {:.1} ms ({:.2}x)  {}", c.name, ms, cmp, cmp / ms, if ok { "ok" } else { "FAIL" });
            }
            Err(_) => println!("   {:<48} STUB", c.name),
        }
    }
}

fn lane3(nc: usize) {
    println!("\n-- lane 3: collation sort keys computed once --");
    let set: StrSet = gen_collation_strings(nc, SEED);
    match catch_unwind(AssertUnwindSafe(|| best3(|| (), |_| sort_by_sort_keys(&set)))) {
        Ok((ms, _)) => {
            let order = sort_by_sort_keys(&set);
            let (cmp, _) = best3(|| (), |_| sort_collating_in_comparator(&set));
            println!("   n = {}: sort keys {:.1} ms vs collating comparator {:.1} ms ({:.2}x)  {}", commas(nc), ms, cmp, cmp / ms, if is_collation_sorted(&set, &order) { "ok" } else { "FAIL" });
        }
        Err(_) => println!("   STUB"),
    }
}
