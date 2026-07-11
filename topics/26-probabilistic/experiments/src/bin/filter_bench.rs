//! filter_bench — the motivation lanes (what a lookup costs WITHOUT these
//! structures) plus the stub lanes.

use probabilistic_experiments::bloom::{standard_fpr, BlockedBloom};
use probabilistic_experiments::cuckoo::CuckooFilter;
use probabilistic_experiments::hll::Hll;
use probabilistic_experiments::pgm::LearnedIndex;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::{BTreeMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

const N: usize = 10_000_000;
const Q: usize = 1_000_000;

fn main() {
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let mut keys: Vec<u64> = (0..N).map(|_| rng.gen::<u64>() | 1).collect(); // odd = present
    keys.sort_unstable();
    keys.dedup();
    let present: Vec<u64> = (0..Q).map(|_| keys[rng.gen_range(0..keys.len())]).collect();
    let absent: Vec<u64> = (0..Q).map(|_| rng.gen::<u64>() & !1).collect(); // even = absent
    println!("{} sorted u64 keys, {} queries each lane", keys.len(), Q);

    // --- provided: what a point-miss costs today ---
    let t = Instant::now();
    let mut hits = 0usize;
    for &q in &absent {
        hits += keys.binary_search(&q).is_ok() as usize;
    }
    println!(
        "binary search (miss): {:.0} ns/lookup  ({} hits)",
        t.elapsed().as_nanos() as f64 / Q as f64,
        hits
    );

    let btree: BTreeMap<u64, ()> = keys.iter().map(|&k| (k, ())).collect();
    let t = Instant::now();
    let mut hits = 0usize;
    for &q in &absent {
        hits += btree.contains_key(&q) as usize;
    }
    println!(
        "BTreeMap (miss):      {:.0} ns/lookup  ({} hits)",
        t.elapsed().as_nanos() as f64 / Q as f64,
        hits
    );

    let hset: HashSet<u64> = keys.iter().copied().collect();
    let t = Instant::now();
    let mut hits = 0usize;
    for &q in &absent {
        hits += hset.contains(&q) as usize;
    }
    println!(
        "HashSet (miss):       {:.0} ns/lookup  ({} hits)  [{} MB]",
        t.elapsed().as_nanos() as f64 / Q as f64,
        hits,
        hset.capacity() * 8 * 2 / (1 << 20) // rough
    );

    // hit-path binary search for the learned-index comparison
    let t = Instant::now();
    let mut found = 0usize;
    for &q in &present {
        found += keys.binary_search(&q).is_ok() as usize;
    }
    println!(
        "binary search (hit):  {:.0} ns/lookup  ({} found)",
        t.elapsed().as_nanos() as f64 / Q as f64,
        found
    );

    // --- stub: blocked bloom ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        for bpk in [8usize, 10, 16] {
            let t = Instant::now();
            let mut b = BlockedBloom::new(keys.len(), bpk);
            for &k in &keys {
                b.insert(k);
            }
            let build = t.elapsed();
            let t = Instant::now();
            let fpr = b.measured_fpr(&absent);
            let ns = t.elapsed().as_nanos() as f64 / Q as f64;
            println!(
                "blocked bloom {:2} bpk: {:.0} ns/query  fpr {:.4} (theory {:.4})  {} MB, build {:?}",
                bpk,
                ns,
                fpr,
                standard_fpr(bpk as f64, 6),
                b.size_bytes() / (1 << 20),
                build
            );
        }
    }));
    if r.is_err() {
        println!("blocked bloom: [stub — implement bloom::BlockedBloom]");
    }

    // --- stub: cuckoo filter ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        let t = Instant::now();
        let mut f = CuckooFilter::new(keys.len() * 10 / 9);
        let mut fails = 0usize;
        for &k in &keys {
            fails += !f.insert(k) as usize;
        }
        let build = t.elapsed();
        let t = Instant::now();
        let fp = absent.iter().filter(|&&k| f.contains(k)).count();
        let ns = t.elapsed().as_nanos() as f64 / Q as f64;
        println!(
            "cuckoo 12-bit fp:     {:.0} ns/query  fpr {:.4}  load {:.2} ({} insert fails)  build {:?}",
            ns,
            fp as f64 / Q as f64,
            f.load_factor(),
            fails,
            build
        );
    }));
    if r.is_err() {
        println!("cuckoo: [stub — implement cuckoo::CuckooFilter]");
    }

    // --- stub: HLL ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        let t = Instant::now();
        let mut h = Hll::new();
        for &k in &keys {
            h.add(k);
        }
        let est = h.count();
        println!(
            "hll P=14 (16 KB):     est {:.0} vs true {} (err {:.3}%)  {:?} total",
            est,
            keys.len(),
            (est - keys.len() as f64).abs() / keys.len() as f64 * 100.0,
            t.elapsed()
        );
    }));
    if r.is_err() {
        println!("hll: [stub — implement hll::Hll]");
    }

    // --- stub: learned index vs binary search (hit path) ---
    let r = catch_unwind(AssertUnwindSafe(|| {
        for eps in [16usize, 64, 256] {
            let t = Instant::now();
            let idx = LearnedIndex::build(&keys, eps);
            let build = t.elapsed();
            let t = Instant::now();
            let mut found = 0usize;
            for &q in &present {
                found += idx.lookup(&keys, q).is_some() as usize;
            }
            let ns = t.elapsed().as_nanos() as f64 / Q as f64;
            println!(
                "learned eps={:3}:      {:.0} ns/lookup  ({} found)  {} segments ({} KB), build {:?}",
                eps,
                ns,
                found,
                idx.segments.len(),
                idx.segments.len() * 24 / 1024,
                build
            );
        }
    }));
    if r.is_err() {
        println!("learned index: [stub — implement pgm::LearnedIndex]");
    }
}
